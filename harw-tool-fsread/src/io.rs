//! Begrenztes Datei-Lesen für die Inhaltswerkzeuge (`head`, `tail`, `cat`,
//! `wc`, `hash`, `diff`, `json`, `file`).
//!
//! # Verantwortung
//! - [`open_text`]: öffnet eine reguläre Datei über [`Scope::open_read`]
//!   (Symlink-frei, Geheimnis-Pfade verweigert) und liefert sie samt `RelPath`.
//! - [`read_prefix`]: liest höchstens `max` Bytes und meldet, ob mehr folgte.
//! - [`looks_binary`]: NUL-Heuristik über die ersten 8 KiB.
//!
//! # Grenzen
//! Kein Lesen ohne Obergrenze: jede Funktion bekommt ein Byte-Limit. Dateien,
//! die während des Lesens wachsen oder schrumpfen, ergeben ein
//! (ggf. gekürztes) Ergebnis, nie eine Endlosschleife.

use crate::scope::{RelPath, Scope};
use std::fs::File;
use std::io::{self, Read};

/// Obergrenze beim Einlesen einer Datei in den Speicher (8 MiB).
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// Obergrenze für streamende Zähler/Hashes (64 MiB).
pub const MAX_STREAM_BYTES: u64 = 64 * 1024 * 1024;

/// Wie viele Anfangsbytes die Binär-Heuristik prüft.
pub const SNIFF_BYTES: usize = 8192;

/// Öffnet `input` als lesbare reguläre Datei.
///
/// # Errors
/// Lesbare Fehlermeldung (Ausbruch, Geheimnis, Symlink, Verzeichnis, fehlt).
pub fn open_text(scope: &Scope, input: &str) -> Result<(RelPath, File), String> {
    let rel = scope.rel_readable(input).map_err(|e| e.to_string())?;
    let file = scope
        .open_read(&rel)
        .map_err(|e| format!("{}: {e}", rel.display()))?;
    Ok((rel, file))
}

/// Liest höchstens `max` Bytes; `true`, wenn die Datei darüber hinaus weiterging.
///
/// # Errors
/// I/O-Fehler.
pub fn read_prefix(file: &mut File, max: u64) -> io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    // Ein Byte mehr lesen, um „mehr vorhanden“ zu erkennen.
    file.by_ref()
        .take(max.saturating_add(1))
        .read_to_end(&mut buf)?;
    let more = u64::try_from(buf.len()).unwrap_or(u64::MAX) > max;
    if more {
        buf.truncate(usize::try_from(max).unwrap_or(usize::MAX));
    }
    Ok((buf, more))
}

/// `true`, wenn die ersten [`SNIFF_BYTES`] ein NUL-Byte enthalten.
#[must_use]
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF_BYTES).any(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult};

    #[test]
    fn read_prefix_reports_more() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f", b"0123456789")?;
        let scope = fx.scope()?;
        let (_, mut file) =
            open_text(&scope, "f").map_err(crate::test_support::TestError::Unexpected)?;
        let (bytes, more) = read_prefix(&mut file, 4)?;
        assert_eq!(bytes, b"0123");
        assert!(more);
        let (_, mut file) =
            open_text(&scope, "f").map_err(crate::test_support::TestError::Unexpected)?;
        let (bytes, more) = read_prefix(&mut file, 10)?;
        assert_eq!(bytes.len(), 10);
        assert!(!more);
        Ok(())
    }

    #[test]
    fn binary_heuristic() -> TestResult {
        assert!(looks_binary(b"ab\0cd"));
        assert!(!looks_binary("grüße".as_bytes()));
        Ok(())
    }

    #[test]
    fn open_text_rejects_escape_secret_and_directory() -> TestResult {
        let fx = Fixture::new()?;
        fx.write(".env", b"A=1")?;
        fx.write("d/x", b"")?;
        let scope = fx.scope()?;
        assert!(open_text(&scope, "../outside/secret.txt").is_err());
        assert!(open_text(&scope, ".env").is_err());
        assert!(open_text(&scope, "d").is_err());
        assert!(open_text(&scope, "missing").is_err());
        Ok(())
    }
}
