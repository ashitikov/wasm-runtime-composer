mod composable;
mod composition;
mod composition_builder;
mod descriptor;
mod error;
mod filtered;
mod priority_pool;
pub mod composable_instance;
pub mod linker_ops;

pub use composable::{Composable, ComposableType, ExportFunc, InterfaceSet};
pub use composition::Composition;
pub use composition_builder::CompositionBuilder;
pub use descriptor::ComposableDescriptor;
pub use error::CompositionError;
pub use filtered::{ExportFilter, Filtered};
pub use priority_pool::{PoolGuard, PriorityPool};

pub use wasmtime;
