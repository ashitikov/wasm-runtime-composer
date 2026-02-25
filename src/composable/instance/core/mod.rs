pub(crate) mod channel;
pub(crate) mod inbox;
pub(crate) mod link;

use std::collections::HashMap;

use tokio::sync::mpsc;
use wasmtime::Store;
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, Instance};

use crate::composable::composition::Composition;
use crate::composable::linker_instance_ops::LinkerInstanceOps;
use crate::composable::{ComposableType, InterfaceSet};
use crate::error::CompositionError;

use channel::ChannelTask;
use inbox::inbox_loop;
use link::{get_func_impl, link_export_item};

/// Shared core for `ComposableInstance` and `ComposableComponent`.
///
/// Owns the channel (`tx`/`rx`), component metadata and type info.
/// Does **not** implement `Composable` — the wrappers do.
pub(crate) struct ComposableInstanceCore {
    tx: mpsc::UnboundedSender<ChannelTask>,
    rx: mpsc::UnboundedReceiver<ChannelTask>,
    ty: ComposableType,
    export_types: HashMap<String, ComponentItem>,
    component: Component,
}

impl ComposableInstanceCore {
    pub(crate) fn new(component: Component) -> Self {
        let component_type = component.component_type();
        let engine = component.engine();

        let export_types: HashMap<String, ComponentItem> = component_type
            .exports(engine)
            .map(|(name, ty)| (name.to_string(), ty))
            .collect();

        let exports: InterfaceSet = export_types.keys().cloned().collect();

        let imports: InterfaceSet = component_type
            .imports(engine)
            .map(|(name, _)| name.to_string())
            .collect();

        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            tx,
            rx,
            ty: ComposableType::new(exports, imports),
            export_types,
            component,
        }
    }

    pub(crate) fn ty(&self) -> &ComposableType {
        &self.ty
    }

    pub(crate) fn component(&self) -> &Component {
        &self.component
    }

    pub(crate) fn link_export(
        &mut self,
        name: &str,
        importer_linker: &mut dyn LinkerInstanceOps,
    ) -> Result<(), CompositionError> {
        let export_type = self.export_types.get(name).ok_or_else(|| {
            CompositionError::LinkingError(format!("Export '{}' not found", name))
        })?;
        link_export_item(
            &self.component,
            &self.tx,
            name,
            export_type,
            None,
            importer_linker,
        )
    }

    /// Spawn the inbox loop and build a `Composition`.
    pub(crate) fn into_composition<T: Send + 'static>(
        self,
        store: Store<T>,
        instance: Instance,
    ) -> Composition {
        let Self {
            tx,
            rx,
            ty,
            export_types,
            component,
        } = self;
        tokio::spawn(inbox_loop(store, instance, rx));

        let component_for_export = component.clone();
        let tx_for_export = tx.clone();
        let export_types_for_export = export_types.clone();

        Composition::new(
            Box::new(move |iface, func| get_func_impl(&component, &tx, &export_types, iface, func)),
            Box::new(move |name, linker| {
                let export_type = export_types_for_export.get(name).ok_or_else(|| {
                    CompositionError::LinkingError(format!("Export '{}' not found", name))
                })?;
                link_export_item(
                    &component_for_export,
                    &tx_for_export,
                    name,
                    export_type,
                    None,
                    linker,
                )
            }),
            ty,
            vec![],
        )
    }
}
