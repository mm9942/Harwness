//! `harw web` — die Kompositionsstelle, die `harw-web`s bisher konsumentenlose
//! Unix-Socket-HTTP-Fläche tatsächlich an einen Prozess bindet.
//!
//! # Verantwortungsbereich
//! Dieses Modul schließt den Bottom-up-Befund „`harw-web` hat null
//! Produktionskonsumenten": kein Prozess band `BoundWebServer`/
//! `WebServerConfig`. [`serve_web`] ist der Konsument — aufgerufen aus
//! `cli::Command::Web` (`main.rs::dispatch`).
//!
//! # Dieselbe Registry wie der Rest der CLI
//! [`crate::build_operation_registry`] ist dieselbe Funktion, die auch
//! `harw analyze` und der Chat-Einstieg verwenden (siehe deren Aufrufer in
//! `main.rs`). [`harw_web::router::WebRouteTable::from_registry`] baut die
//! Routentabelle **ausschließlich** aus dieser einen Registry — es gibt in
//! diesem Modul keine zweite Stelle, die Operationen zusammenstellt.
//!
//! # Warum er nicht ungefragt läuft
//! `harw web` bindet nur, weil der Subcommand explizit aufgerufen wurde —
//! anders als `harw serve` (dessen MCP-Listener zusätzlich hinter
//! `[harness.mcp_listener] enabled` gated ist) trägt `harw-config` heute
//! kein Analogon für die Web-Fläche. Die Kompositionsstelle hier fügt keinen
//! zweiten, stillen Startpfad hinzu: [`serve_web`] wird ausschließlich aus
//! dem `Web`-Subcommand aufgerufen, nirgends sonst.
//!
//! # Der Socket-Pfad
//! `harw-home` kennt — anders als vom Auftrag vermutet — **keinen**
//! zentralen Pfadnamen für einen Kontrollflächen-Socket; auch
//! `harw-sentinel`s `sentinel.sock` ist eine lokale Konstante in
//! `harw-sentinel::cli`, kein Eintrag in `harw_home::paths`. Dieses Modul
//! folgt demselben Muster: [`DEFAULT_SOCKET_FILE_NAME`] ist eine lokale
//! Vorgabe (`<home>/web.sock`), überschreibbar über `--socket`. Ohne
//! `--socket` und ohne auflösbares HARW-Home (externes `--config-dir` ohne
//! `--home`) bricht [`serve_web`] mit einer Fehlermeldung ab, statt einen
//! Pfad zu erraten.
//!
//! # `SO_PEERCRED`-Autorisierung
//! Der aktuelle Prozess-Eigentümer (die eigene UID, über
//! `rustix::process::getuid`) bekommt [`PermissionTier::Owner`] — die
//! höchste Stufe; jede andere UID bleibt unautorisiert
//! ([`harw_web::authz::StaticUidTierMap::new`] ohne Rückfallwert). Der Socket
//! liegt unter dem privaten HARW-Home; ein Peer mit fremder UID, der ihn
//! trotzdem erreicht, bekommt `403 unknown_peer`.
//!
//! # `OpContext` je Aufruf
//! [`harw_web::server::WebContextFactory`] wird einmal pro angenommener
//! Verbindung aufgerufen und muss synchron einen frischen `OpContext`
//! liefern. `OperationRegistry` und `ServiceMap` sind nicht `Clone` (siehe
//! `harw_operations::registry`/`context`-Moduldoku); dieses Modul baut daher
//! bei jedem Aufruf eine neue `OperationRegistry` aus `Arc`-Klonen der immer
//! gleichen Operationsliste sowie eine neue `ServiceMap`, in die dieselben
//! Plan-/Goal-/Finding-Store-`Arc`s eingefügt werden, die beim Start der
//! Planungsfläche geöffnet wurden (`crate::build_plan_services`) — kein
//! zweiter Store wird pro Aufruf geöffnet, nur die `ServiceMap`-Hülle ist
//! neu. Ist die Planungsfläche abgeschaltet oder kein HARW-Home auflösbar,
//! bleiben `/plan`, `/goal`, `/explore`, `/research-*` und `/analyze`
//! registriert, aber ohne Store — sie antworten `OpError::NotAvailable`,
//! genau wie im One-Shot-Chat-Pfad ohne Planungsfläche.
//!
//! Die Sandbox kommt aus [`crate::build_local_spawn_context`] — derselben
//! Funktion, die auch `harw analyze` für seine `OpContext`-Sandbox
//! verwendet.
//!
//! # Woher der `ApprovalActor` kommt — und woher nicht
//! `approval.pending`/`approval.resolve` (`harw-ops::approval`) leiten den
//! [`harw_types::ApprovalActor`] ausschließlich aus zwei Services her, die
//! diese Datei in die [`ServiceMap`] jeder Verbindung legt: den
//! [`PeerCredentials`], die [`harw_web::server::WebContextFactory`] pro
//! angenommener Verbindung aus `SO_PEERCRED` liest (Kernel-verbürgt, siehe
//! `harw_web::peer`-Moduldoku), und einen `Arc<dyn ApprovalActorResolver>`,
//! der diese `uid` serverseitig auf einen `ApprovalActor` abbildet. Der
//! `peer`-Parameter der [`harw_web::server::WebContextFactory`]-Closure war
//! zuvor als `_peer` verworfen — ohne ihn im [`OpContext`] zu registrieren,
//! konnte `approval.resolve` den Aufrufer nie identifizieren und musste
//! fail-closed mit `OpError::NotAvailable` antworten. Es gibt **keinen**
//! zweiten Weg, aus dem ein `ApprovalActor` entstehen könnte: weder
//! `ApprovalResolveArgs` noch diese Datei lesen ihn aus dem Anfragerumpf.
//!
//! # Die Genehmiger-Tabelle — ohne Vorgabewert, wie [`StaticUidTierMap`]
//! [`StaticUidApprovalActorMap`] wird hier — genau wie [`StaticUidTierMap`]
//! wenige Zeilen darüber — **an der Kompositionswurzel** aus der eigenen
//! Prozess-`uid` gebaut, nie aus dem Anfragerumpf: `rustix::process::getuid`
//! liefert dieselbe `uid`, die auch [`PermissionTier::Owner`] bekommt, und
//! diese Datei bildet sie auf `ApprovalActor::Operator { id: "owner" }` ab.
//! Genau wie [`StaticUidTierMap::new`] hier ohne Rückfallwert aufgerufen
//! wird, hat auch [`StaticUidApprovalActorMap::new`] hier **keinen**
//! Eintrag für irgendeine andere `uid` — eine unbekannte `uid` bleibt
//! `None`, nie ein generischer Actor (siehe `StaticUidApprovalActorMap`s
//! eigene Moduldoku: „ein unbekannter Genehmiger ist niemals ein
//! akzeptabler Standardwert"). Diese Datei erweitert die Tabelle bewusst
//! nicht auf mehrere Benutzer — das wäre eine neue, hier nicht getroffene
//! Betriebsentscheidung (wer außer dem Prozess-Eigentümer genehmigen darf),
//! keine, die diese Komposition selbst fällen sollte.
//!
//! # Der Genehmigungsspeicher
//! [`ApprovalStore::new`] ist infallibel und öffnet keine Datei sofort —
//! diese Datei baut ihn deshalb, sobald ein HARW-Home auflösbar ist, direkt
//! aus `home` (dieselbe Bedingung wie bei den Plan-Diensten): der Typ hängt
//! sein eigenes `approvals`-Unterverzeichnis intern an, diese Datei fügt
//! keinen zweiten Verzeichnisnamen hinzu. Der fertige Speicher landet als
//! `Arc<ApprovalStore>` in der `ServiceMap` jeder Verbindung. Ohne
//! HARW-Home (externes `--config-dir` ohne `--home`) bleibt er `None`;
//! `approval.pending`/`approval.resolve` antworten dann
//! `OpError::NotAvailable`, genau wie `/plan`/`/goal` ohne Home.
//!
//! # `harw-dod`: kein Konsument hier
//! `harw-cli` braucht `harw-dod` nicht. Die Fassade bündelt Host-Sicherheits-
//! sensorik (CPU/Speicher/Netz/Auth-Log/Prozessüberwachung, Regelbewertung,
//! Eskalation) für einen Beobachtungsprozess — das ist `harw-sentinel`s
//! Aufgabe, nicht die eines CLI-Kontrollflächen-Servers, der Operationen an
//! einen bereits identifizierten, lokal-vertrauten Peer weiterreicht. Diese
//! Datei zieht deshalb keine `harw-dod`-Abhängigkeit; die Fassade wartet
//! weiter auf ihren richtigen Konsumenten.
//!
//! # Nebenläufigkeit
//! [`serve_web`] baut eine einthreadige `tokio`-Runtime (wie
//! `main::serve_mcp`) und läuft darin bis `BoundWebServer::serve` endet
//! (Annahmefehler oder Prozessende). Jede Verbindung bedient `harw-web`
//! bereits in einer eigenen Task (siehe dortige Moduldoku).
//!
//! # Fehler
//! Ein `String` bei Home-/Config-Auflösung, Registrierungsfehlern
//! ([`harw_web::error::WebError`]) oder wenn kein Socket-Pfad auflösbar ist.

use std::path::PathBuf;
use std::sync::Arc;

use harw_operations::context::{OpContext, ServiceMap};
use harw_operations::operation::PermissionTier;
use harw_operations::registry::OperationRegistry;
use harw_plan::{GoalStore, PlanStore, PlanToolConfig};
use harw_plan_bridge::{FindingStore, register_plan_services};
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
/// - `layers` (`Vec<PathBuf>`): Config-Layer, wie von
///   [`crate::resolve_serve_paths`] geliefert.
/// - `home` (`Option<PathBuf>`): das aufgelöste HARW-Home, falls kein
///   externes `--config-dir` verwendet wurde — bestimmt sowohl den
///   Vorgabe-Socket-Pfad als auch, ob die Planungsfläche Stores öffnen kann.
/// - `socket_override` (`Option<PathBuf>`): `--socket`; hat Vorrang vor der
///   Vorgabe unter `home`.
///
/// # Returns
/// `Ok(())`, wenn die Annahmeschleife regulär endet (siehe
/// [`harw_web::server::BoundWebServer::serve`]).
///
/// # Errors
/// Ein `String`, wenn keine Konfiguration geladen werden kann, kein
/// Socket-Pfad auflösbar ist (`socket_override` und `home` beide `None`),
/// die Sandbox nicht gebaut werden kann, die Routentabelle einen Fehler
/// meldet ([`harw_web::error::WebError::MissingSurfaceMetadata`] o. ä.), oder
/// der Server nicht binden kann.
///
/// # Concurrency
/// Baut eine eigene einthreadige `tokio`-Runtime, siehe Moduldoc.
pub(crate) fn serve_web(
    layers: Vec<PathBuf>,
    home: Option<PathBuf>,
    socket_override: Option<PathBuf>,
) -> Result<(), String> {
    let config = harw_config::discover_config(&layers).map_err(|error| error.to_string())?;
    config.validate().map_err(|error| error.to_string())?;

    let socket_path = socket_override
        .or_else(|| home.as_deref().map(|home| home.join(DEFAULT_SOCKET_FILE_NAME)))
        .ok_or_else(|| {
            "harw web: kein Socket-Pfad auflösbar — gib --socket an oder verwende ein HARW-Home \
             (kein externes --config-dir ohne --home)"
                .to_owned()
        })?;

    let plan_config = crate::plan_tool_config_from_section(&config.harness.tools.plan)?;
    let (operations, plan_tools) = crate::build_operation_registry(&plan_config);
    tracing::info!(
        total = operations.len(),
        plan_tools,
        "web.registry.assembled"
    );

    let routes = WebRouteTable::from_registry(&operations).map_err(|error| error.to_string())?;

    let uid = rustix::process::getuid().as_raw();
    let authorizer: Arc<dyn harw_web::PeerAuthorizer> =
        Arc::new(StaticUidTierMap::new(vec![(uid, PermissionTier::Owner)]));

    // Dieselbe `uid`, die oben `PermissionTier::Owner` bekommt, wird hier auf
    // genau einen Genehmiger abgebildet — ohne Rückfallwert für jede andere
    // `uid` (siehe Moduldoc, Abschnitt „Die Genehmiger-Tabelle").
    let approver: Arc<dyn ApprovalActorResolver> =
        Arc::new(StaticUidApprovalActorMap::new(vec![(uid, "owner".to_owned())]));

    let project_root = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    let sandbox = crate::build_local_spawn_context(&project_root)?.sandbox;

    // Ohne HARW-Home bleibt der Genehmigungsspeicher `None` — dieselbe
    // Bedingung wie bei den Plan-Diensten unten (siehe Moduldoc, Abschnitt
    // „Der Genehmigungsspeicher"). `ApprovalStore::new` hängt sein eigenes
    // `approvals`-Unterverzeichnis an `root` an (siehe dortige
    // Implementierung) — diese Datei erfindet dafür keinen eigenen,
    // doppelten Verzeichnisnamen.
    let approval_store: Option<Arc<ApprovalStore>> =
        home.as_deref().map(|home_path| Arc::new(ApprovalStore::new(home_path)));

    let plan_services = match home.as_deref() {
        Some(home_path) => Some(crate::build_plan_services(
            home_path,
            &plan_config,
            crate::DEFAULT_PLAN_SPACE,
            crate::DEFAULT_GOAL_SPACE,
        )?),
        None => None,
    };
    let plan_store = plan_services.as_ref().and_then(|services| services.plan.clone());
    let goal_store = plan_services.as_ref().and_then(|services| services.goal.clone());
    let finding_store = plan_services
        .as_ref()
        .and_then(|services| services.findings.clone());

    // Alles, was der Server einmal aufbaut, in einem Bündel; die
    // `PeerCredentials` kommen weiterhin je Verbindung vom Kernel und werden
    // hier bewusst nicht eingelagert.
    let web_services = WebServerServices {
        operations,
        sandbox,
        plan: plan_store,
        goal: goal_store,
        findings: finding_store,
        plan_config,
        approver,
        approval_store,
    };

    let context_factory: Arc<harw_web::server::WebContextFactory> =
        Arc::new(move |peer: &PeerCredentials, _tier: PermissionTier| {
            build_web_op_context(&web_services, *peer)
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

/// Die Dienste, die [`serve_web`] **einmal beim Start** aufbaut und die jede
/// angenommene Verbindung unverändert weiterverwendet.
///
/// # Description
/// Bündelt die acht langlebigen Bestandteile, aus denen
/// [`build_web_op_context`] pro Anfrage einen frischen [`OpContext`] formt.
/// Der Schnitt trennt zwei Lebensdauern: alles hier entsteht **einmal** an
/// der Kompositionswurzel in [`serve_web`] und wird nur noch geliehen bzw.
/// über `Arc`-Zeiger geklont; das einzige **je Verbindung** entstehende
/// Datum — die [`PeerCredentials`] — bleibt bewusst ein eigener Parameter
/// von [`build_web_op_context`] und ist **kein Feld dieses Structs**.
///
/// # Woher die Peer-Credentials kommen
/// Die [`PeerCredentials`] stammen **vom Kernel** (`SO_PEERCRED`, gelesen in
/// `harw-web` und der [`harw_web::server::WebContextFactory`] übergeben) und
/// **nie aus dem Anfragerumpf**. Sie sind `Copy` und werden je Verbindung
/// neu gereicht; würde man sie hier einlagern, entstünde ein geteiltes,
/// langlebiges Identitätsfeld — genau der zweite Autoritätspfad, den dieses
/// Modul nicht haben darf.
///
/// # Felder
/// - `operations`: die einmal gebaute [`OperationRegistry`]
///   ([`crate::build_operation_registry`]); je Anfrage wird daraus eine neue
///   Registry aus `Arc`-Klonen der Operationen gebaut, weil
///   `OperationRegistry` nicht `Clone` ist (siehe Moduldoc).
/// - `sandbox`: die Sandbox aus [`crate::build_local_spawn_context`]; wird
///   je Anfrage geklont, da `OpContext::new` Eigentum verlangt.
/// - `plan`: der beim Start geöffnete Plan-Store (`crate::build_plan_services`),
///   `None` bei abgeschalteter Planungsfläche oder ohne HARW-Home.
/// - `goal`: der zugehörige Goal-Store, mit derselben `None`-Bedingung.
/// - `findings`: der zugehörige Finding-Store, mit derselben `None`-Bedingung.
/// - `plan_config`: dieselbe [`PlanToolConfig`], die auch die Operationen
///   registriert hat.
/// - `approver`: die serverseitig gebaute, uid-basierte Genehmiger-Tabelle
///   (siehe Moduldoc, Abschnitt „Die Genehmiger-Tabelle"). Sie hat **keinen
///   Vorgabewert**: eine unbekannte uid wird abgewiesen, nicht auf einen
///   Standard-Actor abgebildet.
/// - `approval_store`: der Genehmigungsspeicher aus [`serve_web`], `None`
///   ohne HARW-Home (siehe Moduldoc, Abschnitt „Der Genehmigungsspeicher").
///
/// # Concurrency
/// Wird hinter der `WebContextFactory` in einem `Arc` geteilt und nur
/// gelesen; kein veränderlicher Zustand.
struct WebServerServices {
    // Einmal gebaute Operationsliste; Vorlage für die Registry je Anfrage.
    operations: OperationRegistry,
    // Sandbox der Kompositionswurzel; je Anfrage geklont.
    sandbox: SandboxSpec,
    // Beim Start geöffnete Planungsfläche — alle drei oder keiner.
    plan: Option<Arc<dyn PlanStore>>,
    goal: Option<Arc<dyn GoalStore>>,
    findings: Option<Arc<FindingStore>>,
    // Konfiguration, mit der die Plan-Operationen registriert wurden.
    plan_config: PlanToolConfig,
    // uid-basierte Genehmiger-Tabelle ohne Rückfallwert.
    approver: Arc<dyn ApprovalActorResolver>,
    // Genehmigungsspeicher; `None` ohne HARW-Home.
    approval_store: Option<Arc<ApprovalStore>>,
}

/// Baut den [`OpContext`] einer einzelnen freigegebenen Web-Anfrage.
///
/// # Description
/// Aufgerufen aus [`harw_web::server::WebContextFactory`] — synchron, einmal
/// pro angenommener Verbindung (siehe dortige Vertragsdoku). Baut eine
/// frische [`OperationRegistry`] aus `Arc`-Klonen von `services.operations`
/// (die Registry selbst ist nicht `Clone`, siehe Moduldoc) und registriert
/// die Plan-Dienste erneut, falls Plan-, Goal- **und** Finding-Store
/// vorhanden sind — dieselbe Bedingung wie im One-Shot-Chat-Pfad
/// (`chat::one_shot_model_tool_context`).
///
/// # Arguments
/// - `services` (`&WebServerServices`): die einmal beim Start gebauten
///   Dienste; geliehen, nie verschoben.
/// - `peer` (`PeerCredentials`): die über `SO_PEERCRED` gelesene Identität
///   **dieser einen** Verbindung, unverändert aus dem
///   [`harw_web::server::WebContextFactory`]-Parameter übernommen — vom
///   Kernel, nie aus dem Anfragerumpf. Als `PeerCredentials`-Service
///   registriert, damit `approval.resolve` (`harw-ops::approval`) den
///   Aufrufer identifizieren kann (siehe Moduldoc, Abschnitt „Woher der
///   `ApprovalActor` kommt").
///
/// # Returns
/// Einen frischen [`OpContext`] mit neuer [`SessionId`]/[`TurnId`].
///
/// # Concurrency
/// Rein synchron; jeder Aufruf klont nur `Arc`-Zeiger und baut eine neue,
/// kleine `ServiceMap`-Hülle — kein gemeinsamer veränderlicher Zustand.
fn build_web_op_context(services: &WebServerServices, peer: PeerCredentials) -> OpContext {
    let mut registry = OperationRegistry::new();
    for operation in services.operations.iter() {
        registry.register(Arc::clone(operation));
    }

    let mut service_map = ServiceMap::new();
    if let (Some(plan), Some(goal), Some(findings)) =
        (services.plan.clone(), services.goal.clone(), services.findings.clone())
    {
        let plan_config = services.plan_config.clone();
        register_plan_services(&mut service_map, plan, goal, findings, plan_config);
    }
    service_map.insert(peer);
    service_map.insert(Arc::clone(&services.approver));
    if let Some(approval_store) = services.approval_store.clone() {
        service_map.insert(approval_store);
    }
    service_map.insert(registry);

    OpContext::new(
        SessionId::new(),
        TurnId::new(),
        services.sandbox.clone(),
        service_map,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // `build_web_op_context` öffnet keinen Socket und keine Datei — die
    // Sandbox und die Operationsliste sind reine In-Memory-Werte. Dieser
    // Test belegt, dass ohne Plan-Store trotzdem ein gültiger `OpContext`
    // entsteht (die abgeschaltete Planungsfläche darf den Aufbau nicht zum
    // Absturz bringen). Kein Test in dieser Datei öffnet einen echten
    // Socket — `build_web_op_context` ist rein synchron und
    // dateisystemfrei, solange kein `Arc<ApprovalStore>` übergeben wird.
    #[test]
    fn test_build_web_op_context_without_plan_services_still_builds_a_context() {
        let operations = OperationRegistry::new();
        let sandbox = test_sandbox();
        let peer = PeerCredentials::new(1, 1, 1);
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![]));

        let services = WebServerServices {
            operations,
            sandbox,
            plan: None,
            goal: None,
            findings: None,
            plan_config: PlanToolConfig::default(),
            approver,
            approval_store: None,
        };
        let ctx = build_web_op_context(&services, peer);

        assert!(ctx.service::<OperationRegistry>().is_some());
        assert!(ctx.service::<PeerCredentials>().is_some());
        assert!(ctx.service::<Arc<dyn ApprovalActorResolver>>().is_some());
        assert!(ctx.service::<Arc<ApprovalStore>>().is_none());
    }

    /// Der wichtigste Test dieses Knotens: mit `PeerCredentials`,
    /// `ApprovalActorResolver` und `ApprovalStore` tatsächlich in der
    /// `ServiceMap` registriert, antwortet `approval.pending` nicht mehr
    /// mit `OpError::NotAvailable` (Befund 1 des Auftrags). Öffnet nur ein
    /// temporäres Verzeichnis für den `ApprovalStore` — keinen Socket.
    #[tokio::test]
    async fn test_approval_pending_is_reachable_once_peer_and_resolver_are_wired() {
        use harw_operations::OpError;
        use harw_operations::operation::OpInput;

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let sandbox = test_sandbox();

        let store_root = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let approval_store = Arc::new(ApprovalStore::new(store_root.path()));
        let peer = PeerCredentials::new(1, 1000, 1000);
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]));

        // Die Op wird vor dem Bündeln als `Arc` gegriffen, weil
        // `WebServerServices` die Registry besitzt.
        let op = Arc::clone(
            registry
                .find_by_name("approval.pending")
                .expect("approval.pending ist registriert"),
        );
        let services = WebServerServices {
            operations: registry,
            sandbox,
            plan: None,
            goal: None,
            findings: None,
            plan_config: PlanToolConfig::default(),
            approver,
            approval_store: Some(approval_store),
        };
        let ctx = build_web_op_context(&services, peer);

        let result = op.run(&ctx, OpInput::model_tool(serde_json::json!({}))).await;

        assert!(
            !matches!(result, Err(OpError::NotAvailable(_))),
            "approval.pending sollte erreichbar sein, war aber: {result:?}"
        );
    }

    /// Eine `uid` ohne Eintrag in der Genehmiger-Tabelle wird abgewiesen —
    /// nie auf einen Vorgabe-Actor abgebildet (siehe Moduldoc, Abschnitt
    /// „Die Genehmiger-Tabelle").
    #[tokio::test]
    async fn test_approval_resolve_rejects_a_peer_unknown_to_the_approver_table() {
        use harw_operations::OpError;
        use harw_operations::operation::OpInput;

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let sandbox = test_sandbox();

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

        // Die Op wird vor dem Bündeln als `Arc` gegriffen, weil
        // `WebServerServices` die Registry besitzt.
        let op = Arc::clone(
            registry
                .find_by_name("approval.resolve")
                .expect("approval.resolve ist registriert"),
        );
        let services = WebServerServices {
            operations: registry,
            sandbox,
            plan: None,
            goal: None,
            findings: None,
            plan_config: PlanToolConfig::default(),
            approver,
            approval_store: Some(approval_store),
        };
        let ctx = build_web_op_context(&services, unknown_peer);

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
    /// nie aus dem Anfragerumpf: `args` unten trägt kein Actor-Feld — der
    /// Actor entsteht trotzdem, weil `peer.uid` in der Genehmiger-Tabelle
    /// steht, und die Auflösung ist an genau diesen Actor gebunden.
    #[tokio::test]
    async fn test_approval_resolve_derives_the_actor_from_peer_credentials_not_the_body() {
        use harw_operations::operation::OpInput;
        use harw_session_store::approval::ApprovalRecord;
        use harw_types::{ApprovalActor, ItemId, SessionId, ToolCallId};
        use jiff::Timestamp;

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let sandbox = test_sandbox();

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

        // Die Op wird vor dem Bündeln als `Arc` gegriffen, weil
        // `WebServerServices` die Registry besitzt.
        let op = Arc::clone(
            registry
                .find_by_name("approval.resolve")
                .expect("approval.resolve ist registriert"),
        );
        let services = WebServerServices {
            operations: registry,
            sandbox,
            plan: None,
            goal: None,
            findings: None,
            plan_config: PlanToolConfig::default(),
            approver,
            approval_store: Some(approval_store),
        };
        let ctx = build_web_op_context(&services, peer);

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

    fn test_sandbox() -> SandboxSpec {
        use harw_sandbox::{
            Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry,
        };
        use harw_types::{TenantId, WorkspaceId};

        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let tenant = TenantId::from_str("test");
        let workspace = WorkspaceId::from_str("project");
        let registry = match WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from("."),
            }],
        ) {
            Ok(registry) => registry,
            Err(error) => panic!("workspace registry: {error}"),
        };
        let binding = match registry.resolve(&tenant, &workspace) {
            Ok(binding) => binding,
            Err(error) => panic!("resolve: {error}"),
        };
        SandboxSpec::from_resolved(binding, PermissionSet::from_policy([Permission::ReadWorkspace]))
    }
}
