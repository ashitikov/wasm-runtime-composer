use std::pin::Pin;
use std::future::Future;

use crate::error::CompositionError;
use super::{Composable, ComposableType};
use super::composition::Composition;
use super::linker_instance_ops::LinkerInstanceOps;

/// Decorator that wraps any `Composable` with pre-filtered exports.
pub struct Filtered<C: Composable> {
    inner: C,
    ty: ComposableType,
}

impl<C: Composable + 'static> Composable for Filtered<C> {
    fn ty(&self) -> &ComposableType {
        &self.ty
    }

    fn link_export(
        &mut self,
        name: &str,
        importer_linker: &mut dyn LinkerInstanceOps,
    ) -> Result<(), CompositionError> {
        self.inner.link_export(name, importer_linker)
    }

    fn link_import(
        &mut self,
        name: &str,
        exporter: &mut dyn Composable,
    ) -> Result<(), CompositionError> {
        self.inner.link_import(name, exporter)
    }

    fn into_composition(
        self: Box<Self>,
    ) -> Pin<Box<dyn Future<Output = Result<Composition, CompositionError>> + Send>> {
        let filtered = *self;
        Box::pin(async move {
            let comp = Box::new(filtered.inner).into_composition().await?;
            Ok(comp.with_ty(filtered.ty))
        })
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
