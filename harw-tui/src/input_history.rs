//! Persistente Eingabe-Historie der TUI.
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt genau eine Aufgabe: die abgesendeten Eingaben über
//! Sitzungsgrenzen hinweg aufzubewahren, damit sie beim nächsten Start wieder
//! mit den Pfeiltasten erreichbar sind. Es kennt weder den [`crate::input_editor::InputEditor`]
//! noch dessen Navigationszustand — es lädt und hängt an, sonst nichts.
//!
//! # Schlüsseltypen
//! - [`InputHistoryStore`] — Dateizugriff auf `<harw-home>/input_history`
//!
//! # Format
//! Eine Zeile je Eintrag, älteste zuerst. Innerhalb eines Eintrags werden
//! Backslash und Zeilenumbruch escaped (`\\` und `\n`), damit ein mehrzeiliger
//! Prompt eine Zeile bleibt und beim Laden unverfälscht zurückkommt.
//!
//! # Nebenläufigkeit
//! [`InputHistoryStore`] hält nur einen Pfad und ist `Send + Sync`. Die
//! Schreiboperation hängt im Anhänge-Modus an und ist damit auch dann
//! verlustfrei, wenn mehrere harw-Instanzen dieselbe Datei benutzen. Eine
//! Sperre gibt es bewusst nicht: die Datei ist Komfort, keine Quelle der
//! Wahrheit.
//!
//! # Fehler
//! Das Modul gibt keine Fehler nach außen. Ein nicht lesbares Home, eine
//! fehlende Datei oder ein fehlgeschlagener Schreibvorgang werden protokolliert
//! und führen zu einer leeren beziehungsweise nicht fortgeschriebenen Historie.
//! Eine kaputte Historie darf die TUI nie am Starten hindern.
//!
//! # Beispiele
//! ```ignore
//! use harw_tui::input_history::InputHistoryStore;
//!
//! let store = InputHistoryStore::open(1000);
//! let entries = store.load();
//! store.append("cargo test");
//! ```

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

/// Dateigestützte Eingabe-Historie.
///
/// # Beschreibung
/// Kapselt Pfad und Obergrenze der persistenten Historie. Ein Store ohne Pfad
/// (weil das harw-Home nicht ermittelbar war) verhält sich wie eine leere,
/// nicht schreibbare Historie — jede Operation bleibt dann folgenlos.
///
/// # Felder
/// - `path` (`Option<PathBuf>`): Ziel-Datei, `None` bei unbekanntem Home.
/// - `cap` (`usize`): wie viele Einträge [`Self::load`] höchstens zurückgibt.
#[derive(Debug, Clone)]
pub(crate) struct InputHistoryStore {
    path: Option<PathBuf>,
    cap: usize,
}

impl InputHistoryStore {
    /// Open history in the home selected by the runtime (including `--home`).
    pub(crate) fn at_home(home: &std::path::Path, cap: usize) -> Self {
        Self {
            path: Some(harw_home::paths::input_history_path(home)),
            cap,
        }
    }

    /// Öffnet den Store für das aktuelle harw-Home.
    ///
    /// # Beschreibung
    /// Ermittelt das Home über [`harw_home::paths::home_dir`]. Schlägt das fehl,
    /// wird der Fehler protokolliert und ein pfadloser Store zurückgegeben; die
    /// TUI läuft dann ohne persistente Historie weiter.
    ///
    /// # Argumente
    /// - `cap` (`usize`): Obergrenze der beim Laden zurückgegebenen Einträge.
    ///
    /// # Rückgabe
    /// Einen [`InputHistoryStore`], gegebenenfalls ohne Pfad.
    ///
    /// # Nebenläufigkeit
    /// Liest nur Umgebungsvariablen und ist von jedem Thread aus aufrufbar.
    pub(crate) fn open(cap: usize) -> Self {
        match harw_home::paths::home_dir() {
            Ok(home) => Self {
                path: Some(harw_home::paths::input_history_path(&home)),
                cap,
            },
            Err(error) => {
                tracing::warn!(%error, "tui.input_history.home_unavailable");
                Self { path: None, cap }
            }
        }
    }

    /// Lädt die jüngsten Einträge, älteste zuerst.
    ///
    /// # Beschreibung
    /// Liest die Datei vollständig, entfernt leere Zeilen, macht das Escaping
    /// rückgängig und schneidet auf die letzten `cap` Einträge zu. Eine
    /// fehlende Datei ist der Normalfall beim ersten Start und liefert eine
    /// leere Liste.
    ///
    /// # Rückgabe
    /// Die Einträge in chronologischer Reihenfolge.
    pub(crate) fn load(&self) -> Vec<String> {
        let Some(path) = self.path.as_ref() else {
            return Vec::new();
        };
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "tui.input_history.read_failed");
                return Vec::new();
            }
        };
        let mut entries: Vec<String> = raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode)
            .collect();
        if entries.len() > self.cap {
            entries.drain(..entries.len() - self.cap);
        }
        entries
    }

    /// Hängt einen Eintrag an die Datei an.
    ///
    /// # Beschreibung
    /// Schreibt den escapten Eintrag als eine Zeile im Anhänge-Modus. Leere
    /// oder rein aus Leerraum bestehende Eingaben werden übersprungen. Ein
    /// Schreibfehler wird protokolliert und sonst ignoriert: die laufende
    /// Sitzung behält ihre Historie im Speicher.
    ///
    /// # Argumente
    /// - `entry` (`&str`): die abgesendete Eingabe im Originaltext.
    ///
    /// # Nebenläufigkeit
    /// Der Anhänge-Modus macht parallele Schreiber unkritisch, solange eine
    /// Zeile in einem Rutsch geschrieben wird — genau das tut diese Funktion.
    pub(crate) fn append(&self, entry: &str) {
        if entry.trim().is_empty() {
            return;
        }
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                tracing::warn!(%error, path = %parent.display(), "tui.input_history.mkdir_failed");
                return;
            }
        }
        let opened = OpenOptions::new().create(true).append(true).open(path);
        match opened {
            Ok(mut file) => {
                let record = format!("{}\n", encode(entry));
                if let Err(error) = file.write_all(record.as_bytes()) {
                    tracing::warn!(%error, path = %path.display(), "tui.input_history.write_failed");
                }
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "tui.input_history.open_failed");
            }
        }
    }
}

// Ersetzt Backslash und Zeilenumbruch durch ihre Escape-Sequenzen, damit ein
// Eintrag garantiert eine Zeile belegt.
fn encode(entry: &str) -> String {
    let mut out = String::with_capacity(entry.len());
    for ch in entry.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

// Kehrt [`encode`] um. Eine unbekannte Escape-Sequenz bleibt unverändert
// stehen, statt Zeichen zu verschlucken.
fn decode(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn history_survives_reopening_with_multiline_and_literal_escapes() -> TestResult {
        let directory = tempfile::tempdir().map_err(ctx("history directory"))?;
        let path = directory.path().join("input_history");
        let store = InputHistoryStore {
            path: Some(path.clone()),
            cap: 2,
        };
        store.append("old");
        store.append("first\nsecond \\n");
        store.append("carriage\rreturn");
        store.append("   ");
        let reopened = InputHistoryStore {
            path: Some(path),
            cap: 2,
        };
        assert_eq!(reopened.load(), ["first\nsecond \\n", "carriage\rreturn"]);
        Ok(())
    }
}
