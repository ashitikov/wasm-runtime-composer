pub mod builder;
pub mod descriptor;

use std::collections::HashMap;

use crate::composable::{Composable, ComposableType, ExportFunc, ResolvedImport};
use crate::composable::linker_ops::LinkerOps;
use crate::error::CompositionError;

/// An immutable graph of linked composables.
pub struct Composition {
    children: HashMap<String, Box<dyn Composable>>,
    resolved_imports: Vec<ResolvedImport>,
    ty: ComposableType,
    /// Maps export name → provider child ID for deterministic routing.
    export_provider_map: HashMap<String, String>,
}

impl Composition {
    pub(crate) fn new(
        children: HashMap<String, Box<dyn Composable>>,
        resolved_imports: Vec<ResolvedImport>,
        ty: ComposableType,
        export_provider_map: HashMap<String, String>,
    ) -> Self {
        Self {
            children,
            resolved_imports,
            ty,
            export_provider_map,
        }
    }

    /// Imports resolved internally during composition.
    pub fn resolved_imports(&self) -> &[ResolvedImport] {
        &self.resolved_imports
    }

    /// Get a child composable by ID.
    pub fn get(&self, id: &str) -> Option<&dyn Composable> {
        self.children.get(id).map(|c| c.as_ref())
    }

    fn resolve_provider(&self, name: &str) -> Result<&dyn Composable, CompositionError> {
        let provider_id = self.export_provider_map.get(name)
            .ok_or(CompositionError::FuncNotFound)?;
        self.children.get(provider_id)
            .map(|c| c.as_ref())
            .ok_or(CompositionError::FuncNotFound)
    }
}

impl Composable for Composition {
    fn ty(&self) -> &ComposableType {
        &self.ty
    }

    fn get_func(&self, interface: Option<&str>, func: &str) -> Result<ExportFunc, CompositionError> {
        let lookup = interface.unwrap_or(func);
        let provider = self.resolve_provider(lookup)?;
        provider.get_func(interface, func)
    }

    fn link_import(
        &mut self,
        import: &ResolvedImport,
        exporter: &mut dyn Composable,
    ) -> Result<(), CompositionError> {
        // Delegate to the child that needs this import
        for child in self.children.values_mut() {
            if child.ty().imports().contains(&import.interface) {
                return child.link_import(import, exporter);
            }
        }
        Ok(())
    }

    fn link_export(
        &mut self,
        name: &str,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        // Use provider map for deterministic routing
        let provider_id = self.export_provider_map.get(name)
            .ok_or(CompositionError::FuncNotFound)?
            .clone();
        if let Some(child) = self.children.get_mut(&provider_id) {
            child.link_export(name, linker)?;
        }
        Ok(())
    }
}
