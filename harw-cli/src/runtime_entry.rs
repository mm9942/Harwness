//! Composition-Root-Helfer für `harw`-Einstiege über [`harw_runtime`].
//!
//! # Beschreibung
//! Sieben kleine, seiteneffektarme Bausteine, aus denen ein Einstieg (TUI,
//! One-Shot, Gateway, Job-Worker, Doctor, …) seinen Aufruf von
//! [`harw_runtime::RuntimeAssembly::builder`] zusammensetzt, statt die
//! Montage-Reihenfolge (Spec → Sitzungswurzel → Speicher → Modell → Bau) an
//! jeder Aufrufstelle erneut hinzuschreiben (Vertrag:
//! `docs/design/runtime-contracts.md` §1.3):
//!
//! - [`runtime_spec`] — die Eingangsbeschreibung eines Laufs ohne Overrides.
//! - [`profile_sessions_root`] — das Transkriptverzeichnis des aktiven Profils
//!   (`[session].store_dir`, `[session].journal_format`), angelegt bei Bedarf.
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

use harw_config::{ResolvedConfig, SessionSection};
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
/// `mode_override`, `active_agent`, `reasoning_effort`, `approval_override`
/// und `model_override` bleiben `None`: sie sind Sache des Einstiegs
/// (`--agent`, `--effort`, `--approval`, `--model`, expliziter Modus), nicht
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
        approval_override: None,
        model_override: None,
    }
}

/// Einziges vom [`TranscriptStore`] unterstütztes Journal-Format
/// (`[session].journal_format`): eine JSON-Zeile je Datensatz.
const SUPPORTED_JOURNAL_FORMAT: &str = "jsonl";

/// Löst das Transkriptverzeichnis des aktiven Profils auf und legt es an.
///
/// # Beschreibung
/// Das aktive Profil folgt `HARW_PROFILE`/der `active_profile`-Zeigerdatei
/// ([`active_profile_name`]); der Profilordner selbst folgt
/// [`profile_dir`]. Der Speicherort kommt aus `[session].store_dir` der
/// vertrauten Home- und Profil-Layer ([`session_store_settings`]):
/// ein absoluter Pfad wird unverändert übernommen, ein relativer gegen den
/// Profilordner (innerhalb des HARW-Homes) aufgelöst — der Default
/// `"sessions"` ergibt damit wie bisher `<profil>/sessions`.
/// `[session].journal_format` wird geprüft: der [`TranscriptStore`] kennt
/// nur `"jsonl"`; jeder andere Wert erzeugt eine `tracing::warn!`-Zeile und
/// es wird trotzdem JSONL geschrieben. Das Verzeichnis wird über
/// [`std::fs::create_dir_all`] angelegt, falls es noch nicht existiert —
/// ohne durables Transkriptverzeichnis gibt es keinen [`StateStore`] für
/// diesen Lauf.
///
/// # Argumente
/// - `home` (`&Path`): aufgelöster Root-Space.
///
/// # Rückgabe
/// Den angelegten Pfad (Default `<profil>/sessions`).
///
/// # Fehler
/// `Err(String)`, wenn der Profilname ungültig ist
/// ([`harw_home::HomeError`]) oder das Verzeichnis nicht angelegt werden
/// kann. Der Fehlertext nennt den Pfad, aber keine Geheimnisse — es gibt
/// hier keine. Eine nicht ladbare Konfiguration ist hier **kein** Fehler:
/// sie wird gewarnt und der Default verwendet (die eigentliche
/// Konfigurationsprüfung meldet sie beim Montieren der Runtime).
pub(crate) fn profile_sessions_root(home: &Path) -> Result<PathBuf, String> {
    let profile = active_profile_name(home);
    let dir = profile_dir(home, &profile).map_err(|error| {
        format!("could not resolve the profile directory for profile '{profile}': {error}")
    })?;
    let settings = session_store_settings(home, &dir);
    let sessions_root = resolve_store_dir(&dir, &settings.store_dir);
    check_journal_format(&settings.journal_format);
    std::fs::create_dir_all(&sessions_root).map_err(|error| {
        format!(
            "could not create the sessions directory at '{}': {error}",
            sessions_root.display()
        )
    })?;
    Ok(sessions_root)
}

/// Liest `[session]` aus den vertrauten Home- und Profil-Layern.
///
/// # Beschreibung
/// Lädt ausschließlich `home` und `profile_dir` über
/// [`harw_config::discover_config`] — kein Repo-Layer, denn `[session]` ist
/// profilbezogen und wird aus nicht vertrauten Repos nie übernommen.
/// Scheitert das Laden, wird gewarnt und [`SessionSection::default`]
/// zurückgegeben.
///
/// # Argumente
/// - `home` (`&Path`): aufgelöster Root-Space.
/// - `profile_dir` (`&Path`): Ordner des aktiven Profils.
///
/// # Rückgabe
/// Die wirksame [`SessionSection`].
fn session_store_settings(home: &Path, profile_dir: &Path) -> SessionSection {
    let layers = [home.to_path_buf(), profile_dir.to_path_buf()];
    match harw_config::discover_config(&layers) {
        Ok(config) => config.harness.session,
        Err(error) => {
            tracing::warn!(
                %error,
                "session store: configuration not loadable, using default [session] settings"
            );
            SessionSection::default()
        }
    }
}

/// Löst `[session].store_dir` gegen den Profilordner auf.
///
/// # Argumente
/// - `profile_dir` (`&Path`): Ordner des aktiven Profils.
/// - `store_dir` (`&str`): konfigurierter Wert; leer (nach `trim`) bedeutet
///   Default `"sessions"`.
///
/// # Rückgabe
/// `store_dir` selbst, wenn absolut; sonst `profile_dir.join(store_dir)`.
fn resolve_store_dir(profile_dir: &Path, store_dir: &str) -> PathBuf {
    let trimmed = store_dir.trim();
    if trimmed.is_empty() {
        tracing::warn!("session store: empty [session].store_dir, using 'sessions'");
        return profile_dir.join("sessions");
    }
    let configured = Path::new(trimmed);
    if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        profile_dir.join(configured)
    }
}

/// Prüft `[session].journal_format` gegen das einzige unterstützte Format.
///
/// # Beschreibung
/// Groß-/Kleinschreibung und umgebende Leerzeichen werden ignoriert. Jeder
/// andere Wert als `"jsonl"` erzeugt eine Warnung; der Lauf schreibt
/// trotzdem JSONL.
///
/// # Rückgabe
/// `true`, wenn das Format unterstützt wird.
fn check_journal_format(journal_format: &str) -> bool {
    let supported = journal_format
        .trim()
        .eq_ignore_ascii_case(SUPPORTED_JOURNAL_FORMAT);
    if !supported {
        tracing::warn!(
            journal_format = %journal_format,
            supported = SUPPORTED_JOURNAL_FORMAT,
            "session store: unsupported [session].journal_format, writing jsonl instead"
        );
    }
    supported
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
/// [`IngressSurface::Cli`] (Vertrag `docs/design/runtime-contracts.md`
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
/// `crate::secret_store::open_configured_secret_resolver_for_active_provider`,
/// der das konkrete `ConfiguredSecretResolver` auf dasselbe Trait-Objekt
/// castet wie
/// `harw-cli/src/gateway.rs::open_gateway_secret_resolver`
/// (`Arc::new(resolver) as Arc<dyn harw_provider_http::SecretResolver + Send + Sync>`),
/// damit lokale Einstiege (TUI, One-Shot, Doctor) denselben Vertrag
/// nutzen können, ohne den Gateway-spezifischen Fehlerpräfix `"gateway: "`
/// zu erben.
///
/// Anders als der Gateway-Pfad wertet dieser Wrapper die KEK-Pflicht
/// **verzögert** aus — nur für `config.harness.default_provider`, den
/// Provider, den `ModelSource::Configured` für diesen Lauf tatsächlich
/// verwendet (`crate::model::build_root_model_with_resolver`). Ein anderer,
/// aktivierter Provider mit `secrets:`-Referenz, den dieser Lauf nie
/// anspricht, verlangt dadurch kein KEK mehr — dieselbe Nicht-Fatal-Haltung
/// wie hängende Katalog-Referenzen
/// (`harw_config::ResolvedConfig::compute_diagnostics`).
///
/// # Argumente
/// - `home` (`&Path`): aufgelöster Root-Space (`~/.harw` bzw. `HARW_HOME`).
/// - `config` (`&ResolvedConfig`): die bereits aufgelöste Konfiguration des
///   Laufs (z. B. aus `harw_runtime::load_config`).
///
/// # Rückgabe
/// `Some(resolver)`, wenn der tatsächlich verwendete Provider
/// (`default_provider`) eine `secrets:`-Referenz nutzt und der versiegelte
/// Speicher geöffnet werden konnte; `None` sonst — auch dann, wenn ein
/// *anderer*, von diesem Lauf nicht verwendeter Provider `secrets:` nutzen
/// würde.
///
/// # Fehler
/// `Err(String)` ohne Geheimnisinhalt — fehlendes KEK, nicht ladbares
/// KEK-Material oder ein nicht zu öffnender versiegelter Speicher (siehe
/// `crate::secret_store::open_configured_secret_resolver_for_active_provider`).
pub(crate) fn configured_secret_resolver(
    home: &Path,
    config: &ResolvedConfig,
) -> Result<Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>, String> {
    let resolver =
        crate::secret_store::open_configured_secret_resolver_for_active_provider(home, config)?;
    // `doc.read_pdf` (docs/design/doc_read_pdf_design.md §W4): dieser Wrapper
    // ist der eine Ort, an dem sowohl `chat`/TUI (`chat.rs`) als auch `harw
    // analyze` (`main.rs::cmd_analyze`) ihren Secret-Resolver für einen
    // konfigurierten Provider öffnen; installiert hier statt an jeder
    // Aufrufstelle einzeln, ohne die Laufzeitpfade sonst zu ändern.
    crate::doc_ocr::install_doc_ocr(
        config,
        Some(home),
        resolver
            .as_ref()
            .map(|resolver| resolver as &dyn harw_provider_http::SecretResolver),
    );
    Ok(resolver.map(|resolver| {
        Arc::new(resolver) as Arc<dyn harw_provider_http::SecretResolver + Send + Sync>
    }))
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
    use crate::test_support::{TestError, TestResult, ctx};
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
        assert_eq!(spec.approval_override, None);
        assert_eq!(spec.model_override, None);
    }

    #[test]
    fn test_profile_sessions_root_creates_directory() -> TestResult {
        let home = TempDir::new().map_err(ctx("home tempdir"))?;

        let sessions_root = profile_sessions_root(home.path())
            .map_err(ctx("sessions root resolves and is created"))?;

        assert!(sessions_root.is_dir(), "{}", sessions_root.display());
        assert!(sessions_root.ends_with("sessions"));
        assert!(sessions_root.starts_with(home.path()));
        Ok(())
    }

    #[test]
    fn test_profile_sessions_root_honors_relative_store_dir() -> TestResult {
        let home = TempDir::new().map_err(ctx("home tempdir"))?;
        let profile = profile_dir(home.path(), &active_profile_name(home.path()))
            .map_err(ctx("profile dir"))?;
        std::fs::create_dir_all(&profile).map_err(ctx("create profile dir"))?;
        std::fs::write(
            profile.join("config.toml"),
            "[session]\nstore_dir = \"journal/custom\"\n",
        )
        .map_err(ctx("write profile config"))?;

        let sessions_root = profile_sessions_root(home.path())
            .map_err(ctx("sessions root resolves and is created"))?;

        assert_eq!(sessions_root, profile.join("journal").join("custom"));
        assert!(sessions_root.is_dir(), "{}", sessions_root.display());
        Ok(())
    }

    #[test]
    fn test_profile_sessions_root_honors_absolute_store_dir() -> TestResult {
        let home = TempDir::new().map_err(ctx("home tempdir"))?;
        let target = TempDir::new().map_err(ctx("target tempdir"))?;
        let absolute = target.path().join("abs-sessions");
        std::fs::write(
            home.path().join("config.toml"),
            format!("[session]\nstore_dir = '{}'\n", absolute.display()),
        )
        .map_err(ctx("write home config"))?;

        let sessions_root = profile_sessions_root(home.path())
            .map_err(ctx("sessions root resolves and is created"))?;

        assert_eq!(sessions_root, absolute);
        assert!(sessions_root.is_dir(), "{}", sessions_root.display());
        Ok(())
    }

    #[test]
    fn test_resolve_store_dir_empty_falls_back_to_sessions() {
        let profile = PathBuf::from("/does/not/exist/profile");

        assert_eq!(resolve_store_dir(&profile, "  "), profile.join("sessions"));
        assert_eq!(
            resolve_store_dir(&profile, "sessions"),
            profile.join("sessions")
        );
    }

    #[test]
    fn test_check_journal_format_accepts_only_jsonl() {
        assert!(check_journal_format("jsonl"));
        assert!(check_journal_format(" JSONL "));
        assert!(!check_journal_format("sqlite"));
        assert!(!check_journal_format(""));
    }

    #[test]
    fn test_build_assembly_local_echo_succeeds() -> TestResult {
        let home = TempDir::new().map_err(ctx("home tempdir"))?;
        let cwd = TempDir::new().map_err(ctx("cwd tempdir"))?;

        let spec = runtime_spec(
            EntryKind::LocalEcho,
            home.path(),
            cwd.path(),
            test_principal(),
        );
        let sessions_root = profile_sessions_root(home.path())
            .map_err(ctx("sessions root resolves and is created"))?;
        let state_store = transcript_state_store(&sessions_root, test_thread_for_session);
        let stores = RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        };

        let assembly = build_assembly(spec, ModelSource::Echo("x".to_owned()), stores, None);

        assert!(assembly.is_ok(), "{:?}", assembly.err());
        Ok(())
    }

    #[test]
    fn test_local_principal_cli_sets_human_operator_tier_and_kernel_uid() -> TestResult {
        let principal = local_principal(IngressSurface::Cli);

        assert_eq!(principal.kind(), PrincipalKind::Human);
        assert_eq!(principal.surface(), IngressSurface::Cli);
        assert_eq!(principal.tier(), PermissionTier::Operator);
        let id = principal.id();
        let raw_uid = id.strip_prefix("uid:").ok_or(TestError::Missing(
            "local_principal id must carry the kernel-witnessed uid under the 'uid:' prefix",
        ))?;
        assert!(
            raw_uid.parse::<u32>().is_ok(),
            "expected a numeric uid suffix, got '{id}'"
        );
        Ok(())
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
    fn test_configured_secret_resolver_without_sealed_provider_returns_none() -> TestResult {
        let config = ResolvedConfig::default();
        let home = TempDir::new().map_err(ctx("home tempdir"))?;

        let resolver = configured_secret_resolver(home.path(), &config)
            .map_err(ctx("no sealed provider must not require a KEK"))?;

        assert!(resolver.is_none());
        Ok(())
    }

    /// F-046-Regression: ein *anderer*, von diesem Lauf nicht als
    /// `default_provider` gewählter Provider mit `secrets:`-Referenz darf
    /// kein KEK verlangen — sonst würde ein unbenutzter, versiegelter
    /// Provider jeden lokalen Einstieg blockieren.
    #[test]
    fn test_configured_secret_resolver_ignores_an_unused_sealed_provider_without_a_kek()
    -> TestResult {
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str::<harw_config::ProviderToml>(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .map_err(ctx("valid test provider"))?,
        );
        config.providers.insert(
            "plain".to_owned(),
            toml::from_str::<harw_config::ProviderToml>(
                "name = \"plain\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"env:PLAIN_TOKEN\"\n",
            )
            .map_err(ctx("valid test provider"))?,
        );
        config.harness.default_provider = Some("plain".to_owned());
        let home = TempDir::new().map_err(ctx("home tempdir"))?;

        let resolver = configured_secret_resolver(home.path(), &config)
            .map_err(ctx("an unused sealed provider must not require a KEK"))?;

        assert!(resolver.is_none());
        Ok(())
    }

    #[test]
    fn test_doctor_assembly_succeeds_with_empty_home() -> TestResult {
        let home = TempDir::new().map_err(ctx("home tempdir"))?;
        let cwd = TempDir::new().map_err(ctx("cwd tempdir"))?;

        let assembly = doctor_assembly(home.path(), cwd.path())
            .map_err(ctx("doctor assembly must build against an empty temp home"))?;

        let snapshot = assembly.rights_snapshot();
        assert_eq!(snapshot.entry, EntryKind::Doctor);
        assert_eq!(snapshot.principal.surface(), IngressSurface::Cli);
        assert_eq!(snapshot.principal.tier(), PermissionTier::Operator);
        Ok(())
    }
}
