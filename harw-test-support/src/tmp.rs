//! Eindeutige Temp-Pfade für Tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Liefert einen eindeutigen Pfad `<temp>/<prefix>-<tag>-<pid>-<n>`; ein
/// eventuell vorhandenes Verzeichnis wird entfernt, der Pfad selbst wird
/// nicht angelegt.
#[must_use]
pub fn unique_tmp(prefix: &str, tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("{prefix}-{tag}-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// Wie [`unique_tmp`], legt das Verzeichnis aber an.
///
/// # Errors
/// Gibt den I/O-Fehler von `create_dir_all` zurück.
pub fn unique_tmp_created(prefix: &str, tag: &str) -> std::io::Result<PathBuf> {
    let path = unique_tmp(prefix, tag);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
