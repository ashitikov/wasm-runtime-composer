use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use wasmtime::Store;
use wasmtime::component::types::{ComponentFunc, ComponentInstance, ComponentItem};
use wasmtime::component::{
    Component, ComponentExportIndex, Instance, ResourceAny, ResourceDynamic, ResourceType, Val,
};

use crate::composable::{ComposableType, ExportFunc, InterfaceSet, ResolvedImport};
use crate::linker_ops::LinkerOps;
use crate::{Composable, CompositionError};

// ---------------------------------------------------------------------------
// Resource proxy table
// ---------------------------------------------------------------------------

/// Global counter for unique `ResourceType::host_dynamic` payloads.
static NEXT_RESOURCE_TY_ID: AtomicU32 = AtomicU32::new(0);

/// Maps proxy IDs (u32) to producer-side `ResourceAny` handles.
///
/// Shared (via `Arc`) between linker callbacks (remap params/results)
/// and the resource destructor (cleanup on drop).
pub struct ResourceProxyTable {
    entries: Mutex<Vec<Option<ResourceAny>>>,
    /// Unique payload for `ResourceType::host_dynamic()`.
    ty_id: u32,
}

impl ResourceProxyTable {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            ty_id: NEXT_RESOURCE_TY_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Store a producer-side `ResourceAny` and return its proxy ID.
    fn insert(&self, producer_ra: ResourceAny) -> u32 {
        let mut entries = self.entries.lock().unwrap();
        // Reuse a vacant slot if available.
        for (i, slot) in entries.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(producer_ra);
                return i as u32;
            }
        }
        let id = entries.len() as u32;
        entries.push(Some(producer_ra));
        id
    }

    /// Look up the producer-side `ResourceAny` for a proxy ID.
    fn get(&self, proxy_id: u32) -> Option<ResourceAny> {
        let entries = self.entries.lock().unwrap();
        entries.get(proxy_id as usize).copied().flatten()
    }

    /// Remove and return the producer-side `ResourceAny` for a proxy ID.
    pub(crate) fn remove(&self, proxy_id: u32) -> Option<ResourceAny> {
        let mut entries = self.entries.lock().unwrap();
        entries.get_mut(proxy_id as usize).and_then(|slot| slot.take())
    }

    /// Replace producer-side `ResourceAny` values in results with consumer
    /// proxy handles created via `ResourceDynamic`.
    pub(crate) fn remap_results<T>(
        &self,
        store: &mut wasmtime::StoreContextMut<'_, T>,
        results: &mut [Val],
    ) -> wasmtime::Result<()> {
        for val in results.iter_mut() {
            if let Val::Resource(producer_ra) = val {
                let proxy_id = self.insert(*producer_ra);
                let rd = ResourceDynamic::new_own(proxy_id, self.ty_id);
                *val = Val::Resource(rd.try_into_resource_any(&mut *store)?);
            }
        }
        Ok(())
    }

    /// Replace consumer proxy handles in params with the original
    /// producer-side `ResourceAny` values.
    ///
    /// Returns `None` if no resources are present (no allocation needed).
    pub(crate) fn remap_params<T>(
        &self,
        store: &mut wasmtime::StoreContextMut<'_, T>,
        params: &[Val],
    ) -> Option<Vec<Val>> {
        if !params.iter().any(|v| matches!(v, Val::Resource(_))) {
            return None;
        }
        let mut remapped = params.to_vec();
        for val in remapped.iter_mut() {
            if let Val::Resource(consumer_ra) = val {
                if let Ok(rd) =
                    ResourceDynamic::try_from_resource_any(*consumer_ra, &mut *store)
                {
                    if let Some(producer_ra) = self.get(rd.rep()) {
                        *val = Val::Resource(producer_ra);
                    }
                }
            }
        }
        Some(remapped)
    }
}

// ---------------------------------------------------------------------------
// Channel task types
// ---------------------------------------------------------------------------

/// Raw pointers to caller-owned params and results buffers.
///
/// # Safety
///
/// The caller **must** await the reply before the references these pointers
/// were created from are dropped.  This is enforced by the `ExportFunc` /
/// linker-callback implementations: the raw pointers are created from
/// references whose lifetime outlasts the reply wait.
struct RawCallData {
    params: *const [Val],
    results: *mut [Val],
}

// SAFETY: The pointed-to data lives on the caller's stack and remains
// valid until the reply is received. Only the inbox loop dereferences these
// pointers, and it does so before sending the reply.
unsafe impl Send for RawCallData {}

/// Reply channel — always async (oneshot).
type ReplyTx = tokio::sync::oneshot::Sender<Result<(), CompositionError>>;

/// A single function-call request sent through the channel.
struct ChannelTask {
    export_index: ComponentExportIndex,
    func_type: ComponentFunc,
    data: RawCallData,
    reply: ReplyTx,
}

// ---------------------------------------------------------------------------
// Error conversions (channel internals → CompositionError / wasmtime::Error)
// ---------------------------------------------------------------------------

impl From<mpsc::error::SendError<ChannelTask>> for CompositionError {
    fn from(_: mpsc::error::SendError<ChannelTask>) -> Self {
        Self::Unavailable
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for CompositionError {
    fn from(_: tokio::sync::oneshot::error::RecvError) -> Self {
        Self::Unavailable
    }
}

fn into_wasmtime_error(e: CompositionError) -> wasmtime::Error {
    match e {
        CompositionError::Runtime(e) => e,
        e => e.into(),
    }
}

/// Send a call through the channel and await the reply.
async fn send_call(
    tx: mpsc::WeakUnboundedSender<ChannelTask>,
    export_index: ComponentExportIndex,
    func_type: ComponentFunc,
    data: RawCallData,
) -> Result<(), CompositionError> {
    let tx = tx.upgrade().ok_or(CompositionError::Unavailable)?;
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send(ChannelTask {
        export_index,
        func_type,
        data,
        reply: reply_tx,
    })?;
    reply_rx.await?
}

// ---------------------------------------------------------------------------
// Inbox loop
// ---------------------------------------------------------------------------

#[cfg(feature = "component-model-async")]
async fn inbox_loop<T: Send + 'static>(
    mut store: Store<T>,
    instance: Instance,
    mut rx: mpsc::UnboundedReceiver<ChannelTask>,
) {
    let mut batch = Vec::new();
    let mut async_run = Vec::new();
    while rx.recv_many(&mut batch, usize::MAX).await > 0 {
        // Process tasks in order, batching consecutive async tasks into
        // dispatch_concurrent while executing sync tasks between them.
        for task in batch.drain(..) {
            if task.func_type.async_() {
                async_run.push(task);
            } else {
                // Flush accumulated async run before the sync task.
                if !async_run.is_empty() {
                    dispatch_concurrent(&mut store, &instance, &mut async_run).await;
                }
                dispatch_async(&mut store, &instance, &mut vec![task]).await;
            }
        }
        // Flush trailing async run.
        if !async_run.is_empty() {
            dispatch_concurrent(&mut store, &instance, &mut async_run).await;
        }
    }
}

#[cfg(not(feature = "component-model-async"))]
async fn inbox_loop<T: Send + 'static>(
    mut store: Store<T>,
    instance: Instance,
    mut rx: mpsc::UnboundedReceiver<ChannelTask>,
) {
    let mut batch = Vec::new();
    while rx.recv_many(&mut batch, usize::MAX).await > 0 {
        dispatch_async(&mut store, &instance, &mut batch).await;
    }
}

/// Shared reply slots for concurrent dispatch.
///
/// Allows the closure inside `run_concurrent` to send replies immediately
/// as each task completes, while the outer scope retains access to unsent
/// replies for error recovery after future cancellation.
///
/// Contention is zero: `poll_until` polls futures sequentially on one thread.
#[cfg(feature = "component-model-async")]
#[derive(Clone)]
struct ReplySlots(std::sync::Arc<tokio::sync::Mutex<Vec<Option<ReplyTx>>>>);

#[cfg(feature = "component-model-async")]
impl ReplySlots {
    fn new(replies: Vec<Option<ReplyTx>>) -> Self {
        Self(std::sync::Arc::new(tokio::sync::Mutex::new(replies)))
    }

    /// Take the reply channel at `idx` and send the result through it.
    async fn send(&self, idx: usize, result: Result<(), CompositionError>) {
        if let Some(reply) = self.0.lock().await[idx].take() {
            let _ = reply.send(result);
        }
    }

    /// Propagate an error to all replies that haven't been sent yet.
    ///
    /// The first unsent slot receives the original error; the rest get `Unavailable`.
    async fn propagate_error(&self, error: wasmtime::Error) {
        let mut error = Some(CompositionError::Runtime(error));
        for slot in self.0.lock().await.iter_mut() {
            if let Some(reply) = slot.take() {
                let e = error.take().unwrap_or(CompositionError::Unavailable);
                let _ = reply.send(Err(e));
            }
        }
    }
}

/// Dispatch tasks concurrently via `run_concurrent` + `call_concurrent`.
///
/// Drains `tasks` and sends replies individually as each completes.
/// If `run_concurrent` fails (e.g. a subtask traps), the closure is cancelled
/// via future-drop; any replies not yet sent receive the error.
#[cfg(feature = "component-model-async")]
async fn dispatch_concurrent<T: Send + 'static>(
    store: &mut Store<T>,
    instance: &Instance,
    tasks: &mut Vec<ChannelTask>,
) {
    let funcs: Vec<_> = tasks
        .iter()
        .map(|task| instance.get_func(&mut *store, &task.export_index))
        .collect();

    // Destructure tasks synchronously so raw pointers never enter
    // the async generator state (raw pointers are !Send).
    let mut replies = Vec::new();
    let prepared: Vec<_> = tasks
        .drain(..)
        .zip(funcs)
        .enumerate()
        .map(|(idx, (task, func))| {
            let params = unsafe { &*task.data.params };
            let results = unsafe { &mut *task.data.results };
            replies.push(Some(task.reply));
            (func, params, results, idx)
        })
        .collect();

    let slots = ReplySlots::new(replies);

    if let Err(e) = store
        .run_concurrent(async |accessor| {
            let futs: Vec<_> = prepared
                .into_iter()
                .map(|(func, params, results, idx)| {
                    let slots = slots.clone();
                    async move {
                        let result = match func {
                            Some(f) => async {
                                let exit = f
                                    .call_concurrent(&accessor, params, results)
                                    .await
                                    .map_err(CompositionError::from)?;
                                exit.block(&accessor).await;
                                Ok(())
                            }
                            .await,
                            None => Err(CompositionError::FuncNotFound),
                        };
                        slots.send(idx, result).await;
                    }
                })
                .collect();
            futures::future::join_all(futs).await;
        })
        .await
    {
        slots.propagate_error(e).await;
    }
}

/// Dispatch tasks sequentially via `call_async` + `post_return_async`.
///
/// Drains `tasks` and sends replies.
async fn dispatch_async<T: Send>(
    store: &mut Store<T>,
    instance: &Instance,
    tasks: &mut Vec<ChannelTask>,
) {
    for task in tasks.drain(..) {
        // SAFETY: caller awaits reply before dropping data.
        let (params, results) = unsafe { (&*task.data.params, &mut *task.data.results) };
        let result = match instance.get_func(&mut *store, &task.export_index) {
            Some(func) => match func.call_async(&mut *store, params, results).await {
                Ok(()) => func
                    .post_return_async(&mut *store)
                    .await
                    .map_err(CompositionError::from),
                Err(e) => Err(CompositionError::from(e)),
            },
            None => Err(CompositionError::FuncNotFound),
        };
        let _ = task.reply.send(result);
    }
}

// ---------------------------------------------------------------------------
// ComposableInstance
// ---------------------------------------------------------------------------

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
        }
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
        self.link_export_item(name, &export_type, None, linker, None)
    }
}

impl ComposableInstance {
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

    fn link_export_item(
        &self,
        name: &str,
        item: &ComponentItem,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
        proxy: Option<&Arc<ResourceProxyTable>>,
    ) -> Result<(), CompositionError> {
        match item {
            ComponentItem::ComponentFunc(func_ty) => {
                self.link_export_function(name, func_ty, parent, linker, proxy)
            }
            ComponentItem::ComponentInstance(instance_ty) => {
                self.link_export_instance(name, instance_ty, parent, linker)
            }
            ComponentItem::Component(component_ty) => {
                self.link_export_component(name, component_ty, parent, linker)
            }
            ComponentItem::Resource(resource_ty) => {
                self.link_export_resource(name, resource_ty, parent, linker, proxy)
            }
            ComponentItem::Type(_) => Ok(()),
            ComponentItem::CoreFunc(_) => Err(CompositionError::LinkingError(
                "CoreFunc exports not supported".to_string(),
            )),
            ComponentItem::Module(_) => Err(CompositionError::LinkingError(
                "Module exports not supported".to_string(),
            )),
        }
    }

    fn link_export_function(
        &self,
        name: &str,
        func_ty: &ComponentFunc,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
        proxy: Option<&Arc<ResourceProxyTable>>,
    ) -> Result<(), CompositionError> {
        let export_index = self
            .component
            .get_export_index(parent, name)
            .ok_or_else(|| {
                CompositionError::LinkingError(format!(
                    "Export index for '{}' not found in component",
                    name
                ))
            })?;

        let tx = self.tx.downgrade();
        let func_type = func_ty.clone();
        let proxy = proxy.cloned();

        #[cfg(feature = "component-model-async")]
        if func_ty.async_() {
            return linker.func_new_concurrent(
                name,
                Box::new(move |params: &[Val], results: &mut [Val]| {
                    let tx = tx.clone();
                    let func_type = func_type.clone();
                    let data = RawCallData {
                        params: params as *const [Val],
                        results: results as *mut [Val],
                    };
                    Box::pin(async move {
                        send_call(tx, export_index, func_type, data)
                            .await
                            .map_err(into_wasmtime_error)
                    })
                }),
                proxy,
            );
        }

        linker.func_new_async(
            name,
            Box::new(move |params: &[Val], results: &mut [Val]| {
                let tx = tx.clone();
                let func_type = func_type.clone();
                let data = RawCallData {
                    params: params as *const [Val],
                    results: results as *mut [Val],
                };
                Box::pin(async move {
                    send_call(tx, export_index, func_type, data)
                        .await
                        .map_err(into_wasmtime_error)
                })
            }),
            proxy,
        )
    }

    fn link_export_component(
        &self,
        name: &str, // TODO: check if needed and add tests ?
        component_ty: &wasmtime::component::types::Component,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        let engine = self.component.engine().clone();
        let items: Vec<_> = component_ty.imports(&engine).collect();
        for (name, item) in items {
            self.link_export_item(name, &item, parent, linker, None)?;
        }

        Ok(())
    }

    fn link_export_resource(
        &self,
        name: &str,
        _resource_ty: &ResourceType,
        _parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
        proxy: Option<&Arc<ResourceProxyTable>>,
    ) -> Result<(), CompositionError> {
        let proxy = proxy.ok_or_else(|| {
            CompositionError::LinkingError(format!(
                "Resource '{}' has no proxy table (should be created by parent instance)",
                name
            ))
        })?;
        let ty = ResourceType::host_dynamic(proxy.ty_id);
        linker.resource(name, ty, Some(proxy.clone()))
    }

    fn link_export_instance(
        &self,
        name: &str,
        instance_ty: &ComponentInstance,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        let instance_export_index =
            self.component
                .get_export_index(parent, name)
                .ok_or_else(|| {
                    CompositionError::LinkingError(format!(
                        "Export index for instance '{}' not found in component",
                        name
                    ))
                })?;

        let engine = self.component.engine();
        let exports: Vec<_> = instance_ty
            .exports(engine)
            .map(|(n, ty)| (n.to_string(), ty))
            .collect();

        // Create a proxy table if the interface contains any resources.
        let has_resources = exports
            .iter()
            .any(|(_, ty)| matches!(ty, ComponentItem::Resource(_)));
        let proxy = if has_resources {
            Some(Arc::new(ResourceProxyTable::new()))
        } else {
            None
        };

        let this = self;
        linker.with_instance(
            name,
            Box::new(move |sub_linker| {
                for (export_name, export_ty) in &exports {
                    this.link_export_item(
                        export_name,
                        export_ty,
                        Some(&instance_export_index),
                        sub_linker,
                        proxy.as_ref(),
                    )?;
                }
                Ok(())
            }),
        )
    }
}
