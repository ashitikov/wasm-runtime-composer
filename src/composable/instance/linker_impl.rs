use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use wasmtime::AsContextMut;
use wasmtime::component::types::{self, ResourceType};
use wasmtime::component::{
    LinkerInstance, Resource, ResourceAny, ResourceDynamic, ResourceTable,
};

use crate::composable::linker_ops::{
    BoxedAsyncFunc, LinkContext, LinkerOps, ResourceMap, ValVisitor,
};
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
    fn visit(&mut self, ra: &mut ResourceAny, _ty: ResourceType) -> wasmtime::Result<()> {
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
///
/// Uses `resource_map` from `LinkContext` to look up the consumer's `ty_id`
/// for each producer `ResourceType`.
struct ResultsVisitor<'a, 'b, 'c, T: ResourceProxyView + 'static> {
    store: &'a mut wasmtime::StoreContextMut<'b, T>,
    resource_map: &'c [(ResourceType, u32)],
}

impl<T: ResourceProxyView + 'static> ValVisitor<ResourceAny> for ResultsVisitor<'_, '_, '_, T> {
    fn visit(&mut self, ra: &mut ResourceAny, ty: ResourceType) -> wasmtime::Result<()> {
        let ty_id = self
            .resource_map
            .iter()
            .find(|(rt, _)| *rt == ty)
            .map(|(_, id)| *id);
        let Some(ty_id) = ty_id else {
            // No mapping — leave ResourceAny as-is (no proxying active).
            return Ok(());
        };
        let rep = self
            .store
            .data_mut()
            .proxy_table()
            .push(*ra)
            .expect("resource table full")
            .rep();
        let rd = ResourceDynamic::new_own(rep, ty_id);
        *ra = rd.try_into_resource_any(&mut *self.store)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ComposableLinker — the sole LinkerOps implementor for wasmtime types
// ---------------------------------------------------------------------------

/// A linker that handles cross-store value proxying for compositions.
///
/// Wraps a wasmtime [`LinkerInstance`] and tracks resource type registrations.
/// When the producer registers resources via [`LinkerOps::resource`],
/// `ComposableLinker` generates a unique `ty_id`, registers a `host_dynamic`
/// resource with wasmtime, and stores the `(ResourceType, ty_id)` mapping.
///
/// When functions are registered, the mapping is injected into
/// [`LinkContext::resource_map`] so that resource values in params/results
/// are automatically proxied between stores.
pub struct ComposableLinker<'a, T: ResourceProxyView + Send + 'static> {
    inner: LinkerInstance<'a, T>,
    resource_map: ResourceMap,
    next_ty_id: Arc<AtomicU32>,
}

impl<'a, T: ResourceProxyView + Send + 'static> ComposableLinker<'a, T> {
    pub fn new(inner: LinkerInstance<'a, T>) -> Self {
        Self {
            inner,
            resource_map: Vec::new(),
            next_ty_id: Arc::new(AtomicU32::new(0)),
        }
    }

    fn new_child(inner: LinkerInstance<'a, T>, next_ty_id: Arc<AtomicU32>) -> Self {
        Self {
            inner,
            resource_map: Vec::new(),
            next_ty_id,
        }
    }
}

impl<'a, T: ResourceProxyView + Send + 'static> LinkerOps for ComposableLinker<'a, T> {
    fn func_new_async(
        &mut self,
        name: &str,
        func: BoxedAsyncFunc,
        mut ctx: LinkContext,
    ) -> Result<(), CompositionError> {
        ctx.resource_map = Some(Arc::new(self.resource_map.clone()));
        let ctx = Arc::new(ctx);
        let func = Arc::new(func);
        LinkerInstance::func_new_async(
            &mut self.inner,
            name,
            move |mut store, _ty: types::ComponentFunc, params, results| {
                let ctx = ctx.clone();
                let func = func.clone();
                let effective_params =
                    (ctx.resource.params)(params, &mut ParamsVisitor(&mut store))
                        .unwrap_or_else(|_| std::borrow::Cow::Borrowed(params));
                Box::new(async move {
                    func(&effective_params, results).await?;
                    let map: &[(ResourceType, u32)] = match &ctx.resource_map {
                        Some(m) => m,
                        None => &[],
                    };
                    (ctx.resource.results)(
                        results,
                        &mut ResultsVisitor {
                            store: &mut store,
                            resource_map: map,
                        },
                    )?;
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
        mut ctx: LinkContext,
    ) -> Result<(), CompositionError> {
        ctx.resource_map = Some(Arc::new(self.resource_map.clone()));
        let ctx = Arc::new(ctx);
        let func = Arc::new(func);
        LinkerInstance::func_new_concurrent(
            &mut self.inner,
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
                        let map: &[(ResourceType, u32)] = match &ctx.resource_map {
                            Some(m) => m,
                            None => &[],
                        };
                        (ctx.resource.results)(
                            results,
                            &mut ResultsVisitor {
                                store: &mut store,
                                resource_map: map,
                            },
                        )
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
        let ty_id = self.next_ty_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let host_ty = ResourceType::host_dynamic(ty_id);
        LinkerInstance::resource_async(&mut self.inner, name, host_ty, move |mut store, rep| {
            let _ = store
                .data_mut()
                .proxy_table()
                .delete(Resource::<ResourceAny>::new_own(rep));
            Box::new(async { Ok(()) })
        })?;
        self.resource_map.push((ty, ty_id));
        Ok(())
    }

    fn with_instance<'b>(
        &'b mut self,
        name: &str,
        f: Box<dyn FnOnce(&mut dyn LinkerOps) -> Result<(), CompositionError> + 'b>,
    ) -> Result<(), CompositionError> {
        let child = LinkerInstance::instance(&mut self.inner, name)?;
        let mut child_linker = ComposableLinker::new_child(child, self.next_ty_id.clone());
        f(&mut child_linker)
    }
}
