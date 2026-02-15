use std::collections::{HashMap, HashSet};

use crate::composable::{Composable, ComposableType, InterfaceSet, ResolvedImport};
use crate::composition::Composition;
use crate::descriptor::ComposableDescriptor;
use crate::error::CompositionError;

/// Builder for creating a Composition.
pub struct CompositionBuilder {
    descriptors: Vec<ComposableDescriptor>,
    export_hints: HashMap<String, String>,
}

impl CompositionBuilder {
    pub fn new() -> Self {
        Self {
            descriptors: Vec::new(),
            export_hints: HashMap::new(),
        }
    }

    /// Add a composable descriptor.
    pub fn add(&mut self, descriptor: ComposableDescriptor) -> &mut Self {
        self.descriptors.push(descriptor);
        self
    }

    /// Add an export hint: when multiple children export the same interface,
    /// this hint selects which child provides it for the composition's public exports.
    pub fn with_export_hint(&mut self, export_name: impl Into<String>, provider_id: impl Into<String>) -> &mut Self {
        self.export_hints.insert(export_name.into(), provider_id.into());
        self
    }

    /// Build the composition, resolving all internal links.
    pub fn build(self) -> Result<Composition, CompositionError> {
        if self.descriptors.is_empty() {
            return Err(CompositionError::Empty);
        }

        // 1. Check for duplicate IDs
        let mut seen_ids: HashSet<&str> = HashSet::new();
        for descriptor in &self.descriptors {
            if !seen_ids.insert(descriptor.id()) {
                return Err(CompositionError::DuplicateId(descriptor.id().to_string()));
            }
        }

        // 2. Validate link references
        for descriptor in &self.descriptors {
            for (interface, target_id) in descriptor.import_hints() {
                if !seen_ids.contains(target_id.as_str()) {
                    return Err(CompositionError::InvalidLinkReference {
                        interface: interface.clone(),
                        target_id: target_id.clone(),
                    });
                }
            }
        }

        // 3. Validate export hint references
        for (_, provider_id) in &self.export_hints {
            if !seen_ids.contains(provider_id.as_str()) {
                return Err(CompositionError::InvalidLinkReference {
                    interface: provider_id.clone(),
                    target_id: provider_id.clone(),
                });
            }
        }

        // 4. Build exports map and resolve export providers
        let mut export_providers: HashMap<String, Vec<String>> = HashMap::new();

        for descriptor in &self.descriptors {
            for name in descriptor.inner.ty().exports() {
                export_providers
                    .entry(name.clone())
                    .or_default()
                    .push(descriptor.id().to_string());
            }
        }

        // Resolve which child provides each export (single → auto, multiple → need hint)
        let mut resolved_exports = InterfaceSet::new();
        // Maps export name → provider child ID
        let mut export_provider_map: HashMap<String, String> = HashMap::new();

        for (name, candidates) in &export_providers {
            if candidates.len() == 1 {
                export_provider_map.insert(name.clone(), candidates[0].clone());
            } else {
                let provider_id = self.export_hints.get(name).ok_or_else(|| {
                    CompositionError::AmbiguousExport {
                        interface: name.clone(),
                        candidates: candidates.clone(),
                    }
                })?;

                if !candidates.contains(provider_id) {
                    return Err(CompositionError::ExportHintInvalid {
                        interface: name.clone(),
                        target_id: provider_id.clone(),
                    });
                }

                export_provider_map.insert(name.clone(), provider_id.clone());
            }
            resolved_exports.insert(name.clone());
        }

        // 5. Resolve imports for each descriptor
        let mut resolved_imports: Vec<ResolvedImport> = Vec::new();
        let mut external_imports = InterfaceSet::new();

        for descriptor in &self.descriptors {
            for import_name in descriptor.inner.ty().imports() {
                let candidates = export_providers.get(import_name);

                match candidates {
                    // No provider → external import
                    None => {
                        external_imports.insert(import_name.clone());
                    }
                    // Single provider → resolved internally
                    Some(candidates) if candidates.len() == 1 => {
                        resolved_imports.push(ResolvedImport::new(
                            descriptor.id().to_string(),
                            candidates[0].clone(),
                            import_name.clone(),
                        ));
                    }
                    // Multiple candidates → need hint
                    Some(candidates) => {
                        let exporter_id = descriptor.import_hints().get(import_name).ok_or_else(|| {
                            CompositionError::AmbiguousImport {
                                interface: import_name.clone(),
                                candidates: candidates.clone(),
                            }
                        })?;

                        if !candidates.contains(exporter_id) {
                            return Err(CompositionError::ImportHintInvalid {
                                interface: import_name.clone(),
                                target_id: exporter_id.clone(),
                            });
                        }

                        resolved_imports.push(ResolvedImport::new(
                            descriptor.id().to_string(),
                            exporter_id.clone(),
                            import_name.clone(),
                        ));
                    }
                }
            }
        }

        // 6. Build children map by ID
        let mut children: HashMap<String, Box<dyn Composable>> = self
            .descriptors
            .into_iter()
            .map(|d| (d.id, d.inner))
            .collect();

        let ty = ComposableType::new(resolved_exports, external_imports);

        // 7. Link resolved imports: exporter provides functions to importer
        for resolved in &resolved_imports {
            // Take importer out to avoid double borrow on children map
            let mut importer = children.remove(&resolved.importer_id).ok_or_else(|| {
                CompositionError::LinkingError(format!(
                    "Importer '{}' not found",
                    resolved.importer_id
                ))
            })?;

            let exporter = children.get_mut(&resolved.exporter_id).ok_or_else(|| {
                CompositionError::LinkingError(format!(
                    "Exporter '{}' not found",
                    resolved.exporter_id
                ))
            })?;

            importer.link_import(resolved, &mut **exporter)?;

            children.insert(resolved.importer_id.clone(), importer);
        }

        // 8. Create Composition with provider map for routing
        Ok(Composition::new(children, resolved_imports, ty, export_provider_map))
    }
}

impl Default for CompositionBuilder {
    fn default() -> Self {
        Self::new()
    }
}
