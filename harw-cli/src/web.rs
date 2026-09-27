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
//! `harw_home::paths`. Dieses Modul folgt demselben Muster. Drei Betriebsarten
//! ([`resolve_listen_plan`], Crypto-Masterplan v2 §6.1/§22.3, H9; Drift-Bericht
//! D1):
//!
//! - **Entwicklung (ohne Flag):** [`DEFAULT_SOCKET_FILE_NAME`] unter dem Home
//!   (`<home>/web.sock`), überschreibbar über `--socket`. Das ist der
//!   Entwicklungs-Rückfall — kein Produktionspfad.
//! - **System (`--system`):** `/run/harw/infra/control.sock`
//!   ([`SYSTEM_RUNTIME_DIR`]/[`SYSTEM_SOCKET_FILE_NAME`]; `--socket`
//!   überschreibt den Pfad ausdrücklich), nach dem Binden Modus `0660`
//!   ([`SYSTEM_SOCKET_MODE`]) und optional die Gruppe aus `--socket-group`
//!   (sonst unverändert). Fehlt das Laufzeitverzeichnis, bricht der Start mit
//!   einer klaren Meldung ab — es gibt **keinen** Rückfall auf `$HOME`
//!   (§22.3: „No production fallback to … `~/.harw` sockets").
//! - **Socket-Aktivierung (`--systemd-socket`):** impliziert den
//!   Systembetrieb, bindet selbst nichts und übernimmt genau den einen von
//!   systemd übergebenen Socket ([`harw_web::systemd::listener_from_systemd`],
//!   `LISTEN_PID`/`LISTEN_FDS=1` werden geprüft). Modus und Gruppe setzt dann
//!   die `.socket`-Unit (`SocketMode=0660`, `SocketGroup=`).
//!
//! Das HARW-Home bleibt in allen drei Arten Pflicht — es trägt Zustand und
//! Freigabespeicher (im Dienst `/var/lib/harw-control`), nur nie den
//! System-Socket.
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
//!   `docs/design/runtime-contracts.md` §runtime-spec) für **alle** Tiers `{ReadWorkspace}`
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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_authority::SandboxSpec;
use harw_config::{WebIdentityModeToml, WebIdentityToml};
use harw_operations::context::{OpContext, ServiceMap};
use harw_operations::operation::PermissionTier;
use harw_runtime::{RuntimeAssembly, ServiceSurface};
use harw_session_store::approval::ApprovalStore;
use harw_types::{SessionId, TurnId};
use harw_web::events::WebEventBus;
use harw_web::identity::{IdentityMode, WebIdentityConfig};
use harw_web::router::WebRouteTable;
use harw_web::security::{ApprovalActorResolver, StaticUidApprovalActorMap};
use harw_web::server::{BoundWebServer, WebServerConfig};
use harw_web::{PeerCredentials, StaticUidTierMap};

/// Dateiname des Web-Sockets unterhalb des HARW-Home, wenn `--socket` fehlt.
///
/// Lokale Vorgabe dieses Moduls, kein Eintrag in `harw_home::paths` — siehe
/// Moduldoc, Abschnitt „Der Socket-Pfad".
const DEFAULT_SOCKET_FILE_NAME: &str = "web.sock";

/// Laufzeitverzeichnis der Infrastruktur-Sockets im Systembetrieb
/// (`--system`, Drift-Bericht D1; angelegt von `deploy/tmpfiles.d/harw.conf`).
const SYSTEM_RUNTIME_DIR: &str = "/run/harw/infra";

/// Dateiname des Kontroll-Sockets im Systembetrieb (Masterplan §6.1).
const SYSTEM_SOCKET_FILE_NAME: &str = "control.sock";

/// Dateimodus des selbst gebundenen System-Sockets: Eigentümer und Gruppe
/// dürfen `connect(2)`, niemand sonst — wie `SocketMode=0660` der anderen
/// Infrastruktur-Sockets.
const SYSTEM_SOCKET_MODE: u32 = 0o660;

/// Wo die Gruppennamen für `--socket-group` aufgelöst werden.
const ETC_GROUP: &str = "/etc/group";

/// Listener-Optionen von `harw web` (`cli::Command::Web`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WebListenOptions {
    /// `--socket`: ausdrücklicher Socket-Pfad.
    pub(crate) socket: Option<PathBuf>,
    /// `--system`: Systembetrieb (`/run/harw/infra/control.sock`).
    pub(crate) system: bool,
    /// `--systemd-socket`: den von systemd übergebenen Socket übernehmen.
    pub(crate) systemd_socket: bool,
    /// `--socket-group`: Gruppe (Name oder GID) des System-Sockets.
    pub(crate) socket_group: Option<String>,
}

/// Wie `harw web` zu seinem Listener kommt (siehe Moduldoc „Der Socket-Pfad").
#[derive(Debug, Clone, PartialEq, Eq)]
enum ListenPlan {
    /// Einen Pfad selbst binden.
    Bind {
        /// Der zu bindende Socket-Pfad.
        path: PathBuf,
        /// Systembetrieb: danach Modus [`SYSTEM_SOCKET_MODE`] setzen.
        system: bool,
        /// `--socket-group` (nur im Systembetrieb), noch unaufgelöst.
        group: Option<String>,
    },
    /// Den von systemd übergebenen Socket übernehmen.
    Activated,
}

/// Der vorbereitete Listener, bevor die `tokio`-Runtime läuft.
enum PreparedListener {
    /// Selbst binden; `gid` ist die bereits aufgelöste `--socket-group`.
    Bind {
        path: PathBuf,
        system: bool,
        gid: Option<u32>,
    },
    /// Bereits übernommener, lauschender Aktivierungs-Socket.
    Activated(std::os::unix::net::UnixListener),
}

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
/// - `listen` ([`WebListenOptions`]): `--socket`, `--system`,
///   `--systemd-socket`, `--socket-group` (siehe [`resolve_listen_plan`]).
///
/// # Returns
/// `Ok(())`, wenn die Annahmeschleife regulär endet (siehe
/// [`harw_web::server::BoundWebServer::serve`]).
///
/// # Errors
/// Ein `String`, wenn `home` fehlt, der Listener-Plan ungültig ist (z. B.
/// `--system` ohne `/run/harw/infra`, ungültige Socket-Aktivierung,
/// unbekannte `--socket-group`), das Arbeitsverzeichnis nicht lesbar ist,
/// die Konfiguration nicht vertrauensbewusst geladen werden kann, die
/// Planungsfläche nicht öffnet, die Montage oder die Web-Wurzel-Sandbox
/// scheitert, die Routentabelle einen Fehler meldet oder der Server nicht
/// binden kann.
///
/// # Concurrency
/// Baut eine eigene einthreadige `tokio`-Runtime, siehe Moduldoc.
pub(crate) fn serve_web(home: Option<PathBuf>, listen: WebListenOptions) -> Result<(), String> {
    let home = home.ok_or_else(|| {
        "harw web benötigt ein HARW-Home (--home); ohne Home gibt es weder Trust-Bericht noch Freigabespeicher"
            .to_owned()
    })?;
    // Vor der Montage: ein falscher Listener-Plan (fehlendes
    // /run/harw/infra, fremde Aktivierungsumgebung) bricht sofort ab.
    let prepared = prepare_listener(resolve_listen_plan(
        &home,
        &listen,
        Path::new(SYSTEM_RUNTIME_DIR),
    )?)?;
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
    let project =
        harw_home::project::discover_project(&cwd, &[]).map_err(|error| error.to_string())?;
    let project_home = harw_home::project::ProjectHome::at(&project);
    let plan = crate::build_plan_services(
        &project_home,
        &plan_config,
        crate::DEFAULT_PLAN_SPACE,
        crate::DEFAULT_GOAL_SPACE,
    )?
    .to_runtime();

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
    let routes =
        WebRouteTable::from_registry(assembly.operations()).map_err(|error| error.to_string())?;
    tracing::info!(
        total = assembly.operations().len(),
        "web.registry.assembled"
    );

    let authorizer: Arc<dyn harw_web::PeerAuthorizer> =
        Arc::new(StaticUidTierMap::new(vec![(uid, PermissionTier::Owner)]));
    // H12: `[web.identity]` (nur aus vertrauten Layern, siehe
    // `harw_config::discovery`) → Resolver über derselben Tier-Tabelle. Ohne
    // Tabelle `tier_map` ohne Mandanten — das Verhalten vor H12. Eine
    // ungültige Tabelle bricht den Start ab, statt still zurückzufallen.
    let identity = web_identity_config(config.web.identity.as_ref())
        .build_resolver(Arc::clone(&authorizer))
        .map_err(|error| error.to_string())?;

    // Dieselbe `uid`, die oben `PermissionTier::Owner` bekommt, wird hier auf
    // genau einen Genehmiger abgebildet — ohne Rückfallwert für jede andere
    // `uid` (siehe Moduldoc, Abschnitt „Die Genehmiger-Tabelle").
    let approver: Arc<dyn ApprovalActorResolver> =
        Arc::new(StaticUidApprovalActorMap::new(vec![(
            uid,
            "owner".to_owned(),
        )]));

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
        let events =
            Arc::new(WebEventBus::new(EVENT_BUS_CAPACITY).map_err(|error| error.to_string())?);
        let server = match prepared {
            PreparedListener::Bind { path, system, gid } => {
                let server = BoundWebServer::bind(
                    WebServerConfig {
                        socket_path: path.clone(),
                    },
                    routes,
                    authorizer,
                    context_factory,
                    events,
                )
                .await
                .map_err(|error| error.to_string())?
                .with_identity_resolver(identity);
                if system {
                    apply_system_socket_permissions(&path, gid)?;
                }
                server
            }
            PreparedListener::Activated(listener) => BoundWebServer::from_std_listener(
                listener,
                routes,
                authorizer,
                context_factory,
                events,
            )
            .map_err(|error| error.to_string())?
            .with_identity_resolver(identity),
        };
        eprintln!(
            "harw web listening on unix:{}",
            server.socket_path().display()
        );
        server.serve().await.map_err(|error| error.to_string())
    })
}

/// Übersetzt die rohe Tabelle `[web.identity]` aus `harw-config` in
/// [`WebIdentityConfig`].
///
/// # Description
/// `harw-config` kennt `harw-web` nicht (Schichtung) und spiegelt die Tabelle
/// deshalb als [`WebIdentityToml`]; die Umwandlung ist Feld für Feld
/// verlustfrei. Die semantische Prüfung (numerische UIDs, gültige Mandanten,
/// `security_hub`-only-Schlüssel) bleibt bei
/// [`WebIdentityConfig::build_resolver`].
///
/// # Arguments
/// - `raw` (`Option<&WebIdentityToml>`): `config.web.identity`.
///
/// # Returns
/// Die Konfiguration; ohne Tabelle [`WebIdentityConfig::default`]
/// (`tier_map` ohne Mandanten).
fn web_identity_config(raw: Option<&WebIdentityToml>) -> WebIdentityConfig {
    let Some(raw) = raw else {
        return WebIdentityConfig::default();
    };
    WebIdentityConfig {
        mode: match raw.mode {
            WebIdentityModeToml::TierMap => IdentityMode::TierMap,
            WebIdentityModeToml::SecurityHub => IdentityMode::SecurityHub,
        },
        security_socket: raw.security_socket.clone(),
        require_context: raw.require_context,
        uid_tenants: raw.uid_tenants.clone(),
        uid_principals: raw.uid_principals.clone(),
    }
}

/// Bestimmt aus den Flags, wie `harw web` zu seinem Listener kommt.
///
/// # Description
/// Siehe Moduldoc „Der Socket-Pfad". Im Systembetrieb wird `home` **nie**
/// für den Socket verwendet; fehlt das Elternverzeichnis des System-Sockets
/// (Vorgabe `runtime_dir` = `/run/harw/infra`), ist das ein Fehler statt
/// eines Rückfalls.
///
/// # Arguments
/// - `home` (`&Path`): HARW-Home — nur für den Entwicklungs-Rückfall.
/// - `options` (`&WebListenOptions`): die Flags.
/// - `runtime_dir` (`&Path`): Laufzeitverzeichnis des Systembetriebs
///   (produktiv [`SYSTEM_RUNTIME_DIR`]; in Tests ein Tempdir).
///
/// # Errors
/// Ein `String` bei widersprüchlichen Flags (`--systemd-socket` mit
/// `--socket`/`--socket-group`, `--socket-group` ohne `--system`), einem
/// relativen System-Socket-Pfad oder fehlendem Laufzeitverzeichnis.
fn resolve_listen_plan(
    home: &Path,
    options: &WebListenOptions,
    runtime_dir: &Path,
) -> Result<ListenPlan, String> {
    if options.systemd_socket {
        if options.socket.is_some() || options.socket_group.is_some() {
            return Err(
                "harw web --systemd-socket bindet selbst nichts: --socket und --socket-group setzt die .socket-Unit (ListenStream=, SocketGroup=)"
                    .to_owned(),
            );
        }
        return Ok(ListenPlan::Activated);
    }
    if !options.system {
        if options.socket_group.is_some() {
            return Err("harw web --socket-group gilt nur mit --system".to_owned());
        }
        let path = options
            .socket
            .clone()
            .unwrap_or_else(|| home.join(DEFAULT_SOCKET_FILE_NAME));
        return Ok(ListenPlan::Bind {
            path,
            system: false,
            group: None,
        });
    }
    let path = options
        .socket
        .clone()
        .unwrap_or_else(|| runtime_dir.join(SYSTEM_SOCKET_FILE_NAME));
    if path.is_relative() {
        return Err(format!(
            "harw web --system: der Socket-Pfad muss absolut sein, erhalten: {}",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            format!(
                "harw web --system: Socket-Pfad {} hat kein Elternverzeichnis",
                path.display()
            )
        })?;
    if !parent.is_dir() {
        return Err(format!(
            "harw web --system: Laufzeitverzeichnis {} fehlt (angelegt von tmpfiles.d/harw.conf) und es wurde kein Socket übergeben (--systemd-socket); im Systembetrieb gibt es keinen Rückfall auf <home>/{DEFAULT_SOCKET_FILE_NAME}",
            parent.display()
        ));
    }
    Ok(ListenPlan::Bind {
        path,
        system: true,
        group: options.socket_group.clone(),
    })
}

/// Setzt einen [`ListenPlan`] vor dem Start der Runtime um: löst
/// `--socket-group` auf bzw. übernimmt den Aktivierungs-Socket.
///
/// # Errors
/// Ein `String`, wenn die Gruppe unbekannt ist oder die Aktivierung
/// ([`harw_web::systemd::listener_from_systemd`]) scheitert.
fn prepare_listener(plan: ListenPlan) -> Result<PreparedListener, String> {
    match plan {
        ListenPlan::Activated => harw_web::systemd::listener_from_systemd()
            .map(PreparedListener::Activated)
            .map_err(|error| error.to_string()),
        ListenPlan::Bind {
            path,
            system,
            group,
        } => {
            let gid = match group {
                None => None,
                Some(spec) => {
                    // Eine numerische GID braucht /etc/group nicht.
                    let groups = if spec.parse::<u32>().is_ok() {
                        String::new()
                    } else {
                        std::fs::read_to_string(ETC_GROUP)
                            .map_err(|error| format!("{ETC_GROUP}: {error}"))?
                    };
                    Some(resolve_group(&spec, &groups)?)
                }
            };
            Ok(PreparedListener::Bind { path, system, gid })
        }
    }
}

/// Löst `--socket-group` zu einer GID auf.
///
/// # Arguments
/// - `spec` (`&str`): Gruppenname oder numerische GID.
/// - `etc_group` (`&str`): Inhalt von `/etc/group` (`name:pw:gid:members`).
///
/// # Errors
/// Ein `String` für eine unbekannte Gruppe, eine ungültige GID oder die
/// reservierte GID `4294967295` (`-1`, „unverändert" für `chown(2)`).
fn resolve_group(spec: &str, etc_group: &str) -> Result<u32, String> {
    let spec = spec.trim();
    let gid = match spec.parse::<u32>() {
        Ok(gid) => gid,
        Err(_) => etc_group
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .find_map(|line| {
                let mut fields = line.split(':');
                let name = fields.next()?;
                let _password = fields.next()?;
                let gid = fields.next()?;
                (name == spec).then(|| gid.trim().parse::<u32>().ok())
            })
            .ok_or_else(|| format!("harw web --socket-group: unbekannte Gruppe '{spec}'"))?
            .ok_or_else(|| format!("harw web --socket-group: ungültige GID für '{spec}'"))?,
    };
    if gid == u32::MAX {
        return Err(format!("harw web --socket-group: ungültige GID {gid}"));
    }
    Ok(gid)
}

/// Setzt Gruppe (falls angegeben) und Modus [`SYSTEM_SOCKET_MODE`] des
/// selbst gebundenen System-Sockets.
///
/// # Description
/// Läuft direkt nach dem Binden und vor der ersten Annahme. Bis dahin trägt
/// der Socket den Modus aus der `umask` (im Dienst `0077` → `0600`) — das
/// Fenster ist also enger, nie weiter als das Ziel. Ohne `gid` bleibt die
/// Gruppe unverändert.
///
/// # Errors
/// Ein `String`, wenn `chown(2)` oder `chmod(2)` scheitert (z. B. Gruppe, der
/// der Prozess nicht angehört).
fn apply_system_socket_permissions(path: &Path, gid: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(gid) = gid {
        std::os::unix::fs::chown(path, None, Some(gid)).map_err(|error| {
            format!(
                "harw web: Gruppe {gid} für {} nicht setzbar: {error}",
                path.display()
            )
        })?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SYSTEM_SOCKET_MODE)).map_err(
        |error| {
            format!(
                "harw web: Modus {SYSTEM_SOCKET_MODE:o} für {} nicht setzbar: {error}",
                path.display()
            )
        },
    )
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
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::Permission;
    use harw_operations::registry::OperationRegistry;
    use harw_runtime::EntryKind;
    use harw_types::Principal;

    // Montage aus leerem Temp-Home ohne Netz/Provider (`ModelSource::Echo`).
    // Die `TempDir`s müssen so lange leben wie die Montage.
    fn test_assembly(
        with_approval_store: bool,
    ) -> TestResult<(tempfile::TempDir, tempfile::TempDir, RuntimeAssembly)> {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let cwd = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = with_approval_store.then(|| Arc::new(ApprovalStore::new(home.path())));
        let assembly = crate::runtime_web::web_assembly(home.path(), cwd.path(), 1000, store, None)
            .map_err(ctx("web assembly"))?;
        Ok((home, cwd, assembly))
    }

    // Web-Wurzel-Sandbox über einem eigenen Temp-Verzeichnis.
    fn test_root_sandbox() -> TestResult<(tempfile::TempDir, SandboxSpec)> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let sandbox =
            harw_runtime::root_sandbox(EntryKind::Web, dir.path()).map_err(ctx("root sandbox"))?;
        Ok((dir, sandbox))
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
    fn test_web_context_for_without_approval_store_still_builds_a_context() -> TestResult {
        let (_home, _cwd, assembly) = test_assembly(false)?;
        let (_root_dir, root) = test_root_sandbox()?;
        let approver: Arc<dyn ApprovalActorResolver> =
            Arc::new(StaticUidApprovalActorMap::new(vec![]));
        let peer = PeerCredentials::new(1, 1, 1);

        let ctx = web_context_for(&assembly, &root, &approver, peer, PermissionTier::Owner);

        assert!(ctx.service::<OperationRegistry>().is_some());
        assert_eq!(ctx.service::<PeerCredentials>(), Some(&peer));
        assert!(ctx.service::<Arc<dyn ApprovalActorResolver>>().is_some());
        assert!(ctx.service::<Arc<ApprovalStore>>().is_none());
        let principal = ctx
            .service::<Principal>()
            .ok_or(TestError::Missing("principal is registered"))?;
        assert_eq!(principal.id(), "uid:1");
        assert_eq!(principal.tier(), PermissionTier::Owner);
        Ok(())
    }

    /// F-045: ein `Observer`-Peer bekommt nur `ReadWorkspace` — und einen
    /// Observer-Principal, nicht den Owner-Principal der Montage.
    #[test]
    fn test_web_context_for_observer_sandbox_has_only_read_workspace() -> TestResult {
        let (_home, _cwd, assembly) = test_assembly(true)?;
        let (_root_dir, root) = test_root_sandbox()?;
        let approver: Arc<dyn ApprovalActorResolver> = Arc::new(StaticUidApprovalActorMap::new(
            vec![(1000, "owner".to_owned())],
        ));
        let peer = PeerCredentials::new(1, 1000, 1000);

        let ctx = web_context_for(&assembly, &root, &approver, peer, PermissionTier::Observer);

        let permissions = ctx.sandbox().permissions();
        assert!(permissions.contains(Permission::ReadWorkspace));
        assert!(!permissions.contains(Permission::WriteWorkspace));
        assert!(!permissions.contains(Permission::ExecuteProcess));
        assert!(permissions.is_subset_of(root.permissions()));
        assert!(ctx.service::<Arc<ApprovalStore>>().is_some());
        let principal = ctx
            .service::<Principal>()
            .ok_or(TestError::Missing("principal is registered"))?;
        assert_eq!(principal.tier(), PermissionTier::Observer);
        Ok(())
    }

    /// Kein Tier erhält mehr als die Web-Wurzel-Sandbox.
    #[test]
    fn test_web_op_context_never_exceeds_root_sandbox_for_any_tier() -> TestResult {
        let (_root_dir, root) = test_root_sandbox()?;
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
        Ok(())
    }

    /// Mit `PeerCredentials`, `ApprovalActorResolver` und `ApprovalStore` in
    /// der `ServiceMap` antwortet `approval.pending` nicht mit
    /// `OpError::NotAvailable`. Öffnet nur ein temporäres Verzeichnis.
    #[tokio::test]
    async fn test_approval_pending_is_reachable_once_peer_and_resolver_are_wired() -> TestResult {
        use harw_operations::OpError;
        use harw_operations::operation::OpInput;

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let (_root_dir, root) = test_root_sandbox()?;

        let store_root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let approval_store = Arc::new(ApprovalStore::new(store_root.path()));
        let peer = PeerCredentials::new(1, 1000, 1000);
        let approver: Arc<dyn ApprovalActorResolver> = Arc::new(StaticUidApprovalActorMap::new(
            vec![(1000, "owner".to_owned())],
        ));

        let op = Arc::clone(
            registry
                .find_by_name("approval.pending")
                .ok_or(TestError::Missing("approval.pending ist registriert"))?,
        );
        let ctx = web_op_context(
            registry_map(&registry),
            Some(&approval_store),
            &root,
            &approver,
            peer,
            PermissionTier::Owner,
        );

        let result = op
            .run(&ctx, OpInput::model_tool(serde_json::json!({})))
            .await;

        assert!(
            !matches!(result, Err(OpError::NotAvailable(_))),
            "approval.pending sollte erreichbar sein, war aber: {result:?}"
        );
        Ok(())
    }

    /// Eine `uid` ohne Eintrag in der Genehmiger-Tabelle wird abgewiesen —
    /// nie auf einen Vorgabe-Actor abgebildet.
    #[tokio::test]
    async fn test_approval_resolve_rejects_a_peer_unknown_to_the_approver_table() -> TestResult {
        use harw_operations::OpError;
        use harw_operations::operation::OpInput;

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let (_root_dir, root) = test_root_sandbox()?;

        let store_root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let approval_store = Arc::new(ApprovalStore::new(store_root.path()));
        // Dieselbe Komposition wie in `serve_web`: nur die eigene uid (hier
        // 1000) ist eingetragen — 9999 bleibt bewusst ohne Eintrag.
        let approver: Arc<dyn ApprovalActorResolver> = Arc::new(StaticUidApprovalActorMap::new(
            vec![(1000, "owner".to_owned())],
        ));
        let unknown_peer = PeerCredentials::new(2, 9999, 9999);

        let op = Arc::clone(
            registry
                .find_by_name("approval.resolve")
                .ok_or(TestError::Missing("approval.resolve ist registriert"))?,
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
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet NotAvailable(..keinem Genehmiger..), erhalten: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Der `ApprovalActor` stammt ausschließlich aus den Peer-Credentials,
    /// nie aus dem Anfragerumpf.
    #[tokio::test]
    async fn test_approval_resolve_derives_the_actor_from_peer_credentials_not_the_body()
    -> TestResult {
        use harw_operations::operation::OpInput;
        use harw_session_store::approval::ApprovalRecord;
        use harw_types::{ApprovalActor, Clock, ItemId, ToolCallId};
        use jiff::Timestamp;

        // Feste Testuhr (siehe `harw-types/src/clock.rs`, Modul-Doku
        // "Serveruhr" in `harw-ops/src/approval.rs`): `resolve_approval`
        // prüft die TTL gegen den registrierten `Arc<dyn Clock>`-Service,
        // niemals gegen ein vom Client mitgeschicktes `resolved_at`. Ohne
        // diesen Service fällt `server_clock` auf `SystemClock` (die echte
        // Wanduhr) zurück — die fixe `issued_at` von 1970 wäre dann relativ
        // zur echten Gegenwart immer abgelaufen. Der Fixture-Fehler lag
        // also im Test, der bislang keine Uhr injizierte, nicht in der
        // Ablaufprüfung selbst.
        struct FixedClock(Timestamp);
        impl Clock for FixedClock {
            fn now(&self) -> Timestamp {
                self.0
            }
        }
        let issued_at = Timestamp::constant(1, 0);

        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let (_root_dir, root) = test_root_sandbox()?;

        let store_root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let approval_store = Arc::new(ApprovalStore::new(store_root.path()));
        approval_store
            .issue(&ApprovalRecord {
                request: ItemId::from_str("approval-1"),
                session: SessionId::from_str("session-1"),
                call_id: ToolCallId::from_str("call-1"),
                actor: ApprovalActor::Operator {
                    id: "owner".to_owned(),
                },
                issued_at,
                tenant: None,
            })
            .map_err(ctx("issue succeeds against a fresh store"))?;

        let peer = PeerCredentials::new(1, 1000, 1000);
        let approver: Arc<dyn ApprovalActorResolver> = Arc::new(StaticUidApprovalActorMap::new(
            vec![(1000, "owner".to_owned())],
        ));

        let op = Arc::clone(
            registry
                .find_by_name("approval.resolve")
                .ok_or(TestError::Missing("approval.resolve ist registriert"))?,
        );
        let mut services = registry_map(&registry);
        let clock: Arc<dyn Clock> = Arc::new(FixedClock(issued_at));
        services.insert(clock);
        let ctx = web_op_context(
            services,
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
        let output =
            op.run(&ctx, OpInput::model_tool(args))
                .await
                .map_err(crate::test_support::ctx(
                    "resolve succeeds: peer.uid ist in der Genehmiger-Tabelle",
                ))?;

        assert!(output.text.contains("owner"));
        Ok(())
    }

    /// Eine halb geöffnete Planungsfläche wird nie an die Montage gereicht.
    ///
    /// `crate::build_plan_services` liefert `crate::PlanServices`, dessen
    /// [`crate::PlanServices::to_runtime`] serve_web direkt in
    /// `harw_runtime::PlanServices` übersetzt (seit W2d-2/F-MAIN: keine
    /// separate `runtime_plan_services`-Übersetzung mehr).
    #[test]
    fn test_plan_services_to_runtime_requires_all_three_stores() {
        let disabled = crate::PlanServices {
            plan: None,
            goal: None,
            findings: None,
            config: harw_plan::PlanToolConfig::default(),
        };
        assert!(disabled.to_runtime().is_none());

        let partial = crate::PlanServices {
            plan: Some(Arc::new(harw_plan::InMemoryPlanStore::new())),
            goal: Some(Arc::new(harw_plan::InMemoryGoalStore::new())),
            findings: None,
            config: harw_plan::PlanToolConfig::default(),
        };
        assert!(partial.to_runtime().is_none());

        let complete = crate::PlanServices {
            plan: Some(Arc::new(harw_plan::InMemoryPlanStore::new())),
            goal: Some(Arc::new(harw_plan::InMemoryGoalStore::new())),
            findings: Some(Arc::new(harw_plan_bridge::FindingStore::new(
                "/nonexistent/w2d1-b1/plans",
            ))),
            config: harw_plan::PlanToolConfig::default(),
        };
        assert!(complete.to_runtime().is_some());
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
    fn test_web_route_table_from_registry_includes_approval_routes() -> TestResult {
        use harw_web::router::WebMethod;

        let (_home, _cwd, assembly) = test_assembly(true)?;

        let routes = WebRouteTable::from_registry(assembly.operations())
            .map_err(ctx("no two operations claim the same web path"))?;

        let pending = routes
            .find("/api/approval-pending")
            .ok_or(TestError::Missing(
                "approval.pending is registered as a web route from the assembly",
            ))?;
        assert_eq!(pending.operation_name(), "approval.pending");
        assert_eq!(
            routes.method_for("/api/approval-pending"),
            Some(WebMethod::Get),
            "approval.pending is declared readonly (GET)"
        );

        let resolve = routes
            .find("/api/approval-resolve")
            .ok_or(TestError::Missing(
                "approval.resolve is registered as a web route from the assembly",
            ))?;
        assert_eq!(resolve.operation_name(), "approval.resolve");
        assert_eq!(
            routes.method_for("/api/approval-resolve"),
            Some(WebMethod::Post),
            "approval.resolve is declared mutating (POST)"
        );
        Ok(())
    }

    // ── Listener-Plan: --system / --systemd-socket / Entwicklungs-Rückfall (H9) ──

    fn options(system: bool, systemd_socket: bool) -> WebListenOptions {
        WebListenOptions {
            system,
            systemd_socket,
            ..WebListenOptions::default()
        }
    }

    /// Ohne Flag: `<home>/web.sock` (Entwicklungs-Rückfall).
    #[test]
    fn test_default_plan_binds_home_web_sock() -> TestResult {
        let home = Path::new("/home/dev/.harw");
        let plan = resolve_listen_plan(home, &options(false, false), Path::new("/nonexistent"))
            .map_err(ctx("Entwicklungsplan"))?;
        assert_eq!(
            plan,
            ListenPlan::Bind {
                path: home.join("web.sock"),
                system: false,
                group: None,
            }
        );
        Ok(())
    }

    /// `--socket` überschreibt den Entwicklungs-Pfad.
    #[test]
    fn test_default_plan_honours_socket_override() -> TestResult {
        let listen = WebListenOptions {
            socket: Some(PathBuf::from("/tmp/x.sock")),
            ..WebListenOptions::default()
        };
        let plan = resolve_listen_plan(Path::new("/h"), &listen, Path::new("/nonexistent"))
            .map_err(ctx("Plan mit --socket"))?;
        assert_eq!(
            plan,
            ListenPlan::Bind {
                path: PathBuf::from("/tmp/x.sock"),
                system: false,
                group: None,
            }
        );
        Ok(())
    }

    /// `--system`: `<runtime_dir>/control.sock`, nie unter dem Home.
    #[test]
    fn test_system_plan_uses_runtime_dir_control_sock_not_home() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let runtime = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let plan = resolve_listen_plan(home.path(), &options(true, false), runtime.path())
            .map_err(ctx("Systemplan"))?;
        let (path, system, group) = match plan {
            ListenPlan::Bind {
                path,
                system,
                group,
            } => (path, system, group),
            other @ ListenPlan::Activated => {
                return Err(TestError::Unexpected(format!(
                    "erwartete Bind, bekam {other:?}"
                )));
            }
        };
        assert_eq!(path, runtime.path().join("control.sock"));
        assert!(!path.starts_with(home.path()));
        assert!(system);
        assert_eq!(group, None);
        Ok(())
    }

    /// `--system` ohne Laufzeitverzeichnis: klarer Fehler, kein Rückfall auf
    /// `<home>/web.sock` — auch wenn das Home existiert.
    #[test]
    fn test_system_plan_without_runtime_dir_fails_without_home_fallback() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let runtime = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let missing = runtime.path().join("infra");
        let result = resolve_listen_plan(home.path(), &options(true, false), &missing);
        let message = match result {
            Err(message) => message,
            Ok(plan) => {
                return Err(TestError::Unexpected(format!(
                    "fehlendes Laufzeitverzeichnis muss scheitern, bekam {plan:?}"
                )));
            }
        };
        assert!(
            message.contains(&missing.display().to_string()),
            "{message}"
        );
        assert!(message.contains("keinen Rückfall"), "{message}");
        assert!(message.contains("--systemd-socket"), "{message}");
        Ok(())
    }

    /// Die produktive Vorgabe ist `/run/harw/infra/control.sock` (D1).
    #[test]
    fn test_system_socket_default_is_run_harw_infra_control_sock() {
        assert_eq!(
            Path::new(SYSTEM_RUNTIME_DIR).join(SYSTEM_SOCKET_FILE_NAME),
            PathBuf::from("/run/harw/infra/control.sock")
        );
        assert_eq!(SYSTEM_SOCKET_MODE, 0o660);
    }

    /// `--system --socket` muss absolut sein und ein existierendes
    /// Elternverzeichnis haben.
    #[test]
    fn test_system_plan_with_socket_override_checks_its_parent() -> TestResult {
        let runtime = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let explicit = runtime.path().join("control.sock");
        let listen = WebListenOptions {
            socket: Some(explicit.clone()),
            system: true,
            socket_group: Some("harw-control".to_owned()),
            ..WebListenOptions::default()
        };
        let plan = resolve_listen_plan(Path::new("/h"), &listen, Path::new("/nonexistent"))
            .map_err(ctx("Systemplan mit --socket"))?;
        assert_eq!(
            plan,
            ListenPlan::Bind {
                path: explicit,
                system: true,
                group: Some("harw-control".to_owned()),
            }
        );

        let relative = WebListenOptions {
            socket: Some(PathBuf::from("control.sock")),
            system: true,
            ..WebListenOptions::default()
        };
        assert!(resolve_listen_plan(Path::new("/h"), &relative, runtime.path()).is_err());

        let orphan = WebListenOptions {
            socket: Some(runtime.path().join("fehlt").join("control.sock")),
            system: true,
            ..WebListenOptions::default()
        };
        assert!(resolve_listen_plan(Path::new("/h"), &orphan, runtime.path()).is_err());
        Ok(())
    }

    /// `--systemd-socket` bindet nichts — auch nicht, wenn das
    /// Laufzeitverzeichnis fehlt — und verträgt kein `--socket`/`--socket-group`.
    #[test]
    fn test_systemd_socket_plan_is_activated_and_exclusive() -> TestResult {
        let plan = resolve_listen_plan(
            Path::new("/h"),
            &options(false, true),
            Path::new("/nonexistent"),
        )
        .map_err(ctx("Aktivierungsplan"))?;
        assert_eq!(plan, ListenPlan::Activated);

        let with_socket = WebListenOptions {
            socket: Some(PathBuf::from("/run/harw/infra/control.sock")),
            systemd_socket: true,
            ..WebListenOptions::default()
        };
        assert!(
            resolve_listen_plan(Path::new("/h"), &with_socket, Path::new("/nonexistent")).is_err()
        );
        let with_group = WebListenOptions {
            systemd_socket: true,
            socket_group: Some("0".to_owned()),
            ..WebListenOptions::default()
        };
        assert!(
            resolve_listen_plan(Path::new("/h"), &with_group, Path::new("/nonexistent")).is_err()
        );
        Ok(())
    }

    /// `--socket-group` ohne `--system` ist ein Fehler.
    #[test]
    fn test_socket_group_requires_system_mode() {
        let listen = WebListenOptions {
            socket_group: Some("harw-control".to_owned()),
            ..WebListenOptions::default()
        };
        assert!(resolve_listen_plan(Path::new("/h"), &listen, Path::new("/nonexistent")).is_err());
    }

    /// Ohne Aktivierungsumgebung scheitert `--systemd-socket` beim Vorbereiten
    /// (fail closed), statt still nichts zu binden.
    #[test]
    fn test_prepare_activated_without_systemd_env_fails() {
        if std::env::var("LISTEN_PID")
            .ok()
            .and_then(|pid| pid.parse::<u32>().ok())
            == Some(std::process::id())
        {
            return;
        }
        let result = prepare_listener(ListenPlan::Activated);
        assert!(
            matches!(&result, Err(message) if message.contains("systemd")),
            "Aktivierung ohne LISTEN_PID muss scheitern"
        );
    }

    #[test]
    fn test_resolve_group_by_number_and_name() -> TestResult {
        let etc_group =
            "# Kommentar\nroot:x:0:\nharw-control:x:991:\nharw-network:x:992:alice,bob\n";
        assert_eq!(resolve_group("0", etc_group).map_err(ctx("GID 0"))?, 0);
        assert_eq!(resolve_group("1234", "").map_err(ctx("numerisch"))?, 1234);
        assert_eq!(
            resolve_group("harw-control", etc_group).map_err(ctx("Name"))?,
            991
        );
        assert_eq!(
            resolve_group("harw-network", etc_group).map_err(ctx("Name"))?,
            992
        );
        assert!(resolve_group("fehlt", etc_group).is_err());
        assert!(resolve_group("kaputt", "kaputt:x:nan:\n").is_err());
        assert!(resolve_group("4294967295", "").is_err());
        Ok(())
    }

    // ── [web.identity] → WebIdentityConfig (H12) ────────────────────────────

    /// Ohne `[web.identity]` entsteht die Vorgabe (`tier_map` ohne Mandanten),
    /// deren Resolver exakt der bisherigen Tier-Tabelle folgt.
    #[tokio::test]
    async fn test_web_identity_config_without_table_is_default_tier_map() -> TestResult {
        let config = web_identity_config(None);
        assert_eq!(config, WebIdentityConfig::default());
        let authorizer: Arc<dyn harw_web::PeerAuthorizer> =
            Arc::new(StaticUidTierMap::new(vec![(1000, PermissionTier::Owner)]));
        let resolver = config
            .build_resolver(authorizer)
            .map_err(ctx("default resolver"))?;
        let owner = resolver
            .resolve(&PeerCredentials::new(1, 1000, 1000), None)
            .await
            .map_err(ctx("owner uid resolves"))?;
        assert_eq!(owner.tier(), PermissionTier::Owner);
        assert_eq!(owner.tenant(), None);
        let stranger = resolver
            .resolve(&PeerCredentials::new(2, 4242, 4242), None)
            .await;
        assert_eq!(
            stranger.err(),
            Some(harw_web::identity::IdentityError::UnknownPeer)
        );
        Ok(())
    }

    /// Die rohe Tabelle aus der TOML-Datei wird Feld für Feld übernommen.
    #[test]
    fn test_web_identity_config_converts_every_field() -> TestResult {
        let section: harw_config::WebSection = toml::from_str(
            r#"
            [identity]
            mode = "security_hub"
            security_socket = "/run/harw/infra/security.sock"
            require_context = true
            uid_tenants = { "1000" = "tenant-a" }
            uid_principals = { "1000" = "alice" }
            "#,
        )
        .map_err(ctx("parse [web.identity]"))?;
        let config = web_identity_config(section.identity.as_ref());
        assert_eq!(config.mode, IdentityMode::SecurityHub);
        assert_eq!(
            config.security_socket,
            Some(PathBuf::from("/run/harw/infra/security.sock"))
        );
        assert!(config.require_context);
        assert_eq!(
            config.uid_tenants,
            [("1000".to_owned(), "tenant-a".to_owned())].into()
        );
        assert_eq!(
            config.uid_principals,
            [("1000".to_owned(), "alice".to_owned())].into()
        );
        Ok(())
    }

    /// `tier_map` mit `uid_tenants`: der konvertierte Resolver liefert den
    /// konfigurierten Mandanten der Peer-UID.
    #[tokio::test]
    async fn test_web_identity_config_tier_map_tenants_reach_the_resolver() -> TestResult {
        let raw = WebIdentityToml {
            uid_tenants: [("1000".to_owned(), "tenant-a".to_owned())].into(),
            ..WebIdentityToml::default()
        };
        let authorizer: Arc<dyn harw_web::PeerAuthorizer> =
            Arc::new(StaticUidTierMap::new(vec![(1000, PermissionTier::Owner)]));
        let resolver = web_identity_config(Some(&raw))
            .build_resolver(authorizer)
            .map_err(ctx("tier_map resolver"))?;
        let resolved = resolver
            .resolve(&PeerCredentials::new(1, 1000, 1000), None)
            .await
            .map_err(ctx("owner uid resolves"))?;
        assert_eq!(
            resolved.tenant().map(harw_types::TenantId::as_str),
            Some("tenant-a")
        );
        Ok(())
    }

    /// Eine semantisch ungültige Tabelle scheitert beim Bau des Resolvers
    /// (Startabbruch in `serve_web`), nicht still.
    #[test]
    fn test_web_identity_config_invalid_table_fails_to_build() {
        let raw = WebIdentityToml {
            uid_principals: [("1000".to_owned(), "alice".to_owned())].into(),
            ..WebIdentityToml::default()
        };
        let authorizer: Arc<dyn harw_web::PeerAuthorizer> =
            Arc::new(StaticUidTierMap::new(vec![(1000, PermissionTier::Owner)]));
        assert_eq!(
            web_identity_config(Some(&raw))
                .build_resolver(authorizer)
                .err(),
            Some(harw_web::identity::IdentityConfigError::SecurityHubOnly {
                field: "uid_principals"
            })
        );
    }

    /// Nach dem Binden: Modus 0660, Gruppe unverändert bzw. gesetzt.
    #[test]
    fn test_apply_system_socket_permissions_sets_0660_and_group() -> TestResult {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("control.sock");
        let _listener =
            std::os::unix::net::UnixListener::bind(&path).map_err(ctx("Socket bindbar"))?;
        let before = std::fs::metadata(&path).map_err(ctx("metadata"))?;

        apply_system_socket_permissions(&path, None).map_err(ctx("ohne Gruppe"))?;
        let after = std::fs::metadata(&path).map_err(ctx("metadata"))?;
        assert_eq!(after.permissions().mode() & 0o777, 0o660);
        assert_eq!(after.gid(), before.gid(), "ohne --socket-group unverändert");

        // Die eigene Gruppe darf jeder Eigentümer setzen (kein CAP_CHOWN nötig).
        apply_system_socket_permissions(&path, Some(before.gid())).map_err(ctx("eigene Gruppe"))?;
        let again = std::fs::metadata(&path).map_err(ctx("metadata"))?;
        assert_eq!(again.gid(), before.gid());
        assert_eq!(again.permissions().mode() & 0o777, 0o660);
        Ok(())
    }
}
