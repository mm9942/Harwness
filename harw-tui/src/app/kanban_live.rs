//! Kanban live (Runde 5, Teil C4): eine offene Board-Ansicht lädt neu, wenn
//! sich die Kartendateien auf der Platte ändern.
//!
//! # Beschreibung
//! Schreibt ein anderer Prozess (`harw serve`, ein Job-Worker) in das Board,
//! erreicht die TUI kein `AgentEventKind::Knowledge`-Ereignis. Deshalb bildet
//! [`KanbanLiveWatch`] bei offener Kanban-Ansicht alle
//! [`KANBAN_LIVE_INTERVAL`] eine mtime-Signatur der Board-Dateien
//! (`<knowledge>/kanban/boards/<board>/…`; ohne gewähltes Board über alle
//! Boards) und reiht bei einer Änderung den normalen Nachlade-Befehl der
//! Ansicht ein ([`ChatApp::queue_overlay_refresh`]). Kein Dateisystem-
//! Beobachter-Crate: die Abfrage läuft im Spinner-Takt (Busy-Pfad, über
//! [`ChatApp::drain_agent_events`]) bzw. in einem eigenen Leerlauf-Takt von
//! `run_loop`, der nur bei offenem Board aktiv ist.
//!
//! Der erste Blick auf ein Board (oder ein Board-Wechsel) legt nur die
//! Grundlinie fest; nachgeladen wird erst bei einer echten Änderung.
//!
//! # Fehler
//! Keine: ein fehlendes oder unlesbares Verzeichnis ergibt eine leere
//! Signatur (wie ein leeres Board).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::{ChatApp, Overlay};

/// Abstand zweier Signatur-Abfragen.
pub(crate) const KANBAN_LIVE_INTERVAL: Duration = Duration::from_secs(2);

/// Maximale Verzeichnistiefe unter dem beobachteten Board-Verzeichnis.
const MAX_DEPTH: usize = 4;

/// Obergrenze der betrachteten Dateien je Abfrage (Schutz vor riesigen
/// Bäumen; darüber hinaus zählt nur die Anzahl).
const MAX_FILES: usize = 4096;

/// Zustand der Board-Beobachtung einer [`ChatApp`].
#[derive(Debug, Default)]
pub(crate) struct KanbanLiveWatch {
    /// Wissenswurzel für Tests; sonst aus der Runtime-Montage.
    root_override: Option<PathBuf>,
    /// Zeitpunkt der letzten Abfrage.
    last_poll: Option<Instant>,
    /// Beobachtetes Verzeichnis und seine letzte Signatur.
    baseline: Option<(PathBuf, u64)>,
}

impl KanbanLiveWatch {
    /// Beobachtung mit fester Wissenswurzel (Tests).
    #[cfg(test)]
    pub(crate) fn with_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root_override: Some(root.into()),
            ..Self::default()
        }
    }

    /// Vergisst Grundlinie und Takt (Ansicht geschlossen).
    fn reset(&mut self) {
        self.last_poll = None;
        self.baseline = None;
    }

    /// Eine Abfrage zum Zeitpunkt `now` für das Verzeichnis `dir`.
    ///
    /// # Rückgabe
    /// `true`, wenn sich die Signatur seit der letzten Abfrage desselben
    /// Verzeichnisses geändert hat; `false` vor Ablauf des Intervalls, beim
    /// ersten Blick und nach einem Verzeichniswechsel.
    fn poll_at(&mut self, dir: &Path, now: Instant) -> bool {
        if self
            .last_poll
            .is_some_and(|last| now.saturating_duration_since(last) < KANBAN_LIVE_INTERVAL)
        {
            return false;
        }
        self.last_poll = Some(now);
        let signature = board_signature(dir);
        let changed = matches!(
            &self.baseline,
            Some((watched, previous)) if watched == dir && *previous != signature
        );
        self.baseline = Some((dir.to_path_buf(), signature));
        changed
    }
}

/// Das Board aus einem Nachlade-Befehl `/kanban --board=<id> show`; nur
/// einfache Kennungen (Buchstaben, Ziffern, `-`, `_`, `.`, nicht `..`), sonst
/// `None` (dann werden alle Boards beobachtet).
pub(crate) fn board_from_refresh(command: &str) -> Option<&str> {
    command
        .split_whitespace()
        .find_map(|token| token.strip_prefix("--board="))
        .filter(|id| {
            !id.is_empty()
                && *id != "."
                && *id != ".."
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

/// Das zu beobachtende Verzeichnis unter der Wissenswurzel.
pub(crate) fn watched_dir(knowledge_root: &Path, board: Option<&str>) -> PathBuf {
    let boards = knowledge_root.join("kanban").join("boards");
    match board {
        Some(board) => boards.join(board),
        None => boards,
    }
}

/// mtime-Signatur aller Dateien unter `dir` (sortiert nach relativem Pfad;
/// Pfad, Größe und Änderungszeit gehen ein). Punktdateien (Sperr- und
/// Temporärdateien des atomaren Schreibens) zählen nicht.
pub(crate) fn board_signature(dir: &Path) -> u64 {
    let mut files: Vec<(PathBuf, u64, u128)> = Vec::new();
    collect_files(dir, dir, 0, &mut files);
    files.sort();
    let mut hasher = DefaultHasher::new();
    files.len().hash(&mut hasher);
    for entry in files.iter().take(MAX_FILES) {
        entry.hash(&mut hasher);
    }
    hasher.finish()
}

fn collect_files(base: &Path, dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64, u128)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_files(base, &path, depth + 1, out);
        } else if file_type.is_file() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            let relative = path
                .strip_prefix(base)
                .map_or(path.clone(), Path::to_path_buf);
            out.push((relative, metadata.len(), modified));
        }
    }
}

impl ChatApp {
    /// Der Nachlade-Befehl der offenen Kanban-Ansicht, sonst `None`.
    fn open_kanban_refresh(&self) -> Option<String> {
        match &self.overlay {
            Some(Overlay::View(view)) => view.refresh_command().filter(|command| {
                command
                    .split_whitespace()
                    .next()
                    .is_some_and(|head| head == "/kanban")
            }),
            _ => None,
        }
    }

    /// Die Wissenswurzel des Profils (Runtime-Montage), falls bekannt.
    fn kanban_knowledge_root(&self) -> Option<PathBuf> {
        self.kanban_live.root_override.clone().or_else(|| {
            self.runtime()
                .and_then(|runtime| runtime.services().knowledge_store())
                .map(|store| store.root().to_path_buf())
        })
    }

    /// `true`, solange ein Board offen ist und eine Wissenswurzel bekannt ist
    /// — dann läuft in `run_loop` der Leerlauf-Takt.
    pub(crate) fn kanban_live_active(&self) -> bool {
        self.open_kanban_refresh().is_some() && self.kanban_knowledge_root().is_some()
    }

    /// Runde 5, Teil C4: prüft (höchstens alle [`KANBAN_LIVE_INTERVAL`]) die
    /// Board-Dateien und reiht bei einer Änderung das Nachladen ein.
    ///
    /// # Rückgabe
    /// `true`, wenn ein Nachladen eingereiht wurde.
    pub(crate) fn poll_kanban_live(&mut self) -> bool {
        self.poll_kanban_live_at(Instant::now())
    }

    fn poll_kanban_live_at(&mut self, now: Instant) -> bool {
        let (Some(refresh), Some(root)) =
            (self.open_kanban_refresh(), self.kanban_knowledge_root())
        else {
            self.kanban_live.reset();
            return false;
        };
        let dir = watched_dir(&root, board_from_refresh(&refresh));
        if !self.kanban_live.poll_at(&dir, now) {
            return false;
        }
        self.queue_overlay_refresh();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_chat_app;
    use crate::kanban_board::KanbanBoard;
    use crate::test_support::{TestResult, ctx};

    fn write_card(dir: &Path, name: &str, text: &str) -> TestResult {
        std::fs::create_dir_all(dir).map_err(ctx("create cards dir"))?;
        std::fs::write(dir.join(name), text).map_err(ctx("write card"))?;
        Ok(())
    }

    #[test]
    fn board_ids_come_only_from_simple_board_flags() {
        assert_eq!(
            board_from_refresh("/kanban --board=sprint show"),
            Some("sprint")
        );
        assert_eq!(board_from_refresh("/kanban show"), None);
        assert_eq!(board_from_refresh("/kanban --board=.. show"), None);
        assert_eq!(board_from_refresh("/kanban --board=a/b show"), None);
        let root = Path::new("/k");
        assert_eq!(
            watched_dir(root, Some("sprint")),
            PathBuf::from("/k/kanban/boards/sprint")
        );
        assert_eq!(watched_dir(root, None), PathBuf::from("/k/kanban/boards"));
    }

    #[test]
    fn signature_changes_with_content_and_ignores_dotfiles() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let cards = temp.path().join("cards");
        assert_eq!(board_signature(temp.path()), board_signature(temp.path()));
        let empty = board_signature(temp.path());
        write_card(&cards, "c1.md", "eins")?;
        let one = board_signature(temp.path());
        assert_ne!(empty, one);
        write_card(&cards, ".c1.md.lock", "")?;
        assert_eq!(board_signature(temp.path()), one);
        write_card(&cards, "c1.md", "eins, länger")?;
        assert_ne!(board_signature(temp.path()), one);
        // Ein fehlendes Verzeichnis ist ein leeres Board.
        assert_eq!(
            board_signature(&temp.path().join("fehlt")),
            board_signature(&temp.path().join("fehlt-auch"))
        );
        Ok(())
    }

    #[test]
    fn watch_sets_a_baseline_throttles_and_reports_changes() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = temp.path().join("board");
        let mut watch = KanbanLiveWatch::default();
        let start = Instant::now();
        assert!(!watch.poll_at(&dir, start), "erster Blick: nur Grundlinie");
        write_card(&dir.join("cards"), "c1.md", "neu")?;
        assert!(
            !watch.poll_at(&dir, start + Duration::from_millis(500)),
            "vor Ablauf des Intervalls wird nicht abgefragt"
        );
        assert!(watch.poll_at(&dir, start + KANBAN_LIVE_INTERVAL));
        assert!(!watch.poll_at(&dir, start + KANBAN_LIVE_INTERVAL * 2));
        // Board-Wechsel: neue Grundlinie, kein Nachladen.
        let other = temp.path().join("other");
        write_card(&other, "x.md", "x")?;
        assert!(!watch.poll_at(&other, start + KANBAN_LIVE_INTERVAL * 3));
        Ok(())
    }

    #[test]
    fn open_board_reloads_after_a_file_change_and_closed_views_stay_quiet() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut app = test_chat_app()?;
        app.kanban_live = KanbanLiveWatch::with_root(temp.path());
        let start = Instant::now();
        // Ohne offenes Board: inaktiv.
        assert!(!app.kanban_live_active());
        assert!(!app.poll_kanban_live_at(start));

        app.open_overlay_view(Box::new(KanbanBoard::new()));
        app.pending_fetches.clear();
        assert!(app.kanban_live_active());
        assert!(!app.poll_kanban_live_at(start));
        assert!(app.pending_fetches.is_empty());

        let cards = watched_dir(temp.path(), None).join("default").join("cards");
        write_card(&cards, "card-1.md", "Karte")?;
        assert!(app.poll_kanban_live_at(start + KANBAN_LIVE_INTERVAL));
        assert_eq!(
            app.pending_fetches,
            vec![super::super::DataFetch::Overlay {
                command: crate::kanban_board::REFRESH_COMMAND.to_owned(),
                generation: app.overlay_generation,
            }]
        );

        app.overlay = None;
        assert!(!app.kanban_live_active());
        assert!(!app.poll_kanban_live_at(start + KANBAN_LIVE_INTERVAL * 2));
        Ok(())
    }
}
