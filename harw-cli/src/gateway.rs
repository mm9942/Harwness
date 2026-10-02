//! `harw gateway` — persistenter Hintergrund-Daemon (openclaw-Stil).
//!
//! Dieser Daemon ersetzt `harw serve` als systemd-`ExecStart`. Er hält die
//! langlebige Runtime am Leben und supervidiert vier Subsysteme:
//!
//! - **Agenten/Gateway** — der Provider-Weg über die Gateway-`RuntimeAssembly`
//!   (mit Secret-Resolver für versiegelte `secrets:`-Credentials, siehe
//!   [`crate::runtime_gateway::gateway_assembly`]), an den Nachrichten als
//!   Turns gehen.
//! - **Channels/Telegram** — je aktivierter `[[channel.telegram]]`-Bindung
//!   ein eigener, supervidierter Ingress-Task (Long-Poll oder Webhook, siehe
//!   [`telegram_ingress_modes`]). Jede Bindung, die ein Vertrauens-Gate nicht
//!   besteht (keine gepinnten Identitäten, nicht auflösbares Credential,
//!   ungültige Webhook-Angaben, geteilter Bot-Token), bleibt einzeln
//!   fail-closed deaktiviert, ohne die übrigen Bindungen mitzureißen.
//! - **Knowledge/Workbench** — [`harw_knowledge::KnowledgeStore`] wird am
//!   Profil-Wissensordner gemountet; beim Start werden persistente
//!   Workbench-Scopes (Session/Projekt-Arbeitssets) gescannt. Die Scopes
//!   überleben Neustarts, aber der Gateway persistiert keinen Laufzeit- oder
//!   Ausführungszustand, den er danach fortsetzen könnte.
//! - **Knowledge/Dream** — ein idle-getriggerter, **governter** Traum-Scheduler
//!   (via [`harw_job_runtime::Job`] + [`harw_job_runtime::Budget`]): schläft die
//!   KI (keine Channel-Aktivität), konsolidiert sie in einem budgetierten
//!   Reflexions-Turn und schreibt einen **review-gated** [`DreamReport`].
//!
//! Der Grund, dass Workbench/Dream hier leben: `harw-knowledge` liefert nur die
//! Datentypen und delegiert File-Watching (Workbench) und Job-Scheduling (Dream)
//! ausdrücklich an den „parent build" — das ist genau dieser Daemon.
//!
//! `harw` (ohne Subcommand) bleibt der TUI-Client; der Daemon läuft getrennt.
//!
//! # Telemetrie
//! `harw gateway` ist die Kompositionsstelle für die Telemetrie-Sinks
//! (`crate::observe`): der rotierende File-Sink unter `<home>/telemetry`
//! läuft immer; `--metrics-prometheus-port`/`--metrics-otlp-endpoint`
//! schalten je einen zusätzlichen Export für **nicht-geschützte** Metriken
//! (Präfix `"app."`) zu. `security.*`/`warden.*` erreichen unabhängig davon
//! ausschließlich den File-Sink — siehe `crate::observe`-Moduldoku für die
//! strukturelle Begründung.
//!
//! # Audit-Kettenprüfung (AW7-04-Scheduling-Knoten)
//! Zwei Fassungen dieses Knotens sind bereits in dieser Datei behoben
//! worden: die erste rief [`harw_secrets::ChainAuditMirror::verify_and_mirror`]
//! nie periodisch auf — der Nullzähler `audit_chain_break` konnte „Kette
//! intakt" nicht von „niemand hat nachgesehen" unterscheiden. Die zweite
//! rief ihn zwar periodisch auf, aber auf der **im Speicher gehaltenen**
//! Kette einer frisch geöffneten `SecretStore`-Instanz — die bleibt leer,
//! weil dieser Gateway-Prozess nie `create`/`rotate`/`delete` aufruft, und
//! eine Manipulationsprüfung auf einer stets leeren Kette prüft nichts. Die
//! Angriffsrichtung, um die es geht, ist die **Datei auf der Platte**.
//!
//! [`supervise`] schließt das über [`audit_chain_scheduler`]: alle
//! [`AUDIT_CHAIN_CHECK_DEFAULT_SECS`] Sekunden (überschreibbar über
//! [`AUDIT_CHAIN_CHECK_INTERVAL_ENV`], `0` deaktiviert) wird die persistierte
//! `audit.log`-Datei des **konfigurierten** `SecretStore` erneut geprüft —
//! über [`crate::secret_store::configured_secret_store_persisted_audit_chain_status`]
//! (die intern `SecretStore::verify_persisted_audit_chain` aufruft), je Tick
//! neu geöffnet statt über ein gehaltenes Handle (Begründung dort) — ohne die
//! Event-Loop zu blockieren (`tokio::task::spawn_blocking`, siehe dortige
//! Doku „Blockier-Falle"). Diese Prüfung unterscheidet **drei** Fälle, die
//! nie zusammenfallen dürfen: **nichts protokolliert** (Datei fehlt — kein
//! Fund), **unlesbar** (die Datei konnte nicht geprüft werden — weder
//! unversehrt noch gebrochen), und **manipuliert** (ein tatsächlicher
//! Kettenbruch). Nur Letzteres erhöht `audit_chain_break` **und** wird über
//! `tracing::error!`/`eprintln!` sichtbar gemeldet; der Gateway läuft
//! unverändert weiter. Ist kein Geheimnisspeicher konfiguriert, meldet der
//! Tick „nichts zu prüfen" statt „unversehrt". Siehe [`audit_chain_scheduler`]s
//! Doku für die volle Begründung — insbesondere, welche Kette geprüft wird
//! und was sie **nicht** entdecken kann, warum das Intervall über eine
//! Umgebungsvariable statt eines neuen `clap`-Schalters konfiguriert wird,
//! und wieso ein je Tick neu geöffnetes Handle hier kein Kompromiss, sondern
//! die einzig sinnvolle Wahl ist.
//!
//! # Fehler
//! Startfehler werden von [`run`] als `String` gemeldet und beenden den
//! Prozess, **bevor** irgendein Subsystem startet:
//!
//! - Home-Auflösung/Scaffolding und ein nicht lesbares Arbeitsverzeichnis.
//!   **cwd-Abhängigkeit:** der Gateway montiert gegen
//!   `std::env::current_dir()` — Repo-Layer-Erkennung (`<cwd>/.harw`),
//!   Vertrauensprüfung und Projekterkennung folgen damit dem
//!   Arbeitsverzeichnis des Prozesses (unter systemd `WorkingDirectory=`).
//! - **Provider fehlt/nicht baubar** (kein `default_provider`/`default_model`,
//!   unauflösbares Credential, ungültiger Endpoint): `Err("gateway: …")`,
//!   kein Echo-Fallback (G-048).
//! - **KEK fehlt:** ein aktivierter Provider mit `secrets:`-Referenz ohne
//!   `[kek]` in `auth.toml` endet mit
//!   `Err("gateway: enabled sealed-secret provider requires a configured KEK")`;
//!   ebenso ein nicht ladbares KEK-Material oder ein nicht zu öffnender
//!   versiegelter Speicher (Meldungen ohne Geheimnisinhalt).
//! - Konfigurations- und Vertrauensfehler (ungültige Config, defekter
//!   Trust-Store, nicht vertrauensfähiges Projekt).
//! - Telemetrie-Aufbau (belegter Prometheus-Port, `https://`-OTLP-Endpunkt)
//!   und Runtime-Start.
//!
//! Ein vorhandenes, aber **nicht freigegebenes** repo-lokales `.harw` ist
//! dagegen **kein** Fehler: es wird nur verengend übernommen und per
//! `tracing::warn!` gemeldet. Scheitert die Montage, startet auch die
//! periodische Audit-Kettenprüfung nicht — sie ist Teil von [`supervise`].
//!
//! Laufzeitfehler der Subsysteme werden geloggt und führen **nicht** zum
//! Prozess-Exit — der Daemon bleibt am Leben (kein Crash-Loop).

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, mpsc};
use std::task::Poll;
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};

use harw_channel::{Admission, ChannelAdapter, InboundEvent, PairingStore, SessionKey};
use harw_channel_telegram::{
    ApprovalTokenStore, ChatStateStore, PairingNotice, TelegramChannel, TelegramChannelConfig,
    ThrottleNotice, TopicMode, WorkRequestStore,
};
use harw_channel_telegram_transport::{
    AdmittedEventConsumer, LongPollConfig, LongPollShutdown, RendererConfig, TelegramClient,
    TelegramOffsetStore, TelegramOutbound, TelegramRenderer, TransportResult, WebhookConfig,
    run_webhook_server_with_shutdown, spawn_long_poll_thread,
};
use harw_config::{ChannelToml, ResolvedConfig, SecretRef, TelegramChannelToml, resolve_env_ref};
use harw_core::{ModelProvider, TranscriptStateStore};
use harw_knowledge::KnowledgeStore;
use harw_observe::TelemetrySink;
use harw_secrets::audit::chain::PersistedChainStatus;
use harw_secrets::audit::telemetry::AUDIT_CHAIN_BREAK;
use harw_secrets::{AuditError, AuditResult};
use harw_session_store::TranscriptStore;
use harw_types::{ChannelId, Principal, SessionId, TenantId, ThreadRef, WorkspaceId};
use secrecy::{ExposeSecret, SecretString};

use crate::home::resolve_home;
use crate::runtime_gateway::{GatewayEntry, channel_principal, gateway_assembly};

mod telegram_attachments;
mod telegram_callbacks;
mod telegram_commands;
mod telegram_session;

use telegram_attachments::{TelegramAttachmentIntake, telegram_attachment_cache_root};
use telegram_callbacks::{GatewayCallbackWorker, spawn_callback_worker};
use telegram_commands::{CommandDisposition, TelegramCommandHandler, UnknownCommandFallback};
use telegram_session::{TelegramSessionConfig, TelegramSessionDispatcher};

/// Zählt jeden Gateway-Start. Geroutet unter dem Präfix `"app."`
/// (`crate::observe`) — erreicht damit optional Prometheus/OTLP, nie ohne
/// eines der beiden Flags.
const GATEWAY_STARTED: harw_observe::MetricKey = harw_observe::MetricKey {
    name: "app.gateway_started_total",
    kind: harw_observe::MetricKind::Counter,
    unit: harw_observe::Unit::Count,
    labels: &[],
    cardinality: harw_observe::Cardinality::Single,
};

/// Name der Umgebungsvariable, die das Intervall der periodischen
/// Audit-Kettenprüfung überschreibt (siehe [`audit_chain_check_interval_secs`]
/// für die Begründung, warum dies eine Umgebungsvariable ist statt eines
/// neuen `clap`-Schalters in `crate::cli`).
const AUDIT_CHAIN_CHECK_INTERVAL_ENV: &str = "HARW_GATEWAY_AUDIT_CHAIN_CHECK_INTERVAL_SECS";

/// Vorgabe-Intervall der periodischen Audit-Kettenprüfung: 15 Minuten.
///
/// Begründung: nicht zu selten — ein Kettenbruch soll innerhalb desselben
/// Betriebstages sichtbar werden, nicht erst nach Tagen unbeobachteten
/// Betriebs; nicht zu häufig — die Prüfung soll die Event-Loop nicht
/// dominieren, und ein Bruch, der einmal erkannt wurde, verschwindet nicht
/// von selbst wieder, sodass keine Notwendigkeit besteht, sekündlich zu
/// prüfen. 15 Minuten liegt in derselben Größenordnung wie
/// [`DREAM_TICK`] bzw. der Traum-Leerlauf (Vorgabe 15 min), die in diesem Daemon bereits als
/// „spürbar, aber nicht aufdringlich" etabliert sind.
const AUDIT_CHAIN_CHECK_DEFAULT_SECS: u64 = 15 * 60;

/// Liest das Prüfintervall der periodischen Audit-Kettenprüfung aus einem
/// rohen Umgebungswert (siehe [`AUDIT_CHAIN_CHECK_INTERVAL_ENV`]).
///
/// # Description
/// **Warum eine Umgebungsvariable statt eines neuen `clap`-Schalters in
/// `crate::cli`:** Der naheliegende Ort wäre ein neues Feld auf
/// `crate::cli::TelemetryArgs` (das über `Command::Gateway` bereits
/// unverändert an [`run`] durchgereicht wird) oder ein neues Feld direkt auf
/// der `Command::Gateway`-Variante. Beide Wege zwingen jedoch eine Datei
/// außerhalb des Schreibbereichs dieses Knotens zum Nicht-mehr-Kompilieren:
/// ein neues `TelemetryArgs`-Feld bricht die vollständige
/// Struct-Literal-Konstruktion `TelemetryArgs { metrics_prometheus_port:
/// None, metrics_otlp_endpoint: None }` in `harw-cli/src/observe.rs`s
/// Testcode (E0063, fehlendes Feld); ein neues Feld auf `Command::Gateway`
/// selbst bricht das Destrukturierungsmuster `Some(Command::Gateway {
/// telemetry }) => gateway::run(home_override, telemetry)` in
/// `harw-cli/src/main.rs` (ebenfalls E0063). Beide Dateien sind ausdrücklich
/// nicht Teil des Schreibbereichs dieses Knotens. Eine Umgebungsvariable
/// erreicht [`run`] ohne eine einzige Signatur oder ein
/// Destrukturierungsmuster außerhalb dieser Datei zu ändern — auf Kosten
/// eines fehlenden `harw gateway --help`-Eintrags, den ein Folge-Knoten mit
/// Schreibzugriff auf `main.rs`/`observe.rs` nachrüsten kann.
///
/// # Arguments
/// - `raw` (`Option<&str>`): der rohe Umgebungswert, oder `None`, wenn die
///   Variable nicht gesetzt ist. Als Parameter statt eines direkten
///   `std::env::var`-Aufrufs gehalten, damit Tests ohne echte
///   Prozessumgebung laufen.
///
/// # Returns
/// Das Intervall in Sekunden; `0` bedeutet „Prüfung deaktiviert". Ein
/// fehlender oder nicht als `u64` parsbarer Wert fällt auf
/// [`AUDIT_CHAIN_CHECK_DEFAULT_SECS`] zurück, statt den Gateway-Start
/// scheitern zu lassen.
fn audit_chain_check_interval_secs(raw: Option<&str>) -> u64 {
    match raw {
        Some(value) => value
            .trim()
            .parse::<u64>()
            .unwrap_or(AUDIT_CHAIN_CHECK_DEFAULT_SECS),
        None => AUDIT_CHAIN_CHECK_DEFAULT_SECS,
    }
}

/// A Telegram binding that has passed every gateway-owned prerequisite.
struct TelegramIngressPlan {
    binding: harw_config::TelegramChannelToml,
    bot_token: SecretString,
    /// Aufgelöster Transport dieser Bindung (Long-Poll oder Webhook).
    transport: TelegramTransportPlan,
}

/// Aufgelöster Ingress-Transport einer Bindung.
enum TelegramTransportPlan {
    /// `getUpdates`-Long-Poll mit profil- und bindungsbezogenem Offset.
    LongPoll {
        /// `true`, wenn `transport = "webhook"` konfiguriert ist, aber der
        /// `[channel.telegram.transport_webhook]`-Block fehlt: dann fällt die
        /// Bindung sichtbar auf Long-Poll zurück, statt stumm zu bleiben.
        webhook_fallback: bool,
    },
    /// Telegram-Webhook: `setWebhook` beim Start, lokaler Listener,
    /// `deleteWebhook` beim geordneten Shutdown.
    Webhook(TelegramWebhookPlan),
}

/// Vollständig aufgelöste Webhook-Angaben einer Bindung.
struct TelegramWebhookPlan {
    /// Öffentliche HTTPS-URL, die Telegram per `setWebhook` erhält.
    public_url: String,
    /// Pfad-Anteil von `public_url`; der lokale Listener lauscht auf genau
    /// diesem Pfad (ein Reverse-Proxy muss ihn unverändert weiterreichen).
    route: String,
    /// Lokale Listen-Adresse des Webhook-Servers.
    listen_addr: SocketAddr,
    /// Aufgelöstes `secret_token` (`X-Telegram-Bot-Api-Secret-Token`).
    secret_token: SecretString,
}

/// Rein aus der Bindungskonfiguration abgeleitete Transportwahl (noch ohne
/// Geheimnisauflösung), siehe [`telegram_transport_choice`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum TelegramTransportChoice {
    /// Long-Poll; `webhook_fallback` wie in [`TelegramTransportPlan::LongPoll`].
    LongPoll { webhook_fallback: bool },
    /// Webhook mit bereits validierter URL, Route und Listen-Adresse.
    Webhook {
        public_url: String,
        route: String,
        listen_addr: SocketAddr,
    },
}

/// Ingress-Entscheidung für genau eine aktivierte Telegram-Bindung.
struct TelegramBindingIngress {
    /// Die konfigurierte Bindungs-ID (`ChannelId`, z. B. `telegram:ops`).
    id: String,
    mode: TelegramIngressMode,
}

/// A Telegram binding remains disabled unless construction completed all
/// trust gates.
///
/// `Enabled` boxes its payload: [`TelegramIngressPlan`] embeds the full
/// [`harw_config::TelegramChannelToml`] (groups/topics/security/rate-limit/
/// attachments/commands sub-configs), which is far larger than the
/// `Disabled(String)` arm. Boxing avoids paying that size on every
/// `TelegramIngressMode` value — this is constructed once per gateway start
/// and matched a handful of times, so the extra indirection has no
/// measurable cost and callers only need a trivial `*plan` deref.
enum TelegramIngressMode {
    Enabled(Box<TelegramIngressPlan>),
    Disabled(String),
}

/// `/pair <code>` belongs to the local `harw connect` workflow, never to the
/// conversational model. Once a peer is paired, old/replayed pairing commands
/// are still admitted by the channel; discard them here so they cannot trigger
/// an arbitrary model answer.
///
/// Telegram appends `@<bot-username>` to commands sent in groups (and clients
/// may send it in any letter case), so `/PAIR@LinLinBot` must be recognized
/// the same as `/pair`. The whole leading token is lowered (ASCII-only —
/// Telegram command/username characters are always ASCII) before comparison,
/// which keeps this case-insensitive without risking a byte-boundary panic
/// from slicing a raw `&str`. It stays strict otherwise: `/pairing` and
/// `/pairx` do not share the `/pair`/`/pair@` prefix, and plain text
/// containing "pair" never reaches this check because only the first
/// whitespace-delimited token is considered.
fn is_telegram_pairing_command(text: &str) -> bool {
    let command = text
        .trim()
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default();
    let lowered = command.to_ascii_lowercase();
    lowered == "/pair"
        || lowered
            .strip_prefix("/pair@")
            .is_some_and(|bot| !bot.is_empty())
}

/// Runtime consumer registered by this gateway composition. It deliberately
/// accepts only events already admitted by `TelegramChannel`.
///
/// Reihenfolge je Ereignis: `/pair` wird verworfen (gehört zum lokalen
/// `harw connect`-Ablauf), geschlossene Befehle verarbeitet der
/// [`TelegramCommandHandler`], alles andere (Text und/oder Anhänge) reiht
/// der [`TelegramSessionDispatcher`] nicht blockierend in die FIFO des Chats
/// ein. Der Consumer hält den Dispatcher am Leben: endet der Admission-Thread
/// (Transport-Neustart), werden Worker und Sweeper mit ihm beendet.
struct GatewayTelegramConsumer {
    commands: TelegramCommandHandler,
    sessions: TelegramSessionDispatcher,
}

impl AdmittedEventConsumer for GatewayTelegramConsumer {
    fn handle_admitted(&self, key: SessionKey, event: InboundEvent) {
        let text = event.text.clone().filter(|text| !text.trim().is_empty());
        if text.is_none() && event.attachments.is_empty() {
            tracing::warn!(channel = %key.channel, peer = %key.peer, "Telegram event has neither text nor attachments for runtime handoff");
            return;
        }
        if let Some(text) = text.as_deref() {
            if is_telegram_pairing_command(text) {
                tracing::debug!(channel = %key.channel, peer = %key.peer, "Telegram pairing command consumed outside model runtime");
                return;
            }
            if self.commands.handle(&key, &event, text) == CommandDisposition::Handled {
                return;
            }
        }
        self.sessions.dispatch(key, event);
    }
}

/// Tick-Intervall des Traum-Schedulers (Idle-/Zeitplan-Prüfung). Leerlauf,
/// Cooldown, Budget und Zeitplan kommen aus `[dream]`
/// (`harw_ops::dream_run::DreamSettings`).
const DREAM_TICK: Duration = Duration::from_secs(60);

/// Ein Zeitstempel-Griff für die Idle-Erkennung des Traum-Schedulers.
type ActivityClock = Arc<Mutex<Instant>>;

/// Startet den Gateway-Daemon (blockiert bis SIGINT/SIGTERM).
///
/// # Description
/// Baut vor dem Eintritt in die `tokio`-Runtime den Telemetrie-Sink
/// ([`crate::observe::build`]) und schreibt eine Start-Zählung
/// (`app.gateway_started_total`) — beide Schritte laufen bewusst **außerhalb**
/// der Runtime, weil ein aktivierter OTLP-Export intern selbst eine
/// `tokio`-Runtime aufbaut und ein verschachtelter `block_on`-Aufruf
/// paniken würde (siehe `crate::observe`-Moduldoku, Abschnitt „Warum
/// außerhalb der Runtime"). Der Sink wird über das Ende von
/// [`supervise`] hinaus gehalten (`_telemetry.sink`) und danach — wieder
/// außerhalb der Runtime — geflusht, damit ein zusätzlich aktivierter
/// Prometheus-/OTLP-Endpunkt nicht mitten im letzten Stapel abreißt.
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter Root-Space (`--home`).
/// - `telemetry` (`crate::cli::TelemetryArgs`): `--metrics-prometheus-port`/
///   `--metrics-otlp-endpoint`; ohne beide bleibt nur der immer aktive
///   File-Sink an (siehe `crate::observe`-Moduldoku).
///
/// # Errors
/// Ein `String` — jeweils bevor ein Subsystem (auch die Audit-Kettenprüfung)
/// startet — bei:
/// - Home-Auflösung, Scaffolding oder nicht lesbarem Arbeitsverzeichnis
///   (`std::env::current_dir()`; Repo-Layer, Vertrauen und Projekterkennung
///   der Montage folgen diesem cwd).
/// - fehlgeschlagener Gateway-Montage ([`mount_gateway_assembly`], Präfix
///   `"gateway: "`): Konfiguration, Vertrauen (defekter Trust-Store, nicht
///   vertrauensfähiges Projekt), **Provider fehlt/nicht baubar** — kein
///   Echo-Fallback, G-048 — und **KEK fehlt** für einen aktivierten
///   `secrets:`-Provider (`"gateway: enabled sealed-secret provider requires
///   a configured KEK"`) bzw. nicht ladbares KEK-Material/nicht zu öffnender
///   versiegelter Speicher.
/// - fehlgeschlagenem Telemetrie-Aufbau (z. B. ein belegter Prometheus-Port
///   oder ein `https://`-OTLP-Endpunkt) oder Runtime-Start.
///
/// Ein nicht freigegebenes repo-lokales `.harw` ist **kein** Fehler, sondern
/// nur eine `tracing::warn!`-Meldung (verengende Übernahme).
pub fn run(
    home_override: Option<PathBuf>,
    telemetry: crate::cli::TelemetryArgs,
    session_socket: Option<Option<PathBuf>>,
) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    crate::home::ensure_home(&home).map_err(|error| error.to_string())?;
    let cwd = std::env::current_dir()
        .map_err(|error| format!("gateway: Arbeitsverzeichnis nicht lesbar: {error}"))?;

    // Profil vor der Montage auflösen: der Telegram-Verlaufsspeicher der
    // Assembly liegt unter der Session-Wurzel des Profils (`[session]
    // store_dir`, Vorgabe `<profil>/sessions`), derselben Wurzel wie Dream.
    let profile_name = harw_home::active_profile_name(&home);
    let profile = harw_home::profile_dir(&home, &profile_name).map_err(|e| e.to_string())?;
    // Einmal auflösen und überall durchreichen (Montage, Dream, Telegram-
    // Consumer), damit ein angepasstes `store_dir` nirgends auseinanderläuft.
    let sessions_root = crate::runtime_entry::profile_sessions_root(&home)?;

    // Die Gateway-Montagen ersetzen `config_layers` + `discover_config` +
    // `validate` und den früher separat gebauten Provider. Scheitert der
    // Provider-Aufbau, endet `run` mit `Err` — kein Echo-Fallback (G-048).
    // Dream und jede aktivierte Telegram-Bindung bekommen je eine eigene
    // Montage (G1/G3), damit Audit und Trace den auslösenden Kanal — und bei
    // Telegram die auslösende Bindung — unterscheiden.
    let assemblies = mount_gateway_assembly(&home, &cwd, &sessions_root)?;
    // Shared with `audit_chain_scheduler`, which clones this `Arc` into a
    // fresh `spawn_blocking` closure on every tick (see its doc for why it
    // re-opens the configured secret store each tick instead of holding one).
    let config = Arc::clone(assemblies.dream.config());
    let session_model = Arc::clone(assemblies.dream.model());
    let providers = GatewayProviders {
        telegram: assemblies
            .telegram
            .iter()
            .map(|entry| (entry.binding_id.clone(), Arc::clone(entry.assembly.model())))
            .collect(),
        dream: Arc::clone(assemblies.dream.model()),
    };

    let telemetry_sinks = crate::observe::build(&home, &telemetry)?;
    telemetry_sinks
        .sink
        .record(&GATEWAY_STARTED, harw_observe::MetricValue::Count(1), &[]);

    // Intervall der periodischen Audit-Kettenprüfung (siehe
    // `audit_chain_check_interval_secs`-Doku für die Begründung, warum dies
    // eine Umgebungsvariable ist statt eines `--metrics-*`-artigen Schalters).
    let audit_chain_check_env = std::env::var(AUDIT_CHAIN_CHECK_INTERVAL_ENV).ok();
    let audit_chain_check_interval_secs =
        audit_chain_check_interval_secs(audit_chain_check_env.as_deref());

    // Knowledge-Store am Profil-Wissensordner mounten (memory/diary/dream/
    // workbench/kanban teilen sich diesen Store).
    let knowledge_root = harw_home::knowledge_dir(&profile);
    std::fs::create_dir_all(&knowledge_root)
        .map_err(|error| format!("knowledge-Ordner anlegen: {error}"))?;
    let knowledge = KnowledgeStore::new(&knowledge_root);
    // Dream turns use the same active-profile transcript root as CLI turns.
    // Keep the root derived before entering the runtime so a profile switch
    // cannot make an in-flight gateway write into another profile.
    let roots = GatewayProfileRoots {
        profile,
        sessions: sessions_root,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Runtime-Start fehlgeschlagen: {error}"))?;

    // Opt-in session ingress (`--session-socket`); default off. The flag is
    // explicit, so a start failure ends the gateway with an error before any
    // other subsystem runs.
    let session_serve = session_socket.map(|socket| {
        (
            crate::session_serve::SessionServeConfig::for_profile(
                &home,
                &cwd,
                &roots.profile,
                socket,
                std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            ),
            session_model,
        )
    });

    let result = runtime.block_on(async {
        let ingress = match session_serve {
            Some((config, model)) => {
                let ingress = crate::session_serve::start(&config, model).await?;
                eprintln!("  session socket : {}", ingress.socket().display());
                Some(ingress)
            }
            None => None,
        };
        let result = supervise(
            &home,
            Arc::clone(&config),
            providers,
            &knowledge,
            &roots,
            Arc::clone(&telemetry_sinks.sink),
            audit_chain_check_interval_secs,
        )
        .await;
        // `supervise` returns on the gateway's shutdown signal; drain the
        // session host (and remove its socket) before the runtime ends.
        if let Some(ingress) = ingress {
            ingress.shutdown().await;
        }
        result
    });

    // Außerhalb der Runtime (siehe Funktionsdoku): ein aktivierter
    // OTLP-Export darf seinen letzten Stapel noch zustellen, bevor der
    // Prozess endet.
    telemetry_sinks.sink.flush();
    result
}

/// Geteilter Auflöser für versiegelte `secrets:`-Provider-Credentials, den
/// beide Gateway-Montagen über `Arc::clone` gemeinsam nutzen.
type GatewaySecretResolver = Arc<dyn harw_provider_http::SecretResolver + Send + Sync>;

/// Die Runtime-Montagen eines Gateway-Starts (Befunde G1/G3).
///
/// Dream und jede aktivierte Telegram-Bindung laufen über **getrennte**
/// [`harw_runtime::RuntimeAssembly`]-Instanzen mit eigener
/// [`harw_runtime::EntryKind`] (`GatewayTelegram`/`GatewayDream`), eigenem
/// Principal und eigenem Transkript-Mapper, damit Audit und Trace den
/// auslösenden Kanal (und bei Telegram die Bindung) unterscheiden können.
/// Alle teilen denselben Secret-Resolver. Es gibt keinen Config-Schalter, der
/// Dream deaktiviert — der Dream-Scheduler läuft immer, deshalb wird die
/// Dream-Montage stets gebaut; ihre Konfiguration speist außerdem
/// Statusausgabe, Telegram-Ingress und Audit-Kettenprüfung.
struct GatewayAssemblies {
    /// Je aktivierter Telegram-Bindung eine Montage (leer, wenn keine
    /// Bindung aktiviert ist).
    telegram: Vec<TelegramAssembly>,
    /// Montage für Dream-Läufe; ihr Modell geht an [`dream_scheduler`].
    dream: harw_runtime::RuntimeAssembly,
}

/// Die Montage einer einzelnen Telegram-Bindung.
struct TelegramAssembly {
    /// Konfigurierte Bindungs-ID, aus der der Principal abgeleitet wurde.
    binding_id: String,
    assembly: harw_runtime::RuntimeAssembly,
}

/// Die Wurzel-Modelle der Gateway-Montagen, gebündelt für
/// [`supervise`] (hält dessen Parameterliste unter der
/// `clippy::too_many_arguments`-Schwelle, ohne `#[allow]`).
struct GatewayProviders {
    /// Modell je Telegram-Bindung (`GatewayAssemblies::telegram`, Schlüssel =
    /// Bindungs-ID); geht an die Supervision der jeweiligen Bindung.
    telegram: HashMap<String, Arc<dyn ModelProvider>>,
    /// Modell der **eigenen** Dream-Montage (`GatewayAssemblies::dream`,
    /// Befunde G1/G3); geht an [`dream_scheduler`].
    dream: Arc<dyn ModelProvider>,
}

/// Die vor dem Runtime-Eintritt aufgelösten Profil-Wurzeln eines
/// Gateway-Starts, gebündelt für [`supervise`] (hält dessen Parameterliste
/// unter der `clippy::too_many_arguments`-Schwelle, ohne `#[allow]`).
struct GatewayProfileRoots {
    /// Aktives Profilverzeichnis; Wurzel für `channel-state` (Pairing,
    /// Offsets, Work-Requests) und den Profil-Jobstore. Bewusst explizit und
    /// nicht aus `sessions` abgeleitet: ein angepasstes `[session] store_dir`
    /// liegt nicht zwingend unter dem Profil.
    profile: PathBuf,
    /// Aufgelöste Session-Wurzel (`crate::runtime_entry::profile_sessions_root`);
    /// Transkripte von Dream und Telegram.
    sessions: PathBuf,
}

/// Öffnet den Secret-Resolver für die Gateway-Montagen genau einmal, verengt
/// auf den Provider, den der Gateway-Prozess tatsächlich verwendet.
///
/// # Description
/// `gateway_assembly` lädt die maßgebliche Konfiguration erst innerhalb von
/// `RuntimeAssemblyBuilder::build`, aber der Secret-Resolver muss vorher
/// stehen (er wird per `Arc::clone` an alle Montagen gereicht). Diese
/// Funktion lädt deshalb vorab dieselbe vertrauensbewusste Konfiguration über
/// [`harw_runtime::load_config`] — mit der [`harw_runtime::RuntimeSpec`] des
/// übergebenen `entry` (`entry.entry_kind()`, Befund G4 statt eines hart
/// codierten `EntryKind::GatewayTelegram`). Die vorab geladene Konfiguration
/// wird zusätzlich zurückgegeben: [`mount_gateway_assembly`] leitet daraus
/// ab, für welche aktivierten Telegram-Bindungen es je eine Montage mit
/// bindungsbezogenem Principal baut.
///
/// **Warum provider-verengt statt eines vollen Scans:** alle
/// Gateway-Montagen (`GatewayEntry::Telegram`/`Dream`) lesen dasselbe
/// `home`/`cwd`, also dieselbe aufgelöste Konfiguration, und beide bauen ihr
/// Modell über `ModelSource::Configured`, das ausschließlich
/// `config.harness.default_provider` verwendet (siehe
/// `harw_provider_http::build_provider_with_home`/`build_provider`, dieselbe
/// Auflösung wie `crate::main::build_serve_provider`). Der Gateway-Prozess
/// hat — anders als `serve`, das zusätzlich beliebige konfigurierte
/// MCP-Principals authentifiziert — keine zweite, vom `default_provider`
/// unabhängige Verwendungsstelle für Provider-Credentials. Diese Funktion
/// ruft deshalb
/// [`crate::secret_store::open_configured_secret_resolver_for_active_provider`]
/// statt [`crate::secret_store::open_configured_secret_resolver`] auf: ein
/// zweiter, aktivierter, aber von diesem Gateway-Start nie angesprochener
/// `secrets:`-Provider ohne KEK darf den Start nicht mehr blockieren. Der
/// tatsächlich genutzte `default_provider` bleibt fail-closed, wenn er
/// `secrets:` referenziert und kein KEK konfiguriert ist.
///
/// # Arguments
/// - `entry` ([`GatewayEntry`]): Einstieg der ersten Montage, deren Spec
///   nachgebildet wird.
/// - `home` (`&Path`): aufgelöster Root-Space.
/// - `cwd` (`&Path`): Arbeitsverzeichnis des Gateway-Prozesses.
/// - `principal` (`&Principal`): Principal derselben Montage (geklont in die
///   vorläufige Spec).
///
/// # Returns
/// Ein Paar aus Resolver und vorab geladener Konfiguration. Der Resolver ist
/// `Some(resolver)`, wenn der tatsächlich genutzte Provider (`default_provider`)
/// `secrets:` nutzt und der versiegelte Speicher geöffnet werden konnte;
/// `None` sonst — auch dann, wenn ein *anderer*, von diesem Gateway-Start
/// nicht verwendeter Provider `secrets:` nutzen würde.
///
/// # Errors
/// `String` mit Präfix `"gateway: "`: Konfigurations-/Vertrauensfehler aus
/// `load_config`, fehlendes KEK (`"gateway: enabled sealed-secret provider
/// requires a configured KEK"`), nicht ladbares KEK-Material oder nicht zu
/// öffnender Speicher — jeweils ohne Geheimnisinhalt.
fn open_gateway_secret_resolver(
    entry: GatewayEntry,
    home: &Path,
    cwd: &Path,
    principal: &Principal,
) -> Result<(Option<GatewaySecretResolver>, ResolvedConfig), String> {
    let spec = crate::runtime_entry::runtime_spec(entry.entry_kind(), home, cwd, principal.clone());
    let (preliminary_config, _trust_report) =
        harw_runtime::load_config(&spec).map_err(|error| format!("gateway: {error}"))?;
    let resolver = crate::secret_store::open_configured_secret_resolver_for_active_provider(
        home,
        &preliminary_config,
    )
    .map_err(|error| format!("gateway: {error}"))?;
    // `doc.read_pdf` (docs/design/doc_read_pdf_design.md §W4): Telegram- und
    // Dream-Montagen teilen sich diesen einen Resolver (siehe
    // `mount_gateway_assembly`), daher genügt eine Installation hier für
    // beide Gateway-Kanäle; nie fatal für den Gateway-Start.
    crate::doc_ocr::install_doc_ocr(
        &preliminary_config,
        Some(home),
        resolver
            .as_ref()
            .map(|resolver| resolver as &dyn harw_provider_http::SecretResolver),
    );
    Ok((
        resolver.map(|resolver| Arc::new(resolver) as GatewaySecretResolver),
        preliminary_config,
    ))
}

/// Montiert die Gateway-Runtimes (Konfiguration, Vertrauen, Modell) einmalig.
///
/// # Description
/// Öffnet zuerst über [`open_gateway_secret_resolver`] genau einmal den
/// Secret-Resolver (Regression B3) samt vorab geladener Konfiguration und baut
/// dann über [`gateway_assembly`] die [`harw_runtime::RuntimeAssembly`]-
/// Instanzen, die sich diesen Resolver per `Arc::clone` teilen:
///
/// - **Dream:** [`GatewayEntry::Dream`], Principal
///   `channel_principal(GatewayEntry::Dream, "")` (Dream hat keinen externen
///   Peer), Transkript-Verlaufsspeicher unter `sessions_root` mit
///   `harw_runtime::dream_run::dream_thread_for_session` — derselbe Mapper, den
///   `harw_runtime::dream_run::build_dream_state_store` für die Dream-Läufe nutzt.
/// - **Telegram, je aktivierter Bindung:** [`GatewayEntry::Telegram`],
///   Principal `channel_principal(GatewayEntry::Telegram, peer)` mit `peer`
///   aus [`telegram_principal_peer`] — also `telegram:<bindung>`, abgeleitet
///   aus der vertrauenswürdig **konfigurierten** Bot-Bindung (nie aus Chat-
///   oder Modelltext). Die Rechte bleiben unabhängig davon `Observer`/`{}`;
///   der jeweils handelnde Mensch wird weiterhin pro Ereignis über
///   `SessionKey`/`SenderRef` an der Admission-Grenze getragen.
///   Transkript-Verlaufsspeicher unter `sessions_root` mit
///   [`telegram_thread_for_session`].
///
/// Meldet ein nicht freigegebenes repo-lokales `.harw` einmal per
/// `tracing::warn!` (Feld `path`); es wurde von der Montage höchstens
/// verengend übernommen und ist **kein** Fehler. Alle Montagen lesen
/// denselben `home`/`cwd`, der Vertrauensbefund ist daher identisch.
///
/// # Arguments
/// - `home` (`&Path`): aufgelöster Root-Space.
/// - `cwd` (`&Path`): Arbeitsverzeichnis des Gateway-Prozesses.
/// - `sessions_root` (`&Path`): `<profil>/sessions` des aktiven Profils.
///
/// # Errors
/// Jeder Montagefehler (Konfiguration, Vertrauen, Projekt, Registry,
/// **Provider**, Speicher) als `String` mit Präfix `"gateway: "`, ebenso jeder
/// Fehler aus [`open_gateway_secret_resolver`] (u. a. fehlendes KEK). Es gibt
/// keinen Echo-Ersatz für einen nicht baubaren Provider (G-048).
fn mount_gateway_assembly(
    home: &Path,
    cwd: &Path,
    sessions_root: &Path,
) -> Result<GatewayAssemblies, String> {
    let dream_principal = channel_principal(GatewayEntry::Dream, "");
    let (secret_resolver, preliminary_config) =
        open_gateway_secret_resolver(GatewayEntry::Dream, home, cwd, &dream_principal)?;

    let dream = gateway_assembly(
        GatewayEntry::Dream,
        home,
        cwd,
        dream_principal,
        crate::runtime_entry::transcript_state_store(
            sessions_root,
            harw_runtime::dream_run::dream_thread_for_session,
        ),
        secret_resolver.as_ref().map(Arc::clone),
    )?;

    let mut telegram = Vec::new();
    for binding_id in enabled_telegram_binding_ids(&preliminary_config) {
        let principal =
            channel_principal(GatewayEntry::Telegram, telegram_principal_peer(&binding_id));
        let assembly = gateway_assembly(
            GatewayEntry::Telegram,
            home,
            cwd,
            principal,
            crate::runtime_entry::transcript_state_store(
                sessions_root,
                telegram_thread_for_session,
            ),
            secret_resolver.as_ref().map(Arc::clone),
        )?;
        telegram.push(TelegramAssembly {
            binding_id,
            assembly,
        });
    }

    let trust_report = dream.trust_report();
    if trust_report.has_untrusted_repo() {
        let path = trust_report
            .untrusted_repo
            .as_deref()
            .unwrap_or_else(|| Path::new(""));
        tracing::warn!(
            path = %path.display(),
            "repo-lokales .harw ist nicht freigegeben; nur verengend übernommen"
        );
    }
    Ok(GatewayAssemblies { telegram, dream })
}

/// Liefert den Peer-Anteil des Telegram-Principals einer Bindung.
///
/// [`channel_principal`] stellt jedem Peer `telegram:` voran; Bindungs-IDs
/// tragen dieses Präfix per Konvention bereits (`telegram:support-bot`).
/// Damit der Principal `telegram:support-bot` statt
/// `telegram:telegram:support-bot` heißt, wird ein führendes `telegram:`
/// abgeschnitten — außer es bliebe danach nichts übrig; dann gilt die
/// vollständige ID. Rein und ohne Seiteneffekte.
fn telegram_principal_peer(binding_id: &str) -> &str {
    match binding_id.strip_prefix("telegram:") {
        Some(rest) if !rest.trim().is_empty() => rest,
        _ => binding_id,
    }
}

/// IDs aller aktivierten Telegram-Bindungen, deterministisch sortiert.
fn enabled_telegram_binding_ids(config: &ResolvedConfig) -> Vec<String> {
    enabled_telegram_bindings(config)
        .into_iter()
        .map(|binding| binding.id.clone())
        .collect()
}

/// Alle aktivierten Telegram-Bindungen, nach ID sortiert (die Channel-Map
/// ist eine `HashMap`; Status und Aufgabenreihenfolge sollen stabil sein).
fn enabled_telegram_bindings(config: &ResolvedConfig) -> Vec<&TelegramChannelToml> {
    let mut bindings = config
        .channels
        .values()
        .filter_map(|channel| match channel {
            ChannelToml::Telegram(binding) if binding.enabled => Some(binding),
            ChannelToml::Telegram(_) => None,
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| left.id.cmp(&right.id));
    bindings
}

/// Statuszeile des Agenten-Subsystems für die Startausgabe.
///
/// Nur aufrufbar mit einem bereits gebauten Provider — deshalb gibt es keinen
/// „degradiert"-Zweig mehr (G-048).
fn gateway_provider_status(config: &ResolvedConfig) -> String {
    format!(
        "bereit (provider={}, model={})",
        config.harness.default_provider.as_deref().unwrap_or("—"),
        config.harness.default_model.as_deref().unwrap_or("—"),
    )
}

/// Supervidiert die Subsysteme und wartet auf ein Shutdown-Signal.
///
/// # Arguments
/// - `config` (`Arc<ResolvedConfig>`): eine geteilte Referenz, statt eines
///   Borrows — [`audit_chain_scheduler`] klont diesen `Arc` in einen
///   `spawn_blocking`-Worker, der bei jedem Tick den konfigurierten
///   Geheimnisspeicher neu öffnet (siehe dortige Doku), was einen
///   `'static`-fähigen Griff auf die Konfiguration statt eines an diesen
///   Stack-Frame gebundenen Borrows braucht.
/// - `providers` ([`GatewayProviders`]): die Wurzel-Modelle der Telegram-
///   Montagen (je Bindung) und der **eigenen** Dream-Montage
///   (`RuntimeAssembly::model`, Befunde G1/G3); `telegram` geht je Bindung an
///   [`supervise_telegram_binding`], `dream` an [`dream_scheduler`].
/// - `roots` ([`GatewayProfileRoots`]): explizites Profilverzeichnis
///   (`channel-state`, Jobstore) und aufgelöste Session-Wurzel (Dream- und
///   Telegram-Transkripte).
///
/// # Shutdown
/// Nach einem Shutdown-Signal wird für jede im Webhook-Modus gestartete
/// Bindung `deleteWebhook` aufgerufen ([`teardown_telegram_webhooks`],
/// zeitlich begrenzt), damit Telegram nicht weiter an einen gestoppten
/// Listener zustellt; ausstehende Updates bleiben dabei bei Telegram
/// gepuffert und erreichen den nächsten Start.
/// - `telemetry_sink` (`Arc<dyn TelemetrySink>`): derselbe zusammengesetzte
///   Sink, den [`run`] baut; [`audit_chain_scheduler`] meldet
///   `audit_chain_break` darüber (siehe dessen Doku für die Routing-
///   Garantie, dass dieser Name nie Prometheus/OTLP erreicht).
/// - `audit_chain_check_interval_secs` (`u64`): Intervall der periodischen
///   Audit-Kettenprüfung; `0` deaktiviert sie (siehe
///   [`audit_chain_check_interval_secs`]).
async fn supervise(
    home: &Path,
    config: Arc<ResolvedConfig>,
    providers: GatewayProviders,
    knowledge: &KnowledgeStore,
    roots: &GatewayProfileRoots,
    telemetry_sink: Arc<dyn TelemetrySink>,
    audit_chain_check_interval_secs: u64,
) -> Result<(), String> {
    // Alle Provider stammen aus den Gateway-Montagen in [`run`] und sind dort
    // bereits erfolgreich gebaut worden; einen degradierten Echo-Zustand gibt
    // es nicht mehr (G-048).
    let provider_status = gateway_provider_status(&config);

    let telegram_bindings = telegram_ingress_modes(&config);
    let telegram_status = telegram_ingress_status(&telegram_bindings);

    // Persistente Workbench-Scopes zählen. Das sind gespeicherte Arbeitssets,
    // keine fortsetzbaren Gateway-Ausführungen.
    let workbench_scope_count = scan_workbench(knowledge);

    eprintln!("harw gateway — Hintergrund-Daemon gestartet");
    eprintln!("  agents/gateway : {provider_status}");
    eprintln!("  channels/tg    : {telegram_status}");
    eprintln!(
        "  knowledge      : gemountet ({})",
        knowledge.root().display()
    );
    eprintln!(
        "  workbench      : {}",
        workbench_status(workbench_scope_count)
    );
    let dream_settings = harw_ops::dream_run::DreamSettings::from_config(&config);
    eprintln!("  dream          : {}", dream_status(&dream_settings));
    if config.harness.mcp_listener.enabled {
        eprintln!("  mcp            : aktiviert in Config (separat via `harw serve`)");
    }

    // Gemeinsame Aktivitätsuhr: Dream liest sie. Eine Telegram-Bindung
    // startet erst, nachdem Transport-, Identity-Pinning-, Credential- und
    // Admitted-Event-Handoff-Gates für genau diese Bindung bestanden sind.
    let activity: ActivityClock = Arc::new(Mutex::new(Instant::now()));

    let GatewayProviders {
        telegram: mut telegram_providers,
        dream: dream_provider,
    } = providers;
    let telegram_profile = roots.profile.clone();
    // Ein einziger `WorkRequestStore` für alle Bindungen: er serialisiert
    // seine Dateizugriffe nur über einen prozessinternen Mutex, zwei parallel
    // laufende Instanzen auf demselben Verzeichnis dürften sich also nicht
    // gegenseitig überschreiben.
    // Genehmigte Aufträge werden als durabler Job im Profil-Jobstore
    // zugelassen; `harw job-worker` führt sie aus.
    let work_requests = Arc::new(
        WorkRequestStore::new(&telegram_profile.join("channel-state").join("work-requests"))
            .with_launcher(Arc::new(
                crate::telegram_launcher::JobStoreWorkLauncher::for_profile_dir(&telegram_profile),
            )),
    );

    let mut webhook_teardowns = Vec::new();
    let mut binding_tasks: Vec<TelegramBindingTask> = Vec::new();
    for binding in telegram_bindings {
        match binding.mode {
            TelegramIngressMode::Disabled(reason) => {
                tracing::warn!(binding = %binding.id, reason = %reason, "Telegram binding remains disabled (fail closed)");
            }
            TelegramIngressMode::Enabled(plan) => {
                // Die Montage wurde aus derselben, vorab geladenen
                // Konfiguration abgeleitet; fehlt sie (Konfiguration hat sich
                // zwischen beiden Ladevorgängen geändert), bleibt diese
                // Bindung zu — nie ein Rückgriff auf einen fremden Principal.
                let Some(provider) = telegram_providers.remove(&binding.id) else {
                    tracing::error!(binding = %binding.id, "Telegram binding has no mounted runtime assembly; remains disabled (fail closed)");
                    continue;
                };
                // Arbeitsbereiche, Befehls-Fallback und Chat-Zustand je
                // Bindung; ein Fehler schließt nur diese Bindung.
                let services = match telegram_binding_services(
                    home,
                    &plan.binding,
                    &telegram_profile,
                ) {
                    Ok(services) => services,
                    Err(reason) => {
                        tracing::warn!(binding = %binding.id, reason = %reason, "Telegram binding remains disabled (fail closed)");
                        continue;
                    }
                };
                if matches!(plan.transport, TelegramTransportPlan::Webhook(_)) {
                    webhook_teardowns.push(TelegramWebhookTeardown {
                        binding_id: binding.id.clone(),
                        bot_token: plan.bot_token.clone(),
                    });
                }
                binding_tasks.push(Box::pin(supervise_telegram_binding(
                    *plan,
                    provider,
                    telegram_profile.clone(),
                    roots.sessions.clone(),
                    services,
                    Arc::clone(&work_requests),
                )));
            }
        }
    }
    let channels = async move {
        if binding_tasks.is_empty() {
            tracing::warn!("Telegram ingress remains disabled (fail closed)");
            std::future::pending::<()>().await;
        } else {
            // Weder ein Start-Fehler (z. B. kein Netz beim Boot) noch ein
            // späteres Enden eines Transports darf eine Bindung für den Rest
            // der Gateway-Laufzeit stillegen (S6/S9) — jede Bindung startet
            // mit Backoff neu, unabhängig von den anderen.
            let never = drive_telegram_bindings(binding_tasks).await;
            match never {}
        }
    };

    // Traum-Scheduler: läuft (auch ohne Telegram), sofern `[dream] enabled`;
    // derselbe Kern wie `/dream run` (`harw_runtime::dream_run`), der Lauf
    // steht als `JobKind::Dream` im Profil-Ledger, der Zustand in
    // `knowledge/dreams/state.json` übersteht Neustarts.
    let dream_launcher = harw_runtime::dream_run::RuntimeDreamLauncher::new(
        Arc::clone(&dream_provider),
        Arc::new(knowledge.clone()),
        Some(Arc::new(harw_session_store::JobStore::new(&roots.profile))),
        roots.sessions.clone(),
        Arc::clone(&config),
    );
    let dream = dream_scheduler(&dream_launcher, &activity);

    // Audit-Kettenprüfung: läuft immer (siehe `audit_chain_scheduler`s Doku);
    // `audit_chain_check_interval_secs == 0` degradiert sie zu einem
    // niemals fertigen Future, ohne den `select!`-Zweig zu entfernen.
    let audit_chain_check = audit_chain_scheduler(
        telemetry_sink,
        audit_chain_check_interval_secs,
        home.to_path_buf(),
        Arc::clone(&config),
    );

    tokio::select! {
        () = channels => {}
        () = dream => {}
        () = audit_chain_check => {}
        () = shutdown_signal() => {
            eprintln!("harw gateway — Shutdown-Signal empfangen, beende sauber.");
        }
    }
    // Der `channels`-Zweig (und damit jeder Webhook-Listener) ist hier
    // bereits verworfen; erst jetzt den Webhook bei Telegram abmelden.
    teardown_telegram_webhooks(&webhook_teardowns).await;
    Ok(())
}

/// Wartet auf `SIGINT` (Ctrl+C) oder `SIGTERM`.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(_) => {
                ctrl_c.await;
                return;
            }
        };
        tokio::select! {
            () = ctrl_c => {}
            _ = term.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}

/// Periodisch wiederholte Prüfung der **auf der Platte persistierten**
/// Audit-Kette des **konfigurierten** `SecretStore` (AW7-04-Scheduling-
/// Knoten, dritte Fassung dieser Datei).
///
/// # Zwei bereits behobene Vorgänger-Befunde
/// Die erste Fassung dieses Knotens rief
/// [`harw_secrets::ChainAuditMirror::verify_and_mirror`] nie periodisch auf
/// — der Nullzähler `audit_chain_break` konnte „Kette intakt" nicht von
/// „niemand hat nachgesehen" unterscheiden (siehe `harw_secrets::audit::telemetry`-
/// Moduldoku, Abschnitt „Die Falle"). Die zweite Fassung rief ihn zwar auf,
/// aber auf [`harw_secrets::SecretStore::audit_log`] einer frisch geöffneten
/// Instanz: `SecretStore::open`/`open_with_key_material` lesen `audit.log`
/// nie von der Platte zurück, jede Instanz beginnt leer und wächst nur um
/// Mutationen, die innerhalb genau dieser Instanz passieren. Da dieser
/// Gateway-Prozess selbst nie `create`/`rotate`/`delete` aufruft, war diese
/// Kette in der Praxis **immer leer** — eine Manipulationsprüfung auf einer
/// stets leeren Kette prüft nichts. Und die Angriffsrichtung, um die es
/// geht (ein Angreifer schreibt an `audit.log` vorbei oder manipuliert es
/// nachträglich), ist von einer im Speicher gehaltenen Kette per Definition
/// nicht sichtbar.
///
/// # Diese Fassung: die persistierte Datei
/// Diese Funktion prüft stattdessen — über
/// [`crate::secret_store::configured_secret_store_persisted_audit_chain_status`]
/// (die intern [`harw_secrets::SecretStore::verify_persisted_audit_chain`]
/// aufruft) — die Bytes von `audit.log` auf der Platte: denselben Pfad,
/// dieselbe KEK-Provenienz, dieselbe Crypto-Policy, die auch
/// [`crate::secret_store::open_configured_secret_resolver`] für den
/// Provider-`SecretResolver` verwendet.
///
/// # Wie oft und warum
/// Alle `interval_secs` Sekunden (Vorgabe [`AUDIT_CHAIN_CHECK_DEFAULT_SECS`],
/// überschreibbar über [`AUDIT_CHAIN_CHECK_INTERVAL_ENV`]); `interval_secs ==
/// 0` deaktiviert die Prüfung vollständig — die Funktion wartet dann nur
/// noch auf das Ende von `supervise`s `select!`, ohne je zu prüfen. Das
/// Intervall blockiert die Event-Loop nicht: sowohl das Öffnen des
/// konfigurierten Speichers als auch das Lesen/Verifizieren von `audit.log`
/// laufen über [`run_configured_secret_store_audit_chain_tick`] auf einem
/// einzigen `tokio::task::spawn_blocking`-Worker je Tick; dieser
/// `select!`-Zweig wartet nur auf `tokio::time::interval`.
///
/// **Je Tick neu geöffnet, kein gehaltenes Handle:** siehe
/// [`crate::secret_store::configured_secret_store_persisted_audit_chain_status`]s
/// Doku für die vollständige Begründung. Kurzfassung: das Öffnen dient hier
/// nur der KEK-Auflösung, um dasselbe Gate wie der Provider-`SecretResolver`
/// zu respektieren — die eigentliche Prüfung liest ohnehin frisch von der
/// Platte. Ein über die gesamte Gateway-Laufzeit gehaltenes Handle hätte nur
/// den Nachteil, entschlüsseltes KEK-Material länger im Speicher zu halten,
/// ohne einen Gegenwert an geprüfter Kettentiefe zu bieten.
///
/// **Ist kein Geheimnisspeicher konfiguriert** (kein aktivierter Provider
/// nutzt eine `secrets:`-Referenz), meldet ein Tick
/// [`AuditChainTickOutcome::NothingConfigured`]: „nichts zu prüfen", niemals
/// „geprüft, unversehrt". Ein fehlendes KEK oder ein sonstiger Öffnungsfehler
/// ergibt [`AuditChainTickOutcome::OpenFailed`] mit einer Meldung ohne
/// Geheimnisinhalt (siehe dort) und lässt den Gateway ebenfalls weiterlaufen
/// — ein Konfigurationsfehler in der Kettenprüfung darf den Daemon nicht
/// mitreißen.
///
/// # Die drei Fälle, die dieser Knoten nie verwechselt
/// [`describe_audit_chain_check`] übersetzt das Ergebnis in einen von vier
/// [`AuditChainCheckReport`]-Werten — die drei aus `harw-secrets` (nichts da
/// / unlesbar / manipuliert) plus den unversehrten Erfolgsfall —, von denen
/// nur einer ein Sicherheitsereignis ist:
/// - [`AuditChainCheckReport::Absent`]: `audit.log` existiert nicht. **Kein
///   Kettenbruch** — ein Store, der noch nie durabel mutiert hat, sieht
///   genauso aus. Der Zähler bleibt bei null.
/// - [`AuditChainCheckReport::Intact`]: die Datei wurde gelesen und
///   verifiziert; sie hängt lückenlos zusammen.
/// - [`AuditChainCheckReport::Unreadable`]: die Datei konnte nicht als
///   dieses Format gelesen werden (kein reguläre Datei, abgeschnitten,
///   falsche Kennung, ...). **Weder** „unversehrt" **noch** „Bruch" — wir
///   konnten schlicht nicht nachsehen. Wird sichtbar gemeldet
///   (`tracing::warn!`), aber getrennt vom eigentlichen Fund gehalten.
/// - [`AuditChainCheckReport::Broken`]: das Sicherheitsereignis. Der
///   `prev_hash` eines Ereignisses stimmt nicht mit dem neu berechneten
///   Hash seines Vorgängers überein.
///
/// # Was ein Kettenbruch auslöst
/// Nur [`AuditChainCheckReport::Broken`] erhöht
/// `harw_secrets::audit::telemetry::AUDIT_CHAIN_BREAK` (Metrikname
/// `audit_chain_break`, kein `security.`/`warden.`-Präfix, erreicht damit
/// laut `crate::observe`-Routing ausschließlich den immer aktiven File-Sink,
/// nie Prometheus/OTLP — die Kompositionsstelle hier kennt keinen Weg, eine
/// `ExportApproval` herzustellen) **und** wird über `tracing::error!`/
/// `eprintln!` sichtbar gemeldet. Der Gateway läuft danach unverändert
/// weiter — ein Absturz würde die Beweislage löschen, kein Protokolleintrag
/// ohne sichtbare Meldung wäre keine Meldung.
///
/// # Wohin `ChainAuditMirror` gehört — und wohin nicht mehr
/// [`harw_secrets::ChainAuditMirror::verify_and_mirror`] bleibt in
/// `harw-secrets` bestehen, wird aber von diesem Knoten **nicht mehr**
/// aufgerufen: es verifiziert eine `&AuditLog` mit ihren einzelnen
/// Ereignissen, aber [`harw_secrets::audit::chain::load_and_verify_persisted_chain`]
/// gibt aus der Datei nur einen [`PersistedChainStatus`] zurück (Ereigniszahl
/// und Ketten-Kopfhash, keine rekonstruierten Einzelereignisse) — es gibt
/// keine `AuditLog`, die sich hier ehrlich an `verify_and_mirror` übergeben
/// ließe. Der Spiegel bliebe außerdem folgenlos: für diesen Knoten ist kein
/// hostexterner Transport konfiguriert (siehe „Was diese Prüfung weiterhin
/// nicht entdecken kann" unten), also würde jeder Sendeversuch ohnehin
/// verworfen. `verify_and_mirror` bleibt also die richtige Wahl für einen
/// Aufrufer, der eine echte, im Prozess gehaltene `AuditLog` besitzt (z. B.
/// unmittelbar nach einer Mutation innerhalb desselben `SecretStore`-
/// Aufrufs) — nicht für eine externe, periodische Prüfung der Plattendatei.
///
/// # Was diese Prüfung weiterhin nicht entdecken kann
/// - **Einen Angreifer, der `audit.log` vollständig und konsistent neu
///   schreibt** — mit einer anderen, aber in sich stimmigen, korrekt
///   gehashten Kette samt eines bei der Genesis verankerten ersten
///   `prev_hash`. Geprüft wird nur die innere Konsistenz der Datei, kein
///   externer Anker (Spiegel, extern aufbewahrter Checkpoint, ...) — siehe
///   [`harw_secrets::audit::chain`]-Moduldoku.
/// - Eine Host-Übernahme, die genau diese Tokio-Task beendet oder ihren
///   Start unterdrückt: ohne konfigurierten hostexternen Transport bleibt
///   ein unterdrückter Aufruf von außen ununterscheidbar von einer intakten
///   Kette — dieselbe Falle wie beim unbeobachteten Nullzähler, nur eine
///   Ebene höher.
async fn audit_chain_scheduler(
    sink: Arc<dyn TelemetrySink>,
    interval_secs: u64,
    home: PathBuf,
    config: Arc<ResolvedConfig>,
) {
    if interval_secs == 0 {
        tracing::info!(
            env = AUDIT_CHAIN_CHECK_INTERVAL_ENV,
            "audit-chain-check disabled"
        );
        std::future::pending::<()>().await;
        return;
    }

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    // `interval` feuert sofort beim ersten `tick()`; einmal verbrauchen,
    // damit die erste echte Prüfung erst nach einem vollen Intervall läuft.
    ticker.tick().await;

    loop {
        ticker.tick().await;

        match run_configured_secret_store_audit_chain_tick(&home, &config).await {
            AuditChainTickOutcome::NothingConfigured => {
                tracing::info!(
                    "audit-chain-check: kein Geheimnisspeicher konfiguriert — nichts zu prüfen"
                );
            }
            AuditChainTickOutcome::OpenFailed(reason) => {
                tracing::warn!(
                    reason = %reason,
                    "audit-chain-check: konfigurierter Geheimnisspeicher konnte nicht geöffnet werden"
                );
            }
            AuditChainTickOutcome::WorkerAborted => {}
            AuditChainTickOutcome::Checked(result) => {
                apply_audit_chain_check_report(describe_audit_chain_check(&result), sink.as_ref());
            }
        }
    }
}

/// Loggt einen [`AuditChainCheckReport`] passend zu seiner Art und erhöht bei
/// einem tatsächlich festgestellten Kettenbruch
/// [`harw_secrets::audit::telemetry::AUDIT_CHAIN_BREAK`] — sonst nicht.
///
/// Von [`audit_chain_scheduler`]s Loop-Rumpf getrennt gehalten, damit Tests
/// diesen Seiteneffekt gezielt auslösen können, ohne einen tickenden
/// Scheduler laufen zu lassen.
///
/// # Arguments
/// - `report` (`AuditChainCheckReport`): das Ergebnis von
///   [`describe_audit_chain_check`].
/// - `sink` (`&dyn TelemetrySink`): Ziel für die
///   [`harw_secrets::audit::telemetry::AUDIT_CHAIN_BREAK`]-Meldung; unbenutzt
///   außer bei [`AuditChainCheckReport::Broken`].
fn apply_audit_chain_check_report(report: AuditChainCheckReport, sink: &dyn TelemetrySink) {
    match report {
        AuditChainCheckReport::Absent => {
            tracing::info!(
                "audit-chain-check: nichts protokolliert (audit.log existiert noch nicht)"
            );
        }
        AuditChainCheckReport::Intact { event_count } => {
            tracing::debug!(
                event_count,
                "audit-chain-check: persistierte Kette unversehrt"
            );
        }
        AuditChainCheckReport::Unreadable(reason) => {
            tracing::warn!(
                reason = %reason,
                "audit-chain-check: persistierte Kette konnte nicht gelesen werden — weder unversehrt noch als Bruch feststellbar"
            );
        }
        AuditChainCheckReport::Broken(message) => {
            AUDIT_CHAIN_BREAK.violated(sink, &[]);
            tracing::error!(message = %message, "AUDIT CHAIN BREAK");
            eprintln!("harw gateway — {message}");
        }
    }
}

/// Ergebnis eines einzelnen Ticks der periodischen Audit-Kettenprüfung
/// (siehe [`run_configured_secret_store_audit_chain_tick`]).
#[derive(Debug)]
enum AuditChainTickOutcome {
    /// Kein aktivierter Provider nutzt eine `secrets:`-Referenz: es gibt
    /// keinen konfigurierten Geheimnisspeicher, also nichts zu prüfen. Muss
    /// niemals als „geprüft, unversehrt" gemeldet werden.
    NothingConfigured,
    /// Der konfigurierte Speicher konnte nicht geöffnet werden (fehlendes
    /// oder ungültiges KEK, ungültige Konfiguration). Die Meldung enthält
    /// laut [`crate::secret_store::configured_secret_store_persisted_audit_chain_status`]
    /// niemals Geheimnisinhalt.
    OpenFailed(String),
    /// Der `spawn_blocking`-Worker wurde abgebrochen (Panic/Cancellation) —
    /// kein festgestellter Kettenzustand, darf nicht als Kettenbruch
    /// gemeldet werden.
    WorkerAborted,
    /// Der Speicher wurde geöffnet und die persistierte Kette tatsächlich
    /// gelesen; das rohe Ergebnis von
    /// [`harw_secrets::SecretStore::verify_persisted_audit_chain`],
    /// weiterzuverarbeiten über [`describe_audit_chain_check`].
    Checked(AuditResult<PersistedChainStatus>),
}

/// Holt und prüft die persistierte Audit-Kette des konfigurierten
/// Geheimnisspeichers für einen einzelnen Tick — über
/// [`crate::secret_store::configured_secret_store_persisted_audit_chain_status`],
/// auf einem einzigen `spawn_blocking`-Worker, da sowohl das Öffnen (Datei-/
/// Schlüssel-I/O) als auch das Lesen und Verifizieren von `audit.log`
/// synchrone Arbeit sind. Anders als in einer früheren Fassung dieses
/// Knotens gibt es hierfür keinen zweiten, getrennten `spawn_blocking`-
/// Schritt mehr: das Öffnen des Speichers und die Kettenprüfung passieren
/// jetzt in derselben Funktion (siehe deren Doku), also genügt ein
/// Worker-Aufruf pro Tick.
///
/// Als eigene, von der `loop`/`interval`-Steuerung getrennte Funktion
/// gehalten, damit Tests **denselben** produktionsförmigen Aufrufpfad direkt
/// ausführen können, ohne virtuelle Zeit voranzutreiben.
async fn run_configured_secret_store_audit_chain_tick(
    home: &Path,
    config: &Arc<ResolvedConfig>,
) -> AuditChainTickOutcome {
    let home = home.to_path_buf();
    let config = Arc::clone(config);
    let opened = match tokio::task::spawn_blocking(move || {
        crate::secret_store::configured_secret_store_persisted_audit_chain_status(&home, &config)
    })
    .await
    {
        Ok(opened) => opened,
        Err(join_error) => {
            tracing::error!(
                error = %join_error,
                "audit-chain-check: Öffnen/Prüfen des konfigurierten Geheimnisspeichers abgebrochen"
            );
            return AuditChainTickOutcome::WorkerAborted;
        }
    };

    match opened {
        Ok(None) => AuditChainTickOutcome::NothingConfigured,
        Ok(Some(status)) => AuditChainTickOutcome::Checked(status),
        Err(reason) => AuditChainTickOutcome::OpenFailed(reason),
    }
}

/// Ein für Tests direkt vergleichbarer, aufbereiteter Bericht über einen
/// einzelnen Kettencheck (siehe [`describe_audit_chain_check`]). Hält die
/// drei Fälle aus [`PersistedChainStatus`]/[`AuditError`] auseinander, damit
/// der Aufrufer in [`audit_chain_scheduler`] sie nicht wieder zusammenfallen
/// lassen kann.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AuditChainCheckReport {
    /// `audit.log` existiert nicht. Kein Fund, kein Kettenbruch.
    Absent,
    /// Die Datei wurde gelesen und ihre Verkettung bestätigt.
    Intact {
        /// Anzahl der in der persistierten Kette enthaltenen Ereignisse.
        event_count: u64,
    },
    /// Die Datei konnte nicht als dieses Format gelesen werden
    /// ([`AuditError::Io`]). Inhaltsfreie, aber sichtbare Meldung — weder
    /// „unversehrt" noch „Bruch".
    Unreadable(String),
    /// Ein tatsächlich festgestellter Kettenbruch
    /// ([`AuditError::ChainBroken`]). Das Sicherheitsereignis dieses Knotens.
    Broken(String),
}

/// Übersetzt das rohe Ergebnis von
/// [`harw_secrets::SecretStore::verify_persisted_audit_chain`] in einen der
/// vier [`AuditChainCheckReport`]-Fälle.
///
/// Als reine Funktion gehalten, damit Tests jede der drei
/// `harw_secrets`-Unterscheidungen (nichts da / unlesbar / manipuliert)
/// isoliert prüfen können, ohne einen echten Geheimnisspeicher anzulegen.
fn describe_audit_chain_check(result: &AuditResult<PersistedChainStatus>) -> AuditChainCheckReport {
    match result {
        Ok(PersistedChainStatus::Absent) => AuditChainCheckReport::Absent,
        Ok(PersistedChainStatus::Intact { event_count, .. }) => AuditChainCheckReport::Intact {
            event_count: *event_count,
        },
        Err(AuditError::ChainBroken { index, .. }) => AuditChainCheckReport::Broken(format!(
            "AUDIT CHAIN BREAK: Audit-Kette am Index {index} gebrochen (siehe Logs für Details)."
        )),
        Err(other) => AuditChainCheckReport::Unreadable(other.to_string()),
    }
}

/// Bindungs-ID, deren Long-Poll-Offset aus Kompatibilitätsgründen weiter
/// direkt unter `channel-state/telegram-offset` liegt: die von
/// `harw connect --channel telegram` angelegte Standardbindung hat dort schon
/// vor der Mehrfachbindungs-Unterstützung ihren Offset persistiert.
const TELEGRAM_LEGACY_OFFSET_BINDING: &str = "telegram:default";

/// Update-Arten, die Telegram per Webhook zustellen soll (`setWebhook`).
///
/// Enthält neben `message`/`edited_message` auch `callback_query`: jede
/// Webhook-Bindung installiert den Callback-Consumer aus
/// [`telegram_callbacks::spawn_callback_worker`]
/// (`WebhookConfig::with_callback_consumer`), der jeden Button-Klick per
/// `answerCallbackQuery` beantwortet — ohne dieses Abonnement stellte
/// Telegram keine Klicks zu. Der Long-Poll-Pfad nutzt diese Konstante nicht:
/// `LongPollConfig::with_callback_consumer` nimmt `callback_query` selbst in
/// seine `allowed_updates` auf.
const TELEGRAM_ALLOWED_UPDATES: [&str; 3] = ["message", "edited_message", "callback_query"];

/// Von Telegram für Webhooks akzeptierte Ports (Bot-API `setWebhook`).
const TELEGRAM_WEBHOOK_PORTS: [u16; 4] = [443, 80, 88, 8443];

/// Obergrenze für einen einzelnen `deleteWebhook`-Aufruf beim Shutdown: der
/// Client wiederholt Serverfehler mit Backoff, der Shutdown darf daran aber
/// nicht beliebig lange hängen.
const TELEGRAM_WEBHOOK_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Mindestlaufzeit eines gestarteten Transports, ab der er als „stabil"
/// gilt und den Neustart-Backoff zurücksetzt (siehe
/// [`telegram_attempt_after_run`]).
const TELEGRAM_STABLE_RUN: Duration = Duration::from_secs(60);

/// Ermittelt die Ingress-Entscheidung jeder aktivierten Telegram-Bindung.
///
/// # Description
/// Jede aktivierte Bindung wird einzeln gegen alle Gateway-eigenen
/// Vertrauens-Gates geprüft ([`telegram_binding_mode`]); anschließend
/// schließt [`disable_conflicting_telegram_bindings`] Bindungen, die sich
/// einen Bot-Token oder eine Webhook-Listen-Adresse teilen. Eine
/// fehlgeschlagene Bindung bleibt fail-closed deaktiviert, ohne die übrigen
/// zu beeinflussen. Die Reihenfolge folgt der sortierten Bindungs-ID.
///
/// # Returns
/// Eine (möglicherweise leere) Liste; leer heißt „keine Bindung aktiviert".
fn telegram_ingress_modes(config: &ResolvedConfig) -> Vec<TelegramBindingIngress> {
    let mut bindings = enabled_telegram_bindings(config)
        .into_iter()
        .map(|binding| TelegramBindingIngress {
            id: binding.id.clone(),
            mode: telegram_binding_mode(binding, config),
        })
        .collect::<Vec<_>>();
    disable_conflicting_telegram_bindings(&mut bindings);
    bindings
}

/// Prüft eine einzelne aktivierte Bindung gegen alle Gateway-eigenen Gates.
///
/// Reihenfolge: gültige Channel-ID, gültiger Topic-Modus, gepinnte
/// Identitäten, Transportwahl ([`telegram_transport_choice`]), Bot-
/// Credential und — nur im Webhook-Modus — das `secret_token`. Meldungen
/// nennen nie einen Geheimniswert oder den Namen einer Umgebungsvariable.
fn telegram_binding_mode(
    binding: &TelegramChannelToml,
    config: &ResolvedConfig,
) -> TelegramIngressMode {
    if ChannelId::try_from(binding.id.clone()).is_err() {
        return TelegramIngressMode::Disabled("Telegram channel id is invalid".to_owned());
    }
    if telegram_topic_mode(&binding.topics.mode).is_none() {
        return TelegramIngressMode::Disabled("Telegram topic mode is invalid".to_owned());
    }
    if binding.security.pinned_identities.is_empty() {
        return TelegramIngressMode::Disabled(
            "enabled Telegram binding has no pinned identities".to_owned(),
        );
    }
    let choice = match telegram_transport_choice(binding) {
        Ok(choice) => choice,
        Err(reason) => return TelegramIngressMode::Disabled(reason),
    };
    let Some(token) = resolve_telegram_secret(&binding.bot_token_ref, config) else {
        return TelegramIngressMode::Disabled(
            "Telegram bot credential could not be resolved through a supported secret boundary"
                .to_owned(),
        );
    };
    let transport = match choice {
        TelegramTransportChoice::LongPoll { webhook_fallback } => {
            TelegramTransportPlan::LongPoll { webhook_fallback }
        }
        TelegramTransportChoice::Webhook {
            public_url,
            route,
            listen_addr,
        } => {
            let Some(secret_token) = binding
                .transport_webhook
                .as_ref()
                .and_then(|webhook| resolve_telegram_secret(&webhook.secret_token_ref, config))
            else {
                return TelegramIngressMode::Disabled(
                    "Telegram webhook secret could not be resolved through a supported secret boundary"
                        .to_owned(),
                );
            };
            if !is_valid_telegram_webhook_secret(secret_token.expose_secret()) {
                return TelegramIngressMode::Disabled(
                    "Telegram webhook secret must be 1-256 characters from [A-Za-z0-9_-]"
                        .to_owned(),
                );
            }
            TelegramTransportPlan::Webhook(TelegramWebhookPlan {
                public_url,
                route,
                listen_addr,
                secret_token,
            })
        }
    };
    TelegramIngressMode::Enabled(Box::new(TelegramIngressPlan {
        binding: binding.clone(),
        bot_token: token,
        transport,
    }))
}

/// Leitet die Transportwahl einer Bindung rein aus ihrer Konfiguration ab.
///
/// - `transport = "long_poll"` → Long-Poll.
/// - `transport = "webhook"` **ohne** `[transport_webhook]`-Block → sichtbarer
///   Rückfall auf Long-Poll (`webhook_fallback = true`).
/// - `transport = "webhook"` mit Block → Webhook, sofern `public_url`
///   ([`telegram_webhook_route`]) und `listen_addr` gültig sind; sonst `Err`
///   (fail closed — eine fehlerhafte Webhook-Angabe fällt **nicht** still auf
///   Long-Poll zurück).
/// - jeder andere Wert → `Err`.
fn telegram_transport_choice(
    binding: &TelegramChannelToml,
) -> Result<TelegramTransportChoice, String> {
    match binding.transport.as_str() {
        "long_poll" => Ok(TelegramTransportChoice::LongPoll {
            webhook_fallback: false,
        }),
        "webhook" => {
            let Some(webhook) = binding.transport_webhook.as_ref() else {
                return Ok(TelegramTransportChoice::LongPoll {
                    webhook_fallback: true,
                });
            };
            let route = telegram_webhook_route(&webhook.public_url)?;
            let listen_addr = webhook
                .listen_addr
                .trim()
                .parse::<SocketAddr>()
                .map_err(|_| "Telegram webhook listen_addr is not a socket address".to_owned())?;
            Ok(TelegramTransportChoice::Webhook {
                public_url: webhook.public_url.trim().to_owned(),
                route,
                listen_addr,
            })
        }
        _ => Err("unknown Telegram transport (expected long_poll or webhook)".to_owned()),
    }
}

/// Validiert eine Webhook-`public_url` und liefert den lokalen Routenpfad.
///
/// Telegram verlangt HTTPS und einen der Ports aus
/// [`TELEGRAM_WEBHOOK_PORTS`]; eingebettete Zugangsdaten werden abgelehnt.
/// Der Pfad wird zur Route des lokalen Listeners und muss deshalb eine vom
/// Router sicher akzeptierte Form haben ([`is_supported_webhook_route`]) —
/// `axum` würde Pfade mit Platzhalter-Syntax sonst beim Aufbau des Routers
/// mit einem Panic ablehnen.
fn telegram_webhook_route(public_url: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(public_url.trim())
        .map_err(|_| "Telegram webhook public_url is not a valid URL".to_owned())?;
    if url.scheme() != "https" {
        return Err("Telegram webhook public_url must use https".to_owned());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("Telegram webhook public_url has no host".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Telegram webhook public_url must not embed credentials".to_owned());
    }
    if !url
        .port_or_known_default()
        .is_some_and(|port| TELEGRAM_WEBHOOK_PORTS.contains(&port))
    {
        return Err(
            "Telegram webhook public_url port is not supported by Telegram (443, 80, 88, 8443)"
                .to_owned(),
        );
    }
    let route = url.path();
    if !is_supported_webhook_route(route) {
        return Err("Telegram webhook public_url path is not a supported route".to_owned());
    }
    Ok(route.to_owned())
}

/// Ob `path` als Webhook-Route taugt: `/` oder `/`-getrennte, nicht leere
/// Segmente aus `[A-Za-z0-9._~-]` ohne `.`/`..`-Segmente. Schließt damit
/// insbesondere `axum`-Platzhalter (`{…}`, `:name`, `*rest`) aus.
fn is_supported_webhook_route(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    rest.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
            })
    })
}

/// Telegrams Regel für `secret_token`: 1–256 Zeichen aus `[A-Za-z0-9_-]`.
/// Vorab geprüft, damit eine ungültige Angabe die Bindung beim Start
/// schließt, statt `setWebhook` in einer Backoff-Schleife scheitern zu lassen.
fn is_valid_telegram_webhook_secret(secret: &str) -> bool {
    (1..=256).contains(&secret.len())
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Übersetzt `[channel.telegram.topics].mode` in den Admission-Modus.
fn telegram_topic_mode(mode: &str) -> Option<TopicMode> {
    match mode {
        "per_topic_session" => Some(TopicMode::PerTopicSession),
        "shared_session" => Some(TopicMode::SharedSession),
        _ => None,
    }
}

/// Schließt Bindungen, die parallel nicht sicher betrieben werden können.
///
/// - **Geteilter Bot-Token:** Telegram erlaubt je Bot genau einen
///   Update-Konsumenten; zwei Bindungen mit demselben Token würden sich
///   gegenseitig `getUpdates`/`setWebhook` wegnehmen. Alle beteiligten
///   Bindungen werden geschlossen (keine willkürliche „erste gewinnt"-Wahl).
/// - **Geteilte Webhook-Listen-Adresse:** jede Webhook-Bindung bindet ihren
///   eigenen Listener; eine zweite Bindung auf derselben Adresse könnte nie
///   starten. Auch hier werden alle Beteiligten geschlossen.
fn disable_conflicting_telegram_bindings(bindings: &mut [TelegramBindingIngress]) {
    let mut shared_token = HashSet::new();
    let mut shared_listener = HashSet::new();
    for (left_index, left) in bindings.iter().enumerate() {
        let TelegramIngressMode::Enabled(left_plan) = &left.mode else {
            continue;
        };
        for (right_index, right) in bindings.iter().enumerate().skip(left_index + 1) {
            let TelegramIngressMode::Enabled(right_plan) = &right.mode else {
                continue;
            };
            if left_plan.bot_token.expose_secret() == right_plan.bot_token.expose_secret() {
                shared_token.insert(left_index);
                shared_token.insert(right_index);
            }
            let same_listener = matches!(
                (&left_plan.transport, &right_plan.transport),
                (TelegramTransportPlan::Webhook(left_hook), TelegramTransportPlan::Webhook(right_hook))
                    if left_hook.listen_addr == right_hook.listen_addr
            );
            if same_listener {
                shared_listener.insert(left_index);
                shared_listener.insert(right_index);
            }
        }
    }
    for (index, binding) in bindings.iter_mut().enumerate() {
        if shared_token.contains(&index) {
            binding.mode = TelegramIngressMode::Disabled(
                "multiple enabled Telegram bindings share one bot credential; Telegram allows a single update consumer per bot"
                    .to_owned(),
            );
        } else if shared_listener.contains(&index) {
            binding.mode = TelegramIngressMode::Disabled(
                "Telegram webhook listen_addr is shared with another enabled binding".to_owned(),
            );
        }
    }
}

/// Startzeile für `channels/tg`: je Bindung `<id>: <status>`, mit `; `
/// verbunden; ohne aktivierte Bindung eine einzelne Fail-closed-Meldung.
/// Enthält nie Geheimnisse oder die öffentliche Webhook-URL.
fn telegram_ingress_status(bindings: &[TelegramBindingIngress]) -> String {
    if bindings.is_empty() {
        return "deaktiviert (fail closed: no enabled Telegram binding is configured)".to_owned();
    }
    bindings
        .iter()
        .map(|binding| format!("{}: {}", binding.id, telegram_binding_status(&binding.mode)))
        .collect::<Vec<_>>()
        .join("; ")
}

fn telegram_binding_status(mode: &TelegramIngressMode) -> String {
    match mode {
        TelegramIngressMode::Enabled(plan) => match &plan.transport {
            TelegramTransportPlan::LongPoll {
                webhook_fallback: false,
            } => "bereit (sicherer Long-Poll-Adapter)".to_owned(),
            TelegramTransportPlan::LongPoll {
                webhook_fallback: true,
            } => "bereit (sicherer Long-Poll-Adapter; transport=webhook ohne [transport_webhook], Rückfall auf Long-Poll)".to_owned(),
            TelegramTransportPlan::Webhook(webhook) => format!(
                "bereit (sicherer Webhook-Adapter auf {}{})",
                webhook.listen_addr, webhook.route
            ),
        },
        TelegramIngressMode::Disabled(reason) => format!("deaktiviert (fail closed: {reason})"),
    }
}

fn resolve_telegram_secret(reference: &SecretRef, config: &ResolvedConfig) -> Option<SecretString> {
    match reference {
        SecretRef::Env(name) => resolve_env_ref(name, &config.env_layer)
            .filter(|value| !value.trim().is_empty())
            .map(SecretString::from),
        // The configured sealed-secret resolver currently opens only for
        // provider credentials. Do not bypass it with ad-hoc file/keyring
        // reads; unsupported references leave ingress disabled.
        SecretRef::File(_)
        | SecretRef::Keyring(_)
        | SecretRef::Secrets(_)
        | SecretRef::FileJson { .. } => None,
    }
}

/// Netzwerk-Timeouts für den Telegram-Bot-Client: `reqwest::Client::new()`
/// (der bisherige Konstruktor) kennt weder Connect- noch Request-Timeout, ein
/// stilles Netzloch hängt `getUpdates`/`sendMessage` sonst unbegrenzt (S7).
const TELEGRAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Request-Timeout für `getMe`/`sendMessage`/`editMessageText` (kurzlebige
/// Aufrufe ohne Long-Poll-Wartezeit).
const TELEGRAM_CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Request-Timeout für `getUpdates`: muss über dem an Telegram übergebenen
/// Long-Poll-`timeout_secs` (`LongPollConfig::new`s Vorgabe, 30 s) liegen,
/// sonst würde der Client jede normale Leerantwort selbst als Timeout werten.
const TELEGRAM_LONG_POLL_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

/// Baut einen `reqwest::Client` mit Connect- und Request-Timeout für einen
/// Telegram-Nutzungszweck. Reiner Konstruktions-Helfer ohne eigenen Zustand.
fn telegram_http_client(request_timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(TELEGRAM_CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .build()
        .map_err(|_| "Telegram HTTP client could not be built".to_owned())
}

/// Ein laufender Ingress-Transport einer Bindung, wie ihn
/// [`start_telegram_binding`] übergibt.
enum RunningTelegramIngress {
    /// Eigener Long-Poll-Thread; endet mit einem `TransportResult`.
    LongPoll(std::thread::JoinHandle<TransportResult<()>>),
    /// Fertig konfigurierter Webhook-Listener; läuft, bis
    /// [`run_webhook_server_with_shutdown`] zurückkehrt oder der Future verworfen wird.
    Webhook(WebhookConfig),
}

/// Zugangsdaten, um den Webhook einer Bindung beim Shutdown abzumelden.
struct TelegramWebhookTeardown {
    binding_id: String,
    bot_token: SecretString,
}

/// Ein nie endender Supervisions-Future je Telegram-Bindung.
type TelegramBindingTask = Pin<Box<dyn std::future::Future<Output = Infallible>>>;

/// Treibt alle Bindungs-Supervisionen nebenläufig auf der aktuellen Task.
///
/// Bewusst ohne `tokio::spawn`: die Supervisions-Futures müssen dadurch
/// nicht `Send` sein, und sie enden mit dem umgebenden `select!` in
/// [`supervise`] (Shutdown verwirft sie alle zugleich). Jeder Weckruf pollt
/// alle Futures; bei einer Handvoll Bindungen ist das vernachlässigbar.
async fn drive_telegram_bindings(mut tasks: Vec<TelegramBindingTask>) -> Infallible {
    std::future::poll_fn(move |cx| {
        for task in &mut tasks {
            if let Poll::Ready(never) = std::future::Future::poll(task.as_mut(), cx) {
                return Poll::Ready(never);
            }
        }
        Poll::Pending
    })
    .await
}

/// Offset-Verzeichnis des Long-Poll-Runners einer Bindung.
///
/// [`TELEGRAM_LEGACY_OFFSET_BINDING`] behält `<state>/telegram-offset`; jede
/// andere Bindung bekommt ein eigenes Unterverzeichnis, benannt nach der
/// Hex-Kodierung ihrer ID (kollisionsfrei, dateisystemsicher und ohne `.`,
/// kann also nie mit `offset.json`/`offset.lock` des Legacy-Pfads
/// kollidieren). Zwei Bindungen teilen sich damit nie einen Offset.
fn telegram_offset_root(channel_state: &Path, binding_id: &str) -> PathBuf {
    let legacy = channel_state.join("telegram-offset");
    if binding_id == TELEGRAM_LEGACY_OFFSET_BINDING {
        return legacy;
    }
    legacy.join(telegram_binding_dir_name(binding_id))
}

/// Gültigkeit der Freigabe-Schaltflächen (Approval-Tokens) und Obergrenze
/// der Wartezeit geparkter Turn-Freigaben.
const TELEGRAM_APPROVAL_TTL: SignedDuration = SignedDuration::from_secs(24 * 60 * 60);

/// Höchstzahl gleichzeitig laufender Chat-Sitzungen je Bindung.
const TELEGRAM_MAX_PARALLEL_SESSIONS: usize = 4;

/// Hex-Kodierung einer Bindungs-ID: kollisionsfrei und dateisystemsicher.
fn telegram_binding_dir_name(binding_id: &str) -> String {
    binding_id
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

/// Chat-Zustand (Workspace-Wahl, Sitzungsgeneration) einer Bindung:
/// `<channel-state>/telegram-chats/<hex(binding id)>`.
fn telegram_chat_state_root(channel_state: &Path, binding_id: &str) -> PathBuf {
    channel_state
        .join("telegram-chats")
        .join(telegram_binding_dir_name(binding_id))
}

/// Über Transport-Neustarts hinweg stabile Dienste einer Telegram-Bindung.
struct TelegramBindingServices {
    /// Aufgelöster Root-Space; Grundlage der Runtime-Montage je Turn im
    /// Sitzungs-Dispatcher (Chats mit Arbeitsbereich).
    home: PathBuf,
    /// Autoritative Workspace-Auflösung aus `binding.workspaces`.
    workspaces: Arc<harw_authority::WorkspaceRegistry>,
    /// Chat-Zustand; geteilt von Befehlsverarbeitung und Sitzungs-Dispatcher.
    chat_state: Arc<ChatStateStore>,
    /// Telegram-User-IDs aus `security.admin_identities`.
    admin_sender_ids: HashSet<String>,
    unknown_command_fallback: UnknownCommandFallback,
    /// Alias des Workspaces mit `default = true`, falls vorhanden.
    default_workspace_alias: Option<String>,
}

/// Baut die bindungsbezogenen Dienste aus der Konfiguration.
///
/// # Errors
/// Eine deutsche Begründung (ohne Geheimnisse), wenn die Workspace-Registry
/// nicht gebaut werden kann (Wurzel fehlt, liegt außerhalb des Harness-Homes,
/// doppelte Registrierung) oder `commands.unknown_command_fallback`
/// unbekannt ist. Die Bindung bleibt dann fail-closed deaktiviert.
fn telegram_binding_services(
    home: &Path,
    binding: &TelegramChannelToml,
    profile: &Path,
) -> Result<TelegramBindingServices, String> {
    let registrations = binding
        .workspaces
        .iter()
        .map(|workspace| {
            let tenant = TenantId::try_from_str(workspace.tenant.trim()).map_err(|_| {
                format!(
                    "Arbeitsbereich {:?}: ungültiger Mandant",
                    workspace.alias.trim()
                )
            })?;
            let alias = WorkspaceId::try_from_str(workspace.alias.trim())
                .map_err(|_| "Arbeitsbereich mit ungültigem Alias".to_owned())?;
            Ok(harw_authority::WorkspaceRegistration {
                tenant,
                workspace: alias,
                root: PathBuf::from(workspace.root.trim()),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let workspaces = harw_authority::WorkspaceRegistry::build(home, registrations)
        .map_err(|error| format!("Arbeitsbereiche nicht auflösbar: {error}"))?;
    let unknown_command_fallback =
        UnknownCommandFallback::parse(&binding.commands.unknown_command_fallback)?;
    let default_workspace_alias = binding
        .workspaces
        .iter()
        .find(|workspace| workspace.default)
        .map(|workspace| workspace.alias.trim().to_owned());
    let admin_sender_ids = binding
        .security
        .admin_identities
        .iter()
        .map(ToString::to_string)
        .collect::<HashSet<_>>();
    let chat_state = Arc::new(ChatStateStore::new(&telegram_chat_state_root(
        &profile.join("channel-state"),
        &binding.id,
    )));
    Ok(TelegramBindingServices {
        home: home.to_path_buf(),
        workspaces: Arc::new(workspaces),
        chat_state,
        admin_sender_ids,
        unknown_command_fallback,
        default_workspace_alias,
    })
}

/// Startet Admission, Runtime-Handoff und Transport einer Bindung.
///
/// # Description
/// Reihenfolge: Bot-Identität (`getMe`), Befehlsmenü
/// ([`telegram_commands::publish_telegram_command_menu`]),
/// Transport-Lebenszyklus — im Long-Poll-Modus ein `deleteWebhook` (ein nach
/// einem Absturz verwaister Webhook würde `getUpdates` sonst dauerhaft mit
/// 409 blockieren), im Webhook-Modus `setWebhook` mit `public_url` und
/// `secret_token` —, danach Renderer (mit dem geteilten `approval_tokens`),
/// Anhang-Aufnahme, Sitzungs-Dispatcher und Befehlsverarbeitung, erst dann
/// die Throttle-/Pairing-Notice-, Callback- und Admission-Threads und der
/// Transport selbst (mit installiertem Callback-Consumer aus
/// [`telegram_callbacks::spawn_callback_worker`]). Scheitert ein Schritt vor
/// den Threads, bleibt nichts halb gestartet zurück.
///
/// # Errors
/// Eine inhaltsfreie Meldung (nie Token oder Secret), wenn ein Schritt
/// scheitert; [`supervise_telegram_binding`] versucht es dann mit Backoff
/// erneut.
async fn start_telegram_binding(
    plan: &TelegramIngressPlan,
    provider: Arc<dyn ModelProvider>,
    profile: &Path,
    sessions_root: &Path,
    services: &TelegramBindingServices,
    work_requests: Arc<WorkRequestStore>,
    approval_tokens: Arc<ApprovalTokenStore>,
) -> Result<RunningTelegramIngress, String> {
    let channel_id = ChannelId::try_from(plan.binding.id.clone())
        .map_err(|_| "Telegram channel id is invalid".to_owned())?;
    let mut channel_config = TelegramChannelConfig::new(channel_id.clone());
    channel_config.pinned_sender_ids = plan
        .binding
        .security
        .pinned_identities
        .iter()
        .map(ToString::to_string)
        .collect::<HashSet<_>>();
    channel_config.allowed_group_chats =
        plan.binding.groups.allowed_chats.iter().cloned().collect();
    channel_config.group_allowed_senders = plan
        .binding
        .groups
        .group_allowed_senders
        .iter()
        .cloned()
        .collect();
    channel_config.require_mention_in_groups = plan.binding.groups.require_mention;
    // §3.5 `max_updates_per_peer_per_min`: without this, `TelegramChannel`
    // would silently fall back to its own compiled-in default instead of the
    // operator's configured ceiling.
    channel_config.max_updates_per_peer_per_min =
        plan.binding.rate_limit.max_updates_per_peer_per_min;
    channel_config.topic_mode = telegram_topic_mode(&plan.binding.topics.mode)
        .ok_or_else(|| "Telegram topic mode is invalid".to_owned())?;
    channel_config.allow_unpinned_pairing = plan.binding.security.allow_unpinned_pairing;
    channel_config.admin_sender_ids = services.admin_sender_ids.clone();
    channel_config.attachment_max_bytes = plan.binding.attachments.max_bytes;
    channel_config.attachment_max_count =
        usize::try_from(plan.binding.attachments.max_count_per_message).unwrap_or(usize::MAX);
    channel_config.attachment_allowed_kinds = plan.binding.attachments.mime_allowlist.clone();

    let bot_http = telegram_http_client(TELEGRAM_CLIENT_REQUEST_TIMEOUT)?;
    let bot_client = Arc::new(TelegramClient::with_http_client(
        bot_http,
        plan.bot_token.expose_secret(),
    ));
    let bot = bot_client
        .get_me()
        .await
        .map_err(|_| "Telegram bot identity lookup failed".to_owned())?;
    telegram_commands::publish_telegram_command_menu(&plan.binding, &bot_client).await;
    match &plan.transport {
        TelegramTransportPlan::LongPoll { webhook_fallback } => {
            if *webhook_fallback {
                tracing::warn!(binding = %plan.binding.id, "transport = \"webhook\" without [transport_webhook]; falling back to long polling");
            }
            if let Err(error) = bot_client.delete_webhook(false).await {
                tracing::warn!(binding = %plan.binding.id, error = %error, "Telegram webhook could not be cleared before long polling");
            }
        }
        TelegramTransportPlan::Webhook(webhook) => {
            if !webhook.listen_addr.ip().is_loopback() {
                tracing::warn!(binding = %plan.binding.id, listen_addr = %webhook.listen_addr, "Telegram webhook listener binds a non-loopback address; exposure is the operator's responsibility");
            }
            bot_client
                .set_webhook(
                    &webhook.public_url,
                    Some(webhook.secret_token.expose_secret()),
                    &TELEGRAM_ALLOWED_UPDATES,
                    false,
                )
                .await
                .map_err(|_| "Telegram webhook registration failed".to_owned())?;
        }
    }
    // Feed the configured per-chat outbound ceiling through instead of the
    // renderer's hardcoded default (§3.5 `max_outbound_per_chat_per_sec`).
    // `RendererConfig::default()` supplies every other field (message-length
    // cap, global bucket, cache size, streaming strategy) unchanged.
    let renderer_config = RendererConfig {
        per_chat_per_sec: plan.binding.rate_limit.max_outbound_per_chat_per_sec,
        ..RendererConfig::default()
    };
    // Derselbe Token-Store wie der Adapter (und über Neustarts hinweg):
    // ausgegebene Freigabe-Schaltflächen bleiben nach einem Transport-Neustart
    // einlösbar.
    let renderer = Arc::new(TelegramRenderer::with_config_and_approval_tokens(
        Arc::clone(&bot_client),
        renderer_config,
        Arc::clone(&approval_tokens),
    ));
    let outbound: Arc<dyn TelegramOutbound> = renderer.clone();
    let throttle_outbound = Arc::clone(&outbound);
    let pairing_outbound = Arc::clone(&outbound);
    let callback_outbound = Arc::clone(&outbound);
    let callback_work_requests = Arc::clone(&work_requests);
    let attachments = match TelegramAttachmentIntake::from_binding(
        &plan.binding,
        Arc::clone(&bot_client),
        &telegram_attachment_cache_root(profile),
    ) {
        Ok(intake) => Some(Arc::new(intake)),
        Err(reason) => {
            tracing::warn!(binding = %plan.binding.id, reason = %reason, "Telegram attachment intake unavailable; attachments will be rejected");
            None
        }
    };
    let sessions = TelegramSessionDispatcher::new(TelegramSessionConfig {
        provider,
        transcript_root: sessions_root.to_path_buf(),
        renderer,
        chat_state: Arc::clone(&services.chat_state),
        attachments,
        max_parallel_sessions: TELEGRAM_MAX_PARALLEL_SESSIONS,
        approval_ttl: TELEGRAM_APPROVAL_TTL,
        home: services.home.clone(),
        workspaces: Arc::clone(&services.workspaces),
        default_workspace_alias: services.default_workspace_alias.clone(),
    });
    let turn_approvals = sessions.turn_approvals();
    let consumer = Arc::new(GatewayTelegramConsumer {
        commands: TelegramCommandHandler {
            outbound,
            work_requests,
            workspaces: Arc::clone(&services.workspaces),
            chat_state: Arc::clone(&services.chat_state),
            admin_sender_ids: services.admin_sender_ids.clone(),
            unknown_command_fallback: services.unknown_command_fallback,
            default_workspace_alias: services.default_workspace_alias.clone(),
        },
        sessions,
    });
    let (ingress_tx, ingress_rx) = mpsc::sync_channel(128);
    let (throttle_tx, throttle_rx) = mpsc::channel::<ThrottleNotice>();
    let (pairing_tx, pairing_rx) = mpsc::channel::<PairingNotice>();
    let adapter = TelegramChannel::with_ingress_receiver(
        channel_config,
        Arc::new(PairingStore::new(
            &profile.join("channel-state").join("pairing"),
        )),
        ingress_rx,
    )
    .with_throttle_sink(throttle_tx)
    .with_pairing_sink(pairing_tx)
    .with_approval_tokens(approval_tokens);
    // Delivers at most one "you're sending too fast" reply per rate-limit
    // window (§3.5): the admission perimeter (`TelegramChannel::admit`) only
    // decides and emits the notice, it never sends network traffic itself.
    std::thread::Builder::new()
        .name("harw-telegram-throttle-notice".to_owned())
        .spawn(move || {
            for notice in throttle_rx {
                let Ok(chat_id) = notice.peer.as_str().parse::<i64>() else {
                    tracing::warn!("Telegram throttle notice has a non-numeric peer");
                    continue;
                };
                let thread_id = notice
                    .thread
                    .as_ref()
                    .and_then(|thread| thread.as_str().parse::<i64>().ok());
                if let Err(error) = throttle_outbound.send(chat_id, thread_id, &notice.content) {
                    tracing::error!(error = %error, "Telegram throttle notice delivery failed");
                }
            }
        })
        .map_err(|_| "Telegram throttle-notice thread could not start".to_owned())?;
    // Delivers the confirmation or the uniform failure reply of an in-channel
    // `/pair <code>` redemption (§3.2). Like throttling, the adapter only
    // decides and emits the notice. The notice content is never logged.
    std::thread::Builder::new()
        .name("harw-telegram-pairing-notice".to_owned())
        .spawn(move || {
            for notice in pairing_rx {
                let Ok(chat_id) = notice.peer.as_str().parse::<i64>() else {
                    tracing::warn!("Telegram pairing notice has a non-numeric peer");
                    continue;
                };
                let thread_id = notice
                    .thread
                    .as_ref()
                    .and_then(|thread| thread.as_str().parse::<i64>().ok());
                if let Err(error) = pairing_outbound.send(chat_id, thread_id, &notice.content) {
                    tracing::error!(error = %error, "Telegram pairing notice delivery failed");
                }
            }
        })
        .map_err(|_| "Telegram pairing-notice thread could not start".to_owned())?;
    // Inline-button clicks: the transport hands them to a non-blocking
    // consumer; this worker validates and answers them off the ingress path.
    let callback_consumer = spawn_callback_worker(GatewayCallbackWorker {
        adapter: adapter.clone(),
        work_requests: callback_work_requests,
        outbound: callback_outbound,
        bot_client: Arc::clone(&bot_client),
        turn_approvals: Some(turn_approvals),
    })?;
    std::thread::Builder::new()
        .name("harw-telegram-admission".to_owned())
        .spawn(move || {
            let (admitted_tx, admitted_rx) = mpsc::channel();
            let adapter_for_ingress = adapter.clone();
            let ingress = std::thread::spawn(move || adapter_for_ingress.run_ingress(admitted_tx));
            for event in admitted_rx {
                match adapter.derive_session_key(&event) {
                    Ok(key) if adapter.admit(&event, &key) == Admission::Admitted => {
                        consumer.handle_admitted(key, event);
                    }
                    Ok(_) => tracing::warn!("Telegram event lost admission before runtime handoff"),
                    Err(error) => tracing::error!(error = %error, "admitted Telegram event lost its session key"),
                }
            }
            if let Ok(Err(error)) = ingress.join() {
                tracing::error!(error = %error, "Telegram admission runner stopped");
            }
        })
        .map_err(|_| "Telegram admission handoff thread could not start".to_owned())?;
    match &plan.transport {
        TelegramTransportPlan::LongPoll { .. } => {
            let offset_store = TelegramOffsetStore::new(telegram_offset_root(
                &profile.join("channel-state"),
                &plan.binding.id,
            ));
            let poll_http = telegram_http_client(TELEGRAM_LONG_POLL_REQUEST_TIMEOUT)?;
            let long_poll = LongPollConfig::new(
                TelegramClient::with_http_client(poll_http, plan.bot_token.expose_secret()),
                channel_id.as_str(),
                bot,
                offset_store,
                ingress_tx,
                LongPollShutdown::default(),
            )
            .with_callback_consumer(callback_consumer);
            spawn_long_poll_thread(long_poll)
                .map(RunningTelegramIngress::LongPoll)
                .map_err(|_| "Telegram long-poll thread could not start".to_owned())
        }
        TelegramTransportPlan::Webhook(webhook) => Ok(RunningTelegramIngress::Webhook(
            WebhookConfig::new(
                webhook.listen_addr,
                webhook.route.clone(),
                webhook.secret_token.expose_secret(),
                channel_id.as_str(),
                bot.id,
                bot.username.clone(),
                ingress_tx,
            )
            .with_callback_consumer(callback_consumer),
        )),
    }
}

/// Supervidiert genau eine Telegram-Bindung mit Neustart-Backoff.
///
/// Zwei Fälle dürfen eine Bindung nicht dauerhaft stillegen (S6/S9): ein
/// Start-Fehler (z. B. kein Netz beim Boot, `getMe`/`setWebhook` schlägt
/// fehl) und ein späteres Enden des Transports (Long-Poll-Thread endet —
/// voller Sink über zu viele Wiederholungen, I/O-Fehler im Offset-Store,
/// Panic — oder der Webhook-Listener kehrt zurück, z. B. weil die
/// Listen-Adresse belegt ist). Beide Fälle führen zu einem erneuten Versuch
/// nach [`telegram_restart_backoff`]. Der Backoff wird nur nach einem
/// **stabilen** Lauf zurückgesetzt ([`telegram_attempt_after_run`]), damit
/// ein sofort wieder endender Transport (z. B. belegter Port) die Bot-API
/// nicht im Sekundentakt mit `getMe`/`setWebhook` belastet.
///
/// Endet nie (`Infallible`); [`supervise`]s `select!` verwirft den Future
/// beim Shutdown. Andere Bindungen laufen davon unabhängig weiter.
async fn supervise_telegram_binding(
    plan: TelegramIngressPlan,
    provider: Arc<dyn ModelProvider>,
    profile: PathBuf,
    sessions_root: PathBuf,
    services: TelegramBindingServices,
    work_requests: Arc<WorkRequestStore>,
) -> Infallible {
    let binding_id = plan.binding.id.clone();
    // Ein einziger Approval-Token-Store je Bindung, bewusst außerhalb der
    // Neustart-Schleife: Adapter (Callback-Prüfung) und Renderer (Ausgabe)
    // teilen ihn, und offene Freigaben überleben einen Transport-Neustart.
    let approval_tokens = Arc::new(ApprovalTokenStore::with_ttl(TELEGRAM_APPROVAL_TTL));
    let mut attempt: u32 = 0;
    loop {
        let started = Instant::now();
        match start_telegram_binding(
            &plan,
            Arc::clone(&provider),
            &profile,
            &sessions_root,
            &services,
            Arc::clone(&work_requests),
            Arc::clone(&approval_tokens),
        )
        .await
        {
            Ok(RunningTelegramIngress::LongPoll(handle)) => {
                // Das blockierende `JoinHandle::join()` gehört nicht auf die
                // Tokio-Event-Loop dieser Task.
                match tokio::task::spawn_blocking(move || handle.join()).await {
                    Ok(Ok(Ok(()))) => {
                        tracing::warn!(binding = %binding_id, "Telegram long-poll runner stopped cleanly; restarting with backoff");
                    }
                    Ok(Ok(Err(error))) => {
                        tracing::error!(binding = %binding_id, error = %error, "Telegram long-poll runner stopped; restarting with backoff");
                    }
                    Ok(Err(_)) => {
                        tracing::error!(binding = %binding_id, "Telegram long-poll runner panicked; restarting with backoff");
                    }
                    Err(_) => {
                        tracing::error!(binding = %binding_id, "Telegram long-poll supervision task panicked; restarting with backoff");
                    }
                }
            }
            Ok(RunningTelegramIngress::Webhook(config)) => {
                match run_webhook_server_with_shutdown(config, std::future::pending::<()>()).await {
                    Ok(()) => {
                        tracing::warn!(binding = %binding_id, "Telegram webhook listener stopped; restarting with backoff");
                    }
                    Err(error) => {
                        tracing::error!(binding = %binding_id, error = %error, "Telegram webhook listener failed; restarting with backoff");
                    }
                }
            }
            Err(error) => {
                tracing::error!(binding = %binding_id, error = %error, "Telegram ingress setup failed; retrying with backoff");
            }
        }
        attempt = telegram_attempt_after_run(attempt, started.elapsed());
        let backoff = telegram_restart_backoff(attempt);
        attempt = attempt.saturating_add(1);
        tokio::time::sleep(backoff).await;
    }
}

/// Reine Entscheidungsfunktion: welcher Backoff-Zähler nach einem Lauf gilt.
///
/// Ein Lauf von mindestens [`TELEGRAM_STABLE_RUN`] gilt als gesund und setzt
/// den Zähler auf `0`; ein kürzerer Lauf (Start-Fehler oder sofort endender
/// Transport) behält ihn, sodass aufeinanderfolgende Fehlschläge länger
/// warten.
fn telegram_attempt_after_run(attempt: u32, ran_for: Duration) -> u32 {
    if ran_for >= TELEGRAM_STABLE_RUN {
        0
    } else {
        attempt
    }
}

/// Meldet beim Shutdown den Webhook jeder Webhook-Bindung ab.
///
/// `drop_pending_updates = false`: Telegram puffert ausstehende Updates und
/// liefert sie nach dem nächsten `setWebhook` (oder an einen Long-Poll)
/// aus. Jeder Aufruf ist durch [`TELEGRAM_WEBHOOK_TEARDOWN_TIMEOUT`]
/// begrenzt; Fehler werden nur geloggt — der Shutdown läuft weiter.
async fn teardown_telegram_webhooks(teardowns: &[TelegramWebhookTeardown]) {
    for teardown in teardowns {
        let http = match telegram_http_client(TELEGRAM_CLIENT_REQUEST_TIMEOUT) {
            Ok(http) => http,
            Err(error) => {
                tracing::warn!(binding = %teardown.binding_id, error = %error, "Telegram webhook could not be removed on shutdown");
                continue;
            }
        };
        let client = TelegramClient::with_http_client(http, teardown.bot_token.expose_secret());
        match tokio::time::timeout(
            TELEGRAM_WEBHOOK_TEARDOWN_TIMEOUT,
            client.delete_webhook(false),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::info!(binding = %teardown.binding_id, "Telegram webhook removed on shutdown");
            }
            Ok(Err(error)) => {
                tracing::warn!(binding = %teardown.binding_id, error = %error, "Telegram webhook could not be removed on shutdown");
            }
            Err(_) => {
                tracing::warn!(binding = %teardown.binding_id, "Telegram webhook removal timed out on shutdown");
            }
        }
    }
}

/// Reine Entscheidungsfunktion für den Telegram-Neustart-Backoff:
/// exponentiell (Faktor 2) ab 1 s, gedeckelt auf 60 s, ohne Jitter — bewusst
/// deterministisch statt zufällig, damit sie ohne Zeitsteuerung testbar
/// bleibt. `attempt` zählt aufeinanderfolgende Fehlschläge seit dem letzten
/// erfolgreichen Start (`0` = erster erneuter Versuch nach dem ersten
/// Fehlschlag).
fn telegram_restart_backoff(attempt: u32) -> Duration {
    const INITIAL: Duration = Duration::from_secs(1);
    const MAX: Duration = Duration::from_secs(60);
    INITIAL
        .checked_mul(1u32 << attempt.min(6))
        .unwrap_or(MAX)
        .min(MAX)
}

/// Zählt persistente Workbench-Scopes unter `<knowledge>/workbench/`.
///
/// Die Scopes überleben jeden Neustart des Daemons und werden hier beim Start
/// sichtbar gemacht. Sie enthalten jedoch keinen Gateway-Laufzeitstatus.
fn scan_workbench(knowledge: &KnowledgeStore) -> usize {
    let dir = knowledge.root().join("workbench");
    match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .count(),
        Err(_) => 0,
    }
}

/// Beschreibt ehrlich, was der Gateway aus gespeicherten Workbench-Scopes
/// ableiten kann: ihre Existenz, nicht die Fortsetzbarkeit einer Ausführung.
fn workbench_status(scope_count: usize) -> String {
    format!(
        "{scope_count} gespeicherte Scope(s) [session/project auf Platte; keine Laufzeitfortsetzung]"
    )
}

/// Aktualisiert die Aktivitätsuhr auf „jetzt".
fn touch_activity(activity: &ActivityClock) {
    if let Ok(mut guard) = activity.lock() {
        *guard = Instant::now();
    }
}

/// Liefert die vergangene Zeit seit der letzten Aktivität.
fn idle_for(activity: &ActivityClock) -> Duration {
    activity
        .lock()
        .map(|guard| guard.elapsed())
        .unwrap_or_default()
}

/// Kurzstatus des Traum-Schedulers für den Start-Banner.
fn dream_status(settings: &harw_ops::dream_run::DreamSettings) -> String {
    if !settings.enabled {
        return "deaktiviert ([dream] enabled = false; /dream run bleibt möglich)".to_owned();
    }
    let trigger = match &settings.schedule {
        Some(expression) => format!("Zeitplan „{expression}“ (UTC)"),
        None => format!("nach {} min Idle", settings.idle.as_secs() / 60),
    };
    format!(
        "aktiv ({trigger}, Abstand ≥ {} min, Budget {} Token; Zustand in dreams/state.json übersteht Neustarts)",
        settings.cooldown.as_secs() / 60,
        settings.budget_tokens,
    )
}

/// Traum-Scheduler: prüft je [`DREAM_TICK`] die Entscheidung
/// ([`harw_ops::dream_run::scheduler_decision`] über den dauerhaften Zustand
/// `dreams/state.json` und die Aktivitätsuhr) und startet bei Fälligkeit
/// einen governten Lauf über den gemeinsamen Kern. Läuft, bis der umgebende
/// `select!` endet; bei `[dream] enabled = false` wartet er untätig.
///
/// # Concurrency
/// Läuft auf derselben Single-Thread-Runtime wie die Channels; ein Traumlauf
/// und ein Channel-Turn wechseln sich kooperativ ab. Ein gleichzeitiger
/// `/dream run` aus einer anderen Sitzung hält die Lauf-Sperre; der
/// Scheduler überspringt dann den Takt (`DreamRunError::Busy`).
async fn dream_scheduler(
    launcher: &harw_runtime::dream_run::RuntimeDreamLauncher,
    activity: &ActivityClock,
) {
    use harw_ops::dream_run::{DreamLauncher, DreamRunError, SchedulerDecision};

    let settings = launcher.settings().clone();
    if !settings.enabled {
        std::future::pending::<()>().await;
    }
    let mut ticker = tokio::time::interval(DREAM_TICK);
    let mut reported_invalid = false;

    loop {
        ticker.tick().await;

        let state = match harw_knowledge::dream::read_scheduler_state(launcher.knowledge()) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("dream: Zustand unlesbar ({error}); rechne ohne letzten Lauf");
                harw_knowledge::DreamSchedulerState::default()
            }
        };
        let decision = harw_ops::dream_run::scheduler_decision(
            &settings,
            &state,
            Some(idle_for(activity)),
            Timestamp::now(),
        );
        let trigger = match decision {
            SchedulerDecision::Due(trigger) => trigger,
            SchedulerDecision::InvalidSchedule(error) => {
                if !reported_invalid {
                    eprintln!("dream: {error} — kein automatischer Lauf");
                    reported_invalid = true;
                }
                continue;
            }
            _ => continue,
        };

        match launcher.launch(trigger).await {
            Ok(outcome) => {
                eprintln!(
                    "dream: Bericht abgelegt → {} ({} Vorschlag/Vorschläge, /dream review {})",
                    outcome.report_path.display(),
                    outcome.data.suggestions.len(),
                    outcome.work_id
                );
                // Nach dem Träumen die Uhr zurücksetzen, damit nicht sofort
                // erneut ausgelöst wird (der Cooldown greift ohnehin).
                touch_activity(activity);
            }
            Err(DreamRunError::Busy) => {}
            Err(error) => eprintln!("dream: Lauf fehlgeschlagen: {error}"),
        }
    }
}

/// Keeps Telegram transcript provenance separate from CLI, worker, and Dream
/// turns. The random session id is trusted runtime state, while the structural
/// channel key remains in the ingress audit path.
fn telegram_thread_for_session(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("gateway-telegram:{}", session_id.as_str()))
}

fn build_telegram_state_store(transcript_root: &Path) -> TranscriptStateStore {
    TranscriptStateStore::new(
        TranscriptStore::new(transcript_root),
        telegram_thread_for_session,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Bildet den Fehlerpfad von [`run`] ohne Prozess-Globalzustand ab: `run`
    /// ruft genau [`mount_gateway_assembly`] vor jedem Subsystemstart auf.
    /// Ein Home ohne Provider-Konfiguration muss dort mit `Err` enden, statt
    /// wie früher still einen Echo-Provider zu montieren (G-048).
    #[test]
    fn test_gateway_run_without_provider_fails_instead_of_echo() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("temp cwd"))?;
        let sessions_root = home.path().join("sessions");

        let result = mount_gateway_assembly(home.path(), cwd.path(), &sessions_root);

        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "a home without provider config must not mount a gateway runtime".into(),
            ));
        };
        assert!(
            error.starts_with("gateway: "),
            "mount error must come from the gateway assembly, got: {error}"
        );
        assert!(
            !error.contains("Echo"),
            "mount error must not describe an echo fallback, got: {error}"
        );
        Ok(())
    }

    #[test]
    fn gateway_provider_status_reports_configured_provider_and_model() {
        let mut config = ResolvedConfig::default();
        config.harness.default_provider = Some("anthropic".to_owned());
        config.harness.default_model = Some("claude".to_owned());

        let status = gateway_provider_status(&config);

        assert_eq!(status, "bereit (provider=anthropic, model=claude)");
    }

    #[test]
    fn scan_workbench_counts_only_scope_dirs() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let store = KnowledgeStore::new(tmp.path());
        let bench = tmp.path().join("workbench");
        std::fs::create_dir_all(bench.join("session:a")).map_err(ctx("create session dir"))?;
        std::fs::create_dir_all(bench.join("project:harwness"))
            .map_err(ctx("create project dir"))?;
        std::fs::write(bench.join("stray.md"), b"x").map_err(ctx("write stray file"))?;
        assert_eq!(scan_workbench(&store), 2);
        Ok(())
    }

    #[test]
    fn scan_workbench_missing_dir_is_zero() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let store = KnowledgeStore::new(tmp.path());
        assert_eq!(scan_workbench(&store), 0);
        Ok(())
    }

    #[test]
    fn workbench_status_does_not_claim_execution_can_resume() {
        let status = workbench_status(2);

        assert!(status.contains("2 gespeicherte Scope(s)"));
        assert!(status.contains("keine Laufzeitfortsetzung"));
        assert!(!status.contains("fortsetzbar"));
    }

    #[test]
    fn dream_status_reflects_the_dream_config() {
        let mut config = ResolvedConfig::default();
        let settings = harw_ops::dream_run::DreamSettings::from_config(&config);
        let status = dream_status(&settings);
        assert!(status.contains("nach 15 min Idle"), "{status}");
        assert!(status.contains("übersteht Neustarts"), "{status}");

        config.harness.dream.schedule = Some("0 3 * * 1".to_owned());
        let status = dream_status(&harw_ops::dream_run::DreamSettings::from_config(&config));
        assert!(status.contains("Zeitplan „0 3 * * 1“"), "{status}");

        config.harness.dream.enabled = Some(false);
        let status = dream_status(&harw_ops::dream_run::DreamSettings::from_config(&config));
        assert!(status.starts_with("deaktiviert"), "{status}");
    }

    #[test]
    fn idle_clock_starts_near_zero_and_touch_resets() {
        let clock: ActivityClock = Arc::new(Mutex::new(Instant::now()));
        assert!(idle_for(&clock) < Duration::from_secs(1));
        touch_activity(&clock);
        assert!(idle_for(&clock) < Duration::from_secs(1));
    }

    #[test]
    fn telegram_ingress_fails_closed_without_an_enabled_binding() {
        let mode = telegram_ingress_modes(&ResolvedConfig::default());

        let diagnostic = telegram_ingress_status(&mode);
        assert!(diagnostic.contains("fail closed"));
        assert!(diagnostic.contains("no enabled Telegram binding"));
        assert!(!diagnostic.contains("TELEGRAM_BOT_TOKEN"));
        assert!(!diagnostic.contains("secrets/"));
    }

    #[test]
    fn telegram_ingress_requires_a_resolved_non_secret_credential_reference() -> TestResult {
        let file: harw_config::ChannelFileToml = toml::from_str(
            r#"
[[channel.telegram]]
id = "telegram:ops"
bot_token_ref = "env:TELEGRAM_TEST_TOKEN"

[channel.telegram.security]
pinned_identities = [123456789]
"#,
        )
        .map_err(ctx("parse channel file"))?;
        let config = ResolvedConfig {
            channels: harw_config::channel_toml::flatten_channel_file(file),
            ..Default::default()
        };

        let mode = telegram_ingress_modes(&config);
        let diagnostic = telegram_ingress_status(&mode);

        assert!(diagnostic.contains("fail closed"));
        assert!(diagnostic.contains("credential"));
        assert!(!diagnostic.contains("TELEGRAM_TEST_TOKEN"));
        Ok(())
    }

    #[test]
    fn telegram_pairing_commands_never_reach_the_model() {
        assert!(is_telegram_pairing_command("/pair Y2GQ-DEYE"));
        assert!(is_telegram_pairing_command(" /PAIR@LinLinBot Y2GQ-DEYE "));
        assert!(!is_telegram_pairing_command("/pairing Y2GQ-DEYE"));
        assert!(!is_telegram_pairing_command("hey /pair Y2GQ-DEYE"));
    }

    #[test]
    fn telegram_restart_backoff_grows_exponentially_and_resets_at_zero() {
        assert_eq!(telegram_restart_backoff(0), Duration::from_secs(1));
        assert_eq!(telegram_restart_backoff(1), Duration::from_secs(2));
        assert_eq!(telegram_restart_backoff(2), Duration::from_secs(4));
        assert_eq!(telegram_restart_backoff(3), Duration::from_secs(8));
    }

    #[test]
    fn telegram_restart_backoff_caps_instead_of_growing_unbounded() {
        assert_eq!(telegram_restart_backoff(6), Duration::from_secs(60));
        assert_eq!(telegram_restart_backoff(100), Duration::from_secs(60));
        assert_eq!(telegram_restart_backoff(u32::MAX), Duration::from_secs(60));
    }

    /// Baut eine `ResolvedConfig` aus einer `channels/*.toml`-Datei und
    /// einem Env-Layer (statt Prozessumgebung, damit Tests isoliert bleiben).
    fn telegram_test_config(
        channel_file: &str,
        env: &[(&str, &str)],
    ) -> TestResult<ResolvedConfig> {
        let file: harw_config::ChannelFileToml =
            toml::from_str(channel_file).map_err(ctx("parse channel file"))?;
        let mut config = ResolvedConfig {
            channels: harw_config::channel_toml::flatten_channel_file(file),
            ..Default::default()
        };
        for (name, value) in env {
            config
                .env_layer
                .insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(config)
    }

    /// Erste (einzige) Telegram-Bindung einer Test-Konfiguration.
    fn first_telegram_binding(config: &ResolvedConfig) -> TestResult<&TelegramChannelToml> {
        enabled_telegram_bindings(config)
            .into_iter()
            .next()
            .ok_or(TestError::Missing("an enabled Telegram binding"))
    }

    #[test]
    fn telegram_principal_is_derived_from_the_binding_id() {
        assert_eq!(
            telegram_principal_peer("telegram:support-bot"),
            "support-bot"
        );
        assert_eq!(telegram_principal_peer("support-bot"), "support-bot");
        assert_eq!(telegram_principal_peer("telegram:"), "telegram:");
        let principal = channel_principal(
            GatewayEntry::Telegram,
            telegram_principal_peer("telegram:ops"),
        );
        assert_eq!(principal.id(), "telegram:ops");
        assert_ne!(principal.id(), "telegram:gateway");
    }

    #[test]
    fn enabled_telegram_binding_ids_are_sorted_and_skip_disabled_bindings() -> TestResult {
        let config = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:zulu"
bot_token_ref = "env:HARW_GW_TEST_TOKEN_Z"

[[channel.telegram]]
id = "telegram:alpha"
bot_token_ref = "env:HARW_GW_TEST_TOKEN_A"

[[channel.telegram]]
id = "telegram:off"
enabled = false
bot_token_ref = "env:HARW_GW_TEST_TOKEN_O"
"#,
            &[],
        )?;
        assert_eq!(
            enabled_telegram_binding_ids(&config),
            vec!["telegram:alpha".to_owned(), "telegram:zulu".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn multiple_telegram_bindings_each_get_their_own_ingress_plan() -> TestResult {
        let config = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:ops"
bot_token_ref = "env:HARW_GW_TEST_MULTI_TOKEN_OPS"
[channel.telegram.security]
pinned_identities = [1]

[[channel.telegram]]
id = "telegram:support"
bot_token_ref = "env:HARW_GW_TEST_MULTI_TOKEN_SUPPORT"
[channel.telegram.security]
pinned_identities = [2]
"#,
            &[
                ("HARW_GW_TEST_MULTI_TOKEN_OPS", "111:ops-token"),
                ("HARW_GW_TEST_MULTI_TOKEN_SUPPORT", "222:support-token"),
            ],
        )?;

        let bindings = telegram_ingress_modes(&config);

        assert_eq!(bindings.len(), 2);
        assert!(
            bindings
                .iter()
                .all(|binding| matches!(binding.mode, TelegramIngressMode::Enabled(_)))
        );
        let status = telegram_ingress_status(&bindings);
        assert!(status.contains("telegram:ops: bereit"));
        assert!(status.contains("telegram:support: bereit"));
        assert!(!status.contains("ops-token"));
        assert!(!status.contains("support-token"));
        Ok(())
    }

    #[test]
    fn telegram_bindings_sharing_a_bot_token_are_all_disabled() -> TestResult {
        let config = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:one"
bot_token_ref = "env:HARW_GW_TEST_SHARED_TOKEN_ONE"
[channel.telegram.security]
pinned_identities = [1]

[[channel.telegram]]
id = "telegram:two"
bot_token_ref = "env:HARW_GW_TEST_SHARED_TOKEN_TWO"
[channel.telegram.security]
pinned_identities = [1]
"#,
            &[
                ("HARW_GW_TEST_SHARED_TOKEN_ONE", "333:same-token"),
                ("HARW_GW_TEST_SHARED_TOKEN_TWO", "333:same-token"),
            ],
        )?;

        let bindings = telegram_ingress_modes(&config);

        assert_eq!(bindings.len(), 2);
        for binding in &bindings {
            let TelegramIngressMode::Disabled(reason) = &binding.mode else {
                return Err(TestError::Unexpected(format!(
                    "{} must be disabled when its bot token is shared",
                    binding.id
                )));
            };
            assert!(reason.contains("share one bot credential"));
        }
        assert!(!telegram_ingress_status(&bindings).contains("same-token"));
        Ok(())
    }

    #[test]
    fn telegram_webhook_binding_resolves_url_route_listener_and_secret() -> TestResult {
        let config = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:hook"
bot_token_ref = "env:HARW_GW_TEST_HOOK_TOKEN"
transport = "webhook"
[channel.telegram.transport_webhook]
public_url = "https://ingress.example.com/telegram/hook-bot"
secret_token_ref = "env:HARW_GW_TEST_HOOK_SECRET"
listen_addr = "127.0.0.1:8443"
[channel.telegram.security]
pinned_identities = [7]
"#,
            &[
                ("HARW_GW_TEST_HOOK_TOKEN", "444:hook-token"),
                ("HARW_GW_TEST_HOOK_SECRET", "hook_secret-value"),
            ],
        )?;

        let bindings = telegram_ingress_modes(&config);
        let [binding] = bindings.as_slice() else {
            return Err(TestError::Unexpected("exactly one binding expected".into()));
        };
        let TelegramIngressMode::Enabled(plan) = &binding.mode else {
            return Err(TestError::Unexpected(
                "webhook binding must be enabled".into(),
            ));
        };
        let TelegramTransportPlan::Webhook(webhook) = &plan.transport else {
            return Err(TestError::Unexpected("webhook transport expected".into()));
        };
        assert_eq!(
            webhook.public_url,
            "https://ingress.example.com/telegram/hook-bot"
        );
        assert_eq!(webhook.route, "/telegram/hook-bot");
        assert_eq!(webhook.listen_addr.to_string(), "127.0.0.1:8443");
        assert_eq!(webhook.secret_token.expose_secret(), "hook_secret-value");

        let status = telegram_ingress_status(&bindings);
        assert!(status.contains("Webhook-Adapter auf 127.0.0.1:8443/telegram/hook-bot"));
        assert!(!status.contains("hook_secret-value"));
        assert!(!status.contains("hook-token"));
        assert!(!status.contains("ingress.example.com"));
        Ok(())
    }

    #[test]
    fn telegram_webhook_without_resolvable_secret_fails_closed() -> TestResult {
        let config = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:hook"
bot_token_ref = "env:HARW_GW_TEST_NOSECRET_TOKEN"
transport = "webhook"
[channel.telegram.transport_webhook]
public_url = "https://ingress.example.com/telegram"
secret_token_ref = "env:HARW_GW_TEST_NOSECRET_UNSET"
listen_addr = "127.0.0.1:8443"
[channel.telegram.security]
pinned_identities = [7]
"#,
            &[("HARW_GW_TEST_NOSECRET_TOKEN", "555:token")],
        )?;

        let status = telegram_ingress_status(&telegram_ingress_modes(&config));

        assert!(status.contains("fail closed"));
        assert!(status.contains("webhook secret"));
        assert!(!status.contains("HARW_GW_TEST_NOSECRET_UNSET"));
        Ok(())
    }

    #[test]
    fn telegram_webhook_bindings_sharing_a_listener_are_disabled() -> TestResult {
        let config = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:a"
bot_token_ref = "env:HARW_GW_TEST_LISTEN_TOKEN_A"
transport = "webhook"
[channel.telegram.transport_webhook]
public_url = "https://ingress.example.com/a"
secret_token_ref = "env:HARW_GW_TEST_LISTEN_SECRET"
listen_addr = "127.0.0.1:8443"
[channel.telegram.security]
pinned_identities = [7]

[[channel.telegram]]
id = "telegram:b"
bot_token_ref = "env:HARW_GW_TEST_LISTEN_TOKEN_B"
transport = "webhook"
[channel.telegram.transport_webhook]
public_url = "https://ingress.example.com/b"
secret_token_ref = "env:HARW_GW_TEST_LISTEN_SECRET"
listen_addr = "127.0.0.1:8443"
[channel.telegram.security]
pinned_identities = [7]
"#,
            &[
                ("HARW_GW_TEST_LISTEN_TOKEN_A", "601:a"),
                ("HARW_GW_TEST_LISTEN_TOKEN_B", "602:b"),
                ("HARW_GW_TEST_LISTEN_SECRET", "secret"),
            ],
        )?;

        let bindings = telegram_ingress_modes(&config);

        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().all(|binding| matches!(
            &binding.mode,
            TelegramIngressMode::Disabled(reason) if reason.contains("listen_addr is shared")
        )));
        Ok(())
    }

    #[test]
    fn telegram_transport_choice_falls_back_to_long_poll_only_when_webhook_is_unconfigured()
    -> TestResult {
        let long_poll = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:lp"
bot_token_ref = "env:HARW_GW_TEST_CHOICE"
"#,
            &[],
        )?;
        assert_eq!(
            telegram_transport_choice(first_telegram_binding(&long_poll)?),
            Ok(TelegramTransportChoice::LongPoll {
                webhook_fallback: false
            })
        );

        let fallback = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:fb"
bot_token_ref = "env:HARW_GW_TEST_CHOICE"
transport = "webhook"
"#,
            &[],
        )?;
        assert_eq!(
            telegram_transport_choice(first_telegram_binding(&fallback)?),
            Ok(TelegramTransportChoice::LongPoll {
                webhook_fallback: true
            })
        );

        let bad_listener = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:bad"
bot_token_ref = "env:HARW_GW_TEST_CHOICE"
transport = "webhook"
[channel.telegram.transport_webhook]
public_url = "https://ingress.example.com/hook"
secret_token_ref = "env:HARW_GW_TEST_CHOICE_SECRET"
listen_addr = "localhost"
"#,
            &[],
        )?;
        assert!(telegram_transport_choice(first_telegram_binding(&bad_listener)?).is_err());

        let unknown = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:unknown"
bot_token_ref = "env:HARW_GW_TEST_CHOICE"
transport = "carrier_pigeon"
"#,
            &[],
        )?;
        assert!(telegram_transport_choice(first_telegram_binding(&unknown)?).is_err());
        Ok(())
    }

    #[test]
    fn telegram_webhook_route_requires_https_supported_port_and_safe_path() {
        assert_eq!(
            telegram_webhook_route("https://ingress.example.com/telegram/support-bot"),
            Ok("/telegram/support-bot".to_owned())
        );
        assert_eq!(
            telegram_webhook_route("https://ingress.example.com:8443"),
            Ok("/".to_owned())
        );
        assert_eq!(
            telegram_webhook_route("https://ingress.example.com/hook?x=1"),
            Ok("/hook".to_owned())
        );
        assert!(telegram_webhook_route("http://ingress.example.com/hook").is_err());
        assert!(telegram_webhook_route("https://ingress.example.com:8080/hook").is_err());
        assert!(telegram_webhook_route("https://user:pw@ingress.example.com/hook").is_err());
        assert!(telegram_webhook_route("https://ingress.example.com/{capture}").is_err());
        assert!(telegram_webhook_route("https://ingress.example.com/hook/").is_err());
        assert!(telegram_webhook_route("not a url").is_err());
    }

    #[test]
    fn webhook_routes_exclude_router_placeholder_syntax() {
        assert!(is_supported_webhook_route("/"));
        assert!(is_supported_webhook_route("/telegram/bot-1_a.b~c"));
        assert!(!is_supported_webhook_route(""));
        assert!(!is_supported_webhook_route("telegram"));
        assert!(!is_supported_webhook_route("//double"));
        assert!(!is_supported_webhook_route("/:param"));
        assert!(!is_supported_webhook_route("/*rest"));
        assert!(!is_supported_webhook_route("/{id}"));
        assert!(!is_supported_webhook_route("/a/../b"));
    }

    #[test]
    fn telegram_webhook_secret_follows_the_bot_api_charset() {
        assert!(is_valid_telegram_webhook_secret("abc_DEF-123"));
        assert!(is_valid_telegram_webhook_secret(&"a".repeat(256)));
        assert!(!is_valid_telegram_webhook_secret(""));
        assert!(!is_valid_telegram_webhook_secret(&"a".repeat(257)));
        assert!(!is_valid_telegram_webhook_secret("has space"));
        assert!(!is_valid_telegram_webhook_secret("umlaut-ä"));
    }

    #[test]
    fn telegram_binding_services_map_workspaces_admins_and_default_alias() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        std::fs::create_dir_all(home.path().join("ws").join("ops"))
            .map_err(ctx("workspace root"))?;
        let config = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:ops"
bot_token_ref = "env:HARW_GW_TEST_SERVICES_TOKEN"
[channel.telegram.security]
pinned_identities = [1]
admin_identities = [42]
[[channel.telegram.workspaces]]
alias = "ops"
tenant = "default"
root = "ws/ops"
default = true
"#,
            &[("HARW_GW_TEST_SERVICES_TOKEN", "444:token")],
        )?;
        let binding = first_telegram_binding(&config)?;
        let services = telegram_binding_services(home.path(), binding, home.path())
            .map_err(TestError::Unexpected)?;
        assert_eq!(services.default_workspace_alias.as_deref(), Some("ops"));
        assert!(services.admin_sender_ids.contains("42"));
        assert_eq!(services.admin_sender_ids.len(), 1);
        services
            .workspaces
            .resolve(
                &TenantId::from_str("default"),
                &WorkspaceId::from_str("ops"),
            )
            .map_err(ctx("configured workspace resolves"))?;
        Ok(())
    }

    #[test]
    fn telegram_binding_services_fail_closed_on_unresolvable_workspace_or_fallback() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let missing_root = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:ops"
bot_token_ref = "env:HARW_GW_TEST_SERVICES_TOKEN"
[channel.telegram.security]
pinned_identities = [1]
[[channel.telegram.workspaces]]
alias = "ops"
tenant = "default"
root = "does-not-exist"
"#,
            &[("HARW_GW_TEST_SERVICES_TOKEN", "444:token")],
        )?;
        assert!(
            telegram_binding_services(
                home.path(),
                first_telegram_binding(&missing_root)?,
                home.path()
            )
            .is_err()
        );
        let bogus_fallback = telegram_test_config(
            r#"
[[channel.telegram]]
id = "telegram:ops"
bot_token_ref = "env:HARW_GW_TEST_SERVICES_TOKEN"
[channel.telegram.security]
pinned_identities = [1]
[channel.telegram.commands]
unknown_command_fallback = "bogus"
"#,
            &[("HARW_GW_TEST_SERVICES_TOKEN", "444:token")],
        )?;
        assert!(
            telegram_binding_services(
                home.path(),
                first_telegram_binding(&bogus_fallback)?,
                home.path()
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn telegram_chat_state_roots_are_per_binding() {
        let state = Path::new("/state");
        assert_eq!(
            telegram_chat_state_root(state, "telegram:a"),
            Path::new("/state/telegram-chats/74656c656772616d3a61")
        );
        assert_ne!(
            telegram_chat_state_root(state, "telegram:a"),
            telegram_chat_state_root(state, "telegram:b")
        );
    }

    #[test]
    fn telegram_offset_roots_are_per_binding_and_keep_the_legacy_default() {
        let state = Path::new("/state");
        assert_eq!(
            telegram_offset_root(state, "telegram:default"),
            PathBuf::from("/state/telegram-offset")
        );
        let ops = telegram_offset_root(state, "telegram:ops");
        assert_eq!(
            ops,
            PathBuf::from("/state/telegram-offset/74656c656772616d3a6f7073")
        );
        assert_ne!(ops, telegram_offset_root(state, "telegram_ops"));
    }

    #[test]
    fn telegram_backoff_resets_only_after_a_stable_run() {
        assert_eq!(telegram_attempt_after_run(5, TELEGRAM_STABLE_RUN), 0);
        assert_eq!(
            telegram_attempt_after_run(5, TELEGRAM_STABLE_RUN + Duration::from_secs(1)),
            0
        );
        assert_eq!(telegram_attempt_after_run(5, Duration::from_millis(10)), 5);
        assert_eq!(telegram_attempt_after_run(0, Duration::ZERO), 0);
    }

    #[test]
    fn telegram_topic_mode_accepts_only_documented_values() {
        assert_eq!(
            telegram_topic_mode("per_topic_session"),
            Some(TopicMode::PerTopicSession)
        );
        assert_eq!(
            telegram_topic_mode("shared_session"),
            Some(TopicMode::SharedSession)
        );
        assert_eq!(telegram_topic_mode("per_user"), None);
    }

    #[test]
    fn audit_chain_check_interval_defaults_when_unset() {
        assert_eq!(
            audit_chain_check_interval_secs(None),
            AUDIT_CHAIN_CHECK_DEFAULT_SECS
        );
    }

    #[test]
    fn audit_chain_check_interval_parses_a_configured_value() {
        assert_eq!(audit_chain_check_interval_secs(Some("120")), 120);
    }

    #[test]
    fn audit_chain_check_interval_zero_disables_the_check() {
        assert_eq!(audit_chain_check_interval_secs(Some("0")), 0);
    }

    #[test]
    fn audit_chain_check_interval_falls_back_on_an_unparsable_value() {
        assert_eq!(
            audit_chain_check_interval_secs(Some("not-a-number")),
            AUDIT_CHAIN_CHECK_DEFAULT_SECS
        );
    }

    /// Serialisiert jeden Test, der `AUDIT_CHAIN_BREAK` (ein prozessweiter
    /// `static` in `harw_secrets::audit::telemetry`) liest oder erhöht —
    /// derselbe Grund, aus dem `harw-secrets` selbst seine eigenen
    /// Mirror-Tests hinter `AUDIT_COUNTER_LOCK` serialisiert: `cargo test`
    /// läuft standardmäßig mit mehreren Threads im selben Prozess, und ein
    /// gemeinsamer Zähler ohne Sperre wäre zwischen parallelen Tests flüchtig.
    static AUDIT_CHAIN_BREAK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn write_gateway_test_kek(home: &Path) -> TestResult<PathBuf> {
        let path = home.join("test.kek");
        std::fs::write(&path, b"01234567890123456789012345678901")
            .map_err(ctx("write test KEK"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(ctx("restrict test KEK permissions"))?;
        }
        Ok(path)
    }

    fn gateway_sealed_provider_config(key_path: &Path) -> TestResult<ResolvedConfig> {
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str::<harw_config::ProviderToml>(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .map_err(ctx("parse test provider"))?,
        );
        config.auth.kek = Some(harw_config::KekConfig {
            provenance: harw_config::KekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        });
        Ok(config)
    }

    /// Der wichtigste Test dieses Knotens: die geprüfte Kette ist die
    /// persistierte Datei auf der Platte, nicht eine im Speicher gehaltene
    /// Attrappe. Eine Mutation, die durch eine separate `SecretStore`-Instanz
    /// an derselben Wurzel geschrieben wurde, muss über
    /// [`run_configured_secret_store_audit_chain_tick`] sichtbar werden.
    #[tokio::test]
    async fn audit_chain_tick_reads_the_persisted_disk_file_not_an_in_memory_chain() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let key_path = write_gateway_test_kek(home.path())?;
        {
            let policy = harw_secrets::CryptoPolicy::strongest();
            let provenance = harw_secrets::KekProvenance::KeyFile {
                path: key_path.clone(),
            };
            let key_material = harw_secrets::load_kek_material(&policy, &provenance)
                .map_err(ctx("load test KEK material"))?;
            let mut store = harw_secrets::SecretStore::with_key_material(
                home.path().join("sealed-secrets"),
                policy,
                provenance,
                harw_secrets::KeyVersion::initial(),
                key_material,
            );
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &secrecy_08::SecretBox::new(b"gateway-test-token".to_vec().into_boxed_slice()),
                )
                .map_err(ctx("seal test token"))?;
        }
        let config = Arc::new(gateway_sealed_provider_config(&key_path)?);

        let outcome = run_configured_secret_store_audit_chain_tick(home.path(), &config).await;

        match outcome {
            AuditChainTickOutcome::Checked(Ok(PersistedChainStatus::Intact {
                event_count,
                ..
            })) => {
                assert_eq!(
                    event_count, 1,
                    "the disk-persisted chain must show the mutation made by the earlier instance"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an intact persisted chain, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Eine fehlende `audit.log` ist kein Kettenbruch: der Zähler bleibt bei
    /// null, und die Meldung lautet „nichts protokolliert", nicht
    /// „unversehrt".
    #[tokio::test]
    async fn audit_chain_tick_reports_absent_for_a_never_mutated_configured_store() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let key_path = write_gateway_test_kek(home.path())?;
        let config = Arc::new(gateway_sealed_provider_config(&key_path)?);

        let outcome = run_configured_secret_store_audit_chain_tick(home.path(), &config).await;

        let report = match outcome {
            AuditChainTickOutcome::Checked(result) => describe_audit_chain_check(&result),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a checked outcome, got {other:?}"
                )));
            }
        };
        assert_eq!(report, AuditChainCheckReport::Absent);

        let _lock = AUDIT_CHAIN_BREAK_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = AUDIT_CHAIN_BREAK.count();
        apply_audit_chain_check_report(report, &harw_observe::NullSink);
        assert_eq!(
            AUDIT_CHAIN_BREAK.count(),
            before,
            "an absent chain must never increment the break counter"
        );
        Ok(())
    }

    /// Ohne konfigurierten Geheimnisspeicher scheitert der Tick nicht — er
    /// meldet ehrlich, dass nichts geprüft wurde, statt „unversehrt"
    /// vorzutäuschen.
    #[tokio::test]
    async fn audit_chain_tick_reports_nothing_configured_without_a_secret_store() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let config = Arc::new(ResolvedConfig::default());

        let outcome = run_configured_secret_store_audit_chain_tick(home.path(), &config).await;

        assert!(matches!(outcome, AuditChainTickOutcome::NothingConfigured));
        Ok(())
    }

    /// Ein aktivierter Provider mit `secrets:`-Referenz, aber ohne
    /// konfiguriertes KEK, scheitert am Öffnen des Speichers — sichtbar über
    /// [`AuditChainTickOutcome::OpenFailed`], niemals stillschweigend als
    /// „geprüft" gezählt, und ohne den Geheimnisnamen in der Meldung.
    #[tokio::test]
    async fn audit_chain_tick_fails_closed_without_leaking_when_kek_is_missing() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let mut raw_config = ResolvedConfig::default();
        raw_config.providers.insert(
            "sealed".to_owned(),
            toml::from_str::<harw_config::ProviderToml>(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .map_err(ctx("parse test provider"))?,
        );
        let config = Arc::new(raw_config);

        let outcome = run_configured_secret_store_audit_chain_tick(home.path(), &config).await;

        match outcome {
            AuditChainTickOutcome::OpenFailed(reason) => {
                assert!(reason.contains("requires a configured KEK"));
                assert!(!reason.contains("provider-token"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected OpenFailed, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Ein Lesefehler (hier: eine abgeschnittene Datei) meldet sich als
    /// [`AuditChainCheckReport::Unreadable`] — weder als Bruch noch als
    /// unversehrt, und ohne den Zähler zu erhöhen.
    #[tokio::test]
    async fn audit_chain_tick_reports_unreadable_neither_as_broken_nor_as_intact() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let key_path = write_gateway_test_kek(home.path())?;
        {
            let policy = harw_secrets::CryptoPolicy::strongest();
            let provenance = harw_secrets::KekProvenance::KeyFile {
                path: key_path.clone(),
            };
            let key_material = harw_secrets::load_kek_material(&policy, &provenance)
                .map_err(ctx("load test KEK material"))?;
            let mut store = harw_secrets::SecretStore::with_key_material(
                home.path().join("sealed-secrets"),
                policy,
                provenance,
                harw_secrets::KeyVersion::initial(),
                key_material,
            );
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &secrecy_08::SecretBox::new(b"gateway-test-token".to_vec().into_boxed_slice()),
                )
                .map_err(ctx("seal test token"))?;
        }
        let audit_log_path = home.path().join("sealed-secrets").join("audit.log");
        let bytes = std::fs::read(&audit_log_path).map_err(ctx("read audit log"))?;
        std::fs::write(&audit_log_path, &bytes[..bytes.len() / 2])
            .map_err(ctx("truncate audit log"))?;
        let config = Arc::new(gateway_sealed_provider_config(&key_path)?);

        let outcome = run_configured_secret_store_audit_chain_tick(home.path(), &config).await;

        let report = match outcome {
            AuditChainTickOutcome::Checked(result) => describe_audit_chain_check(&result),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a checked outcome, got {other:?}"
                )));
            }
        };
        match &report {
            AuditChainCheckReport::Unreadable(reason) => {
                assert!(!reason.contains("provider-token"));
                assert!(!reason.contains("gateway-test-token"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an unreadable report, got {other:?}"
                )));
            }
        }

        let _lock = AUDIT_CHAIN_BREAK_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = AUDIT_CHAIN_BREAK.count();
        apply_audit_chain_check_report(report, &harw_observe::NullSink);
        assert_eq!(
            AUDIT_CHAIN_BREAK.count(),
            before,
            "an unreadable chain is neither intact nor broken and must not increment the counter"
        );
        Ok(())
    }

    #[test]
    fn describe_audit_chain_check_reports_a_detected_break_visibly() -> TestResult {
        let result: AuditResult<PersistedChainStatus> = Err(AuditError::ChainBroken {
            index: 3,
            expected: [0u8; 32],
            found: [1u8; 32],
        });

        let report = describe_audit_chain_check(&result);

        match report {
            AuditChainCheckReport::Broken(message) => {
                assert!(message.contains("AUDIT CHAIN BREAK"));
                assert!(message.contains('3'));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a broken report, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn describe_audit_chain_check_reports_absent_as_neither_intact_nor_broken() {
        let result: AuditResult<PersistedChainStatus> = Ok(PersistedChainStatus::Absent);

        assert_eq!(
            describe_audit_chain_check(&result),
            AuditChainCheckReport::Absent
        );
    }

    #[test]
    fn describe_audit_chain_check_reports_intact_with_its_event_count() {
        let result: AuditResult<PersistedChainStatus> = Ok(PersistedChainStatus::Intact {
            event_count: 2,
            chain_head: [7u8; 32],
        });

        assert_eq!(
            describe_audit_chain_check(&result),
            AuditChainCheckReport::Intact { event_count: 2 }
        );
    }

    #[test]
    fn describe_audit_chain_check_reports_an_io_error_as_unreadable() -> TestResult {
        let result: AuditResult<PersistedChainStatus> = Err(AuditError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "audit log file has an unrecognized header",
        )));

        match describe_audit_chain_check(&result) {
            AuditChainCheckReport::Unreadable(reason) => {
                assert!(reason.contains("unrecognized header"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an unreadable report, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Beweist, dass nur [`AuditChainCheckReport::Broken`] `AUDIT_CHAIN_BREAK`
    /// erhöht — eine manipulierte Kette wird gezählt, sichtbar gemeldet, und
    /// der Aufrufer läuft danach unverändert weiter (kein Panic, kein
    /// Prozess-Exit).
    #[test]
    fn apply_audit_chain_check_report_increments_only_on_a_detected_break() {
        let _lock = AUDIT_CHAIN_BREAK_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = AUDIT_CHAIN_BREAK.count();

        apply_audit_chain_check_report(AuditChainCheckReport::Absent, &harw_observe::NullSink);
        apply_audit_chain_check_report(
            AuditChainCheckReport::Intact { event_count: 1 },
            &harw_observe::NullSink,
        );
        apply_audit_chain_check_report(
            AuditChainCheckReport::Unreadable("unreadable".to_owned()),
            &harw_observe::NullSink,
        );
        assert_eq!(AUDIT_CHAIN_BREAK.count(), before);

        apply_audit_chain_check_report(
            AuditChainCheckReport::Broken("AUDIT CHAIN BREAK: test".to_owned()),
            &harw_observe::NullSink,
        );
        assert_eq!(AUDIT_CHAIN_BREAK.count(), before + 1);
    }

    /// Legt in `home` eine vertraute Gateway-Konfiguration mit genau einem
    /// aktivierten Provider `sealed` an, dessen Credential eine
    /// `secrets:`-Referenz ist (`config.toml`, `providers/sealed.toml`,
    /// `models/model.toml`). Mit `Some(key_path)` kommt eine `auth.toml` mit
    /// `[kek]`-Key-File-Provenance hinzu; mit `None` fehlt das KEK.
    fn write_gateway_sealed_provider_home(home: &Path, key_path: Option<&Path>) -> TestResult {
        std::fs::write(
            home.join("config.toml"),
            "default_provider = \"sealed\"\ndefault_model = \"model\"\n",
        )
        .map_err(ctx("write config.toml"))?;
        std::fs::create_dir_all(home.join("providers")).map_err(ctx("create providers dir"))?;
        std::fs::write(
            home.join("providers").join("sealed.toml"),
            "name = \"sealed\"\napi = \"openai-chat\"\nbase_url = \"https://example.test/v1\"\nauth = \"secrets:provider-token\"\n",
        )
        .map_err(ctx("write sealed provider"))?;
        std::fs::create_dir_all(home.join("models")).map_err(ctx("create models dir"))?;
        std::fs::write(
            home.join("models").join("model.toml"),
            "id = \"model\"\nprovider = \"sealed\"\n",
        )
        .map_err(ctx("write model"))?;
        if let Some(key_path) = key_path {
            let key_file = key_path.to_string_lossy().into_owned();
            std::fs::write(
                home.join("auth.toml"),
                format!("[kek]\nprovenance = \"key_file\"\nkey_file_path = {key_file:?}\n"),
            )
            .map_err(ctx("write auth.toml"))?;
        }
        Ok(())
    }

    /// Versiegelt `provider-token` im Store `<home>/sealed-secrets` unter dem
    /// Test-KEK — derselbe Pfad und dieselbe Provenienz, die
    /// `crate::secret_store::open_configured_secret_resolver` öffnet.
    fn seal_gateway_test_provider_token(home: &Path, key_path: &Path) -> TestResult {
        let policy = harw_secrets::CryptoPolicy::strongest();
        let provenance = harw_secrets::KekProvenance::KeyFile {
            path: key_path.to_path_buf(),
        };
        let key_material = harw_secrets::load_kek_material(&policy, &provenance)
            .map_err(ctx("load test KEK material"))?;
        let mut store = harw_secrets::SecretStore::with_key_material(
            home.join("sealed-secrets"),
            policy,
            provenance,
            harw_secrets::KeyVersion::initial(),
            key_material,
        );
        store
            .create(
                "provider-token",
                "provider authentication",
                &secrecy_08::SecretBox::new(b"gateway-mount-token".to_vec().into_boxed_slice()),
            )
            .map_err(ctx("seal test token"))?;
        Ok(())
    }

    /// Regression B3/G2: ein aktivierter Provider mit `secrets:`-Credential,
    /// konfiguriertem KEK und versiegeltem Token muss über den Resolver
    /// montieren — nicht mit „sealed secret … could not be resolved" bzw.
    /// „secret resolver failed" scheitern. Zugleich Beleg für G1: jede
    /// aktivierte Telegram-Bindung und Dream bekommen je eine eigene Montage
    /// mit eigener `EntryKind` und eigenem, aus der Bindung abgeleitetem
    /// Principal. Kein Netz: der Provider wird nur gebaut, nie
    /// angefragt.
    #[test]
    fn test_mount_gateway_assembly_sealed_secret_provider_resolves_with_kek() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("temp cwd"))?;
        let key_path = write_gateway_test_kek(home.path())?;
        seal_gateway_test_provider_token(home.path(), &key_path)?;
        write_gateway_sealed_provider_home(home.path(), Some(&key_path))?;
        // Eine aktivierte Telegram-Bindung: ihre Montage muss einen aus der
        // Bindungs-ID abgeleiteten Principal tragen (kein Platzhalter mehr).
        std::fs::create_dir_all(home.path().join("channels"))
            .map_err(ctx("create channels dir"))?;
        std::fs::write(
            home.path().join("channels").join("telegram.toml"),
            "[[channel.telegram]]\nid = \"telegram:ops\"\nbot_token_ref = \"env:HARW_GW_TEST_MOUNT_TOKEN\"\n\n[channel.telegram.security]\npinned_identities = [42]\n",
        )
        .map_err(ctx("write telegram channel"))?;
        let sessions_root = home.path().join("sessions");

        let result = mount_gateway_assembly(home.path(), cwd.path(), &sessions_root);
        let assemblies = match result {
            Ok(assemblies) => assemblies,
            Err(error) => {
                assert!(
                    !error.contains("sealed secret") && !error.contains("secret resolver failed"),
                    "a configured KEK must let the sealed provider credential resolve, got: {error}"
                );
                return Err(TestError::Unexpected(format!(
                    "sealed-secret provider with a configured KEK must mount, got: {error}"
                )));
            }
        };

        assert_eq!(
            assemblies
                .dream
                .config()
                .harness
                .default_provider
                .as_deref(),
            Some("sealed")
        );
        let [telegram] = assemblies.telegram.as_slice() else {
            return Err(TestError::Unexpected(
                "exactly one Telegram assembly per enabled binding expected".into(),
            ));
        };
        assert_eq!(telegram.binding_id, "telegram:ops");
        let telegram_rights = telegram.assembly.rights_snapshot();
        assert_eq!(
            telegram_rights.entry,
            harw_runtime::EntryKind::GatewayTelegram
        );
        assert_eq!(telegram_rights.principal.id(), "telegram:ops");
        let dream_rights = assemblies.dream.rights_snapshot();
        assert_eq!(dream_rights.entry, harw_runtime::EntryKind::GatewayDream);
        assert_eq!(dream_rights.principal.id(), "gateway-dream");
        Ok(())
    }

    /// G2 Negativfall: derselbe `secrets:`-Provider ohne `[kek]` muss die
    /// Montage mit dem exakten, geheimnisfreien Text aus
    /// `crate::secret_store::open_configured_secret_resolver` abbrechen.
    #[test]
    fn test_mount_gateway_assembly_sealed_secret_provider_without_kek_returns_err() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("temp cwd"))?;
        write_gateway_sealed_provider_home(home.path(), None)?;
        let sessions_root = home.path().join("sessions");

        let Err(error) = mount_gateway_assembly(home.path(), cwd.path(), &sessions_root) else {
            return Err(TestError::Unexpected(
                "a sealed-secret provider without a KEK must not mount".into(),
            ));
        };

        assert!(
            error.starts_with("gateway: enabled sealed-secret provider requires a configured KEK"),
            "unexpected mount error: {error}"
        );
        assert!(!error.contains("provider-token"));
        Ok(())
    }

    /// Ohne aktivierten `secrets:`-Provider öffnet der Gateway keinen
    /// versiegelten Speicher und verlangt kein KEK (G4-Hilfsfunktion).
    #[test]
    fn test_open_gateway_secret_resolver_without_sealed_provider_returns_none() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("temp cwd"))?;
        let principal = channel_principal(GatewayEntry::Dream, "");

        let (resolver, _config) =
            open_gateway_secret_resolver(GatewayEntry::Dream, home.path(), cwd.path(), &principal)
                .map_err(ctx("an empty home has no sealed provider and needs no KEK"))?;

        assert!(resolver.is_none());
        Ok(())
    }

    /// Legt eine Gateway-Konfiguration mit zwei Providern an: `"plain"`
    /// (`env:`, `default_provider`) und `"sealed"` (`secrets:`, aktiviert,
    /// aber nie ausgewählt, ohne `[kek]`).
    fn write_gateway_home_with_unused_sealed_provider(home: &Path) -> TestResult {
        std::fs::write(
            home.join("config.toml"),
            "default_provider = \"plain\"\ndefault_model = \"model\"\n",
        )
        .map_err(ctx("write config.toml"))?;
        std::fs::create_dir_all(home.join("providers")).map_err(ctx("create providers dir"))?;
        std::fs::write(
            home.join("providers").join("plain.toml"),
            "name = \"plain\"\napi = \"openai-chat\"\nbase_url = \"https://example.test/v1\"\nauth = \"env:GATEWAY_TEST_PLAIN_TOKEN\"\n",
        )
        .map_err(ctx("write plain provider"))?;
        std::fs::write(
            home.join("providers").join("sealed.toml"),
            "name = \"sealed\"\napi = \"openai-chat\"\nbase_url = \"https://example.test/v1\"\nauth = \"secrets:provider-token\"\n",
        )
        .map_err(ctx("write sealed provider"))?;
        std::fs::create_dir_all(home.join("models")).map_err(ctx("create models dir"))?;
        std::fs::write(
            home.join("models").join("model.toml"),
            "id = \"model\"\nprovider = \"plain\"\n",
        )
        .map_err(ctx("write model"))?;
        Ok(())
    }

    /// Kernverhalten dieses Reports: ein aktivierter, aber vom Gateway nicht
    /// ausgewählter `secrets:`-Provider ohne KEK darf den Start nicht
    /// blockieren — der aktive `default_provider` (`"plain"`, `env:`)
    /// bestimmt allein, ob ein KEK verlangt wird.
    #[test]
    fn test_open_gateway_secret_resolver_ignores_unused_sealed_provider_without_kek() -> TestResult
    {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("temp cwd"))?;
        write_gateway_home_with_unused_sealed_provider(home.path())?;
        let principal = channel_principal(GatewayEntry::Dream, "");

        let (resolver, _config) = open_gateway_secret_resolver(
            GatewayEntry::Dream,
            home.path(),
            cwd.path(),
            &principal,
        )
        .map_err(ctx(
            "an unused sealed provider without a KEK must not block the active plain provider",
        ))?;

        assert!(resolver.is_none());
        Ok(())
    }
}
