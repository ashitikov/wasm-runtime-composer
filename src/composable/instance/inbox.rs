use tokio::sync::mpsc;
use wasmtime::Store;
use wasmtime::component::Instance;

use crate::error::CompositionError;
use super::channel::{ChannelTask, ReplyTx};

#[cfg(feature = "component-model-async")]
pub(super) async fn inbox_loop<T: Send + 'static>(
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
pub(super) async fn inbox_loop<T: Send + 'static>(
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
