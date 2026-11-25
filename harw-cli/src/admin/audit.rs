//! Append-only Audit-Log `<home>/admin/audit.log` (JSON-Zeilen, Modus 0600).
//! Enthält nie Passwörter, Codes oder Geheimnisse.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::account::{ensure_admin_dir, now_unix};

pub(crate) fn path(home: &Path) -> PathBuf {
    super::account::admin_dir(home).join("audit.log")
}

/// Hängt ein Ereignis an. Fehler werden nur über `tracing` gemeldet; ein
/// Audit-Fehler darf eine bereits ausgeführte Aktion nicht verschleiern.
pub(crate) fn record(home: &Path, event: &str, ip: &str, detail: serde_json::Value) {
    let line = json!({ "ts": now_unix(), "event": event, "ip": ip, "detail": detail }).to_string();
    let result = (|| -> Result<(), String> {
        ensure_admin_dir(home)?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(path(home)).map_err(|e| e.to_string())?;
        writeln!(file, "{line}").map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "admin audit log not writable");
    }
}

/// Die letzten `n` Zeilen (neueste zuerst) als `(ts, event, ip, detail-Text)`.
pub(crate) fn tail(home: &Path, n: usize) -> Vec<(u64, String, String, String)> {
    let Ok(text) = std::fs::read_to_string(path(home)) else {
        return Vec::new();
    };
    text.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .take(n)
        .map(|v| {
            (
                v["ts"].as_u64().unwrap_or(0),
                v["event"].as_str().unwrap_or("?").to_owned(),
                v["ip"].as_str().unwrap_or("?").to_owned(),
                v["detail"].to_string(),
            )
        })
        .collect()
}
