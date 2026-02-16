use crate::error::CompositionError;
use super::{Composable, ComposableType, ExportFunc, ResolvedImport};
use super::linker_ops::LinkerOps;

/// Decorator that wraps any `Composable` with pre-filtered exports.
pub struct Filtered<C: Composable> {
    inner: C,
    ty: ComposableType,
}

impl<C: Composable> Composable for Filtered<C> {
    fn ty(&self) -> &ComposableType {
        &self.ty
    }

    fn get_func(&self, interface: Option<&str>, func: &str) -> Result<ExportFunc, CompositionError> {
        self.inner.get_func(interface, func)
    }

    fn link_import(
        &mut self,
        import: &ResolvedImport,
        exporter: &mut dyn Composable,
    ) -> Result<(), CompositionError> {
        self.inner.link_import(import, exporter)
    }

    fn link_export(
        &mut self,
        name: &str,
        linker: &mut dyn LinkerOps,
    ) -> Result<(), CompositionError> {
        self.inner.link_export(name, linker)
    }
}

/// Extension methods for filtering exports of any `Composable`.
pub trait ExportFilter: Composable + Sized {
    /// Filter exports by predicate on interface name.
    fn filter_exports(self, f: impl Fn(&str) -> bool) -> Filtered<Self> {
        let ty = self.ty();

        let exports = ty
            .exports()
            .iter()
            .filter(|name| f(name.as_str()))
            .cloned()
            .collect();

        let imports = ty.imports().clone();

        Filtered {
            inner: self,
            ty: ComposableType::new(exports, imports),
        }
    }

    /// Keep only exports whose names are in `allow`.
    fn exposing(self, allow: &[&str]) -> Filtered<Self> {
        self.filter_exports(|name| allow.contains(&name))
    }

    /// Remove exports whose names are in `deny`; keep everything else.
    fn hiding(self, deny: &[&str]) -> Filtered<Self> {
        self.filter_exports(|name| !deny.contains(&name))
    }
}

impl<T: Composable + Sized> ExportFilter for T {}
