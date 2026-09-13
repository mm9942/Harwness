//! `harw web` — die Kompositionsstelle, die `harw-web`s bisher konsumentenlose
//! Unix-Socket-HTTP-Fläche tatsächlich an einen Prozess bindet.
//!
//! # Verantwortungsbereich
//! Dieses Modul schließt den Bottom-up-Befund „`harw-web` hat null
//! Produktionskonsumenten": kein Prozess band `BoundWebServer`/
//! `WebServerConfig`. [`serve_web`] ist der Konsument — aufgerufen aus
//! `cli::Command::Web` (`main.rs::dispatch`).
//!
//! # Eine Montage: `RuntimeAssembly`
//! Seit W2d-1/B1 montiert [`serve_web`] den Lauf ausschließlich über
//! [`crate::runtime_web::web_assembly`] (`harw-runtime`,
//! [`harw_runtime::EntryKind::Web`]). Konfiguration, Trust-Bericht,
//! Operations-Registry und Service-Map kommen aus dieser einen Montage;
//! [`harw_web::router::WebRouteTable::from_registry`] baut die Routentabelle
//! **ausschließlich** aus [`harw_runtime::RuntimeAssembly::operations`] — es
//! gibt in diesem Modul keine zweite Stelle, die Operationen zusammenstellt.
//! Die Konfiguration wird vertrauensbewusst über
//! [`harw_runtime::load_config`] für dieselbe Spec geladen
//! ([`crate::runtime_web::web_spec`]); die frühere Layer-Liste aus
//! `crate::resolve_serve_paths` wird nicht mehr gelesen — seit W2d-2/W1 nimmt
//! [`serve_web`] dafür überhaupt keinen Layer-Parameter mehr entgegen, es gibt
//! kein `--config-dir` für `harw web`.
//!
//! # Warum er nicht ungefragt läuft
//! `harw web` bindet nur, weil der Subcommand explizit aufgerufen wurde —
//! anders als `harw serve` (dessen MCP-Listener zusätzlich hinter
//! `[harness.mcp_listener] enabled` gated ist) trägt `harw-config` heute
//! kein Analogon für die Web-Fläche. Die Kompositionsstelle hier fügt keinen
//! zweiten, stillen Startpfad hinzu: [`serve_web`] wird ausschließlich aus
//! dem `Web`-Subcommand aufgerufen, nirgends sonst.
//!
//! # HARW-Home ist Pflicht
//! Ohne HARW-Home (externes `--config-dir` ohne `--home`) gibt es weder
//! einen Trust-Bericht noch einen Freigabespeicher; [`serve_web`] bricht dann
//! sofort mit einer Fehlermeldung ab, statt eine halbe Fläche ohne
//! `approval.*` zu starten.
//!
//! # Der Socket-Pfad
//! `harw-home` kennt **keinen** zentralen Pfadnamen für einen
//! Kontrollflächen-Socket; auch `harw-sentinel`s `sentinel.sock` ist eine
//! lokale Konstante in `harw-sentinel::cli`, kein Eintrag in
//! `harw_home::paths`. Dieses Modul folgt demselben Muster:
//! [`DEFAULT_SOCKET_FILE_NAME`] ist eine lokale Vorgabe (`<home>/web.sock`),
//! überschreibbar über `--socket`.
//!
//! # `SO_PEERCRED`-Autorisierung
//! Der aktuelle Prozess-Eigentümer (die eigene UID, über
//! `rustix::process::getuid`) bekommt [`PermissionTier::Owner`] — die
//! höchste Stufe; jede andere UID bleibt unautorisiert
//! ([`harw_web::authz::StaticUidTierMap::new`] ohne Rückfallwert). Der Socket
//! liegt unter dem privaten HARW-Home; ein Peer mit fremder UID, der ihn
//! trotzdem erreicht, bekommt `403 unknown_peer`.
//!
//! # `OpContext` je Aufruf — das Tier wirkt (F-045)
//! [`harw_web::server::WebContextFactory`] wird einmal pro angenommener
//! Verbindung aufgerufen, bekommt Peer **und** Tier und muss synchron (ohne
//! `Result`) einen frischen `OpContext` liefern. [`web_context_for`] baut ihn:
//!
//! - Die Service-Map ist [`harw_runtime::RuntimeServices::service_map`] für
//!   [`ServiceSurface::Web`] — dieselbe Map, die
//!   [`harw_runtime::RuntimeAssembly::op_context`] verwendet. Die Montage
//!   selbst kennt die verbindungsgebundenen Dienste nicht; darum legt diese
//!   Datei danach [`PeerCredentials`], den `ApprovalActorResolver`, den
//!   Freigabespeicher der Montage und einen tier-genauen
//!   [`harw_types::Principal`] ([`crate::runtime_web::web_principal`]) hinein.
//!   `ServiceMap::insert` überschreibt gleichen Typs — der Owner-Principal
//!   der Montage wird so durch den des Peers ersetzt, nie erweitert.
//! - Die Sandbox ist [`RuntimeAssembly::sandbox`] — dieselbe Web-Wurzel-Sandbox,
//!   die die Montage beim Bau **einmal** über [`harw_runtime::root_sandbox`]
//!   erzeugt (`harw-runtime/src/assembly.rs::RuntimeAssemblyBuilder::build`).
//!   Dieses Modul baut seit F-W/W4 **keine** zweite, eigene Bindung mehr — vor
//!   W4 rief [`serve_web`] `harw_runtime::root_sandbox(EntryKind::Web, &cwd)`
//!   selbst noch einmal auf und band dabei versehentlich an `cwd` statt an den
//!   von der Montage ermittelten Projekt-Wurzelpfad. Je Anfrage verengt
//!   [`crate::runtime_web::narrow_web_sandbox`] `assembly.sandbox()` auf das
//!   Tier des Peers. Ein `Observer`-Peer bekommt damit nur `ReadWorkspace` —
//!   und, weil die Web-Decke laut Reduktionstabelle ([`EntryKind::Web`],
//!   `docs/remediation/CONTRACTS.md:106`) für **alle** Tiers `{ReadWorkspace}`
//!   ist, bekommt selbst ein `Owner`-Peer nie mehr als das: das Tier verengt
//!   nur innerhalb dieser Decke, es hebt sie nie an. Eine Anhebung der Decke
//!   (z. B. Schreibrechte für `Owner` im Web) ist keine Entscheidung dieses
//!   Moduls, sondern bleibt W5/WB-COMP vorbehalten. Weil die Fabrik keinen
//!   Fehler melden kann, scheitert ein nicht bindbares Arbeitsverzeichnis
//!   weiterhin bereits beim Start und nicht erst je Anfrage — seit W4 nicht
//!   mehr über die separate `root_sandbox`-Zeile, sondern schon beim
//!   Montagebau ([`crate::runtime_web::web_assembly`]), der `sandbox()`
//!   liefert, bevor die Kontext-Fabrik überhaupt entsteht.
//!
//! # Woher der `ApprovalActor` kommt — und woher nicht
//! `approval.pending`/`approval.resolve` (`harw-ops::approval`) leiten den
//! [`harw_types::ApprovalActor`] ausschließlich aus zwei Services her, die
//! diese Datei in die `ServiceMap` jeder Verbindung legt: den
//! [`PeerCredentials`], die [`harw_web::server::WebContextFactory`] pro
//! angenommener Verbindung aus `SO_PEERCRED` liest (Kernel-verbürgt, siehe
//! `harw_web::peer`-Moduldoku), und einen `Arc<dyn ApprovalActorResolver>`,
//! der diese `uid` serverseitig auf einen `ApprovalActor` abbildet. Es gibt
//! **keinen** zweiten Weg, aus dem ein `ApprovalActor` entstehen könnte:
//! weder `ApprovalResolveArgs` noch diese Datei lesen ihn aus dem
//! Anfragerumpf.
//!
//! # Die Genehmiger-Tabelle — ohne Vorgabewert, wie [`StaticUidTierMap`]
//! [`StaticUidApprovalActorMap`] wird hier — genau wie [`StaticUidTierMap`]
//! — **an der Kompositionswurzel** aus der eigenen Prozess-`uid` gebaut, nie
//! aus dem Anfragerumpf, und bildet sie auf `ApprovalActor::Operator { id:
//! "owner" }` ab. Eine unbekannte `uid` bleibt `None`, nie ein generischer
//! Actor. Diese Datei erweitert die Tabelle bewusst nicht auf mehrere
//! Benutzer — das wäre eine Betriebsentscheidung, keine
//! Kompositionsentscheidung.
//!
//! # Der Genehmigungsspeicher
//! [`ApprovalStore::new`] ist infallibel und öffnet keine Datei sofort. Diese
//! Datei baut ihn aus dem (Pflicht-)Home und übergibt ihn der Montage
//! ([`harw_runtime::RuntimeStores::approval_store`]); je Verbindung landet
//! derselbe `Arc<ApprovalStore>` in der `ServiceMap`.
//!
//! # `harw-dod`: kein Konsument hier
//! `harw-cli` braucht `harw-dod` nicht. Die Fassade bündelt Host-Sicherheits-
//! sensorik für einen Beobachtungsprozess — das ist `harw-sentinel`s
//! Aufgabe, nicht die eines CLI-Kontrollflächen-Servers.
//!
//! # Nebenläufigkeit
//! [`serve_web`] baut eine einthreadige `tokio`-Runtime (wie
//! `main::serve_mcp`) und läuft darin bis `BoundWebServer::serve` endet.
//! Die `RuntimeAssembly` liegt in einem `Arc` und wird per `Arc::clone` in
//! die Kontext-Fabrik gereicht; jede Verbindung bedient `harw-web` in einer
//! eigenen Task.
//!
//! # Fehler
//! Ein `String` bei fehlendem Home, Config-/Trust-Fehlern
//! ([`harw_runtime::RuntimeError`]), Montage- oder Sandbox-Fehlern,
//! Routenfehlern ([`harw_web::error::WebError`]) oder Bindefehlern.

use std::path::PathBuf;
use std::sync::Arc;

use harw_operations::context::{OpContext, ServiceMap};
use harw_operations::operation::PermissionTier;
use harw_runtime::{EntryKind, RuntimeAssembly, ServiceSurface};
use harw_sandbox::SandboxSpec;
use harw_session_store::approval::ApprovalStore;
use harw_types::{SessionId, TurnId};
use harw_web::events::WebEventBus;
use harw_web::router::WebRouteTable;
use harw_web::security::{ApprovalActorResolver, StaticUidApprovalActorMap};
use harw_web::server::{BoundWebServer, WebServerConfig};
use harw_web::{PeerCredentials, StaticUidTierMap};

/// Dateiname des Web-Sockets unterhalb des HARW-Home, wenn `--socket` fehlt.
///
/// Lokale Vorgabe dieses Moduls, kein Eintrag in `harw_home::paths` — siehe
/// Moduldoc, Abschnitt „Der Socket-Pfad".
const DEFAULT_SOCKET_FILE_NAME: &str = "web.sock";

/// Kapazität des SSE-Ereignisbusses (`GET /events`).
///
/// Groß genug für einen kurzzeitigen Nachzügler-Client, ohne unbegrenzt zu
/// puffern — derselbe Wert wie `harw_mcp_server::McpEventBus`s
/// `with_default_capacity`-Vorgabe.
const EVENT_BUS_CAPACITY: usize = 64;

/// Startet `harw-web` auf einem Unix-Socket und bedient Verbindungen, bis der
/// Prozess endet.
///
/// # Arguments
/// - `home` (`Option<PathBuf>`): das aufgelöste HARW-Home. **Pflicht** —
///   `None` ist ein Fehler (siehe Moduldoc, Abschnitt „HARW-Home ist Pflicht").
///   Es gibt keinen `--config-dir`-Ersatz: ohne `--home`/`HARW_HOME` bricht
///   `harw web` sofort ab.
/// - `socket_override` (`Option<PathBuf>`): `--socket`; hat Vorrang vor
///   `<home>/web.sock`.
///
/// # Returns
/// `Ok(())`, wenn die Annahmeschleife regulär endet (siehe
/// [`harw_web::server::BoundWebServer::serve`]).
///
/// # Errors
/// Ein `String`, wenn `home` fehlt, das Arbeitsverzeichnis nicht lesbar ist,
/// die Konfiguration nicht vertrauensbewusst geladen werden kann, die
/// Planungsfläche nicht öffnet, die Montage oder die Web-Wurzel-Sandbox
/// scheitert, die Routentabelle einen Fehler meldet oder der Server nicht
/// binden kann.
///
/// # Concurrency
/// Baut eine eigene einthreadige `tokio`-Runtime, siehe Moduldoc.
pub(crate) fn serve_web(
    home: Option<PathBuf>,
    socket_override: Option<PathBuf>,
) -> Result<(), String> {
    let home = home.ok_or_else(|| {
        "harw web benötigt ein HARW-Home (--home); ohne Home gibt es weder Trust-Bericht noch Freigabespeicher"
            .to_owned()
    })?;
    let socket_path = socket_override.unwrap_or_else(|| home.join(DEFAULT_SOCKET_FILE_NAME));
    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    let uid = rustix::process::getuid().as_raw();

    // Dieselbe Spec, aus der `web_assembly` montiert: die Plan-Dienste sind
    // eine Builder-Eingabe und brauchen deshalb die Konfiguration vorab.
    let (config, trust) =
        harw_runtime::load_config(&crate::runtime_web::web_spec(&home, &cwd, uid))
            .map_err(|error| error.to_string())?;
    tracing::info!(
        layers = trust.layers.len(),
        untrusted_repo = trust.has_untrusted_repo(),
        "web.config.loaded"
    );

    let plan_config = crate::plan_tool_config_from_section(&config.harness.tools.plan)?;
    let plan = runtime_plan_services(crate::build_plan_services(
        &home,
        &plan_config,
        crate::DEFAULT_PLAN_SPACE,
        crate::DEFAULT_GOAL_SPACE,
    )?);

    // `ApprovalStore::new` hängt sein eigenes `approvals`-Unterverzeichnis an
    // (siehe Moduldoc, Abschnitt „Der Genehmigungsspeicher").
    let approval_store = Some(Arc::new(ApprovalStore::new(&home)));

    let assembly = Arc::new(crate::runtime_web::web_assembly(
        &home,
        &cwd,
        uid,
        approval_store,
        plan,
    )?);
    // `operations()` liefert `&Arc<OperationRegistry>`; Deref-Koerzion auf
    // `&OperationRegistry` für `from_registry`.
    let routes = WebRouteTable::from_registry(assembly.operations())
        .map_err(|error| error.to_string())?;
    tracing::info!(
        total = assembly.operations().len(),
        "web.registry.assembled"
    );

    let authorizer: Arc<dyn harw_web::PeerAuthorizer> =
        Arc::new(StaticUidTierMap::new(vec![(uid, PermissionTier::Owner)]));

    // Dieselbe `uid`, die oben `PermissionTier::Owner` bekommt, wird hier auf
    // genau einen Genehmiger abgebildet — ohne Rückfallwert für jede andere
    // `uid` (siehe Moduldoc, Abschnitt „Die Genehmiger-Tabelle").
    let approver: Arc<dyn ApprovalActorResolver> =
        Arc::new(StaticUidApprovalActorMap::new(vec![(uid, "owner".to_owned())]));

    // F-W/W4: aus der Montage übernommen statt ein zweites Mal gebaut.
    // `assembly.sandbox()` ist dieselbe Web-Wurzel-Sandbox, die
    // `RuntimeAssembly::builder(..).build()` oben bereits über
    // `harw_runtime::root_sandbox(EntryKind::Web, &project.project_root)`
    // erzeugt hat (`harw-runtime/src/assembly.rs:495`) — die vorherige, hier
    // erneut an `cwd` statt an den Projekt-Wurzelpfad gebundene Zeile entfällt.
    // Ein nicht bindbares Arbeitsverzeichnis bricht deshalb weiterhin beim
    // Start ab, jetzt aber schon durch den Montagebau (`web_assembly(..)?`
    // oben), nicht mehr durch eine eigene Zeile hier — die Kontext-Fabrik
    // unten kann je Anfrage ohnehin keinen Fehler melden.
    let root_sandbox = assembly.sandbox().clone();

    let factory_assembly = Arc::clone(&assembly);
    let context_factory: Arc<harw_web::server::WebContextFactory> =
        Arc::new(move |peer: &PeerCredentials, tier: PermissionTier| {
            web_context_for(&factory_assembly, &root_sandbox, &approver, *peer, tier)
        });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start web runtime: {error}"))?;
    runtime.block_on(async move {
        let events = Arc::new(WebEventBus::new(EVENT_BUS_CAPACITY).map_err(|error| error.to_string())?);
        let server = BoundWebServer::bind(
            WebServerConfig {
                socket_path: socket_path.clone(),
            },
            routes,
            authorizer,
            context_factory,
            events,
        )
        .await
        .map_err(|error| error.to_string())?;
        eprintln!("harw web listening on unix:{}", socket_path.display());
        server.serve().await.map_err(|error| error.to_string())
    })
}

/// Übersetzt die crate-eigene Planungsfläche in [`harw_runtime::PlanServices`].
///
/// # Description
/// `crate::build_plan_services` trägt Plan-, Goal- und Finding-Store als
/// einzelne `Option`s (abgeschaltete Planungsfläche ⇒ alle `None`). Die
/// Montage nimmt nur eine vollständige Fläche an: alle drei oder keiner.
///
/// # Arguments
/// - `services` (`crate::PlanServices`): Ergebnis von
///   `crate::build_plan_services`; wird verbraucht.
///
/// # Returns
/// `Some`, wenn Plan, Goal **und** Findings vorhanden sind, sonst `None`.
fn runtime_plan_services(services: crate::PlanServices) -> Option<harw_runtime::PlanServices> {
    let crate::PlanServices {
        plan,
        goal,
        findings,
        config,
        ..
    } = services;
    match (plan, goal, findings) {
        (Some(plan), Some(goal), Some(findings)) => Some(harw_runtime::PlanServices {
            plan,
            goal,
            findings,
            plan_config: config,
        }),
        _ => None,
    }
}

/// Baut den [`OpContext`] einer freigegebenen Web-Anfrage aus der Montage.
///
/// # Description
/// Aufgerufen aus der [`harw_web::server::WebContextFactory`] — synchron,
/// einmal pro angenommener Verbindung. Holt die
/// [`ServiceSurface::Web`]-Service-Map der Montage (dieselbe wie in
/// [`RuntimeAssembly::op_context`]) und ergänzt sie über [`web_op_context`]
/// um die verbindungsgebundenen Dienste und die tier-verengte Sandbox.
///
/// # Arguments
/// - `assembly` (`&RuntimeAssembly`): die einmal beim Start gebaute Montage.
/// - `root_sandbox` (`&SandboxSpec`): Web-Wurzel-Sandbox aus
///   [`harw_runtime::root_sandbox`].
/// - `approver` (`&Arc<dyn ApprovalActorResolver>`): uid-basierte
///   Genehmiger-Tabelle ohne Rückfallwert.
/// - `peer` (`PeerCredentials`): Kernel-verbürgte Identität dieser Verbindung.
/// - `tier` ([`PermissionTier`]): vom `PeerAuthorizer` zugeteilte Stufe.
///
/// # Returns
/// Einen frischen [`OpContext`] mit neuer [`SessionId`]/[`TurnId`].
///
/// # Concurrency
/// Rein synchron; klont nur `Arc`-Zeiger und baut eine neue `ServiceMap`.
fn web_context_for(
    assembly: &RuntimeAssembly,
    root_sandbox: &SandboxSpec,
    approver: &Arc<dyn ApprovalActorResolver>,
    peer: PeerCredentials,
    tier: PermissionTier,
) -> OpContext {
    web_op_context(
        assembly.services().service_map(ServiceSurface::Web),
        assembly.approval_store(),
        root_sandbox,
        approver,
        peer,
        tier,
    )
}

/// Ergänzt eine Service-Map um die verbindungsgebundenen Web-Dienste und baut
/// daraus den [`OpContext`].
///
/// # Description
/// Legt einen tier-genauen [`harw_types::Principal`]
/// ([`crate::runtime_web::web_principal`], überschreibt den Owner-Principal
/// der Montage), die [`PeerCredentials`], den `ApprovalActorResolver` und —
/// falls vorhanden — den Freigabespeicher in `services`; die Sandbox ist
/// `root_sandbox` verengt auf `tier` ([`crate::runtime_web::narrow_web_sandbox`]).
///
/// # Arguments
/// - `services` (`ServiceMap`): Basis-Map; wird verbraucht.
/// - `approval_store` (`Option<&Arc<ApprovalStore>>`): Freigabespeicher.
/// - `root_sandbox` (`&SandboxSpec`): Web-Wurzel-Sandbox.
/// - `approver` (`&Arc<dyn ApprovalActorResolver>`): Genehmiger-Tabelle.
/// - `peer` (`PeerCredentials`): Identität dieser Verbindung — vom Kernel,
///   nie aus dem Anfragerumpf.
/// - `tier` ([`PermissionTier`]): Stufe des Peers.
///
/// # Returns
/// Einen frischen [`OpContext`] mit neuer [`SessionId`]/[`TurnId`].
fn web_op_context(
    mut services: ServiceMap,
    approval_store: Option<&Arc<ApprovalStore>>,
    root_sandbox: &SandboxSpec,
    approver: &Arc<dyn ApprovalActorResolver>,
    peer: PeerCredentials,
    tier: PermissionTier,
) -> OpContext {
    services.insert(crate::runtime_web::web_principal(peer.uid, tier));
    services.insert(peer);
    services.insert(Arc::clone(approver));
    if let Some(approval_store) = approval_store {
        services.insert(Arc::clone(approval_store));
    }
    OpContext::new(
        SessionId::new(),
        TurnId::new(),
        crate::runtime_web::narrow_web_sandbox(root_sandbox, tier),
        services,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_operations::registry::OperationRegistry;
    use harw_sandbox::Permission;
    use harw_types::Principal;

    // Montage aus leerem Temp-Home ohne Netz/Provider (`ModelSource::Echo`).
    // Die `TempDir`s müssen so lange leben wie die Montage.
    fn test_assembly(
        with_approval_store: bool,
    ) -> (tempfile::TempDir, tempfile::TempDir, RuntimeAssembly) {
        let home = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let cwd = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let store = with_approval_store.then(|| Arc::new(ApprovalStore::new(home.path())));
        let assembly =
            match crate::runtime_web::web_assembly(home.path(), cwd.path(), 1000, store, None) {
                Ok(assembly) => assembly,
                Err(error) => panic!("web assembly: {error}"),
            };
        (home, cwd, assembly)
    }

    // Web-Wurzel-Sandbox über einem eigenen Temp-Verzeichnis.
    fn test_root_sandbox() -> (tempfile::TempDir, SandboxSpec) {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let sandbox = match harw_runtime::root_sandbox(EntryKind::Web, dir.path()) {
            Ok(sandbox) => sandbox,
            Err(error) => panic!("root sandbox: {error}"),
        };
        (dir, sandbox)
    }

    // Basis-Map wie früher `build_web_op_context`: nur die Registry.
    fn registry_map(registry: &OperationRegistry) -> ServiceMap {
        let mut fresh = OperationRegistry::new();
        for operation in registry.iter() {
            fresh.register(Arc::clone(operation));
        }
        let mut map = ServiceMap::new();
        map.insert(fresh);
        map
    }

    /// Ohne Freigabespeicher entsteht trotzdem ein gültiger Kontext mit
    /// Registry, Peer, Genehmiger und tier-genauem Principal aus der Montage.
    #[test]
    fn test_web_context_for_without_approval_store_still_builds_a_context() {
        let (_home, _cwd, assembly) = test_assembly(false);
        let (_root_dir, root) = test_root_sandbox();
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![]));
        let peer = PeerCredentials::new(1, 1, 1);

        let ctx = web_context_for(&assembly, &root, &approver, peer, PermissionTier::Owner);

        assert!(ctx.service::<OperationRegistry>().is_some());
        assert_eq!(ctx.service::<PeerCredentials>(), Some(&peer));
        assert!(ctx.service::<Arc<dyn ApprovalActorResolver>>().is_some());
        assert!(ctx.service::<Arc<ApprovalStore>>().is_none());
        let principal = ctx.service::<Principal>().expect("principal is registered");
        assert_eq!(principal.id(), "uid:1");
        assert_eq!(principal.tier(), PermissionTier::Owner);
    }

    /// F-045: ein `Observer`-Peer bekommt nur `ReadWorkspace` — und einen
    /// Observer-Principal, nicht den Owner-Principal der Montage.
    #[test]
    fn test_web_context_for_observer_sandbox_has_only_read_workspace() {
        let (_home, _cwd, assembly) = test_assembly(true);
        let (_root_dir, root) = test_root_sandbox();
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]));
        let peer = PeerCredentials::new(1, 1000, 1000);

        let ctx = web_context_for(&assembly, &root, &approver, peer, PermissionTier::Observer);

        let permissions = ctx.sandbox().permissions();
        assert!(permissions.contains(Permission::ReadWorkspace));
        assert!(!permissions.contains(Permission::WriteWorkspace));
        assert!(!permissions.contains(Permission::ExecuteProcess));
        assert!(permissions.is_subset_of(root.permissions()));
        assert!(ctx.service::<Arc<ApprovalStore>>().is_some());
        let principal = ctx.service::<Principal>().expect("principal is registered");
        assert_eq!(principal.tier(), PermissionTier::Observer);
    }

    /// Kein Tier erhält mehr als die Web-Wurzel-Sandbox.
    #[test]
    fn test_web_op_context_never_exceeds_root_sandbox_for_any_tier() {
        let (_root_dir, root) = test_root_sandbox();
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![]));
        for tier in [
            PermissionTier::Observer,
            PermissionTier::Operator,
            PermissionTier::Maintainer,
            PermissionTier::Owner,
        ] {
            let ctx = web_op_context(
                ServiceMap::new(),
                None,
                &root,
                &approver,
                PeerCredentials::new(1, 1000, 1000),
                tier,
            );
            assert!(
                ctx.sandbox().permissions().is_subset_of(root.permissions()),
                "{tier:?} exceeds the web root sandbox"
            );
            assert_eq!(
                ctx.sandbox().permissions(),
                crate::runtime_web::narrow_web_sandbox(&root, tier).permissions()
            );
        }
    }

    /// Mit `PeerCredentials`, `ApprovalActorResolver` und `ApprovalStore` in
    /// der `ServiceMap` antwortet `approval.pending` nicht mit
    /// `OpError::NotAvailable`. Öffnet nur ein temporäres Verzeichnis.
    #[tokio::test]
    async fn test_approval_pending_is_reachable_once_peer_and_resolver_are_wired() {
        use harw_operations::OpError;
        use harw_operations::operation::OpInput;

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let (_root_dir, root) = test_root_sandbox();

        let store_root = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let approval_store = Arc::new(ApprovalStore::new(store_root.path()));
        let peer = PeerCredentials::new(1, 1000, 1000);
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]));

        let op = Arc::clone(
            registry
                .find_by_name("approval.pending")
                .expect("approval.pending ist registriert"),
        );
        let ctx = web_op_context(
            registry_map(&registry),
            Some(&approval_store),
            &root,
            &approver,
            peer,
            PermissionTier::Owner,
        );

        let result = op.run(&ctx, OpInput::model_tool(serde_json::json!({}))).await;

        assert!(
            !matches!(result, Err(OpError::NotAvailable(_))),
            "approval.pending sollte erreichbar sein, war aber: {result:?}"
        );
    }

    /// Eine `uid` ohne Eintrag in der Genehmiger-Tabelle wird abgewiesen —
    /// nie auf einen Vorgabe-Actor abgebildet.
    #[tokio::test]
    async fn test_approval_resolve_rejects_a_peer_unknown_to_the_approver_table() {
        use harw_operations::OpError;
        use harw_operations::operation::OpInput;

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let (_root_dir, root) = test_root_sandbox();

        let store_root = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let approval_store = Arc::new(ApprovalStore::new(store_root.path()));
        // Dieselbe Komposition wie in `serve_web`: nur die eigene uid (hier
        // 1000) ist eingetragen — 9999 bleibt bewusst ohne Eintrag.
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]));
        let unknown_peer = PeerCredentials::new(2, 9999, 9999);

        let op = Arc::clone(
            registry
                .find_by_name("approval.resolve")
                .expect("approval.resolve ist registriert"),
        );
        let ctx = web_op_context(
            registry_map(&registry),
            Some(&approval_store),
            &root,
            &approver,
            unknown_peer,
            PermissionTier::Owner,
        );

        let args = serde_json::json!({
            "session": "session-1",
            "request": "approval-1",
            "decision": "approved",
            "resolved_at": "1970-01-01T00:00:00Z",
        });
        let result = op.run(&ctx, OpInput::model_tool(args)).await;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("keinem Genehmiger zugeordnet"));
            }
            other => panic!("erwartet NotAvailable(..keinem Genehmiger..), erhalten: {other:?}"),
        }
    }

    /// Der `ApprovalActor` stammt ausschließlich aus den Peer-Credentials,
    /// nie aus dem Anfragerumpf.
    #[tokio::test]
    async fn test_approval_resolve_derives_the_actor_from_peer_credentials_not_the_body() {
        use harw_operations::operation::OpInput;
        use harw_session_store::approval::ApprovalRecord;
        use harw_types::{ApprovalActor, ItemId, ToolCallId};
        use jiff::Timestamp;

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let (_root_dir, root) = test_root_sandbox();

        let store_root = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let approval_store = Arc::new(ApprovalStore::new(store_root.path()));
        approval_store
            .issue(&ApprovalRecord {
                request: ItemId::from_str("approval-1"),
                session: SessionId::from_str("session-1"),
                call_id: ToolCallId::from_str("call-1"),
                actor: ApprovalActor::Operator {
                    id: "owner".to_owned(),
                },
                issued_at: Timestamp::constant(1, 0),
            })
            .expect("issue succeeds against a fresh store");

        let peer = PeerCredentials::new(1, 1000, 1000);
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]));

        let op = Arc::clone(
            registry
                .find_by_name("approval.resolve")
                .expect("approval.resolve ist registriert"),
        );
        let ctx = web_op_context(
            registry_map(&registry),
            Some(&approval_store),
            &root,
            &approver,
            peer,
            PermissionTier::Owner,
        );

        // Bewusst ohne jedes Actor-Feld — `ApprovalResolveArgs` hat keins.
        let args = serde_json::json!({
            "session": "session-1",
            "request": "approval-1",
            "decision": "approved",
            "resolved_at": "1970-01-01T00:00:00Z",
        });
        let output = op
            .run(&ctx, OpInput::model_tool(args))
            .await
            .expect("resolve succeeds: peer.uid ist in der Genehmiger-Tabelle");

        assert!(output.text.contains("owner"));
    }

    /// Eine halb geöffnete Planungsfläche wird nie an die Montage gereicht.
    #[test]
    fn test_runtime_plan_services_requires_all_three_stores() {
        let disabled = crate::PlanServices {
            services: ServiceMap::new(),
            plan: None,
            goal: None,
            findings: None,
            config: harw_plan::PlanToolConfig::default(),
        };
        assert!(runtime_plan_services(disabled).is_none());

        let partial = crate::PlanServices {
            services: ServiceMap::new(),
            plan: Some(Arc::new(harw_plan::InMemoryPlanStore::new())),
            goal: Some(Arc::new(harw_plan::InMemoryGoalStore::new())),
            findings: None,
            config: harw_plan::PlanToolConfig::default(),
        };
        assert!(runtime_plan_services(partial).is_none());

        let complete = crate::PlanServices {
            services: ServiceMap::new(),
            plan: Some(Arc::new(harw_plan::InMemoryPlanStore::new())),
            goal: Some(Arc::new(harw_plan::InMemoryGoalStore::new())),
            findings: Some(Arc::new(harw_plan_bridge::FindingStore::new(
                "/nonexistent/w2d1-b1/plans",
            ))),
            config: harw_plan::PlanToolConfig::default(),
        };
        assert!(runtime_plan_services(complete).is_some());
    }

    /// F-W/W6: `WebRouteTable::from_registry(assembly.operations())` gegen
    /// eine echte `EntryKind::Web`-Montage (`OperationSurface::CommandsOnly`,
    /// `harw-runtime/src/assembly.rs:774-786`) enthält beide
    /// Genehmigungsrouten. `approval.pending`/`approval.resolve`
    /// (`harw-ops/src/approval.rs:320-326,444-449`) deklarieren nur
    /// `Surface::Web`, kein `Surface::ModelTool` — der CommandsOnly-Filter
    /// lässt jede Operation mit mindestens einer Nicht-ModelTool-Fläche
    /// durch, sie fehlen also nicht in der Web-Registrierung.
    #[test]
    fn test_web_route_table_from_registry_includes_approval_routes() {
        let (_home, _cwd, assembly) = test_assembly(true);

        let routes = WebRouteTable::from_registry(assembly.operations())
            .expect("no two operations claim the same web path");

        let pending = routes
            .find("/api/approval-pending")
            .expect("approval.pending is registered as a web route from the assembly");
        assert_eq!(pending.operation_name(), "approval.pending");
        assert!(pending.readonly(), "approval.pending is declared readonly");

        let resolve = routes
            .find("/api/approval-resolve")
            .expect("approval.resolve is registered as a web route from the assembly");
        assert_eq!(resolve.operation_name(), "approval.resolve");
        assert!(!resolve.readonly(), "approval.resolve is declared mutating");
    }
}
