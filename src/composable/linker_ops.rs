use std::borrow::Cow;

use wasmtime::component::types::ResourceType;
use wasmtime::component::{ResourceAny, Val};

use crate::error::CompositionError;

/// Visitor for cross-store value remapping.
///
/// Generic over the value type `V` (e.g. `ResourceAny`).
/// Implemented by the concrete `LinkerOps` implementation that has
/// access to the store and knows `T`.
///
/// Direction is determined by which visitor instance is passed,
/// not by which method is called.
pub trait ValVisitor<V> {
    fn visit(&mut self, val: &mut V, ty_id: u32) -> wasmtime::Result<()>;
}

/// Pre-compiled navigator for results (in-place on `&mut [Val]`).
pub type ResultsMapper = Box<
    dyn Fn(&mut [Val], &mut dyn ValVisitor<ResourceAny>) -> wasmtime::Result<()> + Send + Sync,
>;

/// Pre-compiled navigator for params.
///
/// Returns `Cow`: `Borrowed` when no resources (zero clone),
/// `Owned` when resources are present (clone + remap).
/// wasmtime gives params as `&[Val]` (immutable), so mutation requires a clone.
pub type ParamsMapper = Box<
    dyn for<'a> Fn(
            &'a [Val],
            &mut dyn ValVisitor<ResourceAny>,
        ) -> wasmtime::Result<Cow<'a, [Val]>>
        + Send
        + Sync,
>;

/// Compiled navigators for resource values in params/results.
pub struct ValMapper {
    pub params: ParamsMapper,
    pub results: ResultsMapper,
}

impl ValMapper {
    pub fn noop() -> Self {
        Self {
            params: Box::new(|params, _visitor| Ok(Cow::Borrowed(params))),
            results: Box::new(|_results, _visitor| Ok(())),
        }
    }
}

/// Link context for a single function.
///
/// Always present (no Option). Default — no-op mappers.
pub struct LinkContext {
    pub resource: ValMapper,
}

impl LinkContext {
    /// No-op context: no remapping performed.
    pub fn noop() -> Self {
        Self {
            resource: ValMapper::noop(),
        }
    }
}

/// Type-erased linker operations.
///
/// This trait allows composables to add definitions to a linker
/// without knowing the concrete store type `T`.
pub trait LinkerOps {
    /// Add an async function definition.
    fn func_new_async(
        &mut self,
        name: &str,
        func: BoxedAsyncFunc,
        ctx: LinkContext,
    ) -> Result<(), CompositionError>;

    /// Add a concurrent async function definition.
    ///
    /// Registered via wasmtime's `func_new_concurrent` so multiple invocations
    /// can run concurrently (uses `Accessor<T>` instead of exclusive `StoreContextMut`).
    #[cfg(feature = "component-model-async")]
    fn func_new_concurrent(
        &mut self,
        name: &str,
        func: BoxedConcurrentFunc,
        ctx: LinkContext,
    ) -> Result<(), CompositionError>;

    /// Register a resource type.
    ///
    /// The implementation provides the appropriate destructor for its store type.
    fn resource(
        &mut self,
        name: &str,
        ty: ResourceType,
    ) -> Result<(), CompositionError>;

    /// Execute a callback with a sub-linker for a nested instance.
    fn with_instance<'a>(
        &'a mut self,
        name: &str,
        f: Box<dyn FnOnce(&mut dyn LinkerOps) -> Result<(), CompositionError> + 'a>,
    ) -> Result<(), CompositionError>;
}

/// Type-erased function for async calls.
/// Receives params and a pre-allocated results slice, writes results in-place.
pub type BoxedAsyncFunc = Box<
    dyn for<'a> Fn(
            &'a [Val],
            &'a mut [Val],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = wasmtime::Result<()>> + Send + 'a>,
        > + Send
        + Sync
        + 'static,
>;

/// Type-erased function for concurrent async calls.
/// Same callback shape as `BoxedAsyncFunc`, but registered via
/// `func_new_concurrent` so multiple invocations can run concurrently.
#[cfg(feature = "component-model-async")]
pub type BoxedConcurrentFunc = Box<
    dyn for<'a> Fn(
            &'a [Val],
            &'a mut [Val],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = wasmtime::Result<()>> + Send + 'a>,
        > + Send
        + Sync
        + 'static,
>;
