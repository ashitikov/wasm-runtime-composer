mod channel;
mod inbox;
mod link;
mod linker_impl;

pub use linker_impl::ResourceProxyView;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::mpsc;
use wasmtime::Store;
use wasmtime::component::types::{ComponentFunc, ComponentItem};
use wasmtime::component::{Component, ComponentExportIndex, Instance, Val};

use crate::composable::{Composable, ComposableType, ExportFunc, InterfaceSet, ResolvedImport};
use crate::composable::linker_ops::LinkerOps;
use crate::error::CompositionError;

use channel::{ChannelTask, RawCallData, send_call};
use inbox::inbox_loop;

/// Global counter for unique `ResourceType::host_dynamic` payloads.
static NEXT_RESOURCE_TY_ID: AtomicU32 = AtomicU32::new(0);

/// A composable backed by a wasmtime Instance.
///
/// The underlying [`Store`] is owned by a background inbox loop task and
/// accessed exclusively through an internal channel — no `Arc<Mutex>` needed.
/// This makes `ComposableInstance` non-generic over the store data type `T`;
/// the type is erased at construction time.
pub struct ComposableInstance {
    /// Strong sender — keeps the inbox loop alive while this instance exists.
    tx: mpsc::UnboundedSender<ChannelTask>,
    ty: ComposableType,
    export_types: HashMap<String, ComponentItem>,
    component: Component,
    /// Unique `ResourceType::host_dynamic` payload for this instance.
    /// Used for cross-store resource proxying when this instance acts as a producer.
    proxy_ty_id: u32,
}

impl ComposableInstance {
    /// Create a new composable instance.
    ///
    /// Spawns a background inbox loop task on the current tokio runtime that
    /// owns the store and processes all function calls.
    ///
    /// # Panics
    ///
    /// Panics if called outside a tokio runtime context.
    pub fn new<T: Send + 'static>(instance: Instance, store: Store<T>) -> Self {
        let instance_pre = instance.instance_pre(&store);
        let component = instance_pre.component().clone();
        let component_type = component.component_type();
        let engine = instance_pre.engine();

        let exports: InterfaceSet = component_type
            .exports(engine)
            .map(|(name, _)| name.to_string())
            .collect();

        let imports: InterfaceSet = component_type
            .imports(engine)
            .map(|(name, _)| name.to_string())
            .collect();

        let export_types: HashMap<String, ComponentItem> = component_type
            .exports(engine)
            .map(|(name, ty)| (name.to_string(), ty))
            .collect();

        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(inbox_loop(store, instance, rx));

        Self {
            tx,
            ty: ComposableType::new(exports, imports),
            export_types,
            component,
            proxy_ty_id: NEXT_RESOURCE_TY_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Build an `ExportFunc` that sends a channel task to the inbox loop.
    fn make_export_func(
        &self,
        export_index: ComponentExportIndex,
        func_type: ComponentFunc,
    ) -> ExportFunc {
        let tx = self.tx.downgrade();

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
}

impl Composable for ComposableInstance {
    fn ty(&self) -> &ComposableType {
        &self.ty
    }

    fn component(&self) -> Option<&Component> {
        Some(&self.component)
    }

    fn get_func(
        &self,
        interface: Option<&str>,
        func: &str,
    ) -> Result<ExportFunc, CompositionError> {
        match interface {
            None => {
                let func_type = match self.export_types.get(func) {
                    Some(ComponentItem::ComponentFunc(f)) => f.clone(),
                    _ => return Err(CompositionError::FuncNotFound),
                };
                let export_index = self
                    .component
                    .get_export_index(None, func)
                    .ok_or(CompositionError::FuncNotFound)?;
                Ok(self.make_export_func(export_index, func_type))
            }
            Some(iface) => {
                let inst_ty = match self.export_types.get(iface) {
                    Some(ComponentItem::ComponentInstance(i)) => i,
                    _ => return Err(CompositionError::FuncNotFound),
                };
                let inst_index = self
                    .component
                    .get_export_index(None, iface)
                    .ok_or(CompositionError::FuncNotFound)?;

                let engine = self.component.engine();
                for (child_name, child_item) in inst_ty.exports(engine) {
                    if child_name == func {
                        if let ComponentItem::ComponentFunc(f) = &child_item {
                            let func_index = self
                                .component
                                .get_export_index(Some(&inst_index), func)
                                .ok_or(CompositionError::FuncNotFound)?;
                            return Ok(self.make_export_func(func_index, f.clone()));
                        }
                    }
                }
                Err(CompositionError::FuncNotFound)
            }
        }
    }

    fn link_import(
        &mut self,
        _import: &ResolvedImport,
        _exporter: &mut dyn Composable,
    ) -> Result<(), CompositionError> {
        Ok(())
    }

    fn link_export(
        &mut self,
        name: &str,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        let export_type = self.export_types.get(name).cloned().ok_or_else(|| {
            CompositionError::LinkingError(format!("Export '{}' not found", name))
        })?;
        self.link_export_item(name, &export_type, None, linker, None::<u32>)
    }
}
