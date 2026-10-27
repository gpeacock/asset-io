//! Assembly of the feature-selected built-in handlers.

use asset_io_core::ContainerHandler;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

/// A core registry with convenience constructors for feature-selected handlers.
///
/// [`new`](Self::new) and [`default`](Self::default) are always empty.
/// [`builtin`](Self::builtin) includes only the explicitly enabled container
/// features. All lookup, parsing and registration operations are supplied by the
/// underlying [`asset_io_core::HandlerRegistry`].
#[derive(Debug, Default)]
pub struct HandlerRegistry {
    inner: Arc<asset_io_core::HandlerRegistry>,
}

impl HandlerRegistry {
    /// An empty registry with no fallback.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty registry that defers to `fallback` for unclaimed formats.
    pub fn with_fallback(fallback: Arc<Self>) -> Self {
        Self {
            inner: Arc::new(asset_io_core::HandlerRegistry::with_fallback(Arc::clone(
                &fallback.inner,
            ))),
        }
    }

    /// An empty registry that defers to the feature-selected built-ins.
    pub fn extending_builtin() -> Self {
        Self::with_fallback(Self::builtin())
    }

    /// The enabled built-in handlers, shared. Empty when no handlers are enabled.
    pub fn builtin() -> Arc<Self> {
        static BUILTIN: OnceLock<Arc<HandlerRegistry>> = OnceLock::new();
        Arc::clone(BUILTIN.get_or_init(|| {
            let mut registry = Self::new();
            crate::containers::register_builtin(&mut registry);
            Arc::new(registry)
        }))
    }

    /// Register `handler`, returning the registry for chaining.
    pub fn with_handler(mut self, handler: impl ContainerHandler + 'static) -> Self {
        self.add_handler(handler);
        self
    }
}

impl Deref for HandlerRegistry {
    type Target = asset_io_core::HandlerRegistry;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for HandlerRegistry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.inner)
    }
}
