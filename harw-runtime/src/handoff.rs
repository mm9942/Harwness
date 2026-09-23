//! Sitzungs-Übergabe (`handoff.json`) — automatische Kompaktierungs-Notiz.
//!
//! # Verantwortungsbereich
//! Dieses Modul schreibt und liest eine einzige, projekt-lokale Datei
//! (`<projekt>/.harw/state/handoff.json`), die den letzten Kompaktierungslauf
//! einer Session zusammenfasst — die "Übergabe" an eine folgende Session
//! desselben Projekts. [`HandoffWriter`] implementiert
//! [`harw_core::compaction::CompactionObserver`] und schreibt die Datei
//! best-effort, sobald `harw-core::compaction::compact_session` eine
//! [`harw_core::compaction::CompactionOutcome`] meldet.
//! [`HandoffContextProvider`] implementiert
//! [`harw_extension_api::contributors::ContextProvider`] und liest dieselbe
//! Datei zurück, um sie als Kontext-Fragment in eine neue Session
//! einzuspeisen.
//!
//! # Schlüsseltypen
//! - [`SessionHandoff`] — das persistierte, auf [`HANDOFF_MAX_BYTES`]
//!   begrenzte JSON-Dokument.
//! - [`HandoffWriter`] — Kompaktierungs-Observer, schreibt atomar.
//! - [`HandoffContextProvider`] — Kontext-Anbieter, liest die Übergabe zurück.
//!
//! # Nebenläufigkeit
//! Beide Typen sind `Send + Sync` (sie halten nur ein geklontes
//! [`harw_home::project::ProjectHome`], keine innere Veränderlichkeit).
//! [`HandoffWriter::on_compacted`] schreibt synchron; ein Fehler dabei wird
//! ausschließlich mit `tracing::warn!` gemeldet, nie propagiert — eine
//! misslungene Übergabe darf eine laufende Kompaktierung nicht scheitern
//! lassen.
//!
//! # Fehlertypen
//! Kein eigener Fehlertyp: Schreiben scheitert best-effort (nur `warn!`),
//! Lesen liefert bei jedem Fehler schlicht `None` ([`read_handoff`]).
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_home::project::{ProjectHome, discover_project};
//! use harw_runtime::handoff::{HandoffContextProvider, read_handoff};
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), harw_home::HomeError> {
//! let project = discover_project(Path::new("."), &[])?;
//! let home = ProjectHome::at(&project);
//! if let Some(handoff) = read_handoff(&home) {
//!     println!("{:?}", handoff.summary);
//! }
//! let _provider = HandoffContextProvider::new(home);
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;

use harw_extension_api::contributors::{ContextProvider, ExtFuture};
use harw_extension_api::types::{ContextFragment, TurnInputContext};
use harw_home::project::ProjectHome;

/// Obergrenze der serialisierten `handoff.json` in Bytes.
///
/// [`HandoffWriter::on_compacted`] kürzt `summary` zeichensicher, bis das
/// serialisierte Dokument diese Grenze einhält.
pub const HANDOFF_MAX_BYTES: usize = 16 * 1024;

/// Dateiname der Übergabe innerhalb von [`ProjectHome::state_dir`].
const HANDOFF_FILE_NAME: &str = "handoff.json";

/// Persistiertes Übergabe-Dokument einer kompaktierten Session.
///
/// # Description
/// Wird von [`HandoffWriter::on_compacted`] geschrieben und von
/// [`read_handoff`]/[`HandoffContextProvider`] gelesen. `open_items` und
/// `touched_files` werden von [`HandoffWriter`] derzeit immer leer
/// geschrieben (keine Datenquelle dafür in [`harw_core::compaction::CompactionOutcome`]);
/// sie bleiben Teil des Formats, damit ein künftiger Schreiber sie befüllen
/// kann, ohne das Format zu brechen.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionHandoff {
    /// Session, aus der diese Übergabe stammt.
    pub session_id: String,
    /// Zeitpunkt des Schreibens, RFC3339 (`jiff::Timestamp::to_string()`).
    pub written_at: String,
    /// Anlass der Kompaktierung (`{:?}`-Darstellung von
    /// [`harw_core::auto_compact::CompactDecision`]), falls bekannt.
    pub reason: Option<String>,
    /// Zusammenfassung des kompaktierten Verlaufs, falls vorhanden.
    pub summary: Option<String>,
    /// Offene Punkte für die Folge-Session (derzeit immer leer, siehe oben).
    pub open_items: Vec<String>,
    /// Zuletzt berührte Dateien (derzeit immer leer, siehe oben).
    pub touched_files: Vec<String>,
}

/// Leitet den Pfad der Übergabe-Datei her: `<state_dir>/handoff.json`.
#[must_use]
pub fn handoff_path(project_home: &ProjectHome) -> PathBuf {
    project_home.state_dir().join(HANDOFF_FILE_NAME)
}

/// Liest die zuletzt geschriebene Übergabe eines Projekts, falls vorhanden.
///
/// # Description
/// `None` bei jedem Fehler (Datei fehlt, nicht lesbar, nicht dekodierbar) —
/// eine fehlende oder defekte Übergabe ist kein Fehlerfall für den Aufrufer,
/// sondern schlicht "keine Übergabe verfügbar".
#[must_use]
pub fn read_handoff(project_home: &ProjectHome) -> Option<SessionHandoff> {
    let path = handoff_path(project_home);
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Kompaktierungs-Observer, der eine [`SessionHandoff`] atomar in
/// [`ProjectHome::state_dir`] schreibt.
pub struct HandoffWriter {
    project_home: ProjectHome,
}

impl HandoffWriter {
    /// Baut einen Writer für das gegebene Projekt-Home.
    #[must_use]
    pub fn new(project_home: ProjectHome) -> Self {
        Self { project_home }
    }
}

impl harw_core::compaction::CompactionObserver for HandoffWriter {
    /// Schreibt die Übergabe der kompaktierten Session best-effort.
    ///
    /// # Description
    /// Baut eine [`SessionHandoff`] aus `outcome` (Zusammenfassung und
    /// Kompaktierungsgrund; `open_items`/`touched_files` bleiben leer, siehe
    /// [`SessionHandoff`]-Doku), kürzt `summary` zeichensicher, bis das
    /// serialisierte Dokument [`HANDOFF_MAX_BYTES`] einhält, und schreibt
    /// dann atomar über [`harw_fsutil::write_atomic`] (Tempdatei im selben
    /// Verzeichnis, `rename`). [`ProjectHome::state_dir`] wird vorher über
    /// `create_dir_all` sichergestellt.
    ///
    /// Jeder Fehler (Verzeichnis nicht anlegbar, Schreiben schlägt fehl) wird
    /// ausschließlich mit `tracing::warn!` gemeldet — eine misslungene
    /// Übergabe darf die Kompaktierung selbst nicht scheitern lassen.
    fn on_compacted(
        &self,
        session_id: &harw_types::SessionId,
        outcome: &harw_core::compaction::CompactionOutcome,
    ) {
        let handoff = SessionHandoff {
            session_id: session_id.to_string(),
            written_at: jiff::Timestamp::now().to_string(),
            reason: outcome.reason.as_ref().map(|reason| format!("{reason:?}")),
            summary: outcome.summary_text.clone(),
            open_items: Vec::new(),
            touched_files: Vec::new(),
        };
        let bytes = cap_to_max_bytes(handoff);

        let state_dir = self.project_home.state_dir();
        if let Err(error) = std::fs::create_dir_all(&state_dir) {
            tracing::warn!(
                error = %error,
                path = %state_dir.display(),
                "handoff: Zustandsverzeichnis konnte nicht angelegt werden"
            );
            return;
        }

        let path = handoff_path(&self.project_home);
        if let Err(error) =
            harw_fsutil::write_atomic(&path, &bytes, harw_fsutil::AtomicWriteOptions::private())
        {
            tracing::warn!(
                error = %error,
                path = %path.display(),
                "handoff: Übergabe konnte nicht geschrieben werden"
            );
        }
    }
}

/// Serialisiert `handoff` als hübsch formatiertes JSON und kürzt `summary`
/// zeichensicher (nie mitten in einem UTF-8-Codepunkt), bis das Ergebnis
/// [`HANDOFF_MAX_BYTES`] einhält.
///
/// Ist `summary` bereits leer oder `None` und das Dokument immer noch zu
/// groß (praktisch nur bei extrem vielen `open_items`/`touched_files`
/// erreichbar — derzeit stets leer, siehe [`SessionHandoff`]-Doku), wird das
/// zuletzt erreichte Ergebnis unverändert zurückgegeben statt in eine
/// Endlosschleife zu laufen.
fn cap_to_max_bytes(mut handoff: SessionHandoff) -> Vec<u8> {
    loop {
        let bytes = serde_json::to_vec_pretty(&handoff).unwrap_or_default();
        if bytes.len() <= HANDOFF_MAX_BYTES {
            return bytes;
        }
        match handoff.summary.as_mut() {
            Some(summary) if !summary.is_empty() => {
                let mut chars = summary.chars();
                chars.next_back();
                *summary = chars.as_str().to_owned();
            }
            Some(_) => {
                handoff.summary = None;
            }
            None => return bytes,
        }
    }
}

/// Kontext-Anbieter, der die zuletzt geschriebene [`SessionHandoff`] eines
/// Projekts als Kontext-Fragment einspeist.
pub struct HandoffContextProvider {
    project_home: ProjectHome,
}

impl HandoffContextProvider {
    /// Baut einen Provider für das gegebene Projekt-Home.
    #[must_use]
    pub fn new(project_home: ProjectHome) -> Self {
        Self { project_home }
    }
}

impl ContextProvider for HandoffContextProvider {
    /// Liefert genau ein Fragment (Label `"handoff"`) mit der zuletzt
    /// geschriebenen Übergabe, oder keines, wenn keine Übergabe existiert.
    ///
    /// # Description
    /// Der Fragment-Inhalt beginnt mit der deutschen Überschrift „Übergabe
    /// aus vorheriger Session (automatisch):", gefolgt von der
    /// Zusammenfassung ([`SessionHandoff::summary`]) und — falls vorhanden —
    /// einer Liste offener Punkte ([`SessionHandoff::open_items`]).
    fn contribute<'a>(&'a self, _ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
        Box::pin(async move {
            let Some(handoff) = read_handoff(&self.project_home) else {
                return Vec::new();
            };

            let mut content = String::from("Übergabe aus vorheriger Session (automatisch):\n");
            match handoff.summary.as_deref().map(str::trim) {
                Some(summary) if !summary.is_empty() => {
                    content.push_str(summary);
                    content.push('\n');
                }
                _ => {}
            }
            if !handoff.open_items.is_empty() {
                content.push_str("\nOffene Punkte:\n");
                for item in &handoff.open_items {
                    content.push_str("- ");
                    content.push_str(item);
                    content.push('\n');
                }
            }

            vec![ContextFragment {
                label: "handoff".to_owned(),
                content,
            }]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_core::auto_compact::CompactDecision;
    use harw_core::compaction::{CompactionObserver, CompactionOutcome};
    use harw_home::project::{ProjectKind, ProjectRoot};

    fn temp_home(tag: &str) -> TestResult<ProjectHome> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-runtime-handoff-{tag}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("Testwurzel anlegen"))?;
        Ok(ProjectHome::at(&ProjectRoot {
            trust_key: root.clone(),
            root,
            kind: ProjectKind::Directory,
        }))
    }

    fn cleanup(home: &ProjectHome) -> TestResult {
        let parent = home
            .dir
            .parent()
            .ok_or(TestError::Missing("Projekt-Root"))?;
        let _ = std::fs::remove_dir_all(parent);
        Ok(())
    }

    #[test]
    fn on_compacted_writes_a_readable_handoff() -> TestResult {
        let home = temp_home("write-read")?;
        let writer = HandoffWriter::new(home.clone());
        let session = harw_types::SessionId::from_str("session-a");
        let outcome = CompactionOutcome {
            summary_text: Some("Zusammenfassung des Verlaufs".to_owned()),
            reason: Some(CompactDecision::BudgetExceeded),
            ..CompactionOutcome::default()
        };

        writer.on_compacted(&session, &outcome);

        let loaded = read_handoff(&home)
            .ok_or(TestError::Missing("handoff must be readable after writing"))?;
        assert_eq!(loaded.session_id, "session-a");
        assert_eq!(
            loaded.summary.as_deref(),
            Some("Zusammenfassung des Verlaufs")
        );
        assert_eq!(loaded.reason.as_deref(), Some("BudgetExceeded"));
        assert!(loaded.open_items.is_empty());
        assert!(loaded.touched_files.is_empty());

        cleanup(&home)
    }

    #[test]
    fn read_handoff_without_a_written_file_returns_none() -> TestResult {
        let home = temp_home("missing")?;
        assert!(read_handoff(&home).is_none());
        cleanup(&home)
    }

    #[test]
    fn cap_to_max_bytes_shrinks_an_oversized_summary() {
        let handoff = SessionHandoff {
            session_id: "session-a".to_owned(),
            written_at: jiff::Timestamp::UNIX_EPOCH.to_string(),
            reason: None,
            summary: Some("x".repeat(HANDOFF_MAX_BYTES * 2)),
            open_items: Vec::new(),
            touched_files: Vec::new(),
        };

        let bytes = cap_to_max_bytes(handoff);

        assert!(bytes.len() <= HANDOFF_MAX_BYTES);
        assert!(!bytes.is_empty());
    }

    // Treibt eine `ExtFuture` synchron zu Ende, ohne einen externen
    // Async-Executor als Dev-Dependency zu brauchen — wie
    // `harw_extension_api::contributors`-Tests. Jede Zukunft hier ist nach
    // dem ersten `Box::pin(async { .. })`-Schritt ohne inneres `.await`
    // aufgebaut und wird deshalb beim ersten Poll fertig.
    fn block_on<T>(mut fut: ExtFuture<'_, T>) -> T {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        loop {
            if let std::task::Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }

    #[test]
    fn context_provider_contributes_nothing_without_a_handoff() -> TestResult {
        let home = temp_home("provider-empty")?;
        let provider = HandoffContextProvider::new(home.clone());

        let ctx = TurnInputContext::default();
        let fragments = block_on(provider.contribute(&ctx));

        assert!(fragments.is_empty());
        cleanup(&home)
    }

    #[test]
    fn context_provider_contributes_a_labeled_fragment_with_summary() -> TestResult {
        let home = temp_home("provider-fragment")?;
        let writer = HandoffWriter::new(home.clone());
        let session = harw_types::SessionId::from_str("session-b");
        let outcome = CompactionOutcome {
            summary_text: Some("Kurzfassung".to_owned()),
            ..CompactionOutcome::default()
        };
        writer.on_compacted(&session, &outcome);

        let provider = HandoffContextProvider::new(home.clone());
        let ctx = TurnInputContext::default();
        let fragments = block_on(provider.contribute(&ctx));

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].label, "handoff");
        assert!(
            fragments[0]
                .content
                .starts_with("Übergabe aus vorheriger Session (automatisch):")
        );
        assert!(fragments[0].content.contains("Kurzfassung"));

        cleanup(&home)
    }
}
