//! Web-Einstieg auf die gemeinsame Runtime-Montage (`harw-runtime`).
//!
//! # Zweck
//! Schließt Befund F-045 (`docs/remediation/CONTRACTS.md` §runtime-spec,
//! `harw-runtime/src/sandbox.rs::permissions_for_tier`-Moduldoc): der
//! frühere `harw-cli/src/web.rs`-Pfad (vor W2d-1/B1) reichte jedes [`PermissionTier`] eines Peers unverändert an
//! [`harw_web::server::WebContextFactory`] weiter, ohne die Sandbox danach zu
//! verengen — ein `Observer`-Peer erhielt dieselbe Autorität wie ein `Owner`.
//! Dieses Modul liefert die eine Stelle, die
//!
//! - den vertrauenswürdigen [`Principal`] eines Web-Peers baut
//!   ([`web_principal`]),
//! - die Web-Runtime-Montage über den gemeinsamen Vertrag
//!   `crate::runtime_entry` und den Builder zusammensetzt ([`web_spec`],
//!   [`web_assembly`], optional mit Plan-Diensten), und
//! - die Anfrage-Sandbox eines Peers strikt auf sein Tier verengt
//!   ([`narrow_web_sandbox`]).
//!
//! # Verantwortung
//! Der Web-Einstieg führt laut Reduktionstabelle
//! ([`harw_runtime::EntryKind::Web`], `OperationSurface::CommandsOnly`) keine
//! Modell-Turns aus; [`web_assembly`] baut deshalb ausschließlich ein
//! [`harw_runtime::ModelSource::Echo`]-Root-Modell, das nie aufgerufen wird.
//! Die Wurzel-Sandbox eines Web-Laufs trägt bereits nur `{ReadWorkspace}`
//! (Profil-Obergrenze); [`narrow_web_sandbox`] schneidet sie zusätzlich mit
//! [`harw_runtime::permissions_for_tier`] auf das Tier des anfragenden Peers —
//! die Autorität kann dadurch nur sinken, nie steigen
//! ([`harw_sandbox::SandboxSpec::restrict`]).
//!
//! # Typen
//! Reine Komposition; dieses Modul definiert keine eigenen Datentypen,
//! sondern verkettet [`Principal`], [`harw_runtime::RuntimeSpec`],
//! [`harw_runtime::RuntimeStores`] und [`harw_sandbox::SandboxSpec`] aus den
//! Verträgen von `harw-types`, `harw-runtime` und `crate::runtime_entry`.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind seiteneffektfrei bis auf das Öffnen des
//! Transkript-Speichers in [`web_assembly`] (über
//! `crate::runtime_entry::transcript_state_store`); keine hält einen Lock
//! über eine `.await`-Grenze, da keine Funktion `async` ist.
//!
//! # Fehler
//! [`web_assembly`] meldet Fehler als `String`
//! (Vertrag von `crate::runtime_entry` bzw. Display von
//! [`harw_runtime::RuntimeError`]) — dieselbe Fehlerform, die
//! `harw-cli/src/web.rs` bereits für seine Kompositionsfehler verwendet.
//!
//! # Beispiel
//! ```no_run
//! use std::path::Path;
//! use harw_operations::operation::PermissionTier;
//!
//! # fn demo() -> Result<(), String> {
//! let root = harw_runtime::root_sandbox(
//!     harw_runtime::EntryKind::Web,
//!     Path::new("/home/mia/projects/harwness"),
//! )
//! .map_err(|error| error.to_string())?;
//! let sandbox = crate::runtime_web::narrow_web_sandbox(&root, PermissionTier::Observer);
//! assert!(sandbox.permissions().contains(harw_sandbox::Permission::ReadWorkspace));
//! # Ok(())
//! # }
//! ```

use std::path::Path;
use std::sync::Arc;

use harw_runtime::{
    EntryKind, ModelSource, PlanServices, RuntimeAssembly, RuntimeSpec, RuntimeStores,
};
use harw_sandbox::SandboxSpec;
use harw_session_store::ApprovalStore;
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind, SessionId, ThreadRef};

/// Baut den vertrauenswürdigen [`Principal`] eines Web-Peers.
///
/// # Description
/// Der Web-Einstieg authentifiziert einen Peer über `SO_PEERCRED`
/// (`harw-cli/src/web.rs`-Moduldoc, Abschnitt „`SO_PEERCRED`-Autorisierung"):
/// die vom Kernel gelieferte `uid` ist die einzige vertrauenswürdige Kennung,
/// nie ein Wert aus dem Anfragerumpf. `tier` stammt aus derselben
/// Autorisierungstabelle, die auch diese `uid` liefert.
///
/// # Arguments
/// - `uid` (`u32`): die vom Kernel bezeugte Prozess-UID des Peers
///   (`rustix::process::getuid`-Äquivalent auf Peer-Seite).
/// - `tier` ([`PermissionTier`]): die dieser `uid` zugeteilte Stufe.
///
/// # Returns
/// Ein [`Principal`] mit [`PrincipalKind::Human`] auf [`IngressSurface::Web`].
#[must_use]
pub(crate) fn web_principal(uid: u32, tier: PermissionTier) -> Principal {
    Principal::trusted_ingress(
        PrincipalKind::Human,
        format!("uid:{uid}"),
        IngressSurface::Web,
        tier,
    )
}

/// Leitet den Transkript-Thread einer Web-Sitzung deterministisch aus ihrer
/// [`SessionId`] ab.
///
/// # Description
/// Erfüllt den `SessionThreadMapper`-Vertrag
/// (`harw-core/src/state_store.rs`): dieselbe `SessionId` muss über
/// Prozessneustarts hinweg denselben [`ThreadRef`] liefern. Das
/// `web:`-Präfix trennt Web-Threads von TUI-/CLI-Threads im selben
/// Transkript-Speicher.
///
/// # Arguments
/// - `session_id` (`&SessionId`): die Kennung der Web-Sitzung.
///
/// # Returns
/// `ThreadRef("web:<session_id>")`.
fn web_thread_for_session(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("web:{}", session_id.as_str()))
}

/// Baut die Eingangsbeschreibung eines Web-Laufs.
///
/// # Description
/// [`crate::runtime_entry::runtime_spec`] mit [`EntryKind::Web`] und einem
/// [`PermissionTier::Owner`]-Principal für den Prozess-Eigentümer (die
/// Montage selbst trägt keine Tier-Verengung — die leistet erst
/// [`narrow_web_sandbox`] je Anfrage). Eine eigene Funktion, damit
/// `crate::web::serve_web` die Konfiguration über
/// [`harw_runtime::load_config`] für **dieselbe** Spec lädt, aus der
/// [`web_assembly`] später montiert.
///
/// # Arguments
/// - `home` (`&Path`): aufgelöster Root-Space (`~/.harw`).
/// - `cwd` (`&Path`): Arbeitsverzeichnis des Laufs.
/// - `uid` (`u32`): Prozess-UID des Server-Eigentümers.
///
/// # Returns
/// Eine [`RuntimeSpec`] ohne Overrides.
#[must_use]
pub(crate) fn web_spec(home: &Path, cwd: &Path, uid: u32) -> RuntimeSpec {
    crate::runtime_entry::runtime_spec(
        EntryKind::Web,
        home,
        cwd,
        web_principal(uid, PermissionTier::Owner),
    )
}

/// Baut die Runtime-Montage des Web-Einstiegs.
///
/// # Description
/// Nutzt den Builder direkt ([`RuntimeAssembly::builder`]) statt
/// [`crate::runtime_entry::build_assembly`], weil dieser Pfad optional
/// [`harw_runtime::RuntimeAssemblyBuilder::plan_services`] setzen muss und
/// `build_assembly` dafür keinen Parameter hat. Reihenfolge: [`web_spec`],
/// [`crate::runtime_entry::profile_sessions_root`],
/// [`crate::runtime_entry::transcript_state_store`], Modell, Speicher,
/// optional Plan-Dienste, Bau. Der Web-Einstieg führt laut
/// Reduktionstabelle (`OperationSurface::CommandsOnly`) keine Modell-Turns
/// aus; das Root-Modell ist deshalb ein [`ModelSource::Echo`], der nie
/// aufgerufen wird. Ohne Plan-Dienste registriert die Montage keine
/// Plan-Operationen.
///
/// # Arguments
/// - `home` (`&Path`): aufgelöster Root-Space (`~/.harw`).
/// - `cwd` (`&Path`): Arbeitsverzeichnis des Laufs.
/// - `uid` (`u32`): Prozess-UID des Server-Eigentümers, siehe [`web_spec`].
/// - `approval_store` (`Option<Arc<ApprovalStore>>`): durabler
///   Genehmigungsspeicher.
/// - `plan` (`Option<PlanServices>`): vollständig geöffnete Planungsfläche
///   oder `None`.
///
/// # Returns
/// Die fertig montierte [`RuntimeAssembly`] des Laufs.
///
/// # Errors
/// `String`, wenn der Sitzungs-Wurzelpfad nicht auflösbar ist
/// ([`crate::runtime_entry::profile_sessions_root`]) oder die Montage
/// scheitert (Display von [`harw_runtime::RuntimeError`]).
pub(crate) fn web_assembly(
    home: &Path,
    cwd: &Path,
    uid: u32,
    approval_store: Option<Arc<ApprovalStore>>,
    plan: Option<PlanServices>,
) -> Result<RuntimeAssembly, String> {
    let spec = web_spec(home, cwd, uid);
    let sessions_root = crate::runtime_entry::profile_sessions_root(home)?;
    let state_store =
        crate::runtime_entry::transcript_state_store(&sessions_root, web_thread_for_session);
    let stores = RuntimeStores {
        state_store,
        job_store: None,
        approval_store,
    };
    // Web-Einstieg ist CommandsOnly (CONTRACTS.md §runtime-spec-Tabelle):
    // der Root-Turn ruft nie ein Modell auf.
    let model = ModelSource::Echo("harw web führt keine Modell-Turns aus".to_owned());
    let mut builder = RuntimeAssembly::builder(spec).model(model).stores(stores);
    if let Some(plan) = plan {
        builder = builder.plan_services(plan);
    }
    builder.build().map_err(|error| error.to_string())
}

/// Verengt die Wurzel-Sandbox des Web-Einstiegs auf das Tier eines Peers.
///
/// # Description
/// Schließt F-045 (Moduldoc oben): [`harw_runtime::root_sandbox`] liefert die
/// Profil-Obergrenze `{ReadWorkspace}` für [`EntryKind::Web`];
/// [`harw_runtime::permissions_for_tier`] liefert die tier-abhängige weitere
/// Obergrenze, und [`SandboxSpec::restrict`] schneidet — Autorität wird nie
/// neu vergeben, nur weiter beschnitten. Infallibel: `crate::web` baut die
/// Wurzel-Sandbox einmal beim Start (Fehler dort brechen den Start ab) und
/// verengt sie je Anfrage hiermit, weil die `WebContextFactory` keinen
/// Fehler melden kann.
///
/// # Arguments
/// - `root` (`&SandboxSpec`): Wurzel-Sandbox aus
///   [`harw_runtime::root_sandbox`]`(EntryKind::Web, …)`.
/// - `tier` ([`PermissionTier`]): die dem anfragenden Peer zugeteilte Stufe.
///
/// # Returns
/// Eine [`SandboxSpec`], deren Rechte Teilmenge von `root` und von
/// [`harw_runtime::permissions_for_tier`]`(tier)` sind.
#[must_use]
pub(crate) fn narrow_web_sandbox(root: &SandboxSpec, tier: PermissionTier) -> SandboxSpec {
    root.restrict(&harw_runtime::permissions_for_tier(tier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_sandbox::Permission;

    const ALL_TIERS: [PermissionTier; 4] = [
        PermissionTier::Observer,
        PermissionTier::Operator,
        PermissionTier::Maintainer,
        PermissionTier::Owner,
    ];

    #[test]
    fn test_web_principal_is_human_web_with_given_tier() {
        for tier in ALL_TIERS {
            let principal = web_principal(1000, tier);
            assert_eq!(principal.kind(), PrincipalKind::Human);
            assert_eq!(principal.surface(), IngressSurface::Web);
            assert_eq!(principal.tier(), tier, "{tier:?}");
            assert_eq!(principal.id(), "uid:1000");
        }
    }

    fn web_root(dir: &Path) -> SandboxSpec {
        harw_runtime::root_sandbox(EntryKind::Web, dir).expect("temp dir binds as workspace root")
    }

    #[test]
    fn test_narrow_web_sandbox_observer_has_only_read_workspace() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let sandbox = narrow_web_sandbox(&web_root(dir.path()), PermissionTier::Observer);
        assert!(sandbox.permissions().contains(Permission::ReadWorkspace));
        assert!(!sandbox.permissions().contains(Permission::WriteWorkspace));
        assert!(!sandbox.permissions().contains(Permission::ExecuteProcess));
    }

    #[test]
    fn test_narrow_web_sandbox_never_exceeds_root_permissions() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = web_root(dir.path());
        for tier in ALL_TIERS {
            let narrowed = narrow_web_sandbox(&root, tier);
            assert!(
                narrowed.permissions().is_subset_of(root.permissions()),
                "{tier:?} exceeds the EntryKind::Web profile ceiling"
            );
            assert!(
                narrowed
                    .permissions()
                    .is_subset_of(&harw_runtime::permissions_for_tier(tier)),
                "{tier:?} exceeds its tier ceiling"
            );
        }
    }

    #[test]
    fn test_web_spec_is_web_entry_with_owner_principal() {
        let home = tempfile::TempDir::new().expect("temp home");
        let cwd = tempfile::TempDir::new().expect("temp cwd");
        let spec = web_spec(home.path(), cwd.path(), 1000);
        assert_eq!(spec.entry, EntryKind::Web);
        assert_eq!(spec.home, home.path());
        assert_eq!(spec.cwd, cwd.path());
        assert_eq!(spec.principal.tier(), PermissionTier::Owner);
        assert_eq!(spec.principal.id(), "uid:1000");
    }

    // Kein Netz, kein Provider: `ModelSource::Echo` und ein leeres
    // Temp-Home ohne `config.toml` (gleiche Annahme wie
    // `runtime_entry::tests::test_build_assembly_local_echo_succeeds`).
    #[test]
    fn test_web_assembly_without_home_config_builds_with_approval_store() {
        let home = tempfile::TempDir::new().expect("temp home");
        let cwd = tempfile::TempDir::new().expect("temp cwd");
        let store = Arc::new(ApprovalStore::new(home.path()));
        let assembly = web_assembly(
            home.path(),
            cwd.path(),
            1000,
            Some(Arc::clone(&store)),
            None,
        )
        .expect("web assembly builds from an empty home");
        let held = assembly.approval_store().expect("approval store is kept");
        assert!(Arc::ptr_eq(held, &store));
    }

    #[test]
    fn test_web_thread_for_session_prefixes_web() {
        let session = SessionId::try_from_str("abc-123").expect("valid session id");
        let thread = web_thread_for_session(&session);
        assert_eq!(thread.as_str(), "web:abc-123");
    }
}
