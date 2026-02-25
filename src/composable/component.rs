use std::future::Future;
use std::pin::Pin;

use wasmtime::Store;
use wasmtime::component::{Component, Linker};

use crate::composable::composition::Composition;
use crate::composable::linker_instance_ops::LinkerInstanceOps;
use crate::composable::{Composable, ComposableType};
use crate::error::CompositionError;

use super::instance::{ComposableInstanceCore, ComposableLinker, ResourceProxyView};

/// A composable backed by an uninstantiated Component + Linker.
///
/// Wraps a [`ComposableInstanceCore`] for channel and type info delegation.
/// The `Linker<T>` is owned directly — `link_import` borrows it via `root()`,
/// `into_composition` moves it for `instantiate_async`.
///
/// `T` is erased when the component is boxed as `Box<dyn Composable>`.
pub struct ComposableComponent<T: ResourceProxyView + Send + 'static> {
    inner: ComposableInstanceCore,
    linker: Linker<T>,
    store_factory: Box<dyn Fn() -> Store<T> + Send + Sync>,
}

impl<T: ResourceProxyView + Send + 'static> ComposableComponent<T> {
    /// Create a new composable component.
    ///
    /// The channel is created immediately. `link_export` will
    /// queue messages until `into_composition()` spawns the inbox loop.
    pub fn new(
        component: Component,
        linker: Linker<T>,
        store_factory: impl Fn() -> Store<T> + Send + Sync + 'static,
    ) -> Self {
        let inner = ComposableInstanceCore::new(component);
        Self {
            inner,
            linker,
            store_factory: Box::new(store_factory),
        }
    }
}

impl<T: ResourceProxyView + Send + 'static> Composable for ComposableComponent<T> {
    fn ty(&self) -> &ComposableType {
        self.inner.ty()
    }

    fn link_export(
        &mut self,
        name: &str,
        importer_linker: &mut dyn LinkerInstanceOps,
    ) -> Result<(), CompositionError> {
        self.inner.link_export(name, importer_linker)
    }

    fn link_import(
        &mut self,
        name: &str,
        exporter: &mut dyn Composable,
    ) -> Result<(), CompositionError> {
        let mut ops = ComposableLinker::new(self.linker.root());
        exporter.link_export(name, &mut ops)
    }

    fn into_composition(
        self: Box<Self>,
    ) -> Pin<Box<dyn Future<Output = Result<Composition, CompositionError>> + Send>> {
        Box::pin(async move {
            let this = *self;
            let mut store = (this.store_factory)();
            let instance = this.linker.instantiate_async(&mut store, this.inner.component()).await?;
            Ok(this.inner.into_composition(store, instance))
        })
    }
}
