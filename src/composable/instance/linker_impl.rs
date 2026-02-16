use wasmtime::AsContextMut;
use wasmtime::component::types::{self, ResourceType};
use wasmtime::component::{
    LinkerInstance, Resource, ResourceAny, ResourceDynamic, ResourceTable, Val,
};

use crate::composable::linker_ops::{BoxedAsyncFunc, LinkerOps};
#[cfg(feature = "component-model-async")]
use crate::composable::linker_ops::BoxedConcurrentFunc;
use crate::error::CompositionError;

/// Trait for store data types (`T` in `Store<T>`) that support cross-store
/// resource proxying.
///
/// Only required when using [`ComposableInstance`] with resources that cross
/// store boundaries. The `Composable` trait itself is store-agnostic and
/// does not require this.
///
/// Follows the same pattern as `wasmtime_wasi::WasiView`.
pub trait ResourceProxyView {
    fn proxy_table(&mut self) -> &mut ResourceTable;
}

// ---------------------------------------------------------------------------
// Resource remapping helpers — access proxy table via T: ResourceProxyView
// ---------------------------------------------------------------------------

/// Replace producer-side `ResourceAny` values in results with consumer
/// proxy handles created via `ResourceDynamic`.
fn remap_results<T: ResourceProxyView>(
    ty_id: u32,
    store: &mut wasmtime::StoreContextMut<'_, T>,
    results: &mut [Val],
) -> wasmtime::Result<()> {
    for val in results.iter_mut() {
        if let Val::Resource(producer_ra) = val {
            let proxy_id = store
                .data_mut()
                .proxy_table()
                .push(*producer_ra)
                .expect("resource table full")
                .rep();
            let rd = ResourceDynamic::new_own(proxy_id, ty_id);
            *val = Val::Resource(rd.try_into_resource_any(&mut *store)?);
        }
    }
    Ok(())
}

/// Replace consumer proxy handles in params with the original
/// producer-side `ResourceAny` values.
///
/// Returns `None` if no resources are present (no allocation needed).
fn remap_params<T: ResourceProxyView>(
    _ty_id: u32,
    store: &mut wasmtime::StoreContextMut<'_, T>,
    params: &[Val],
) -> Option<Vec<Val>> {
    if !params.iter().any(|v| matches!(v, Val::Resource(_))) {
        return None;
    }
    let mut remapped = params.to_vec();
    for val in remapped.iter_mut() {
        if let Val::Resource(consumer_ra) = val {
            if let Ok(rd) = ResourceDynamic::try_from_resource_any(*consumer_ra, &mut *store) {
                let proxy_id = rd.rep();
                if let Ok(producer_ra) = store
                    .data_mut()
                    .proxy_table()
                    .get(&Resource::<ResourceAny>::new_borrow(proxy_id))
                {
                    *val = Val::Resource(*producer_ra);
                }
            }
        }
    }
    Some(remapped)
}

impl<'a, T: ResourceProxyView + Send + 'static> LinkerOps for LinkerInstance<'a, T> {
    fn func_new_async(
        &mut self,
        name: &str,
        func: BoxedAsyncFunc,
        proxy_ty_id: Option<u32>,
    ) -> Result<(), CompositionError> {
        match proxy_ty_id {
            None => {
                LinkerInstance::func_new_async(
                    self,
                    name,
                    move |_store, _ty: types::ComponentFunc, params, results| {
                        Box::new(func(params, results))
                    },
                )?;
            }
            Some(ty_id) => {
                let func = std::sync::Arc::new(func);
                LinkerInstance::func_new_async(
                    self,
                    name,
                    move |mut store, _ty: types::ComponentFunc, params, results| {
                        let func = func.clone();
                        let remapped = remap_params(ty_id, &mut store, params)
                            .unwrap_or_else(|| params.to_vec());
                        Box::new(async move {
                            func(&remapped, results).await?;
                            remap_results(ty_id, &mut store, results)?;
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
        proxy_ty_id: Option<u32>,
    ) -> Result<(), CompositionError> {
        match proxy_ty_id {
            None => {
                LinkerInstance::func_new_concurrent(
                    self,
                    name,
                    move |_accessor, _ty: types::ComponentFunc, params, results| {
                        Box::pin(func(params, results))
                    },
                )?;
            }
            Some(ty_id) => {
                let func = std::sync::Arc::new(func);
                LinkerInstance::func_new_concurrent(
                    self,
                    name,
                    move |accessor, _ty: types::ComponentFunc, params, results| {
                        let remapped = accessor.with(|mut access| {
                            let mut store = access.as_context_mut();
                            remap_params(ty_id, &mut store, params)
                                .unwrap_or_else(|| params.to_vec())
                        });
                        let func = func.clone();
                        Box::pin(async move {
                            func(&remapped, results).await?;
                            accessor.with(|mut access| {
                                let mut store = access.as_context_mut();
                                remap_results(ty_id, &mut store, results)
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
        has_proxy: bool,
    ) -> Result<(), CompositionError> {
        LinkerInstance::resource_async(self, name, ty, move |mut store, rep| {
            if has_proxy {
                let _ = store
                    .data_mut()
                    .proxy_table()
                    .delete(Resource::<ResourceAny>::new_own(rep));
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
