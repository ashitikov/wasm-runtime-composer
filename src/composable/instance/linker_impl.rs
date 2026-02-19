use std::sync::Arc;

use wasmtime::AsContextMut;
use wasmtime::component::types::{self, ResourceType};
use wasmtime::component::{
    LinkerInstance, Resource, ResourceAny, ResourceDynamic, ResourceTable,
};

use crate::composable::linker_ops::{BoxedAsyncFunc, LinkContext, LinkerOps, ValVisitor};
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
// Visitor implementations — access proxy table via T: ResourceProxyView
// ---------------------------------------------------------------------------

/// Params visitor: consumer proxy handle → producer `ResourceAny`.
struct ParamsVisitor<'a, 'b, T: ResourceProxyView + 'static>(
    &'a mut wasmtime::StoreContextMut<'b, T>,
);

impl<T: ResourceProxyView + 'static> ValVisitor<ResourceAny> for ParamsVisitor<'_, '_, T> {
    fn visit(&mut self, ra: &mut ResourceAny, _ty_id: u32) -> wasmtime::Result<()> {
        if let Ok(rd) = ResourceDynamic::try_from_resource_any(*ra, &mut *self.0) {
            let proxy_id = rd.rep();
            if let Ok(producer_ra) = self
                .0
                .data_mut()
                .proxy_table()
                .get(&Resource::<ResourceAny>::new_borrow(proxy_id))
            {
                *ra = *producer_ra;
            }
        }
        Ok(())
    }
}

/// Results visitor: producer `ResourceAny` → consumer proxy handle.
struct ResultsVisitor<'a, 'b, T: ResourceProxyView + 'static>(
    &'a mut wasmtime::StoreContextMut<'b, T>,
);

impl<T: ResourceProxyView + 'static> ValVisitor<ResourceAny> for ResultsVisitor<'_, '_, T> {
    fn visit(&mut self, ra: &mut ResourceAny, ty_id: u32) -> wasmtime::Result<()> {
        let rep = self
            .0
            .data_mut()
            .proxy_table()
            .push(*ra)
            .expect("resource table full")
            .rep();
        let rd = ResourceDynamic::new_own(rep, ty_id);
        *ra = rd.try_into_resource_any(&mut *self.0)?;
        Ok(())
    }
}

impl<'a, T: ResourceProxyView + Send + 'static> LinkerOps for LinkerInstance<'a, T> {
    fn func_new_async(
        &mut self,
        name: &str,
        func: BoxedAsyncFunc,
        ctx: LinkContext,
    ) -> Result<(), CompositionError> {
        let ctx = Arc::new(ctx);
        let func = Arc::new(func);
        LinkerInstance::func_new_async(
            self,
            name,
            move |mut store, _ty: types::ComponentFunc, params, results| {
                let ctx = ctx.clone();
                let func = func.clone();
                let effective_params =
                    (ctx.resource.params)(params, &mut ParamsVisitor(&mut store))
                        .unwrap_or_else(|_| std::borrow::Cow::Borrowed(params));
                Box::new(async move {
                    func(&effective_params, results).await?;
                    (ctx.resource.results)(results, &mut ResultsVisitor(&mut store))?;
                    Ok(())
                })
            },
        )?;
        Ok(())
    }

    #[cfg(feature = "component-model-async")]
    fn func_new_concurrent(
        &mut self,
        name: &str,
        func: BoxedConcurrentFunc,
        ctx: LinkContext,
    ) -> Result<(), CompositionError> {
        let ctx = Arc::new(ctx);
        let func = Arc::new(func);
        LinkerInstance::func_new_concurrent(
            self,
            name,
            move |accessor, _ty: types::ComponentFunc, params, results| {
                let effective_params = accessor.with(|mut access| {
                    let mut store = access.as_context_mut();
                    (ctx.resource.params)(params, &mut ParamsVisitor(&mut store))
                        .unwrap_or_else(|_| std::borrow::Cow::Borrowed(params))
                        .into_owned()
                });
                let ctx = ctx.clone();
                let func = func.clone();
                Box::pin(async move {
                    func(&effective_params, results).await?;
                    accessor.with(|mut access| {
                        let mut store = access.as_context_mut();
                        (ctx.resource.results)(results, &mut ResultsVisitor(&mut store))
                    })
                })
            },
        )?;
        Ok(())
    }

    fn resource(
        &mut self,
        name: &str,
        ty: ResourceType,
    ) -> Result<(), CompositionError> {
        LinkerInstance::resource_async(self, name, ty, move |mut store, rep| {
            let _ = store
                .data_mut()
                .proxy_table()
                .delete(Resource::<ResourceAny>::new_own(rep));
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
