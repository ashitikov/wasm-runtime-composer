pub(crate) mod core;
mod linker_impl;

pub use linker_impl::ComposableLinker;
pub use linker_impl::ResourceProxyView;
pub(super) use self::core::ComposableInstanceCore;

use std::future::Future;
use std::pin::Pin;

use wasmtime::Store;
use wasmtime::component::Instance;

use crate::composable::composition::Composition;
use crate::composable::linker_instance_ops::LinkerInstanceOps;
use crate::composable::{Composable, ComposableType, InterfaceSet};
use crate::error::CompositionError;

/// Type-erased finalizer: captures `store` + `instance`, calls
/// `ComposableInstanceCore::into_composition` with concrete `T`.
type Finalize = Box<dyn FnOnce(ComposableInstanceCore) -> Composition + Send>;

/// A composable backed by an already-instantiated wasmtime Component.
///
/// The underlying [`Store`] is owned by the inbox loop task (spawned during
/// [`into_composition`](Composable::into_composition)) and accessed
/// exclusively through an internal channel — no `Arc<Mutex>` needed.
///
/// `T` is erased at construction time via a [`Finalize`] closure.
pub struct ComposableInstance {
    inner: ComposableInstanceCore,
    ty: ComposableType,
    finalize: Finalize,
}

impl ComposableInstance {
    /// Create from an already-instantiated component.
    ///
    /// All imports are already satisfied, so the reported type has empty imports.
    /// The inbox loop is deferred to `into_composition()`.
    pub fn from_existing<T: Send + 'static>(instance: Instance, store: Store<T>) -> Self {
        let instance_pre = instance.instance_pre(&store);
        let component = instance_pre.component().clone();
        let inner = ComposableInstanceCore::new(component);
        let ty = ComposableType::new(inner.ty().exports().clone(), InterfaceSet::new());
        let finalize: Finalize = Box::new(move |core| core.into_composition(store, instance));
        Self { inner, ty, finalize }
    }
}

impl Composable for ComposableInstance {
    fn ty(&self) -> &ComposableType {
        &self.ty
    }

    fn link_export(
        &mut self,
        name: &str,
        importer_linker: &mut dyn LinkerInstanceOps,
    ) -> Result<(), CompositionError> {
        self.inner.link_export(name, importer_linker)
    }

    fn into_composition(
        self: Box<Self>,
    ) -> Pin<Box<dyn Future<Output = Result<Composition, CompositionError>> + Send>> {
        let composition = (self.finalize)(self.inner);
        Box::pin(async { Ok(composition) })
    }
}
