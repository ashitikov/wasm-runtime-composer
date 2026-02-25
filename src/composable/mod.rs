pub mod component;
pub mod composition;
pub mod filtered;
pub mod instance;
pub mod linker_instance_ops;

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use wasmtime::component::Val;

use crate::error::CompositionError;
use linker_instance_ops::LinkerInstanceOps;

/// Set of interface names (exports or imports).
pub type InterfaceSet = HashSet<String>;

/// A resolved internal link between two composables.
#[derive(Clone, Debug)]
pub struct ResolvedImport {
    /// ID of the composable that imports.
    pub importer_id: String,
    /// ID of the composable that exports (provides the import).
    pub exporter_id: String,
    /// Interface name.
    pub interface: String,
}

/// Type signature of a composable (what it exports and imports).
#[derive(Clone, Default)]
pub struct ComposableType {
    exports: InterfaceSet,
    imports: InterfaceSet,
}

impl ComposableType {
    pub fn new(exports: InterfaceSet, imports: InterfaceSet) -> Self {
        Self { exports, imports }
    }

    pub fn exports(&self) -> &InterfaceSet {
        &self.exports
    }

    pub fn imports(&self) -> &InterfaceSet {
        &self.imports
    }
}

impl ResolvedImport {
    pub fn new(importer_id: String, exporter_id: String, interface: String) -> Self {
        Self {
            importer_id,
            exporter_id,
            interface,
        }
    }
}

/// Pre-resolved handle to an exported function.
///
/// Obtained via [`Composition::get_func`]. The closure inside captures
/// everything needed for the call (store, instance, export index), so
/// no further lookups happen at call time.
pub struct ExportFunc(
    Box<
        dyn for<'a> Fn(
                &'a [Val],
                &'a mut [Val],
            )
                -> Pin<Box<dyn Future<Output = Result<(), CompositionError>> + Send + 'a>>
            + Send
            + Sync,
    >,
);

impl ExportFunc {
    pub fn new<F>(f: F) -> Self
    where
        F: for<'a> Fn(
                &'a [Val],
                &'a mut [Val],
            )
                -> Pin<Box<dyn Future<Output = Result<(), CompositionError>> + Send + 'a>>
            + Send
            + Sync
            + 'static,
    {
        Self(Box::new(f))
    }

    pub fn call<'a>(
        &self,
        params: &'a [Val],
        results: &'a mut [Val],
    ) -> Pin<Box<dyn Future<Output = Result<(), CompositionError>> + Send + 'a>> {
        (self.0)(params, results)
    }
}

/// A composable unit — the composition-time API.
///
/// Implemented by `ComposableInstance`, `ComposableComponent`, `Filtered`,
/// and `Composition` (identity — for nesting a finalized composition).
pub trait Composable: Send {
    /// Get the type signature (imports/exports) of this composable.
    fn ty(&self) -> &ComposableType;

    /// Link a single export into the importer's linker.
    fn link_export(
        &mut self,
        name: &str,
        importer_linker: &mut dyn LinkerInstanceOps,
    ) -> Result<(), CompositionError>;

    /// Link a single import from the given exporter.
    ///
    /// Default returns an error — most composables don't accept import linking.
    ///
    /// `ComposableComponent` overrides this to register the exporter's
    /// functions in its own `Linker<T>`.
    fn link_import(
        &mut self,
        _name: &str,
        _exporter: &mut dyn Composable,
    ) -> Result<(), CompositionError> {
        Err(CompositionError::LinkingError(
            "this composable does not accept import linking".to_string(),
        ))
    }

    /// Finalize into a running `Composition`.
    ///
    /// Consumes the composable. For `ComposableInstance` this spawns the inbox loop.
    /// For `ComposableComponent` this creates a Store, instantiates, and spawns the inbox loop.
    fn into_composition(
        self: Box<Self>,
    ) -> Pin<Box<dyn Future<Output = Result<composition::Composition, CompositionError>> + Send>>;
}
