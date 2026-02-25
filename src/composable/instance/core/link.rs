use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use wasmtime::component::Val;
use wasmtime::component::types::{ComponentFunc, ComponentInstance, ComponentItem, Type};
use wasmtime::component::{Component, ComponentExportIndex, ResourceAny};

use crate::composable::linker_instance_ops::{
    LinkContext, LinkerInstanceOps, ParamsMapper, ValMapper, ResultsMapper, ValVisitor,
};
use crate::composable::ExportFunc;
use crate::error::CompositionError;

use super::channel::{ChannelTask, RawCallData, into_wasmtime_error, send_call};

// ---------------------------------------------------------------------------
// Compiled mapper builders
// ---------------------------------------------------------------------------

type ValNavigator = Arc<
    dyn Fn(&mut Val, &mut dyn ValVisitor<ResourceAny>) -> wasmtime::Result<()> + Send + Sync,
>;

/// Build a navigator for a single `Val`. Returns `None` if the type contains no resources.
fn build_val_navigator(ty: &Type) -> Option<ValNavigator> {
    match ty {
        Type::Own(rt) | Type::Borrow(rt) => {
            let rt = *rt;
            Some(Arc::new(move |val, visitor| {
                if let Val::Resource(ra) = val {
                    visitor.visit(ra, rt)?;
                }
                Ok(())
            }))
        }
        Type::Record(record) => {
            let subs: Vec<(usize, ValNavigator)> = record
                .fields()
                .enumerate()
                .filter_map(|(i, field)| {
                    build_val_navigator(&field.ty).map(|nav| (i, nav))
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
            let nav = build_val_navigator(&inner_ty)?;
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
                    build_val_navigator(&ty).map(|nav| (i, nav))
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
            let nav = build_val_navigator(&inner_ty)?;
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
                .and_then(|ty| build_val_navigator(&ty));
            let err_nav = result
                .err()
                .and_then(|ty| build_val_navigator(&ty));
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
                    let nav = case.ty.and_then(|ty| build_val_navigator(&ty));
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
fn build_resource_mapper(func_ty: &ComponentFunc) -> ValMapper {
    let param_navs: Vec<(usize, ValNavigator)> = func_ty
        .params()
        .enumerate()
        .filter_map(|(i, (_, ty))| {
            build_val_navigator(&ty).map(|nav| (i, nav))
        })
        .collect();

    let result_navs: Vec<(usize, ValNavigator)> = func_ty
        .results()
        .enumerate()
        .filter_map(|(i, ty)| {
            build_val_navigator(&ty).map(|nav| (i, nav))
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
fn build_link_context(func_ty: &ComponentFunc) -> LinkContext {
    LinkContext {
        resource: build_resource_mapper(func_ty),
        resource_map: None,
    }
}

// ---------------------------------------------------------------------------
// Free functions for linking exports and building ExportFuncs
// ---------------------------------------------------------------------------

/// Build an `ExportFunc` that sends a channel task to the inbox loop.
pub(crate) fn make_export_func(
    tx: &mpsc::UnboundedSender<ChannelTask>,
    export_index: ComponentExportIndex,
    func_type: ComponentFunc,
) -> ExportFunc {
    let tx = tx.downgrade();

    ExportFunc::new(move |params, results| {
        let tx = tx.clone();
        let func_type = func_type.clone();
        let data = RawCallData {
            params: params as *const [Val],
            results: results as *mut [Val],
        };
        Box::pin(send_call(tx, export_index, func_type, data))
    })
}

/// Resolve an exported function into a pre-resolved handle.
pub(crate) fn get_func_impl(
    component: &Component,
    tx: &mpsc::UnboundedSender<ChannelTask>,
    export_types: &HashMap<String, ComponentItem>,
    interface: Option<&str>,
    func: &str,
) -> Result<ExportFunc, CompositionError> {
    match interface {
        None => {
            let func_type = match export_types.get(func) {
                Some(ComponentItem::ComponentFunc(f)) => f.clone(),
                _ => return Err(CompositionError::FuncNotFound),
            };
            let export_index = component
                .get_export_index(None, func)
                .ok_or(CompositionError::FuncNotFound)?;
            Ok(make_export_func(tx, export_index, func_type))
        }
        Some(iface) => {
            let inst_ty = match export_types.get(iface) {
                Some(ComponentItem::ComponentInstance(i)) => i,
                _ => return Err(CompositionError::FuncNotFound),
            };
            let inst_index = component
                .get_export_index(None, iface)
                .ok_or(CompositionError::FuncNotFound)?;

            let engine = component.engine();
            for (child_name, child_item) in inst_ty.exports(engine) {
                if child_name == func {
                    if let ComponentItem::ComponentFunc(f) = &child_item {
                        let func_index = component
                            .get_export_index(Some(&inst_index), func)
                            .ok_or(CompositionError::FuncNotFound)?;
                        return Ok(make_export_func(tx, func_index, f.clone()));
                    }
                }
            }
            Err(CompositionError::FuncNotFound)
        }
    }
}

/// Link a single export item into the given linker.
pub(crate) fn link_export_item(
    component: &Component,
    tx: &mpsc::UnboundedSender<ChannelTask>,
    name: &str,
    item: &ComponentItem,
    parent: Option<&ComponentExportIndex>,
    linker: &mut dyn LinkerInstanceOps,
) -> Result<(), CompositionError> {
    match item {
        ComponentItem::ComponentFunc(func_ty) => {
            link_export_function(component, tx, name, func_ty, parent, linker)
        }
        ComponentItem::ComponentInstance(instance_ty) => {
            link_export_instance(component, tx, name, instance_ty, parent, linker)
        }
        ComponentItem::Component(component_ty) => {
            link_export_component(component, tx, name, component_ty, parent, linker)
        }
        ComponentItem::Resource(resource_ty) => {
            linker.resource(name, *resource_ty)
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
    component: &Component,
    tx: &mpsc::UnboundedSender<ChannelTask>,
    name: &str,
    func_ty: &ComponentFunc,
    parent: Option<&ComponentExportIndex>,
    linker: &mut dyn LinkerInstanceOps,
) -> Result<(), CompositionError> {
    let export_index = component
        .get_export_index(parent, name)
        .ok_or_else(|| {
            CompositionError::LinkingError(format!(
                "Export index for '{}' not found in component",
                name
            ))
        })?;

    let ctx = build_link_context(func_ty);

    let tx = tx.downgrade();
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
    component: &Component,
    tx: &mpsc::UnboundedSender<ChannelTask>,
    _name: &str,
    component_ty: &wasmtime::component::types::Component,
    parent: Option<&ComponentExportIndex>,
    linker: &mut dyn LinkerInstanceOps,
) -> Result<(), CompositionError> {
    let items: Vec<_> = component_ty.imports(component.engine()).collect();
    for (name, item) in items {
        link_export_item(component, tx, name, &item, parent, linker)?;
    }

    Ok(())
}

fn link_export_instance(
    component: &Component,
    tx: &mpsc::UnboundedSender<ChannelTask>,
    name: &str,
    instance_ty: &ComponentInstance,
    parent: Option<&ComponentExportIndex>,
    linker: &mut dyn LinkerInstanceOps,
) -> Result<(), CompositionError> {
    let instance_export_index =
        component
            .get_export_index(parent, name)
            .ok_or_else(|| {
                CompositionError::LinkingError(format!(
                    "Export index for instance '{}' not found in component",
                    name
                ))
            })?;

    let engine = component.engine();
    let exports: Vec<_> = instance_ty
        .exports(engine)
        .map(|(n, ty)| (n.to_string(), ty))
        .collect();

    linker.with_instance(
        name,
        Box::new(move |sub_linker| {
            // Pass 1: register resources via linker (ComposableLinker stores the mapping).
            for (export_name, export_ty) in &exports {
                if let ComponentItem::Resource(rt) = export_ty {
                    sub_linker.resource(export_name, *rt)?;
                }
            }
            // Pass 2: link everything else.
            for (export_name, export_ty) in &exports {
                if !matches!(export_ty, ComponentItem::Resource(_)) {
                    link_export_item(
                        component,
                        tx,
                        export_name,
                        export_ty,
                        Some(&instance_export_index),
                        sub_linker,
                    )?;
                }
            }
            Ok(())
        }),
    )
}
