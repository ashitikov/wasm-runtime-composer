use wasmtime::component::types::ResourceType;
use wasmtime::component::Val;

use crate::error::CompositionError;

/// Type-erased linker operations.
///
/// This trait allows composables to add definitions to a linker
/// without knowing the concrete store type `T`.
pub trait LinkerOps {
    /// Add an async function definition.
    ///
    /// When `proxy_ty_id` is `Some`, resource values in params/results are
    /// remapped between the consumer and producer stores via `ResourceDynamic`.
    fn func_new_async(
        &mut self,
        name: &str,
        func: BoxedAsyncFunc,
        proxy_ty_id: Option<u32>,
    ) -> Result<(), CompositionError>;

    /// Add a concurrent async function definition.
    ///
    /// Registered via wasmtime's `func_new_concurrent` so multiple invocations
    /// can run concurrently (uses `Accessor<T>` instead of exclusive `StoreContextMut`).
    ///
    /// When `proxy_ty_id` is `Some`, resource remapping uses `Accessor::with()`
    /// for scoped store access.
    #[cfg(feature = "component-model-async")]
    fn func_new_concurrent(
        &mut self,
        name: &str,
        func: BoxedConcurrentFunc,
        proxy_ty_id: Option<u32>,
    ) -> Result<(), CompositionError>;

    /// Register a resource type.
    ///
    /// When `has_proxy` is true, the destructor removes the proxy entry from
    /// the store's proxy table (cleaning up the mapping when the consumer
    /// drops the resource).
    fn resource(
        &mut self,
        name: &str,
        ty: ResourceType,
        has_proxy: bool,
    ) -> Result<(), CompositionError>;

    /// Execute a callback with a sub-linker for a nested instance.
    fn with_instance<'a>(
        &'a mut self,
        name: &str,
        f: Box<dyn FnOnce(&mut dyn LinkerOps) -> Result<(), CompositionError> + 'a>,
    ) -> Result<(), CompositionError>;
}

/// Type-erased function for async calls.
/// Receives params and a pre-allocated results slice, writes results in-place.
pub type BoxedAsyncFunc = Box<
    dyn for<'a> Fn(
            &'a [Val],
            &'a mut [Val],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = wasmtime::Result<()>> + Send + 'a>,
        > + Send
        + Sync
        + 'static,
>;

/// Type-erased function for concurrent async calls.
/// Same callback shape as `BoxedAsyncFunc`, but registered via
/// `func_new_concurrent` so multiple invocations can run concurrently.
#[cfg(feature = "component-model-async")]
pub type BoxedConcurrentFunc = Box<
    dyn for<'a> Fn(
            &'a [Val],
            &'a mut [Val],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = wasmtime::Result<()>> + Send + 'a>,
        > + Send
        + Sync
        + 'static,
>;
