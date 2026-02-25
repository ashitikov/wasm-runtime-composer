use thiserror::Error;

/// Errors that can occur during composition or function dispatch.
#[derive(Debug, Error)]
pub enum CompositionError {
    /// No composable descriptors were added to the [`Composer`](crate::Composer).
    #[error("empty composition: no children added")]
    Empty,

    /// Two descriptors share the same ID.
    #[error("duplicate composable id: '{0}'")]
    DuplicateId(String),

    /// An import hint references a composable ID that doesn't exist.
    #[error("import hint for '{interface}' references unknown id '{target_id}'")]
    InvalidLinkReference { interface: String, target_id: String },

    /// Multiple composables export the same interface and no import hint was provided.
    #[error("ambiguous import '{interface}': multiple candidates {candidates:?}, add an import hint")]
    AmbiguousImport {
        interface: String,
        candidates: Vec<String>,
    },

    /// Multiple composables export the same interface and no export hint was provided.
    #[error("ambiguous export '{interface}': multiple providers {candidates:?}, add an export hint")]
    AmbiguousExport {
        interface: String,
        candidates: Vec<String>,
    },

    /// An import hint points to a composable that doesn't export the interface.
    #[error("import hint for '{interface}' points to '{target_id}', which does not export it")]
    ImportHintInvalid { interface: String, target_id: String },

    /// An export hint points to a composable that doesn't export the interface.
    #[error("export hint for '{interface}' points to '{target_id}', which does not export it")]
    ExportHintInvalid { interface: String, target_id: String },

    /// The requested function was not found in the composition's exports.
    #[error("function not found")]
    FuncNotFound,

    /// The link type for an export is not supported (e.g. CoreFunc, Module).
    #[error("unsupported link type for interface '{interface}'")]
    UnsupportedLinkType { interface: String },

    /// A generic linking error with a descriptive message.
    #[error("linking error: {0}")]
    LinkingError(String),

    /// The composable's inbox channel has been closed (component dropped or panicked).
    #[error("composable unavailable")]
    Unavailable,

    /// A wasmtime runtime error (instantiation, execution, traps, etc.).
    #[error(transparent)]
    Runtime(#[from] wasmtime::Error),
}
