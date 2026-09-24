//! Lebenszyklus leerer TUI-Sitzungen (Runde 5, Teil H).
//!
//! # Befund
//! Jeder TUI-Start (auch `harw -r` vor der Auswahl im Picker) und jedes
//! `/new` baut eine frische Wurzelsitzung. Beim Wechsel (`/resume`, `/new`)
//! und beim Beenden sicherte `runtime_root.rs` deren Zustand über
//! `AgentSession::persist_state` → `StateStore::save_session_state` — für den
//! `TranscriptStateStore` ein Anhängen an `<id>.jsonl`. Damit bekam auch eine
//! Sitzung ohne einen einzigen Nutzer-Turn ein Transcript und erschien danach
//! im Picker als „(ohne Titel)“. Zusätzlich legt die CLI-Montage
//! (`harw-cli/src/chat.rs::tag_session_project`) sofort einen
//! Metadaten-Sidecar an.
//!
//! # Regel
//! - Eine Sitzung ohne Nutzer-Turn wird nicht persistiert
//!   ([`should_persist`]).
//! - Beim Verlassen einer solchen Sitzung werden ihre Sidecars entfernt,
//!   **nur** wenn sie wirklich kein Transcript mit Nutzer-Turn hat
//!   ([`discard_empty_session_sidecars`]).

use std::path::Path;

use harw_core::AgentSession;
use harw_protocol::items::TurnItem;
use harw_types::SessionId;

/// `true`, wenn der Verlauf der Sitzung mindestens eine Nutzernachricht
/// enthält.
#[must_use]
pub(crate) fn has_user_turn(session: &AgentSession) -> bool {
    session
        .history()
        .items()
        .iter()
        .any(|item| matches!(item, TurnItem::UserMessage(_)))
}

/// Systemzeile für eine fortgesetzte Sitzung (statt der Begrüßung).
///
/// # Beschreibung
/// `/resume` und `harw -r <id>` setzen eine Sitzung fort; die Begrüßung
/// („Guten Morgen …“) klänge dort, als wäre die Sitzung neu gestartet.
///
/// # Argumente
/// - `id` (`&SessionId`): die fortgesetzte Sitzung.
/// - `items` (`usize`): Anzahl geladener Verlaufseinträge.
#[must_use]
pub(crate) fn resume_notice(id: &SessionId, items: usize) -> String {
    let short: String = id.as_str().chars().take(8).collect();
    format!("Sitzung {short} fortgesetzt — {items} Verlaufseinträge geladen.")
}

/// Ob der Zustand der Sitzung beim Wechsel/Beenden gesichert werden soll.
///
/// # Rückgabe
/// `false` für eine Sitzung ohne Nutzer-Turn — sie hinterlässt dann kein
/// Transcript und erscheint nie als leere Sitzung im Picker.
#[must_use]
pub(crate) fn should_persist(session: &AgentSession) -> bool {
    has_user_turn(session)
}

/// Entfernt den Metadaten-Sidecar einer verlassenen, leeren Sitzung.
///
/// # Beschreibung
/// Wirkt nur, wenn die Sitzung im Speicher keinen Nutzer-Turn hat **und**
/// unter `store_root` kein Transcript `<id>.jsonl` existiert — ein
/// vorhandenes Transcript wird nie angefasst. Best-effort: Fehler werden nur
/// protokolliert.
///
/// # Argumente
/// - `store_root` (`&Path`): Wurzel der Transcripts/Sidecars des Profils.
/// - `session` (`&AgentSession`): die verlassene Sitzung.
///
/// # Rückgabe
/// `true`, wenn ein Sidecar entfernt wurde.
pub(crate) fn discard_empty_session_sidecars(store_root: &Path, session: &AgentSession) -> bool {
    if has_user_turn(session) {
        return false;
    }
    discard_sidecars_without_transcript(store_root, session.id())
}

/// Kern von [`discard_empty_session_sidecars`] ohne Sitzungsobjekt.
fn discard_sidecars_without_transcript(store_root: &Path, id: &SessionId) -> bool {
    let transcript = store_root.join(format!("{}.jsonl", id.as_str()));
    if transcript.exists() {
        return false;
    }
    let meta = harw_session_store::meta::meta_path(store_root, id);
    match std::fs::remove_file(&meta) {
        Ok(()) => {
            tracing::info!(session = %id, "tui.session.empty_sidecar_removed");
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(session = %id, %error, "tui.session.empty_sidecar_remove_failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{discard_empty_session_sidecars, has_user_turn, should_persist};
    use crate::test_support::{TestResult, ctx};
    use harw_core::{AgentSession, InMemoryStateStore};
    use harw_extension_api::ExtensionRegistryBuilder;
    use harw_types::AgentRole;

    fn session() -> AgentSession {
        let (event_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        AgentSession::new(
            AgentRole::Assistant,
            None,
            ExtensionRegistryBuilder::default().build(),
            event_tx,
        )
    }

    #[test]
    fn resume_notice_names_the_session_and_never_greets() {
        let id = harw_types::SessionId::from_str("0123456789abcdef");
        let notice = super::resume_notice(&id, 65);
        assert!(notice.contains("01234567"), "{notice}");
        assert!(notice.contains("65"), "{notice}");
        assert!(!notice.contains("Guten"), "{notice}");
    }

    #[test]
    fn a_session_without_user_turn_is_not_persisted() {
        let mut session = session();
        assert!(!has_user_turn(&session));
        assert!(!should_persist(&session));
        session.history_mut().push_user_text("hallo".to_owned());
        assert!(has_user_turn(&session));
        assert!(should_persist(&session));
    }

    #[tokio::test]
    async fn skipping_persist_leaves_no_state_behind() -> TestResult {
        let session = session();
        let store = InMemoryStateStore::new();
        if should_persist(&session) {
            session
                .persist_state(&store)
                .await
                .map_err(ctx("persist"))?;
        }
        let state = harw_core::StateStore::load_session_state(&store, session.id())
            .await
            .map_err(ctx("load state"))?;
        assert!(state.is_none(), "an empty session must leave no state");
        Ok(())
    }

    #[test]
    fn sidecar_of_an_empty_session_without_transcript_is_removed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let session = session();
        let meta = harw_session_store::meta::meta_path(dir.path(), session.id());
        if let Some(parent) = meta.parent() {
            std::fs::create_dir_all(parent).map_err(ctx("meta dir"))?;
        }
        std::fs::write(&meta, "{}").map_err(ctx("write meta"))?;
        assert!(discard_empty_session_sidecars(dir.path(), &session));
        assert!(!meta.exists());
        Ok(())
    }

    #[test]
    fn an_existing_transcript_or_user_turn_keeps_the_sidecar() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut session = session();
        let meta = harw_session_store::meta::meta_path(dir.path(), session.id());
        if let Some(parent) = meta.parent() {
            std::fs::create_dir_all(parent).map_err(ctx("meta dir"))?;
        }
        std::fs::write(&meta, "{}").map_err(ctx("write meta"))?;

        // Ein Transcript existiert: nie anfassen.
        let transcript = dir.path().join(format!("{}.jsonl", session.id().as_str()));
        std::fs::write(&transcript, "").map_err(ctx("write transcript"))?;
        assert!(!discard_empty_session_sidecars(dir.path(), &session));
        assert!(meta.exists());
        std::fs::remove_file(&transcript).map_err(ctx("remove transcript"))?;

        // Ein Nutzer-Turn im Speicher: nie anfassen.
        session.history_mut().push_user_text("hallo".to_owned());
        assert!(!discard_empty_session_sidecars(dir.path(), &session));
        assert!(meta.exists());
        Ok(())
    }
}
