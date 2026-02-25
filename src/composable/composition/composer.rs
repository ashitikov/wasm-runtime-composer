use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::composable::{Composable, ComposableType, InterfaceSet, ResolvedImport};
use crate::error::CompositionError;
use super::Composition;
use super::descriptor::ComposableDescriptor;

/// Builder for creating a Composition.
pub struct Composer {
    descriptors: Vec<ComposableDescriptor>,
    export_hints: HashMap<String, String>,
}

impl Composer {
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

    /// Compose all children: resolve imports, link, instantiate, and return the final Composition.
    pub async fn compose(self) -> Result<Composition, CompositionError> {
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

        // 7. Group resolved imports by importer
        let mut imports_by_importer: HashMap<String, Vec<ResolvedImport>> = HashMap::new();
        for resolved in &resolved_imports {
            imports_by_importer.entry(resolved.importer_id.clone())
                .or_default().push(resolved.clone());
        }

        // 8. Link imports (sync — registers closures only)
        for (id, imports) in &imports_by_importer {
            let mut importer = children.remove(id).ok_or_else(|| {
                CompositionError::LinkingError(format!(
                    "Importer '{}' not found",
                    id
                ))
            })?;

            for import in imports {
                let mut exporter = children.remove(&import.exporter_id)
                    .ok_or_else(|| CompositionError::LinkingError(
                        format!("Exporter '{}' not found", import.exporter_id)
                    ))?;
                importer.link_import(&import.interface, &mut *exporter)?;
                children.insert(import.exporter_id.clone(), exporter);
            }

            children.insert(id.clone(), importer);
        }

        // 9. Convert each child into Composition
        let mut composed: HashMap<String, Composition> = HashMap::new();
        for (id, child) in children {
            let comp = child.into_composition().await?;
            composed.insert(id, comp);
        }

        // 10. Assemble outer Composition with closures
        let children = Arc::new(composed);
        let epm = Arc::new(export_provider_map);

        let children_for_func = children.clone();
        let epm_for_func = epm.clone();

        Ok(Composition::new(
            Box::new(move |iface, func| {
                let lookup = iface.unwrap_or(func);
                let id = epm_for_func.get(lookup)
                    .ok_or(CompositionError::FuncNotFound)?;
                children_for_func.get(id)
                    .ok_or(CompositionError::FuncNotFound)?
                    .get_func(iface, func)
            }),
            Box::new(move |name, importer_linker| {
                let id = epm.get(name)
                    .ok_or(CompositionError::FuncNotFound)?;
                children.get(id)
                    .ok_or(CompositionError::FuncNotFound)?
                    .link_export(name, importer_linker)
            }),
            ty,
            resolved_imports,
        ))
    }
}

impl Default for Composer {
    fn default() -> Self {
        Self::new()
    }
}
