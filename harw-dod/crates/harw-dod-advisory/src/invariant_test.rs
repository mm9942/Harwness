//! Testbeleg für die Offline- und Uhr-Invariante dieser Crate.
//!
//! # Zweck
//! Der Sensor läuft im Sentinel, der kein Netz hat und nie selbst die Uhr
//! liest (Zeit wird injiziert). Dieser Test durchsucht den eigenen
//! Quelltext nach Netz-, Prozess- und Uhrzugriffen und schlägt bei einem
//! Treffer fehl — auch für einen später hinzugefügten Aufruf.
//!
//! # Warum der Test sich nicht selbst meldet
//! Die gesuchten Muster werden zur Laufzeit aus Fragmenten zusammengesetzt
//! ([`banned_patterns`]), und Kommentarzeilen werden übersprungen (die
//! Dokumentation darf die Regeln beim Namen nennen) — dasselbe Vorgehen wie
//! `harw-dod-scanreport/src/invariant_test.rs`.
//!
//! # Nebenläufigkeit
//! Liest nur Dateien; keine geteilten Daten.
//!
//! # Fehler
//! Ein Treffer oder eine leere Dateiliste ergibt `Err(TestError)`.
//!
//! # Examples
//! ```rust,ignore
//! // Läuft mit `cargo test -p harw-dod-advisory`.
//! ```

use std::path::{Path, PathBuf};

use crate::test_support::{TestError, TestResult, ctx};

// Sammelt alle `.rs`-Dateien unter `dir` (nur Testinfrastruktur).
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> TestResult {
    let entries = std::fs::read_dir(dir).map_err(ctx("Quellverzeichnis lesen"))?;
    for entry in entries {
        let path = entry.map_err(ctx("Verzeichniseintrag lesen"))?.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

// Die verbotenen Muster, aus Fragmenten gebaut (siehe Moduldoku).
fn banned_patterns() -> Vec<String> {
    vec![
        ["std::", "net"].concat(),
        ["Tcp", "Stream"].concat(),
        ["Udp", "Socket"].concat(),
        ["Com", "mand"].concat(),
        ["sp", "awn("].concat(),
        ["Timestamp::", "now"].concat(),
        ["System", "Time"].concat(),
    ]
}

#[test]
fn test_source_has_no_network_process_or_clock_access() -> TestResult {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files)?;
    if files.is_empty() {
        return Err(TestError::Unexpected(
            "Quelltext-Scan ohne Dateien (Pfad falsch aufgelöst?)".to_owned(),
        ));
    }

    let patterns = banned_patterns();
    for file in &files {
        let content = std::fs::read_to_string(file).map_err(ctx("Quelldatei lesen"))?;
        for line in content.lines() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(pattern) = patterns
                .iter()
                .find(|pattern| line.contains(pattern.as_str()))
            {
                return Err(TestError::Unexpected(format!(
                    "verbotenes Muster {pattern:?} in {}: {line}",
                    file.display()
                )));
            }
        }
    }
    Ok(())
}
