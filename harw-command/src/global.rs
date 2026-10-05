//! The process-wide command port, installed once by the composition root.
//!
//! Surfaces deep in the tool and TUI layers (`shell.exec`, `!`) do not own a
//! job runtime; they ask for the installed port. Nothing installed means
//! nothing runs: the surface reports "no job runtime" instead of falling back
//! to spawning a process itself.

use std::sync::{Arc, OnceLock};

use crate::CommandPort;

static PORT: OnceLock<Arc<dyn CommandPort>> = OnceLock::new();

/// Installs the process-wide port. Only the first call wins.
///
/// # Errors
/// The rejected port, when one is already installed.
pub fn install(port: Arc<dyn CommandPort>) -> Result<(), Arc<dyn CommandPort>> {
    PORT.set(port)
}

/// The installed port, if the composition root installed one.
#[must_use]
pub fn installed() -> Option<Arc<dyn CommandPort>> {
    PORT.get().cloned()
}
