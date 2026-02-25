//! Runtime composition of WebAssembly components.
//!
//! This crate composes multiple wasmtime WebAssembly components at runtime
//! by automatically resolving imports/exports and wiring them together
//! through internal channels. Resources are transparently proxied across
//! component boundaries.
//!
//! # Core workflow
//!
//! 1. Create composable units ([`ComposableComponent`] or [`ComposableInstance`])
//! 2. Add them to a [`Composer`] via [`ComposableDescriptor`]
//! 3. Call [`Composer::compose()`] — imports are resolved, components are
//!    instantiated, and an inbox loop is spawned for each
//! 4. Use the resulting [`Composition`] to call exported functions

pub mod error;
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
