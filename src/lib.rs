pub mod error;
mod priority_pool;
pub mod composable;

pub use composable::{Composable, ComposableType, ExportFunc, InterfaceSet};
pub use composable::component::ComposableComponent;
pub use composable::instance::{ComposableInstance, ComposableLinker, ResourceProxyView};
pub use composable::composition::Composition;
pub use composable::composition::composer::Composer;
pub use composable::composition::descriptor::ComposableDescriptor;
pub use composable::filtered::{ExportFilter, Filtered};
pub use error::CompositionError;

pub use wasmtime;
