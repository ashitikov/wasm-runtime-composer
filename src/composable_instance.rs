use std::collections::HashMap;

use tokio::sync::mpsc;
use wasmtime::Store;
use wasmtime::component::types::{ComponentFunc, ComponentInstance, ComponentItem};
use wasmtime::component::{Component, ComponentExportIndex, Instance, Val};

use crate::composable::{ComposableType, ExportFunc, InterfaceSet, ResolvedImport};
use crate::linker_ops::LinkerOps;
use crate::{Composable, CompositionError};

// ---------------------------------------------------------------------------
// Call task types
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
// valid until the reply is received. Only the actor dereferences these
// pointers, and it does so before sending the reply.
unsafe impl Send for RawCallData {}

/// Reply channel — always async (oneshot).
type CallReply = tokio::sync::oneshot::Sender<Result<(), CompositionError>>;

/// A single function-call request sent to the store actor.
struct CallTask {
    export_index: ComponentExportIndex,
    func_type: ComponentFunc,
    data: RawCallData,
    reply: CallReply,
}

// ---------------------------------------------------------------------------
// Store actor
// ---------------------------------------------------------------------------

async fn run_actor<T: Send + 'static>(
    mut store: Store<T>,
    instance: Instance,
    mut rx: mpsc::UnboundedReceiver<CallTask>,
) {
    let mut batch = Vec::new();
    while rx.recv_many(&mut batch, usize::MAX).await > 0 {
        #[cfg(feature = "component-model-async")]
        {
            // Process tasks in order, batching consecutive async tasks into
            // run_concurrent while executing sync tasks between them.
            let mut async_run = Vec::new();
            for task in batch.drain(..) {
                if task.func_type.async_() {
                    async_run.push(task);
                } else {
                    // Flush accumulated async run before the sync task.
                    if !async_run.is_empty() {
                        flush_concurrent(&mut store, &instance, &mut async_run).await;
                    }
                    call_func(&mut store, &instance, task).await;
                }
            }
            // Flush trailing async run.
            if !async_run.is_empty() {
                flush_concurrent(&mut store, &instance, &mut async_run).await;
            }
        }

        #[cfg(not(feature = "component-model-async"))]
        {
            for task in batch.drain(..) {
                call_func(&mut store, &instance, task).await;
            }
        }
    }
}

/// Flush a run of consecutive async tasks via `run_concurrent` + `call_concurrent`.
///
/// Drains `tasks` and sends replies. If `run_concurrent` fails (e.g.
/// `concurrency_support` disabled in engine config), reply channels are dropped
/// and callers receive `ActorStopped`.
#[cfg(feature = "component-model-async")]
async fn flush_concurrent<T: Send + 'static>(
    store: &mut Store<T>,
    instance: &Instance,
    tasks: &mut Vec<CallTask>,
) {
    // Resolve Func handles while we still have &mut store.
    let funcs: Vec<_> = tasks
        .iter()
        .map(|task| instance.get_func(&mut *store, &task.export_index))
        .collect();

    // Destructure tasks synchronously so raw pointers never enter
    // the async generator state (raw pointers are !Send).
    let prepared: Vec<_> = tasks
        .drain(..)
        .zip(funcs)
        .map(|(task, func)| {
            // SAFETY: caller awaits reply before dropping data.
            let params = unsafe { &*task.data.params };
            let results = unsafe { &mut *task.data.results };
            (func, params, results, task.reply)
        })
        .collect();

    let _ = store
        .run_concurrent(async |accessor| {
            let futs: Vec<_> = prepared
                .into_iter()
                .map(|(func, params, results, reply)| async move {
                    let result = match func {
                        Some(f) => f
                            .call_concurrent(&accessor, params, results)
                            .await
                            .map(|_exit| ())
                            .map_err(CompositionError::from),
                        None => Err(CompositionError::FuncNotFound),
                    };
                    let _ = reply.send(result);
                })
                .collect();
            futures::future::join_all(futs).await;
        })
        .await;
}

/// Process a single function call task using `call_async`.
async fn call_func<T: Send>(store: &mut Store<T>, instance: &Instance, task: CallTask) {
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

// ---------------------------------------------------------------------------
// ComposableInstance
// ---------------------------------------------------------------------------

/// A composable backed by a wasmtime Instance.
///
/// The underlying [`Store`] is owned by a background actor task and accessed
/// exclusively through an internal channel — no `Arc<Mutex>` needed.
/// This makes `ComposableInstance` non-generic over the store data type `T`;
/// the type is erased at construction time.
pub struct ComposableInstance {
    /// Strong sender — keeps the actor alive while this instance exists.
    tx: mpsc::UnboundedSender<CallTask>,
    ty: ComposableType,
    export_types: HashMap<String, ComponentItem>,
    component: Component,
}

impl ComposableInstance {
    /// Create a new composable instance.
    ///
    /// Spawns a background actor task on the current tokio runtime that
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
        tokio::spawn(run_actor(store, instance, rx));

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
        self.link_export_item(name, &export_type, None, linker)
    }
}

impl ComposableInstance {
    /// Build an `ExportFunc` that sends a call task to the actor.
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
            Box::pin(async move {
                let tx = tx.upgrade().ok_or(CompositionError::ActorStopped)?;
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                tx.send(CallTask {
                    export_index,
                    func_type,
                    data,
                    reply: reply_tx,
                })
                .map_err(|_| CompositionError::ActorStopped)?;
                reply_rx.await.map_err(|_| CompositionError::ActorStopped)?
            })
        })
    }

    fn link_export_item(
        &self,
        name: &str,
        item: &ComponentItem,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        match item {
            ComponentItem::ComponentFunc(func_ty) => {
                self.link_export_function(name, func_ty, parent, linker)
            }
            ComponentItem::ComponentInstance(instance_ty) => {
                self.link_export_instance(name, instance_ty, parent, linker)
            }
            ComponentItem::Type(_) => Ok(()),
            ComponentItem::CoreFunc(_) => Err(CompositionError::LinkingError(
                "CoreFunc exports not supported".to_string(),
            )),
            ComponentItem::Module(_) => Err(CompositionError::LinkingError(
                "Module exports not supported".to_string(),
            )),
            ComponentItem::Component(_) => Err(CompositionError::LinkingError(
                "Component exports not supported".to_string(),
            )),
            ComponentItem::Resource(_) => Err(CompositionError::LinkingError(
                "Resource exports not yet supported".to_string(),
            )),
        }
    }

    fn link_export_function(
        &self,
        name: &str,
        func_ty: &ComponentFunc,
        parent: Option<&ComponentExportIndex>,
        linker: &mut dyn LinkerOps,
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

        #[cfg(feature = "component-model-async")]
        if func_ty.async_() {
            return linker.func_new_concurrent(
                name,
                Box::new(move |params: &[Val], results: &mut [Val]| {
                    let tx = tx.clone();
                    let export_index = export_index.clone();
                    let func_type = func_type.clone();
                    let data = RawCallData {
                        params: params as *const [Val],
                        results: results as *mut [Val],
                    };
                    Box::pin(async move {
                        let tx = tx
                            .upgrade()
                            .ok_or_else(|| wasmtime::Error::msg("actor stopped"))?;
                        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                        tx.send(CallTask {
                            export_index,
                            func_type,
                            data,
                            reply: reply_tx,
                        })
                        .map_err(|_| wasmtime::Error::msg("actor stopped"))?;
                        reply_rx
                            .await
                            .map_err(|_| wasmtime::Error::msg("actor stopped"))?
                            .map_err(|e| match e {
                                CompositionError::Runtime(e) => e,
                                other => wasmtime::Error::msg(other.to_string()),
                            })
                    })
                }),
            );
        }

        linker.func_new_async(
            name,
            Box::new(move |params: &[Val], results: &mut [Val]| {
                let tx = tx.clone();
                let export_index = export_index.clone();
                let func_type = func_type.clone();
                let data = RawCallData {
                    params: params as *const [Val],
                    results: results as *mut [Val],
                };
                Box::pin(async move {
                    let tx = tx
                        .upgrade()
                        .ok_or_else(|| wasmtime::Error::msg("actor stopped"))?;
                    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                    tx.send(CallTask {
                        export_index,
                        func_type,
                        data,
                        reply: reply_tx,
                    })
                    .map_err(|_| wasmtime::Error::msg("actor stopped"))?;
                    reply_rx
                        .await
                        .map_err(|_| wasmtime::Error::msg("actor stopped"))?
                        .map_err(|e| match e {
                            CompositionError::Runtime(e) => e,
                            other => wasmtime::Error::msg(other.to_string()),
                        })
                })
            }),
        )
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
                    )?;
                }
                Ok(())
            }),
        )
    }
}
