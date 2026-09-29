//! clap-basierte Kommandozeilen-Grammatik für `harw`.
//!
//! Ein Root-Parser mit optionalem Subcommand: globale Flags ([`GlobalArgs`])
//! gelten hinter jedem Befehl, die Chat-Flags ([`ChatArgs`]) sind an der
//! Wurzel geflattet, sodass `harw [PROMPT]` ohne Subcommand den Chat startet
//! (`Cli::command == None`). `--help`/`-h`/`--version` behandelt clap selbst.
//!
//! Die Grammatik ist nach Domänen auf Untermodule verteilt; alle Typen werden
//! hier per Glob re-exportiert, sodass jeder Pfad `crate::cli::X` stabil bleibt.
//! Typisierte Werte (`ValueEnum`s, Wert-Parser für `--log`/`--mode`/
//! `--approval`) liegen in [`values`] und speisen sowohl die
//! Parse-Validierung als auch die Shell-Completions. Den fertigen
//! clap-Befehl (inklusive deutscher Hilfe für `completions`) liefert
//! [`command`].

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand, ValueHint};

mod agent;
mod analyze;
mod auth;
mod channel;
mod completions;
mod debug;
mod gateway;
mod global;
mod jobs;
mod knowledge;
mod lens;
mod mcp;
mod models;
mod pr_review;
mod project;
mod provider;
mod sandbox;
mod service;
mod session;
mod settings;
mod uia;
mod worker;
pub mod values;

#[cfg(test)]
mod tests;

pub use agent::*;
pub use analyze::*;
pub use auth::*;
pub use channel::*;
pub use completions::*;
pub use debug::*;
pub use gateway::*;
pub use global::*;
pub use jobs::*;
pub use knowledge::*;
pub use lens::*;
pub use mcp::*;
pub use models::*;
pub use pr_review::*;
pub use project::*;
pub use provider::*;
pub use sandbox::*;
pub use service::*;
pub use session::*;
pub use settings::*;
pub use uia::*;
pub use worker::*;
pub use values::*;

/// Root-Parser des `harw`-Binaries.
#[derive(Debug, Parser)]
#[command(
    name = "harw",
    bin_name = "harw",
    version,
    about = "Harwness — eigenständiger Agent-Harness",
    subcommand_negates_reqs = true,
    subcommand_precedence_over_arg = true
)]
pub struct Cli {
    /// Globale Flags, die hinter jedem Befehl gelten.
    #[command(flatten)]
    pub global: GlobalArgs,
    /// Chat-Flags an der Wurzel, damit `harw [PROMPT]` ohne Subcommand funktioniert.
    #[command(flatten)]
    pub chat: ChatArgs,
    /// Optionaler Subcommand; ohne diesen startet der Chat.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Liefert den vollständigen clap-Befehl von `harw`.
///
/// Entspricht [`Cli::command`], ergänzt um deutsche Hilfetexte für die
/// Argumente von `completions`, die aus dem gemeinsamen Completions-Crate
/// stammen. Hilfe-Ausgabe, Parse-Fehler und Shell-Completions sollten diesen
/// Befehl verwenden.
///
/// # Returns
/// Den fertig konfigurierten [`clap::Command`].
#[must_use]
pub fn command() -> clap::Command {
    Cli::command().mut_subcommand("completions", |sub| {
        sub.about("Erzeugt oder installiert Shell-Vervollständigungen.")
            .long_about(
                "Erzeugt oder installiert Shell-Vervollständigungen \
                 (bash, zsh, fish, elvish, powershell).",
            )
            .mut_arg("shell", |arg| {
                arg.help("Ziel-Shell; ohne Angabe wird sie aus $SHELL ermittelt.")
            })
            .mut_arg("install", |arg| {
                arg.help(
                    "Installiert das Vervollständigungsskript (ersetzt ältere Installationen).",
                )
            })
            .mut_arg("uninstall", |arg| {
                arg.help("Entfernt alle von harw angelegten Vervollständigungsdateien.")
            })
            .mut_arg("dry_run", |arg| {
                arg.help("Zeigt nur, was --install bzw. --uninstall ändern würde.")
            })
            .mut_arg("all_binaries", |arg| {
                arg.help(
                    "Installiert bzw. entfernt die Vervollständigungen auch für alle \
                     weiteren harw-Programme im PATH.",
                )
            })
    })
}

/// Alle Subcommands von `harw`.
///
/// Die Reihenfolge der Varianten bestimmt die Reihenfolge in `harw --help`.
/// Versteckte Varianten (`hide = true`) sind ältere Schreibweisen, die
/// weiterhin geparst werden; `main.rs` leitet sie mit einem Hinweis auf den
/// neuen Namen um.
#[derive(Debug, Subcommand)]
pub enum Command {
    // ── Arbeiten ────────────────────────────────────────────────────────
    /// Startet den interaktiven Chat, optional mit einem ersten Prompt.
    Chat(ChatArgs),
    /// Führt einen einzelnen Prompt ohne Rückfragen aus und beendet sich danach.
    Exec(ExecArgs),
    /// Analysiert die Crate-Abhängigkeiten im Workspace und bearbeitet sie der Reihe nach.
    Analyze(AnalyzeArgs),
    /// Listet, zeigt oder setzt gespeicherte Sitzungen fort.
    Session {
        /// Auszuführende Sitzungs-Aktion.
        #[command(subcommand)]
        action: SessionAction,
    },

    // ── Konfiguration ───────────────────────────────────────────────────
    /// Zeigt und ändert Konfigurationswerte und Freigaben; ohne Unterbefehl startet ein Menü.
    #[command(visible_alias = "settings")]
    Config {
        /// Auszuführende Konfigurations-Aktion; ohne Angabe startet das Menü.
        #[command(subcommand)]
        action: Option<SettingsAction>,
    },
    /// Verwaltet die Modell-Anbieter.
    Provider {
        /// Auszuführende Anbieter-Aktion.
        #[command(subcommand)]
        action: ProviderAction,
    },
    /// Verwaltet Modelle und das Standardmodell; ohne Unterbefehl werden sie gelistet.
    #[command(visible_alias = "models")]
    Model {
        /// Auszuführende Modell-Aktion; ohne Angabe wird gelistet.
        #[command(subcommand)]
        action: Option<ModelsAction>,
    },
    /// Verwaltet Zugangsdaten für Anbieter.
    Auth {
        /// Auszuführende Auth-Aktion.
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Gibt Projektverzeichnisse frei oder entzieht die Freigabe.
    Project {
        /// Auszuführende Freigabe-Aktion.
        #[command(subcommand)]
        action: ProjectAction,
    },

    // ── Agenten und Wissen ──────────────────────────────────────────────
    /// Verwaltet Agentin, Skills und Plugins.
    Agent {
        /// Auszuführende Agenten-Aktion.
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Verwaltet Wissensindex, Gedächtnis und Kontext-Vorschläge.
    Knowledge {
        /// Auszuführende Wissens-Aktion.
        #[command(subcommand)]
        action: KnowledgeAction,
    },
    /// Zeigt und steuert Hintergrundaufträge.
    Jobs {
        /// Auszuführende Auftrags-Aktion.
        #[command(subcommand)]
        action: JobsAction,
    },
    /// Baut, testet und führt Befehle im Container-Worker aus (P2 Builder-API).
    Worker {
        /// Auszuführende Worker-Aktion.
        #[command(subcommand)]
        action: WorkerAction,
    },
    /// Holt einen GitHub-PR read-only, legt den Diff als Fixture ab und
    /// reviewt ihn mit dem Agenten `github-pr-reviewer`.
    ///
    /// Der Diff ist nicht vertrauenswürdiger Input; Veröffentlichung auf
    /// GitHub passiert nur mit `--post` plus interaktiver Bestätigung und
    /// ist bis R5 nicht implementiert (bewusster Abbruch statt stiller
    /// Veröffentlichung).
    PrReview(PrReviewArgs),

    // ── Dienste ─────────────────────────────────────────────────────────
    /// Startet den Hintergrunddienst mit Agenten und Kanälen oder verwaltet seine Dienst-Einheit.
    Gateway {
        /// Dienst-Aktion; ohne Unterbefehl läuft der Dienst im Vordergrund.
        #[command(subcommand)]
        action: Option<GatewayAction>,
        /// Zusätzliche Telemetrie-Exportziele (Vorgabe: beide aus).
        #[command(flatten)]
        telemetry: TelemetryArgs,
    },
    /// Startet den lokalen MCP-Server über HTTP.
    Serve {
        /// Statt der Home-Konfiguration genau dieses Verzeichnis verwenden.
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        config_dir: Option<PathBuf>,
    },
    /// Startet die lokale Web-Oberfläche (Kontrollebene) auf einem Unix-Socket.
    ///
    /// Ohne Flag bindet sie `<home>/web.sock` (Entwicklungs-Rückfall). Mit
    /// `--system` bindet sie `/run/harw/infra/control.sock` (Modus 0660) und
    /// fällt nie auf das Home zurück; `--systemd-socket` übernimmt den von
    /// systemd übergebenen Socket.
    Web {
        /// Socket-Pfad überschreiben (Vorgabe: `<home>/web.sock`, mit
        /// `--system` `/run/harw/infra/control.sock`).
        #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
        socket: Option<PathBuf>,
        /// Systembetrieb: `/run/harw/infra/control.sock`, Modus 0660, kein
        /// Rückfall auf `<home>/web.sock`.
        #[arg(long)]
        system: bool,
        /// Den von systemd übergebenen Socket übernehmen (LISTEN_PID/LISTEN_FDS=1);
        /// impliziert `--system`, bindet selbst nichts.
        #[arg(long, conflicts_with_all = ["socket", "socket_group"])]
        systemd_socket: bool,
        /// Gruppe (Name oder GID) des selbst gebundenen System-Sockets; ohne
        /// Angabe bleibt die Gruppe unverändert.
        #[arg(long, value_name = "GROUP", requires = "system")]
        socket_group: Option<String>,
        /// Zusätzlich im eigenen Tailnet erreichbar machen: lauscht auf der
        /// Tailnet-Adresse dieses Knotens (nie öffentlich), lässt nur per
        /// `tailscaled`-whois identifizierte Geräte durch und gibt ihnen den
        /// Tier Operator.
        #[arg(long)]
        tailnet: bool,
        /// TCP-Port auf der Tailnet-Adresse (mit `--tailnet`).
        #[arg(long, value_name = "PORT", default_value_t = harw_tailscale::DEFAULT_TAILNET_PORT, requires = "tailnet")]
        tailnet_port: u16,
    },
    /// Verwaltet den Hintergrunddienst (systemd/launchd).
    Service {
        /// Auszuführende Dienst-Aktion.
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Richtet die MCP-Verbindung ein oder prüft sie.
    Mcp {
        /// Auszuführende MCP-Aktion.
        #[command(subcommand)]
        action: McpAction,
    },
    /// Bindet Nachrichtenkanäle wie Telegram an.
    Channel {
        /// Auszuführende Kanal-Aktion.
        #[command(subcommand)]
        action: ChannelAction,
    },

    // ── System ──────────────────────────────────────────────────────────
    /// Legt das Harwness-Verzeichnis an, falls es noch fehlt.
    Init,
    /// Richtet Anbieter, Modell und optional einen Kanal Schritt für Schritt ein.
    Onboard,
    /// Prüft Konfiguration und Verweise auf Fehler.
    Doctor {
        /// Statt der Home-Konfiguration genau dieses Verzeichnis prüfen.
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        config_dir: Option<PathBuf>,
    },
    /// Sucht nach einer neuen Version und installiert sie (Release, sonst aus den Quellen).
    ///
    /// Mit `--check` wird nur geprüft; Exit-Code 10 heißt „neuere Release verfügbar“.
    Update {
        /// Nur prüfen und Stand anzeigen, nichts installieren.
        #[arg(long, conflicts_with_all = ["yes", "dismiss"])]
        check: bool,
        /// Ohne Rückfrage installieren.
        #[arg(long, short = 'y', conflicts_with = "dismiss")]
        yes: bool,
        /// Den Start-Hinweis auf die bekannte neueste Version ausblenden.
        #[arg(long)]
        dismiss: bool,
    },
    /// Tailscale: Zustand des eigenen Knotens (Zugang über `harw web --tailnet`).
    Tailscale {
        /// Auszuführende Tailscale-Aktion.
        #[command(subcommand)]
        action: TailscaleAction,
    },
    /// Zeigt die eingebetteten systemd-Systemunits aus `deploy/`.
    ///
    /// Die DoD-Units installiert `make -C dod install` aus derselben Quelle;
    /// dieser Befehl installiert nichts, er gibt nur aus.
    Install {
        /// Eingebettete systemd-Unit ausgeben (z. B. `harw-warden.socket`); ohne UNIT alle.
        #[arg(long, value_name = "UNIT", num_args = 0..=1, value_hint = ValueHint::Other)]
        print_systemd: Option<Option<String>>,
    },
    /// Deinstalliert Harwness ganz oder teilweise.
    Uninstall {
        /// Zu entfernende Bereiche: service, state, workspace, binary.
        #[arg(long, value_enum, value_delimiter = ',')]
        scope: Vec<UninstallScope>,
        /// Nur anzeigen, was entfernt würde.
        #[arg(long)]
        dry_run: bool,
        /// Nicht rückfragen (nicht-interaktiv erforderlich).
        #[arg(long)]
        yes: bool,
    },
    /// Erzeugt oder installiert Shell-Vervollständigungen.
    #[command(name = "completions", alias = "completion")]
    Completions(CompletionsCommand),
    /// Speichert einen lokalen Fehlerbericht, ohne ihn zu versenden.
    BugReport {
        /// Berichtsart (z. B. `manual`, `crash`).
        #[arg(long = "type", value_hint = ValueHint::Other)]
        report_type: Option<String>,
        /// Kurztitel des Berichts.
        #[arg(long, value_hint = ValueHint::Other)]
        title: Option<String>,
        /// Betroffener Bereich.
        #[arg(long, value_hint = ValueHint::Other)]
        area: Option<String>,
        /// Beobachteter Fehlermodus.
        #[arg(long = "failure-mode", value_hint = ValueHint::Other)]
        failure_mode: Option<String>,
        /// Optionale Aufgabenkategorie.
        #[arg(long = "task-category", value_hint = ValueHint::Other)]
        task_category: Option<String>,
        /// Freitext-Beschreibung des Vorfalls.
        #[arg(long = "what-happened", value_hint = ValueHint::Other)]
        what_happened: Option<String>,
        /// Optionale, bereits geschwärzte Aussage des Nutzers.
        #[arg(long = "what-user-said", value_hint = ValueHint::Other)]
        what_user_said: Option<String>,
        /// Optionale Reproduktionsschritte.
        #[arg(long, value_hint = ValueHint::Other)]
        repro: Option<String>,
        /// Optionale Beleg-Ausschnitte.
        #[arg(long, value_hint = ValueHint::Other)]
        evidence: Option<String>,
    },
    /// Zeigt, welche Werkzeuge direkt auf dem Host laufen dürfen.
    Sandbox {
        /// Auszuführende Sandbox-Aktion; ohne Angabe wird der Status gezeigt.
        #[command(subcommand)]
        action: Option<SandboxAction>,
    },
    /// Diagnosewerkzeuge für Entwickler.
    Debug {
        /// Auszuführende Diagnose-Aktion.
        #[command(subcommand)]
        action: DebugAction,
    },
    /// Wählt Prozesse gezielt aus und beendet sie zuverlässig (nur Linux).
    ///
    /// Alle folgenden Argumente gehen unverändert an das Kill-Werkzeug;
    /// `harw kill --help` zeigt dessen eigene Hilfe.
    #[command(disable_help_flag = true)]
    Kill {
        /// Argumente für das Kill-Werkzeug (z. B. `-p NAME`, `--pid PID`, `--dry-run`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "KILLER_ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<OsString>,
    },

    // ── Versteckte ältere Schreibweisen ─────────────────────────────────
    /// Ältere Schreibweise von `harw channel connect`.
    #[command(hide = true)]
    Connect {
        /// Kanal-Art (derzeit nur `telegram`).
        #[arg(long, value_enum)]
        channel: Channel,
        /// Einmaligen Code nach einer `/pair CODE`-Nachricht an den Bot einlösen.
        #[arg(long, value_name = "CODE", value_hint = ValueHint::Other)]
        pair: Option<String>,
    },
    /// Ältere Schreibweise von `harw knowledge index`.
    #[command(hide = true)]
    Lens {
        /// Auszuführende Index-Aktion; ohne Angabe wird der Stand gezeigt.
        #[command(subcommand)]
        action: Option<LensAction>,
    },
    /// Ältere Schreibweise von `harw agent uia-new`.
    #[command(hide = true)]
    Uia {
        /// Auszuführende Aktion.
        #[command(subcommand)]
        action: UiaAction,
    },
    /// Ältere Schreibweise von `harw model catalog`.
    #[command(hide = true)]
    Catalog {
        /// Modell-Listen aus models.dev aktualisieren.
        #[arg(long)]
        refresh: bool,
    },
    /// Ältere Schreibweise von `harw debug echo`.
    #[command(hide = true)]
    Run {
        /// Die Eingabezeile.
        #[arg(required = true, num_args = 1.., value_hint = ValueHint::Other)]
        input: Vec<String>,
    },
    /// Ältere Schreibweise von `harw debug classify`.
    #[command(hide = true)]
    Classify {
        /// Die einzuordnende Eingabe.
        #[arg(required = true, num_args = 1.., value_hint = ValueHint::Other)]
        input: Vec<String>,
    },
}

/// Aktionen des `harw tailscale`-Subcommands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Subcommand)]
pub enum TailscaleAction {
    /// Zeigt, ob `tailscaled` erreichbar und verbunden ist, samt Knotenname
    /// und Tailnet-Adressen.
    Status,
}
