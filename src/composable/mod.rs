pub mod composition;
pub mod filtered;
pub mod instance;
pub mod linker_ops;

use std::collections::HashSet;
use std::pin::Pin;
use std::future::Future;

use wasmtime::component::types::ComponentItem;
use wasmtime::component::{ComponentExportIndex, Val};

use crate::error::CompositionError;
use linker_ops::LinkerOps;

/// Set of interface names (exports or imports).
pub type InterfaceSet = HashSet<String>;

/// A resolved internal link between two composables.
#[derive(Clone)]
pub struct ResolvedImport {
    /// ID of the composable that imports.
    pub importer_id: String,
    /// ID of the composable that exports (provides the import).
    pub exporter_id: String,
    /// Interface name.
    pub interface: String,
    /// Wasmtime type of the linked item (populated during linking).
    pub ty: Option<ComponentItem>,
    /// Parent export index for nested exports (e.g. functions inside an instance).
    pub parent_export_index: Option<ComponentExportIndex>,
}

impl std::fmt::Debug for ResolvedImport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedImport")
            .field("importer_id", &self.importer_id)
            .field("exporter_id", &self.exporter_id)
            .field("interface", &self.interface)
            .field("ty", &self.ty.as_ref().map(|_| ".."))
            .field("parent_export_index", &self.parent_export_index.as_ref().map(|_| ".."))
            .finish()
    }
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
    /// Create a new top-level resolved import (no type info yet).
    pub fn new(importer_id: String, exporter_id: String, interface: String) -> Self {
        Self {
            importer_id,
            exporter_id,
            interface,
            ty: None,
            parent_export_index: None,
        }
    }
}

/// Pre-resolved handle to an exported function.
///
/// Obtained via [`Composable::get_func`]. The closure inside captures
/// everything needed for the call (store, instance, export index), so
/// no further lookups happen at call time.
pub struct ExportFunc(
    Box<
        dyn for<'a> Fn(
                &'a [Val],
                &'a mut [Val],
            ) -> Pin<Box<dyn Future<Output = Result<(), CompositionError>> + Send + 'a>>
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
            ) -> Pin<Box<dyn Future<Output = Result<(), CompositionError>> + Send + 'a>>
            + Send
            + Sync
            + 'static,
    {
        Self(Box::new(f))
    }

    pub fn call<'a>(
        &'a self,
        params: &'a [Val],
        results: &'a mut [Val],
    ) -> Pin<Box<dyn Future<Output = Result<(), CompositionError>> + Send + 'a>> {
        (self.0)(params, results)
    }
}

/// A composable unit in the runtime graph.
///
/// Implemented by leaf components (single WASM component + pool)
/// and by `Composition` (graph of linked composables).
pub trait Composable: Send + Sync {
    /// Get the type signature (imports/exports) of this composable.
    fn ty(&self) -> &ComposableType;

    /// Resolve an exported function into a pre-resolved handle.
    ///
    /// - `interface`: `None` for top-level exports, `Some("ns:pkg/iface@ver")`
    ///   for functions inside an exported interface.
    /// - `func`: the function name within that scope.
    ///
    /// The returned [`ExportFunc`] captures everything needed for the call,
    /// so repeated invocations avoid redundant lookups.
    fn get_func(&self, interface: Option<&str>, func: &str) -> Result<ExportFunc, CompositionError>;

    /// Link a single resolved import.
    ///
    /// Called during composition building to wire up an import.
    /// The exporter is passed directly so the importer can call
    /// `exporter.link_export()` to get the functions it needs.
    fn link_import(
        &mut self,
        import: &ResolvedImport,
        exporter: &mut dyn Composable,
    ) -> Result<(), CompositionError>;

    /// Link a single export into the given linker.
    ///
    /// Called by the composition builder to wire up an export from this
    /// composable into an importer's linker.
    fn link_export(
        &mut self,
        name: &str,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError>;
}
