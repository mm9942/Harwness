//! Composition-Root-Helfer für `harw`-Einstiege über [`harw_runtime`].
//!
//! # Beschreibung
//! Sieben kleine, seiteneffektarme Bausteine, aus denen ein Einstieg (TUI,
//! One-Shot, Gateway, Job-Worker, Doctor, …) seinen Aufruf von
//! [`harw_runtime::RuntimeAssembly::builder`] zusammensetzt, statt die
//! Montage-Reihenfolge (Spec → Sitzungswurzel → Speicher → Modell → Bau) an
//! jeder Aufrufstelle erneut hinzuschreiben (Vertrag:
//! `docs/remediation/AGENT-BRIEF.md`, `docs/remediation/CONTRACTS-W2d2.md`
//! §1.3, `docs/remediation/ledger/W2d1/A1.md`):
//!
//! - [`runtime_spec`] — die Eingangsbeschreibung eines Laufs ohne Overrides.
//! - [`profile_sessions_root`] — das Transkriptverzeichnis des aktiven Profils,
//!   angelegt bei Bedarf.
//! - [`transcript_state_store`] — der durable [`StateStore`] darüber.
//! - [`build_assembly`] — die eine Bau-Aufrufstelle, Fehler als `String` für
//!   Aufrufer, die (noch) keinen eigenen Fehlertyp tragen.
//! - [`local_principal`] — der vertrauenswürdige Principal eines lokalen
//!   (TUI/CLI-)Aufrufers, über die vom Kernel bezeugte Prozess-UID.
//! - [`configured_secret_resolver`] — Wrapper um
//!   `crate::secret_store::open_configured_secret_resolver` für lokale
//!   Einstiege.
//! - [`doctor_assembly`] — die Runtime-Montage für `harw doctor`
//!   ([`EntryKind::Doctor`], flüchtiger Speicher, keine Jobs/Freigaben).
//!
//! # Nebenläufigkeit
//! Alle sieben Funktionen sind zustandslos bezüglich `self`;
//! [`profile_sessions_root`] legt ein Verzeichnis an
//! (`std::fs::create_dir_all`), sonst kein I/O außer dem, das
//! [`build_assembly`]/[`doctor_assembly`] über den Builder und
//! [`configured_secret_resolver`] über den versiegelten Speicher auslösen.
//!
//! # Fehler
//! [`profile_sessions_root`], [`build_assembly`], [`configured_secret_resolver`]
//! und [`doctor_assembly`] geben `Err(String)` ohne Geheimnisse zurück — nur
//! Pfade und die `Display`-Form der Fach-Fehler (siehe
//! [`harw_runtime::RuntimeError`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_config::ResolvedConfig;
use harw_core::{InMemoryStateStore, SessionThreadMapper, StateStore, TranscriptStateStore};
use harw_home::{active_profile_name, profile_dir};
use harw_protocol::SessionEvent;
use harw_runtime::{EntryKind, ModelSource, RuntimeAssembly, RuntimeSpec, RuntimeStores};
use harw_session_store::TranscriptStore;
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};
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

/// Baut den vertrauenswürdigen [`Principal`] eines lokalen Aufrufers.
///
/// # Beschreibung
/// Deckt die beiden lokalen Eingangsflächen ab, an denen `harw` selbst den
/// Prozess-Eigentümer authentifiziert: [`IngressSurface::Tui`] und
/// [`IngressSurface::Cli`] (Vertrag `docs/remediation/CONTRACTS-W2d2.md`
/// §1.3, E4: lokaler Principal Tier [`PermissionTier::Operator`]). Die
/// Kennung ist die vom Kernel bezeugte Prozess-UID
/// (`rustix::process::getuid`, dasselbe Muster wie
/// `crate::runtime_web::web_principal` für den Web-Einstieg,
/// `harw-cli/src/web.rs:198`) — nie ein Wert aus Konfiguration oder
/// Modelltext.
///
/// **Andere Eingangsflächen:** diese Funktion baut auch für jede andere
/// [`IngressSurface`] anstandslos einen Principal (kein `panic!`) — sie
/// prüft `surface` nicht gegen eine Zulassungsliste. Sinnvoll ist das
/// Ergebnis aber nur für `Tui`/`Cli`: alle anderen Flächen (Web, Mcp,
/// Telegram, Gateway, JobWorker, Child) haben eigene, an ihre
/// Vertrauensgrenze angepasste Principal-Konstruktoren
/// (`crate::runtime_web::web_principal`,
/// `crate::runtime_gateway::channel_principal`, `job_principal`, …) und
/// dürfen diese Funktion nicht für sich verwenden. Aufrufer sind dafür
/// verantwortlich, nur `Tui` oder `Cli` zu übergeben.
///
/// # Argumente
/// - `surface` ([`IngressSurface`]): die lokale Eingangsfläche des Laufs;
///   praktisch immer `Tui` oder `Cli`.
///
/// # Rückgabe
/// Ein [`Principal`] mit [`PrincipalKind::Human`], Kennung `"uid:<uid>"`
/// und Stufe [`PermissionTier::Operator`].
#[must_use]
pub(crate) fn local_principal(surface: IngressSurface) -> Principal {
    let uid = rustix::process::getuid().as_raw();
    Principal::trusted_ingress(
        PrincipalKind::Human,
        format!("uid:{uid}"),
        surface,
        PermissionTier::Operator,
    )
}

/// Öffnet — falls konfiguriert — den versiegelten Secret-Resolver für einen
/// lokalen Einstieg.
///
/// # Beschreibung
/// Dünner Wrapper um
/// `crate::secret_store::open_configured_secret_resolver`, der das
/// konkrete `ConfiguredSecretResolver` auf dasselbe Trait-Objekt castet wie
/// `harw-cli/src/gateway.rs::open_gateway_secret_resolver`
/// (`Arc::new(resolver) as Arc<dyn harw_provider_http::SecretResolver + Send + Sync>`),
/// damit lokale Einstiege (TUI, One-Shot, Doctor) denselben Vertrag
/// nutzen können, ohne den Gateway-spezifischen Fehlerpräfix `"gateway: "`
/// zu erben.
///
/// # Argumente
/// - `home` (`&Path`): aufgelöster Root-Space (`~/.harw` bzw. `HARW_HOME`).
/// - `config` (`&ResolvedConfig`): die bereits aufgelöste Konfiguration des
///   Laufs (z. B. aus `harw_runtime::load_config`).
///
/// # Rückgabe
/// `Some(resolver)`, wenn ein aktivierter Provider eine `secrets:`-Referenz
/// nutzt und der versiegelte Speicher geöffnet werden konnte; `None`, wenn
/// kein aktivierter Provider `secrets:` nutzt.
///
/// # Fehler
/// `Err(String)` ohne Geheimnisinhalt — fehlendes KEK, nicht ladbares
/// KEK-Material oder ein nicht zu öffnender versiegelter Speicher (siehe
/// `crate::secret_store::open_configured_secret_resolver`).
pub(crate) fn configured_secret_resolver(
    home: &Path,
    config: &ResolvedConfig,
) -> Result<Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>, String> {
    let resolver = crate::secret_store::open_configured_secret_resolver(home, config)?;
    Ok(resolver
        .map(|resolver| Arc::new(resolver) as Arc<dyn harw_provider_http::SecretResolver + Send + Sync>))
}

/// Montiert die Runtime für `harw doctor`.
///
/// # Beschreibung
/// [`EntryKind::Doctor`] führt laut Reduktionstabelle keine Modell-Turns
/// aus; die Montage trägt deshalb ausschließlich ein
/// [`ModelSource::Echo`]-Root-Modell (`"doctor"`, nie aufgerufen) und einen
/// flüchtigen [`InMemoryStateStore`] — ein Doctor-Lauf hinterlässt keinen
/// durablen Verlauf. Weder Jobs noch durable Freigaben: `job_store` und
/// `approval_store` bleiben `None`. Principal über
/// [`local_principal`]`(`[`IngressSurface::Cli`]`)`, da `harw doctor` ein
/// nicht-interaktiver CLI-Aufruf ist. Baut über [`build_assembly`], ohne
/// Sitzungsereignis-Kanal (`session_events: None`) — passend zu
/// [`EntryKind::Doctor`]s `SpawnerPolicy::None`.
///
/// # Argumente
/// - `home` (`&Path`): aufgelöster Root-Space.
/// - `cwd` (`&Path`): Arbeitsverzeichnis des Aufrufs.
///
/// # Rückgabe
/// Die fertig montierte [`RuntimeAssembly`] für Doctor-Nachweise
/// (`rights_snapshot`, `spawn_context`, …).
///
/// # Fehler
/// `Err(String)` mit Präfix `"doctor: "` und der `Display`-Form von
/// [`harw_runtime::RuntimeError`], wenn der Bau in einer der
/// Montagephasen scheitert (Konfiguration, Vertrauen, Projekterkennung,
/// Sandbox, Registry, Speicher).
pub(crate) fn doctor_assembly(home: &Path, cwd: &Path) -> Result<RuntimeAssembly, String> {
    let principal = local_principal(IngressSurface::Cli);
    let spec = runtime_spec(EntryKind::Doctor, home, cwd, principal);
    let stores = RuntimeStores {
        state_store: Arc::new(InMemoryStateStore::new()),
        job_store: None,
        approval_store: None,
    };
    build_assembly(spec, ModelSource::Echo("doctor".to_owned()), stores, None)
        .map_err(|error| format!("doctor: {error}"))
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

    #[test]
    fn test_local_principal_cli_sets_human_operator_tier_and_kernel_uid() {
        let principal = local_principal(IngressSurface::Cli);

        assert_eq!(principal.kind(), PrincipalKind::Human);
        assert_eq!(principal.surface(), IngressSurface::Cli);
        assert_eq!(principal.tier(), PermissionTier::Operator);
        let id = principal.id();
        let raw_uid = id.strip_prefix("uid:").expect(
            "local_principal id must carry the kernel-witnessed uid under the 'uid:' prefix",
        );
        assert!(
            raw_uid.parse::<u32>().is_ok(),
            "expected a numeric uid suffix, got '{id}'"
        );
    }

    #[test]
    fn test_local_principal_tui_uses_the_same_uid_as_cli() {
        let cli = local_principal(IngressSurface::Cli);
        let tui = local_principal(IngressSurface::Tui);

        assert_eq!(cli.id(), tui.id());
        assert_eq!(tui.surface(), IngressSurface::Tui);
        assert_eq!(tui.tier(), PermissionTier::Operator);
    }

    #[test]
    fn test_configured_secret_resolver_without_sealed_provider_returns_none() {
        let config = ResolvedConfig::default();
        let home = TempDir::new().expect("home tempdir");

        let resolver = configured_secret_resolver(home.path(), &config)
            .expect("no sealed provider must not require a KEK");

        assert!(resolver.is_none());
    }

    #[test]
    fn test_doctor_assembly_succeeds_with_empty_home() {
        let home = TempDir::new().expect("home tempdir");
        let cwd = TempDir::new().expect("cwd tempdir");

        let assembly = doctor_assembly(home.path(), cwd.path())
            .expect("doctor assembly must build against an empty temp home");

        let snapshot = assembly.rights_snapshot();
        assert_eq!(snapshot.entry, EntryKind::Doctor);
        assert_eq!(snapshot.principal.surface(), IngressSurface::Cli);
        assert_eq!(snapshot.principal.tier(), PermissionTier::Operator);
    }
}
