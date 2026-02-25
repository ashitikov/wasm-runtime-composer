pub mod composer;
pub mod descriptor;

use std::pin::Pin;
use std::future::Future;

use crate::composable::{Composable, ComposableType, ExportFunc, ResolvedImport};
use crate::composable::linker_instance_ops::LinkerInstanceOps;
use crate::error::CompositionError;

/// Type alias for the function resolution closure.
type FuncResolver = Box<
    dyn Fn(Option<&str>, &str) -> Result<ExportFunc, CompositionError> + Send + Sync,
>;

/// Type alias for the export linking closure.
type ExportLinker = Box<
    dyn Fn(&str, &mut dyn LinkerInstanceOps) -> Result<(), CompositionError> + Send + Sync,
>;

/// A finalized, running composition — the runtime API.
///
/// Obtained via [`Composable::into_composition`] or [`Composer::compose`].
/// Provides [`get_func`](Composition::get_func) for calling exported functions.
///
/// Implements `Composable`, so it can be nested inside another `Composer`
/// (e.g. via [`ComposableDescriptor`](descriptor::ComposableDescriptor)).
pub struct Composition {
    func_resolver: FuncResolver,
    export_linker: ExportLinker,
    ty: ComposableType,
    resolved_imports: Vec<ResolvedImport>,
}

impl Composition {
    pub(crate) fn new(
        func_resolver: FuncResolver,
        export_linker: ExportLinker,
        ty: ComposableType,
        resolved_imports: Vec<ResolvedImport>,
    ) -> Self {
        Self {
            func_resolver,
            export_linker,
            ty,
            resolved_imports,
        }
    }

    /// Resolve an exported function into a pre-resolved handle.
    pub fn get_func(
        &self,
        interface: Option<&str>,
        func: &str,
    ) -> Result<ExportFunc, CompositionError> {
        (self.func_resolver)(interface, func)
    }

    /// Imports resolved internally during composition.
    pub fn resolved_imports(&self) -> &[ResolvedImport] {
        &self.resolved_imports
    }

    /// Link a single export into the given linker (used for nesting).
    fn link_export(
        &self,
        name: &str,
        importer_linker: &mut dyn LinkerInstanceOps,
    ) -> Result<(), CompositionError> {
        (self.export_linker)(name, importer_linker)
    }

    /// Replace the type signature (used by `Filtered::into_composition`).
    pub(crate) fn with_ty(mut self, ty: ComposableType) -> Self {
        self.ty = ty;
        self
    }
}

impl Composable for Composition {
    fn ty(&self) -> &ComposableType {
        &self.ty
    }

    fn link_export(
        &mut self,
        name: &str,
        importer_linker: &mut dyn LinkerInstanceOps,
    ) -> Result<(), CompositionError> {
        (self.export_linker)(name, importer_linker)
    }

    fn into_composition(
        self: Box<Self>,
    ) -> Pin<Box<dyn Future<Output = Result<Composition, CompositionError>> + Send>> {
        Box::pin(async { Ok(*self) })
    }
}
