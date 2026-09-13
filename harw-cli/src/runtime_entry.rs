//! Composition-Root-Helfer für `harw`-Einstiege über [`harw_runtime`].
//!
//! # Beschreibung
//! Vier kleine, seiteneffektarme Bausteine, aus denen ein Einstieg (TUI,
//! One-Shot, Gateway, Job-Worker, …) seinen Aufruf von
//! [`harw_runtime::RuntimeAssembly::builder`] zusammensetzt, statt die
//! Montage-Reihenfolge (Spec → Sitzungswurzel → Speicher → Modell → Bau) an
//! jeder Aufrufstelle erneut hinzuschreiben (Vertrag:
//! `docs/remediation/AGENT-BRIEF.md`, `docs/remediation/ledger/W2d1/A1.md`):
//!
//! - [`runtime_spec`] — die Eingangsbeschreibung eines Laufs ohne Overrides.
//! - [`profile_sessions_root`] — das Transkriptverzeichnis des aktiven Profils,
//!   angelegt bei Bedarf.
//! - [`transcript_state_store`] — der durable [`StateStore`] darüber.
//! - [`build_assembly`] — die eine Bau-Aufrufstelle, Fehler als `String` für
//!   Aufrufer, die (noch) keinen eigenen Fehlertyp tragen.
//!
//! # Nebenläufigkeit
//! Alle vier Funktionen sind zustandslos bezüglich `self`; [`profile_sessions_root`]
//! legt ein Verzeichnis an (`std::fs::create_dir_all`), sonst kein I/O.
//!
//! # Fehler
//! [`profile_sessions_root`] und [`build_assembly`] geben `Err(String)` ohne
//! Geheimnisse zurück — nur Pfade und die `Display`-Form der Fach-Fehler
//! (siehe [`harw_runtime::RuntimeError`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_core::{SessionThreadMapper, StateStore, TranscriptStateStore};
use harw_home::{active_profile_name, profile_dir};
use harw_protocol::SessionEvent;
use harw_runtime::{EntryKind, ModelSource, RuntimeAssembly, RuntimeSpec, RuntimeStores};
use harw_session_store::TranscriptStore;
use harw_types::Principal;
use tokio::sync::mpsc::UnboundedSender;

/// Baut die Eingangsbeschreibung eines Laufs ohne explizite Overrides.
///
/// # Beschreibung
/// `mode_override`, `active_agent` und `reasoning_effort` bleiben `None`: sie
/// sind Sache des Einstiegs (`--agent`, `--effort`, expliziter Modus), nicht
/// dieser Vorlage. `home`/`cwd` werden über [`Path::to_path_buf`] übernommen.
///
/// # Argumente
/// - `entry` ([`EntryKind`]): der Einstieg; bestimmt über
///   [`EntryKind::profile`] alle Rechte des Laufs.
/// - `home` (`&Path`): aufgelöster Root-Space (`~/.harw` bzw. `HARW_HOME`).
/// - `cwd` (`&Path`): Arbeitsverzeichnis des Laufs.
/// - `principal` ([`Principal`]): vertrauenswürdig ermittelter Aufrufer.
///
/// # Rückgabe
/// Eine [`RuntimeSpec`] mit den vier übergebenen Werten und keinem Override.
#[must_use]
pub(crate) fn runtime_spec(
    entry: EntryKind,
    home: &Path,
    cwd: &Path,
    principal: Principal,
) -> RuntimeSpec {
    RuntimeSpec {
        entry,
        home: home.to_path_buf(),
        cwd: cwd.to_path_buf(),
        principal,
        mode_override: None,
        active_agent: None,
        reasoning_effort: None,
    }
}

/// Löst das Transkriptverzeichnis des aktiven Profils auf und legt es an.
///
/// # Beschreibung
/// `<profile_dir(home, active_profile_name(home))>/sessions`. Das aktive
/// Profil folgt `HARW_PROFILE`/der `active_profile`-Zeigerdatei
/// ([`active_profile_name`]); der Profilordner selbst folgt
/// [`profile_dir`]. Das Verzeichnis wird über
/// [`std::fs::create_dir_all`] angelegt, falls es noch nicht existiert —
/// ohne durables Transkriptverzeichnis gibt es keinen [`StateStore`] für
/// diesen Lauf.
///
/// # Argumente
/// - `home` (`&Path`): aufgelöster Root-Space.
///
/// # Rückgabe
/// Den angelegten Pfad `<profil>/sessions`.
///
/// # Fehler
/// `Err(String)`, wenn der Profilname ungültig ist
/// ([`harw_home::HomeError`]) oder das Verzeichnis nicht angelegt werden
/// kann. Der Fehlertext nennt den Pfad, aber keine Geheimnisse — es gibt
/// hier keine.
pub(crate) fn profile_sessions_root(home: &Path) -> Result<PathBuf, String> {
    let profile = active_profile_name(home);
    let dir = profile_dir(home, &profile).map_err(|error| {
        format!("could not resolve the profile directory for profile '{profile}': {error}")
    })?;
    let sessions_root = dir.join("sessions");
    std::fs::create_dir_all(&sessions_root).map_err(|error| {
        format!(
            "could not create the sessions directory at '{}': {error}",
            sessions_root.display()
        )
    })?;
    Ok(sessions_root)
}

/// Baut den durablen Verlaufsspeicher eines Laufs über [`TranscriptStore`].
///
/// # Argumente
/// - `sessions_root` (`&Path`): Wurzel der Transkript-Dateien, typischerweise
///   das Ergebnis von [`profile_sessions_root`].
/// - `mapper` ([`SessionThreadMapper`]): leitet aus einer [`harw_types::SessionId`]
///   deterministisch den [`harw_types::ThreadRef`] ab, unter dem der
///   Verlauf abgelegt wird (muss über Prozessneustarts hinweg stabil sein).
///
/// # Rückgabe
/// `Arc<dyn StateStore>` — ein [`TranscriptStateStore`] über einem frisch
/// gebauten [`TranscriptStore`] für `sessions_root`.
#[must_use]
pub(crate) fn transcript_state_store(
    sessions_root: &Path,
    mapper: SessionThreadMapper,
) -> Arc<dyn StateStore> {
    Arc::new(TranscriptStateStore::new(
        TranscriptStore::new(sessions_root),
        mapper,
    ))
}

/// Montiert eine [`RuntimeAssembly`] aus Spec, Modell und Speichern.
///
/// # Beschreibung
/// Dünner Wrapper um [`RuntimeAssembly::builder`]: setzt die beiden
/// Pflichtfelder ([`harw_runtime::RuntimeAssemblyBuilder::model`],
/// [`harw_runtime::RuntimeAssemblyBuilder::stores`]), hängt bei `Some`
/// den Sitzungs-Ereigniskanal an
/// ([`harw_runtime::RuntimeAssemblyBuilder::session_events`]) und baut. Ein
/// Einstieg mit [`harw_runtime::SpawnerPolicy::None`] (z. B.
/// [`EntryKind::LocalEcho`]) braucht keinen Kanal — `session_events: None`
/// baut dort unverändert durch, weil
/// [`harw_runtime::RuntimeAssemblyBuilder::build`] den Kanal nur für
/// [`harw_runtime::SpawnerPolicy::BuiltinRoles`] verlangt.
///
/// # Argumente
/// - `spec` ([`RuntimeSpec`]): die Eingangsbeschreibung, z. B. aus
///   [`runtime_spec`].
/// - `model` ([`ModelSource`]): Quelle des Wurzel-Modells.
/// - `stores` ([`RuntimeStores`]): die durablen Speicher des Laufs.
/// - `session_events` (`Option<UnboundedSender<SessionEvent>>`): Kanal der
///   Sitzungs-Ereignisse; `None`, wenn der Einstieg keine Kind-Agenten
///   spawnt.
///
/// # Rückgabe
/// Die fertig montierte [`RuntimeAssembly`].
///
/// # Fehler
/// `Err(String)` mit der `Display`-Form von [`harw_runtime::RuntimeError`],
/// wenn der Bau in einer der Montagephasen scheitert (Konfiguration,
/// Vertrauen, Projekterkennung, Sandbox, Registry, Provider, Speicher,
/// Spawner).
pub(crate) fn build_assembly(
    spec: RuntimeSpec,
    model: ModelSource,
    stores: RuntimeStores,
    session_events: Option<UnboundedSender<SessionEvent>>,
) -> Result<RuntimeAssembly, String> {
    let mut builder = RuntimeAssembly::builder(spec).model(model).stores(stores);
    if let Some(events) = session_events {
        builder = builder.session_events(events);
    }
    builder.build().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::{IngressSurface, PermissionTier, PrincipalKind, SessionId, ThreadRef};
    use tempfile::TempDir;

    fn test_principal() -> Principal {
        Principal::trusted_ingress(
            PrincipalKind::Human,
            "test",
            IngressSurface::Tui,
            PermissionTier::Owner,
        )
    }

    /// Deterministischer, aber einlaufspezifischer Test-Mapper — spiegelt das
    /// Muster aus `harw-cli/src/gateway.rs` (`dream_thread_for_session`).
    fn test_thread_for_session(session_id: &SessionId) -> ThreadRef {
        ThreadRef::from_str(format!("runtime-entry-test:{}", session_id.as_str()))
    }

    #[test]
    fn test_runtime_spec_sets_entry_home_cwd_and_no_overrides() {
        let home = PathBuf::from("/does/not/need/to/exist/home");
        let cwd = PathBuf::from("/does/not/need/to/exist/cwd");
        let principal = test_principal();

        let spec = runtime_spec(EntryKind::LocalEcho, &home, &cwd, principal.clone());

        assert_eq!(spec.entry, EntryKind::LocalEcho);
        assert_eq!(spec.home, home);
        assert_eq!(spec.cwd, cwd);
        assert_eq!(spec.principal, principal);
        assert_eq!(spec.mode_override, None);
        assert_eq!(spec.active_agent, None);
        assert_eq!(spec.reasoning_effort, None);
    }

    #[test]
    fn test_profile_sessions_root_creates_directory() {
        let home = TempDir::new().expect("home tempdir");

        let sessions_root =
            profile_sessions_root(home.path()).expect("sessions root resolves and is created");

        assert!(sessions_root.is_dir(), "{}", sessions_root.display());
        assert!(sessions_root.ends_with("sessions"));
        assert!(sessions_root.starts_with(home.path()));
    }

    #[test]
    fn test_build_assembly_local_echo_succeeds() {
        let home = TempDir::new().expect("home tempdir");
        let cwd = TempDir::new().expect("cwd tempdir");

        let spec = runtime_spec(EntryKind::LocalEcho, home.path(), cwd.path(), test_principal());
        let sessions_root =
            profile_sessions_root(home.path()).expect("sessions root resolves and is created");
        let state_store = transcript_state_store(&sessions_root, test_thread_for_session);
        let stores = RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        };

        let assembly = build_assembly(spec, ModelSource::Echo("x".to_owned()), stores, None);

        assert!(assembly.is_ok(), "{:?}", assembly.err());
    }
}
