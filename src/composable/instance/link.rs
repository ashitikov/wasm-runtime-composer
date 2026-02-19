use std::sync::Arc;

use wasmtime::component::Val;
use wasmtime::component::types::{ComponentFunc, ComponentInstance, ComponentItem, Type};
use wasmtime::component::{ComponentExportIndex, ResourceAny, ResourceType};

use crate::composable::linker_ops::{
    LinkContext, LinkerOps, ParamsMapper, ValMapper, ResultsMapper, ValVisitor,
};
use crate::error::CompositionError;

use super::ComposableInstance;
use super::NEXT_RESOURCE_TY_ID;
use super::channel::{RawCallData, into_wasmtime_error, send_call};

// ---------------------------------------------------------------------------
// Compiled mapper builders
// ---------------------------------------------------------------------------

type ValNavigator = Arc<
    dyn Fn(&mut Val, &mut dyn ValVisitor<ResourceAny>) -> wasmtime::Result<()> + Send + Sync,
>;

/// Build a navigator for a single `Val`. Returns `None` if the type contains no resources.
fn build_val_navigator(
    ty: &Type,
    resource_map: &[(ResourceType, u32)],
) -> Option<ValNavigator> {
    match ty {
        Type::Own(rt) | Type::Borrow(rt) => {
            let ty_id = resource_map.iter().find(|(r, _)| *r == *rt)?.1;
            Some(Arc::new(move |val, visitor| {
                if let Val::Resource(ra) = val {
                    visitor.visit(ra, ty_id)?;
                }
                Ok(())
            }))
        }
        Type::Record(record) => {
            let subs: Vec<(usize, ValNavigator)> = record
                .fields()
                .enumerate()
                .filter_map(|(i, field)| {
                    build_val_navigator(&field.ty, resource_map).map(|nav| (i, nav))
                })
                .collect();
            if subs.is_empty() {
                return None;
            }
            Some(Arc::new(move |val, visitor| {
                if let Val::Record(fields) = val {
                    for (idx, nav) in &subs {
                        nav(&mut fields[*idx].1, visitor)?;
                    }
                }
                Ok(())
            }))
        }
        Type::List(list) => {
            let inner_ty = list.ty();
            let nav = build_val_navigator(&inner_ty, resource_map)?;
            Some(Arc::new(move |val, visitor| {
                if let Val::List(items) = val {
                    for item in items.iter_mut() {
                        nav(item, visitor)?;
                    }
                }
                Ok(())
            }))
        }
        Type::Tuple(tuple) => {
            let subs: Vec<(usize, ValNavigator)> = tuple
                .types()
                .enumerate()
                .filter_map(|(i, ty)| {
                    build_val_navigator(&ty, resource_map).map(|nav| (i, nav))
                })
                .collect();
            if subs.is_empty() {
                return None;
            }
            Some(Arc::new(move |val, visitor| {
                if let Val::Tuple(vals) = val {
                    for (idx, nav) in &subs {
                        nav(&mut vals[*idx], visitor)?;
                    }
                }
                Ok(())
            }))
        }
        Type::Option(opt) => {
            let inner_ty = opt.ty();
            let nav = build_val_navigator(&inner_ty, resource_map)?;
            Some(Arc::new(move |val, visitor| {
                if let Val::Option(Some(inner)) = val {
                    nav(inner, visitor)?;
                }
                Ok(())
            }))
        }
        Type::Result(result) => {
            let ok_nav = result
                .ok()
                .and_then(|ty| build_val_navigator(&ty, resource_map));
            let err_nav = result
                .err()
                .and_then(|ty| build_val_navigator(&ty, resource_map));
            if ok_nav.is_none() && err_nav.is_none() {
                return None;
            }
            Some(Arc::new(move |val, visitor| {
                match val {
                    Val::Result(Ok(Some(v))) => {
                        if let Some(nav) = &ok_nav {
                            nav(v, visitor)?;
                        }
                    }
                    Val::Result(Err(Some(v))) => {
                        if let Some(nav) = &err_nav {
                            nav(v, visitor)?;
                        }
                    }
                    _ => {}
                }
                Ok(())
            }))
        }
        Type::Variant(variant) => {
            let case_navs: Vec<(String, Option<ValNavigator>)> = variant
                .cases()
                .map(|case| {
                    let nav = case.ty.and_then(|ty| build_val_navigator(&ty, resource_map));
                    (case.name.to_string(), nav)
                })
                .collect();
            if case_navs.iter().all(|(_, nav)| nav.is_none()) {
                return None;
            }
            Some(Arc::new(move |val, visitor| {
                if let Val::Variant(disc, Some(payload)) = val {
                    if let Some((_, Some(nav))) =
                        case_navs.iter().find(|(name, _)| name == disc.as_str())
                    {
                        nav(payload, visitor)?;
                    }
                }
                Ok(())
            }))
        }
        // Primitives, Flags, Enum, String, Future, Stream — no resources
        _ => None,
    }
}

/// Build a `ValMapper` for a function type.
fn build_resource_mapper(
    func_ty: &ComponentFunc,
    resource_map: &[(ResourceType, u32)],
) -> ValMapper {
    if resource_map.is_empty() {
        return ValMapper::noop();
    }

    let param_navs: Vec<(usize, ValNavigator)> = func_ty
        .params()
        .enumerate()
        .filter_map(|(i, (_, ty))| {
            build_val_navigator(&ty, resource_map).map(|nav| (i, nav))
        })
        .collect();

    let result_navs: Vec<(usize, ValNavigator)> = func_ty
        .results()
        .enumerate()
        .filter_map(|(i, ty)| {
            build_val_navigator(&ty, resource_map).map(|nav| (i, nav))
        })
        .collect();

    if param_navs.is_empty() && result_navs.is_empty() {
        return ValMapper::noop();
    }

    let params: ParamsMapper = if param_navs.is_empty() {
        Box::new(|params, _visitor| Ok(std::borrow::Cow::Borrowed(params)))
    } else {
        Box::new(
            move |params: &[Val], visitor: &mut dyn ValVisitor<ResourceAny>| {
                let mut owned = params.to_vec();
                for (idx, nav) in &param_navs {
                    nav(&mut owned[*idx], visitor)?;
                }
                Ok(std::borrow::Cow::Owned(owned))
            },
        )
    };

    let results: ResultsMapper = if result_navs.is_empty() {
        Box::new(|_results, _visitor| Ok(()))
    } else {
        Box::new(
            move |results: &mut [Val], visitor: &mut dyn ValVisitor<ResourceAny>| {
                for (idx, nav) in &result_navs {
                    nav(&mut results[*idx], visitor)?;
                }
                Ok(())
            },
        )
    };

    ValMapper { params, results }
}

/// Build a `LinkContext` for a function type.
///
/// If `resource_map` is empty, returns `LinkContext::noop()`.
fn build_link_context(
    func_ty: &ComponentFunc,
    resource_map: &[(ResourceType, u32)],
) -> LinkContext {
    LinkContext {
        resource: build_resource_mapper(func_ty, resource_map),
    }
}

// ---------------------------------------------------------------------------
// Link export methods
// ---------------------------------------------------------------------------

impl ComposableInstance {
    pub(super) fn link_export_item(
        &self,
        name: &str,
        item: &ComponentItem,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
        resource_map: &[(ResourceType, u32)],
    ) -> Result<(), CompositionError> {
        match item {
            ComponentItem::ComponentFunc(func_ty) => {
                self.link_export_function(name, func_ty, parent, linker, resource_map)
            }
            ComponentItem::ComponentInstance(instance_ty) => {
                self.link_export_instance(name, instance_ty, parent, linker)
            }
            ComponentItem::Component(component_ty) => {
                self.link_export_component(name, component_ty, parent, linker)
            }
            ComponentItem::Resource(resource_ty) => {
                self.link_export_resource(name, resource_ty, parent, linker, resource_map)
            }
            ComponentItem::Type(_) => Ok(()),
            ComponentItem::CoreFunc(_) => Err(CompositionError::LinkingError(
                "CoreFunc exports not supported".to_string(),
            )),
            ComponentItem::Module(_) => Err(CompositionError::LinkingError(
                "Module exports not supported".to_string(),
            )),
        }
    }

    fn link_export_function(
        &self,
        name: &str,
        func_ty: &ComponentFunc,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
        resource_map: &[(ResourceType, u32)],
    ) -> Result<(), CompositionError> {
        let export_index = self
            .component
            .get_export_index(parent, name)
            .ok_or_else(|| {
                CompositionError::LinkingError(format!(
                    "Export index for '{}' not found in component",
                    name
                ))
            })?;

        let ctx = build_link_context(func_ty, resource_map);

        let tx = self.tx.downgrade();
        let func_type = func_ty.clone();

        #[cfg(feature = "component-model-async")]
        if func_ty.async_() {
            return linker.func_new_concurrent(
                name,
                Box::new(move |params: &[Val], results: &mut [Val]| {
                    let tx = tx.clone();
                    let func_type = func_type.clone();
                    let data = RawCallData {
                        params: params as *const [Val],
                        results: results as *mut [Val],
                    };
                    Box::pin(async move {
                        send_call(tx, export_index, func_type, data)
                            .await
                            .map_err(into_wasmtime_error)
                    })
                }),
                ctx,
            );
        }

        linker.func_new_async(
            name,
            Box::new(move |params: &[Val], results: &mut [Val]| {
                let tx = tx.clone();
                let func_type = func_type.clone();
                let data = RawCallData {
                    params: params as *const [Val],
                    results: results as *mut [Val],
                };
                Box::pin(async move {
                    send_call(tx, export_index, func_type, data)
                        .await
                        .map_err(into_wasmtime_error)
                })
            }),
            ctx,
        )
    }

    fn link_export_component(
        &self,
        _name: &str,
        component_ty: &wasmtime::component::types::Component,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        let items: Vec<_> = component_ty.imports(self.component.engine()).collect();
        for (name, item) in items {
            self.link_export_item(name, &item, parent, linker, &[])?;
        }

        Ok(())
    }

    fn link_export_resource(
        &self,
        name: &str,
        resource_ty: &ResourceType,
        _parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
        resource_map: &[(ResourceType, u32)],
    ) -> Result<(), CompositionError> {
        let ty_id = resource_map
            .iter()
            .find(|(rt, _)| *rt == *resource_ty)
            .map(|(_, id)| *id)
            .ok_or_else(|| {
                CompositionError::LinkingError(format!(
                    "Resource '{}' has no proxy type ID in resource_map",
                    name
                ))
            })?;
        let ty = ResourceType::host_dynamic(ty_id);
        linker.resource(name, ty)
    }

    fn link_export_instance(
        &self,
        name: &str,
        instance_ty: &ComponentInstance,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        let instance_export_index =
            self.component
                .get_export_index(parent, name)
                .ok_or_else(|| {
                    CompositionError::LinkingError(format!(
                        "Export index for instance '{}' not found in component",
                        name
                    ))
                })?;

        let engine = self.component.engine();
        let exports: Vec<_> = instance_ty
            .exports(engine)
            .map(|(n, ty)| (n.to_string(), ty))
            .collect();

        // Pass 1: build resource_map — each resource type gets a unique ty_id.
        let resource_map: Vec<(ResourceType, u32)> = exports
            .iter()
            .filter_map(|(_, item)| match item {
                ComponentItem::Resource(rt) => Some((
                    *rt,
                    NEXT_RESOURCE_TY_ID
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                )),
                _ => None,
            })
            .collect();

        // Pass 2: link all exports with the resource_map.
        linker.with_instance(
            name,
            Box::new(move |sub_linker| {
                for (export_name, export_ty) in &exports {
                    self.link_export_item(
                        export_name,
                        export_ty,
                        Some(&instance_export_index),
                        sub_linker,
                        &resource_map,
                    )?;
                }
                Ok(())
            }),
        )
    }
}
