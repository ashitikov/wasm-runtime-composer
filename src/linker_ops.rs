use wasmtime::component::types;
use wasmtime::component::{LinkerInstance, Val};

use crate::CompositionError;

/// Type-erased linker operations.
///
/// This trait allows composables to add definitions to a linker
/// without knowing the concrete store type `T`.
pub trait LinkerOps {
    /// Add an async function definition.
    fn func_new_async(&mut self, name: &str, func: BoxedAsyncFunc) -> Result<(), CompositionError>;

    /// Add a concurrent async function definition.
    ///
    /// Registered via wasmtime's `func_new_concurrent` so multiple invocations
    /// can run concurrently (uses `Accessor<T>` instead of exclusive `StoreContextMut`).
    #[cfg(feature = "component-model-async")]
    fn func_new_concurrent(&mut self, name: &str, func: BoxedConcurrentFunc) -> Result<(), CompositionError>;

    /// Execute a callback with a sub-linker for a nested instance.
    fn with_instance<'a>(
        &'a mut self,
        name: &str,
        f: Box<dyn FnOnce(&mut dyn LinkerOps) -> Result<(), CompositionError> + 'a>,
    ) -> Result<(), CompositionError>;
}

/// Type-erased function for async calls.
/// Matches wasmtime's `func_new_async` callback: receives params and
/// a pre-allocated results slice, writes results in-place.
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

impl<'a, T: Send + 'static> LinkerOps for LinkerInstance<'a, T> {
    fn func_new_async(&mut self, name: &str, func: BoxedAsyncFunc) -> Result<(), CompositionError> {
        LinkerInstance::func_new_async(
            self,
            name,
            move |_store, _ty: types::ComponentFunc, params, results| {
                Box::new(func(params, results))
            },
        )?;
        Ok(())
    }

    #[cfg(feature = "component-model-async")]
    fn func_new_concurrent(&mut self, name: &str, func: BoxedConcurrentFunc) -> Result<(), CompositionError> {
        LinkerInstance::func_new_concurrent(
            self,
            name,
            move |_accessor, _ty: types::ComponentFunc, params, results| {
                Box::pin(func(params, results))
            },
        )?;
        Ok(())
    }

    fn with_instance<'b>(
        &'b mut self,
        name: &str,
        f: Box<dyn FnOnce(&mut dyn LinkerOps) -> Result<(), CompositionError> + 'b>,
    ) -> Result<(), CompositionError> {
        let mut instance = LinkerInstance::instance(self, name)?;
        f(&mut instance)
    }
}
