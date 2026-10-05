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

/// Installs the host job runtime (file-backed job records below `state_dir`,
/// Linux executor in new-session mode) unless a port is already installed.
/// Returns whether a port is installed afterwards. Without Linux support, or
/// when the runtime cannot be created, nothing is installed and surfaces fail
/// closed.
pub fn install_host_default(state_dir: &std::path::Path) -> bool {
    if PORT.get().is_some() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        match crate::JobCommandPort::host(state_dir) {
            Ok(port) => {
                let _ = install(Arc::new(port));
            }
            Err(error) => tracing::warn!(%error, "no job runtime for commands"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = state_dir;
    PORT.get().is_some()
}

/// The installed port, if the composition root installed one.
#[must_use]
pub fn installed() -> Option<Arc<dyn CommandPort>> {
    PORT.get().cloned()
}
