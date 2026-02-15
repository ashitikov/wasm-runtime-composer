use std::collections::HashMap;

use crate::composable::Composable;

/// Descriptor wrapping a Composable with ID and import hints.
pub struct ComposableDescriptor {
    pub(crate) id: String,
    pub(crate) inner: Box<dyn Composable>,
    pub(crate) import_hints: HashMap<String, String>,
}

impl ComposableDescriptor {
    /// Create a new descriptor with the given ID.
    pub fn new(id: impl Into<String>, inner: impl Composable + 'static) -> Self {
        Self {
            id: id.into(),
            inner: Box::new(inner),
            import_hints: HashMap::new(),
        }
    }

    /// Add an import hint: this import should be satisfied by target_id.
    pub fn with_import_hint(mut self, import: impl Into<String>, target_id: impl Into<String>) -> Self {
        self.import_hints.insert(import.into(), target_id.into());
        self
    }

    /// Set all import hints at once.
    pub fn with_import_hints(mut self, import_hints: HashMap<String, String>) -> Self {
        self.import_hints = import_hints;
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn import_hints(&self) -> &HashMap<String, String> {
        &self.import_hints
    }
}
