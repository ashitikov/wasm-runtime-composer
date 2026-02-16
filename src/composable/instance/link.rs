use wasmtime::component::types::{ComponentFunc, ComponentInstance, ComponentItem};
use wasmtime::component::{ComponentExportIndex, ResourceType, Val};

use crate::composable::linker_ops::LinkerOps;
use crate::error::CompositionError;

use super::ComposableInstance;
use super::channel::{RawCallData, into_wasmtime_error, send_call};

impl ComposableInstance {
    pub(super) fn link_export_item(
        &self,
        name: &str,
        item: &ComponentItem,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
        proxy_ty_id: Option<u32>,
    ) -> Result<(), CompositionError> {
        match item {
            ComponentItem::ComponentFunc(func_ty) => {
                self.link_export_function(name, func_ty, parent, linker, proxy_ty_id)
            }
            ComponentItem::ComponentInstance(instance_ty) => {
                self.link_export_instance(name, instance_ty, parent, linker)
            }
            ComponentItem::Component(component_ty) => {
                self.link_export_component(name, component_ty, parent, linker)
            }
            ComponentItem::Resource(resource_ty) => {
                self.link_export_resource(name, resource_ty, parent, linker, proxy_ty_id)
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
        proxy_ty_id: Option<u32>,
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
                proxy_ty_id,
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
            proxy_ty_id,
        )
    }

    fn link_export_component(
        &self,
        _name: &str,
        component_ty: &wasmtime::component::types::Component,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        let engine = self.component.engine().clone();
        let items: Vec<_> = component_ty.imports(&engine).collect();
        for (name, item) in items {
            self.link_export_item(name, &item, parent, linker, None::<u32>)?;
        }

        Ok(())
    }

    fn link_export_resource(
        &self,
        name: &str,
        _resource_ty: &ResourceType,
        _parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
        proxy_ty_id: Option<u32>,
    ) -> Result<(), CompositionError> {
        let ty_id = proxy_ty_id.ok_or_else(|| {
            CompositionError::LinkingError(format!(
                "Resource '{}' has no proxy type ID (should be set by parent instance)",
                name
            ))
        })?;
        let ty = ResourceType::host_dynamic(ty_id);
        linker.resource(name, ty, true)
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

        // Pass the proxy type ID if the interface contains any resources.
        let has_resources = exports
            .iter()
            .any(|(_, ty)| matches!(ty, ComponentItem::Resource(_)));
        let proxy_ty_id = if has_resources {
            Some(self.proxy_ty_id)
        } else {
            None
        };

        let this = self;
        linker.with_instance(
            name,
            Box::new(move |sub_linker| {
                for (export_name, export_ty) in &exports {
                    this.link_export_item(
                        export_name,
                        export_ty,
                        Some(&instance_export_index),
                        sub_linker,
                        proxy_ty_id,
                    )?;
                }
                Ok(())
            }),
        )
    }
}
