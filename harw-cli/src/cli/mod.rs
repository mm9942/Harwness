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
mod project;
mod provider;
mod sandbox;
mod service;
mod session;
mod settings;
mod uia;
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
pub use project::*;
pub use provider::*;
pub use sandbox::*;
pub use service::*;
pub use session::*;
pub use settings::*;
pub use uia::*;
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
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Root-Space `~/.harw` anlegen/scaffolden (idempotent).
    Init,
    /// Onboarding-Wizard: Provider, Modell und optional Channel einrichten.
    Onboard,
    /// Richtet einen Channel ein oder schließt ein ausstehendes Pairing ab.
    Connect {
        /// Channel-Art (derzeit nur `telegram`).
        #[arg(long, value_enum)]
        channel: Channel,
        /// Einmaligen Code nach einer `/pair CODE`-Nachricht an den Bot einlösen.
        #[arg(long, value_name = "CODE", value_hint = ValueHint::Other)]
        pair: Option<String>,
    },
    /// Layered-Konfiguration und Katalog-Referenzen validieren.
    Doctor {
        /// Statt der Home-Layer genau dieses Verzeichnis prüfen.
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        config_dir: Option<PathBuf>,
    },
    /// Den lokalen Streamable-HTTP-MCP-Listener binden.
    Serve {
        /// Statt der Home-Layer genau dieses Verzeichnis verwenden.
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        config_dir: Option<PathBuf>,
    },
    /// Cloudflare-MCP-Verbindung einrichten oder prüfen.
    Mcp {
        /// Auszuführende MCP-Aktion.
        #[command(subcommand)]
        action: McpAction,
    },
    /// Persistenter Hintergrund-Daemon: Gateway + Agenten + Channels
    /// (Telegram) + Knowledge (Workbench/Dream/Diary). Ohne Aktion läuft der
    /// Daemon im Vordergrund; die Aktionen verwalten seine Service-Unit.
    Gateway {
        /// Service-Aktion; ohne Subcommand wird der Gateway direkt gestartet.
        #[command(subcommand)]
        action: Option<GatewayAction>,
        /// Zusätzliche Telemetrie-Exportziele (Vorgabe: beide aus).
        #[command(flatten)]
        telemetry: TelemetryArgs,
    },
    /// Startet die HTTP-Kontrollfläche (`harw-web`) auf einem Unix-Socket.
    ///
    /// Läuft **nicht** ungefragt: ohne diesen Subcommand bindet kein
    /// Prozess `harw-web`s Socket. Bedient dieselbe [`harw_operations`]-
    /// Registry, die auch der Chat- und `analyze`-Pfad zusammenstellen
    /// (siehe `crate::web`-Moduldoku). Der Root-Space wird ausschließlich
    /// über `--home` bzw. `HARW_HOME` aufgelöst — erfordert `--home` bzw.
    /// `HARW_HOME`.
    Web {
        /// Socket-Pfad überschreiben (Vorgabe: `<home>/web.sock`).
        #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
        socket: Option<PathBuf>,
    },
    /// Projekt-Freigabe (Trust) für ein Verzeichnis verwalten.
    ///
    /// Steuert `<home>/trusted-projects.toml` (siehe
    /// [`harw_home::trust`]): ein freigegebenes Projekt darf sein
    /// repo-lokales `.harw` als zusätzlichen Config-Layer beisteuern.
    Project {
        /// Auszuführende Trust-Aktion.
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Eine Eingabezeile mit dem Front-End-Klassifikator einordnen.
    Classify {
        /// Die zu klassifizierende Eingabe.
        #[arg(required = true, num_args = 1.., value_hint = ValueHint::Other)]
        input: Vec<String>,
    },
    /// Einen einzelnen Turn gegen den lokalen Echo-Bootstrap-Provider laufen.
    Run {
        /// Die Eingabezeile.
        #[arg(required = true, num_args = 1.., value_hint = ValueHint::Other)]
        input: Vec<String>,
    },
    /// Crate-Abhängigkeiten im Workspace analysieren und Kind-Agenten planen.
    Analyze(AnalyzeArgs),
    /// Generate or install shell completions (bash, zsh, fish, elvish, powershell).
    #[command(name = "completions", alias = "completion")]
    Completions(CompletionsCommand),
    /// Auf eine neue Version prüfen (schreibt `version.json`).
    Update {
        /// Nur prüfen und Stand anzeigen, nichts installieren.
        #[arg(long)]
        check: bool,
    },
    /// Den Hintergrunddienst (systemd/launchd) verwalten.
    Service {
        /// Auszuführende Dienst-Aktion.
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Provider-Katalog anzeigen oder (mit `--refresh`) via models.dev anreichern.
    Catalog {
        /// Modell-Listen aus models.dev aktualisieren.
        #[arg(long)]
        refresh: bool,
    },
    /// Konfigurierte Modelle entdecken/verwalten und interne Modellstellen
    /// (Session-Titel, Kompaktierung, Speicher-Konsolidierung,
    /// Traumreflexion, Explorer, Recherche) einzeln konfigurieren.
    /// Ohne Unterbefehl entspricht dies `harw models list`.
    Models {
        /// Auszuführende Models-Aktion; ohne Angabe wird gelistet.
        #[command(subcommand)]
        action: Option<ModelsAction>,
    },
    /// Auth-/Credential-Verwaltung (Claude-Setup-Token, Import, Status).
    Auth {
        /// Auszuführende Auth-Aktion.
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Provider, Modelle, Freigaben und einzelne Konfigurationswerte
    /// verwalten. Ohne Unterbefehl startet ein zeilenbasiertes Menü
    /// (siehe `crate::settings`).
    Settings {
        /// Auszuführende Settings-Aktion; ohne Angabe startet das
        /// interaktive Menü.
        #[command(subcommand)]
        action: Option<SettingsAction>,
    },
    /// Harwness deinstallieren (Bereiche wählbar, Dry-Run möglich).
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
    /// Benutzeroberflächen-Agentin (UIA) verwalten.
    ///
    /// Der interaktive Chat-Einstieg richtet ohne konfigurierte UIA bereits
    /// automatisch eine ein (siehe `crate::uia_bootstrap`); dieser Befehl
    /// erlaubt, den Einrichtungsdialog gezielt erneut aufzurufen, z. B. um
    /// eine zusätzliche UIA neben einer bereits aktiven anzulegen.
    Uia {
        /// Auszuführende UIA-Aktion.
        #[command(subcommand)]
        action: UiaAction,
    },
    /// Host-Profil-Permit-Ledger prüfen und verwalten (siehe
    /// [`harw_sandbox::ProcessPermitLedger`], `host-process-worker.toml`).
    /// Ohne Unterbefehl entspricht dies `harw sandbox status`.
    ///
    /// **Achtung**: Ledger und Sitzungs-Registry leben ausschließlich im
    /// Speicher der laufenden Sitzung (`harw`-Chat/-TUI-Prozess), die sie
    /// ausgestellt hat — wie jede In-Memory-Freigabe in dieser Harness gibt
    /// es keine Persistenz über Prozessgrenzen. Dieser Befehl ist deshalb
    /// die Konfigurations-/Audit-Fläche (analog zu `harw settings`/
    /// `harw models`), kein Fenster in eine fremde, bereits laufende
    /// Sitzung: `leases`/`revoke` wirken nur auf den Ledger **dieses**
    /// Prozesses.
    Sandbox {
        /// Auszuführende Sandbox-Aktion; ohne Angabe wird der Status gezeigt.
        #[command(subcommand)]
        action: Option<SandboxAction>,
    },
    /// Speichert einen lokalen Bug-Report unter `<home>/bug-report/`.
    ///
    /// Rein lokal — kein Netzwerk-Versand. Fehlende Pflichtfelder
    /// (`--type`, `--title`, `--area`, `--failure-mode`, `--what-happened`)
    /// werden interaktiv nachgefragt. Die automatische Incident-Erkennung
    /// (gekillte Kinder, erschöpfte Retries, Panics) ist ein separates, noch
    /// ausstehendes Arbeitspaket — dieser Befehl ist der manuelle Fallback.
    BugReport {
        /// Berichtsart (z. B. `manual`, `crash`).
        #[arg(long = "type", value_hint = ValueHint::Other)]
        report_type: Option<String>,
        /// Kurztitel des Berichts.
        #[arg(long, value_hint = ValueHint::Other)]
        title: Option<String>,
        /// Betroffener Bereich/Modul.
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
        /// Optionaler, bereits redigierter Nutzer-O-Ton.
        #[arg(long = "what-user-said", value_hint = ValueHint::Other)]
        what_user_said: Option<String>,
        /// Optionale Reproduktionsschritte.
        #[arg(long, value_hint = ValueHint::Other)]
        repro: Option<String>,
        /// Optionale Beleg-Ausschnitte.
        #[arg(long, value_hint = ValueHint::Other)]
        evidence: Option<String>,
    },
    /// Den Wissensindex des Retrieval-Subsystems (`harw-lens`) bauen,
    /// aktualisieren oder seinen Status anzeigen.
    ///
    /// Baut **niemals** automatisch beim Sitzungsstart — eine Indizierung
    /// kann bei einem großen Bestand mehrere Minuten dauern; dieser Befehl
    /// ist der bewusste, vom Betreiber angestoßene Einstiegspunkt (siehe
    /// `crate::lens`). Ohne dieses Kommando bleiben die von `lens.ask`
    /// befragten Indizes leer, und `lens.ask` meldet sie als `skipped`
    /// statt Treffer zu liefern. Ohne Unterbefehl entspricht dies
    /// `harw lens status`.
    Lens {
        /// Auszuführende Lens-Aktion; ohne Angabe wird der Status gezeigt.
        #[command(subcommand)]
        action: Option<LensAction>,
    },
    /// Prozesse präzise auswählen und per doppeltem SIGKILL beenden (killer).
    ///
    /// Reicht alle folgenden Argumente unverändert an `harw-killer` durch;
    /// `harw kill --help` zeigt deshalb die Hilfe von killer selbst. Nur
    /// unter Linux verfügbar (pidfd). `main.rs` erkennt `harw kill …` bereits
    /// vor dem clap-Parse, damit globale `harw`-Flags wie `--log` oder
    /// `--verbose` hinter `kill` bei killer ankommen statt vom Root-Parser
    /// verschluckt zu werden; diese Variante dient Hilfe, Completions und dem
    /// Fall `harw <Root-Flags> kill …`.
    #[command(disable_help_flag = true)]
    Kill {
        /// Argumente für killer (z. B. `-p NAME`, `--pid PID`, `--dry-run`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "KILLER_ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<OsString>,
    },
}
