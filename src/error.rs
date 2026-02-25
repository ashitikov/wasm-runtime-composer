use thiserror::Error;

#[derive(Debug, Error)]
pub enum CompositionError {
    #[error("empty composition: no children added")]
    Empty,

    #[error("duplicate composable id: '{0}'")]
    DuplicateId(String),

    #[error("import hint for '{interface}' references unknown id '{target_id}'")]
    InvalidLinkReference { interface: String, target_id: String },

    #[error("ambiguous import '{interface}': multiple candidates {candidates:?}, add an import hint")]
    AmbiguousImport {
        interface: String,
        candidates: Vec<String>,
    },

    #[error("ambiguous export '{interface}': multiple providers {candidates:?}, add an export hint")]
    AmbiguousExport {
        interface: String,
        candidates: Vec<String>,
    },

    #[error("import hint for '{interface}' points to '{target_id}', which does not export it")]
    ImportHintInvalid { interface: String, target_id: String },

    #[error("export hint for '{interface}' points to '{target_id}', which does not export it")]
    ExportHintInvalid { interface: String, target_id: String },
    
    #[error("function not found")]
    FuncNotFound,

    #[error("unsupported link type for interface '{interface}'")]
    UnsupportedLinkType { interface: String },

    #[error("linking error: {0}")]
    LinkingError(String),

    #[error("composable unavailable")]
    Unavailable,

    #[error("component already instantiated")]
    AlreadyInstantiated,

    #[error(transparent)]
    Runtime(#[from] wasmtime::Error),
}
