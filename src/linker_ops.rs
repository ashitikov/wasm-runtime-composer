use std::sync::Arc;

use wasmtime::AsContextMut;
use wasmtime::component::types::{self, ResourceType};
use wasmtime::component::{LinkerInstance, Val};

use crate::composable_instance::ResourceProxyTable;
use crate::CompositionError;

/// Type-erased linker operations.
///
/// This trait allows composables to add definitions to a linker
/// without knowing the concrete store type `T`.
pub trait LinkerOps {
    /// Add an async function definition.
    ///
    /// When `proxy` is `Some`, resource values in params/results are remapped
    /// between the consumer and producer stores via `ResourceDynamic`.
    fn func_new_async(
        &mut self,
        name: &str,
        func: BoxedAsyncFunc,
        proxy: Option<Arc<ResourceProxyTable>>,
    ) -> Result<(), CompositionError>;

    /// Add a concurrent async function definition.
    ///
    /// Registered via wasmtime's `func_new_concurrent` so multiple invocations
    /// can run concurrently (uses `Accessor<T>` instead of exclusive `StoreContextMut`).
    ///
    /// When `proxy` is `Some`, resource remapping uses `Accessor::with()` for
    /// scoped store access.
    #[cfg(feature = "component-model-async")]
    fn func_new_concurrent(
        &mut self,
        name: &str,
        func: BoxedConcurrentFunc,
        proxy: Option<Arc<ResourceProxyTable>>,
    ) -> Result<(), CompositionError>;

    /// Register a resource type.
    ///
    /// When `dtor` is `Some`, the destructor removes the proxy entry from the
    /// table (cleaning up the mapping when the consumer drops the resource).
    fn resource(
        &mut self,
        name: &str,
        ty: ResourceType,
        dtor: Option<Arc<ResourceProxyTable>>,
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

impl<'a, T: Send + 'static> LinkerOps for LinkerInstance<'a, T> {
    fn func_new_async(
        &mut self,
        name: &str,
        func: BoxedAsyncFunc,
        proxy: Option<Arc<ResourceProxyTable>>,
    ) -> Result<(), CompositionError> {
        match proxy {
            None => {
                LinkerInstance::func_new_async(
                    self,
                    name,
                    move |_store, _ty: types::ComponentFunc, params, results| {
                        Box::new(func(params, results))
                    },
                )?;
            }
            Some(proxy) => {
                let func = Arc::new(func);
                LinkerInstance::func_new_async(
                    self,
                    name,
                    move |mut store, _ty: types::ComponentFunc, params, results| {
                        let func = func.clone();
                        let remapped = proxy
                            .remap_params(&mut store, params)
                            .unwrap_or_else(|| params.to_vec());
                        let proxy = proxy.clone();
                        Box::new(async move {
                            func(&remapped, results).await?;
                            proxy.remap_results(&mut store, results)?;
                            Ok(())
                        })
                    },
                )?;
            }
        }
        Ok(())
    }

    #[cfg(feature = "component-model-async")]
    fn func_new_concurrent(
        &mut self,
        name: &str,
        func: BoxedConcurrentFunc,
        proxy: Option<Arc<ResourceProxyTable>>,
    ) -> Result<(), CompositionError> {
        match proxy {
            None => {
                LinkerInstance::func_new_concurrent(
                    self,
                    name,
                    move |_accessor, _ty: types::ComponentFunc, params, results| {
                        Box::pin(func(params, results))
                    },
                )?;
            }
            Some(proxy) => {
                let func = Arc::new(func);
                LinkerInstance::func_new_concurrent(
                    self,
                    name,
                    move |accessor, _ty: types::ComponentFunc, params, results| {
                        let remapped = accessor.with(|mut access| {
                            let mut store = access.as_context_mut();
                            proxy
                                .remap_params(&mut store, params)
                                .unwrap_or_else(|| params.to_vec())
                        });
                        let func = func.clone();
                        let proxy = proxy.clone();
                        Box::pin(async move {
                            func(&remapped, results).await?;
                            accessor.with(|mut access| {
                                let mut store = access.as_context_mut();
                                proxy.remap_results(&mut store, results)
                            })
                        })
                    },
                )?;
            }
        }
        Ok(())
    }

    fn resource(
        &mut self,
        name: &str,
        ty: ResourceType,
        dtor: Option<Arc<ResourceProxyTable>>,
    ) -> Result<(), CompositionError> {
        LinkerInstance::resource_async(self, name, ty, move |_store, rep| {
            if let Some(table) = &dtor {
                table.remove(rep);
            }
            Box::new(async { Ok(()) })
        })?;
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
