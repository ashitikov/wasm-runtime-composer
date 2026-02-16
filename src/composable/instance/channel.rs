use tokio::sync::mpsc;
use wasmtime::component::types::ComponentFunc;
use wasmtime::component::{ComponentExportIndex, Val};

use crate::error::CompositionError;

/// Raw pointers to caller-owned params and results buffers.
///
/// # Safety
///
/// The caller **must** await the reply before the references these pointers
/// were created from are dropped.  This is enforced by the `ExportFunc` /
/// linker-callback implementations: the raw pointers are created from
/// references whose lifetime outlasts the reply wait.
pub(super) struct RawCallData {
    pub params: *const [Val],
    pub results: *mut [Val],
}

// SAFETY: The pointed-to data lives on the caller's stack and remains
// valid until the reply is received. Only the inbox loop dereferences these
// pointers, and it does so before sending the reply.
unsafe impl Send for RawCallData {}

/// Reply channel — always async (oneshot).
pub(super) type ReplyTx = tokio::sync::oneshot::Sender<Result<(), CompositionError>>;

/// A single function-call request sent through the channel.
pub(super) struct ChannelTask {
    pub export_index: ComponentExportIndex,
    pub func_type: ComponentFunc,
    pub data: RawCallData,
    pub reply: ReplyTx,
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

pub(super) fn into_wasmtime_error(e: CompositionError) -> wasmtime::Error {
    match e {
        CompositionError::Runtime(e) => e,
        e => e.into(),
    }
}

/// Send a call through the channel and await the reply.
pub(super) async fn send_call(
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
