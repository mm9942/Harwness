//! clap-basierte Kommandozeilen-Grammatik für `harw`.
//!
//! Spiegelt das codex-Muster: ein Root-Parser mit optionalem Subcommand und
//! geflatteten Chat-Flags. Fehlt ein Subcommand, startet der interaktive Chat
//! (`Cli::command == None`); `--help`/`-h`/`--version` behandelt clap selbst.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
    /// Chat-Flags am Root, damit `harw [PROMPT]` ohne Subcommand funktioniert.
    #[command(flatten)]
    pub chat: ChatArgs,
    /// Interaktionsmodus beim Start: chat, plan, explore oder work.
    /// Ohne Angabe gilt der Wert aus `[mode] default` der Harness-Konfiguration.
    ///
    /// Der Wert wird hier bewusst **nicht** validiert und bleibt `Option<String>`:
    /// geprüft wird er ausschließlich von `harw_core::InteractionMode::parse` im
    /// Composition-Root. `harw-cli` soll die Modusliste nicht ein zweites Mal
    /// führen — zwei Listen laufen sonst auseinander, und die in `harw-core`
    /// ist die maßgebliche.
    #[arg(long, value_name = "MODE", global = true)]
    pub mode: Option<String>,
    /// Ziel-Statement, das beim Start gesetzt und an den Plan gebunden wird.
    #[arg(long, value_name = "TEXT", global = true)]
    pub goal: Option<String>,
    /// Optionaler Subcommand; ohne diesen startet der Chat.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Flags, die für den Default-Chat-Pfad gelten.
#[derive(Debug, Args)]
pub struct ChatArgs {
    /// Optionaler erster Prompt. Ohne Prompt startet der interaktive Chat.
    pub prompt: Option<String>,
    /// Bestehende Sitzung fortsetzen; ohne Wert wird interaktiv ausgewählt.
    #[arg(short = 'r', long, num_args = 0..=1, value_name = "SESSION")]
    pub resume: Option<Option<String>>,
    /// Root-Space überschreiben (Vorrang vor `HARW_HOME`/`$HOME/.harw`).
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,
    /// Log-Level für das Tracing-Framework.
    ///
    /// Gültige Werte: `trace`, `debug`, `info`, `warn`, `error`.
    /// Entspricht der `RUST_LOG`-Syntax von `tracing-subscriber`.
    #[arg(long, global = true, value_name = "LEVEL", default_value = "info")]
    pub log: String,
    /// Aktiviert das Loggen sensibler Daten (Prompts, Tool-Args, Responses).
    ///
    /// **Achtung – Redaction-by-Default**: Ohne dieses Flag werden Prompts,
    /// Tool-Argumente und Modell-Responses **nicht** geloggt. Das Flag öffnet
    /// den Debug-Kanal; es darf **nicht** in Produktionsumgebungen gesetzt
    /// werden.
    #[arg(long, global = true, default_value_t = false)]
    pub log_sensitive: bool,
    /// Zeigt jeden Tool-Aufruf samt Argumenten in der TUI/Konsole an, statt
    /// nur die verdichtete `ToolCell`-Vorschau.
    #[arg(long, global = true, default_value_t = false)]
    pub verbose: bool,
    /// Zusätzliche Arbeitswurzel, unter der Datei-Werkzeuge ohne erneute
    /// Rückfrage lesen/schreiben dürfen (mehrfach angebbar). Nur für diesen
    /// Prozess gültig — dauerhaftes Merken läuft über `/add-workdir merken`
    /// bzw. `harw settings`.
    #[arg(long = "add-dir", global = true, value_name = "PFAD")]
    pub add_dir: Vec<PathBuf>,
    /// Zeigt bei `-r` die Sessions **aller** Projekte statt nur die des
    /// aktuellen Projekts (Projekt = nächster Ordner mit `.git`, sonst das
    /// Arbeitsverzeichnis selbst).
    #[arg(long, global = true, default_value_t = false)]
    pub all: bool,
}

/// Zusätzliche Telemetrie-Exportziele für `harw gateway`.
///
/// # Description
/// Der rotierende JSONL-Sink unter `<home>/telemetry`
/// ([`harw_home::paths::telemetry_dir`]) läuft **immer** — er ist kein Flag,
/// weil `security.*`/`warden.*`-Metriken ohne Operator-Freigabe ausschließlich
/// dorthin gehen müssen (siehe `crate::observe`-Moduldoku). Beide Flags hier
/// steuern ausschließlich *zusätzliche* Ziele für gewöhnliche Metriken; ohne
/// sie bleibt jedes zusätzliche Ziel **aus** — kein Endpunkt bindet, kein
/// Stapel wird gesendet.
#[derive(Debug, Args)]
pub struct TelemetryArgs {
    /// Bindet einen lokalen Prometheus-`/metrics`-Endpunkt auf `127.0.0.1:<PORT>`
    /// (nur Loopback, siehe `harw_observe_prom::BindAddr`). Ohne dieses Flag
    /// bindet kein Endpunkt.
    #[arg(long, value_name = "PORT")]
    pub metrics_prometheus_port: Option<u16>,
    /// Exportiert nicht-geschützte Metriken als OTLP/JSON-Stapel per HTTP an
    /// diesen Collector, z. B. `http://127.0.0.1:4318/v1/metrics`. Ohne
    /// dieses Flag wird kein Stapel gesendet. `security.*`/`warden.*`
    /// erreichen dieses Ziel nie, unabhängig von diesem Flag (siehe
    /// `crate::observe`-Moduldoku).
    #[arg(long, value_name = "URL")]
    pub metrics_otlp_endpoint: Option<String>,
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
        #[arg(long)]
        channel: String,
        /// Einmaligen Code nach einer `/pair CODE`-Nachricht an den Bot einlösen.
        #[arg(long, value_name = "CODE")]
        pair: Option<String>,
    },
    /// Layered-Konfiguration und Katalog-Referenzen validieren.
    Doctor {
        /// Statt der Home-Layer genau dieses Verzeichnis prüfen.
        #[arg(long, value_name = "DIR")]
        config_dir: Option<PathBuf>,
    },
    /// Den lokalen Streamable-HTTP-MCP-Listener binden.
    Serve {
        /// Statt der Home-Layer genau dieses Verzeichnis verwenden.
        #[arg(long, value_name = "DIR")]
        config_dir: Option<PathBuf>,
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
        #[arg(long, value_name = "PATH")]
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
        #[arg(required = true, num_args = 1..)]
        input: Vec<String>,
    },
    /// Einen einzelnen Turn gegen den lokalen Echo-Bootstrap-Provider laufen.
    Run {
        /// Die Eingabezeile.
        #[arg(required = true, num_args = 1..)]
        input: Vec<String>,
    },
    /// Crate-Abhängigkeiten im Workspace analysieren und Kind-Agenten planen.
    Analyze(AnalyzeArgs),
    /// Shell-Completion-Skript für die angegebene Shell ausgeben.
    Completion {
        /// Ziel-Shell (bash, zsh, fish, powershell, elvish).
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
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
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        /// Nur anzeigen, was entfernt würde.
        #[arg(long)]
        dry_run: bool,
        /// Nicht rückfragen (nicht-interaktiv erforderlich).
        #[arg(long)]
        yes: bool,
    },
}

/// Argumente des `harw analyze`-Subcommands.
#[derive(Debug, Clone, clap::Args)]
pub struct AnalyzeArgs {
    /// Zu analysierendes Crate; ohne Angabe der gesamte Workspace.
    pub crate_name: Option<String>,
    /// Erzwingt die Analyse des gesamten Workspace, auch wenn ein Crate genannt ist.
    #[arg(long)]
    pub workspace: bool,
    /// Analysiert von den Blatt-Crates aufwärts (Vorgabe).
    ///
    /// Schließt sich mit `--top-down` aus: werden beide Flags gemeinsam
    /// angegeben, bricht das Parsing mit einem Fehler ab — unabhängig von der
    /// Reihenfolge, in der sie stehen. Es gibt bewusst keinen stillen
    /// Vorrang eines Flags vor dem anderen.
    #[arg(long, default_value_t = true, conflicts_with = "top_down")]
    pub bottom_up: bool,
    /// Kehrt die Reihenfolge um: von den Wurzeln abwärts.
    ///
    /// Siehe `--bottom-up`: beide Flags zusammen sind ein Fehler, kein
    /// stiller Vorrang.
    #[arg(long)]
    pub top_down: bool,
    /// Legt den Plan an, startet aber keine Kind-Agenten.
    #[arg(long)]
    pub dry_run: bool,
    /// Obergrenze gleichzeitig laufender Kind-Agenten.
    #[arg(long, value_name = "N")]
    pub max_parallel: Option<usize>,
}

/// Aktionen des `harw auth`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthAction {
    /// Setup-Token per PKCE-Paste-Flow beschaffen und hinterlegen.
    ///
    /// Zeigt die Authorize-URL, nimmt den zurückgegebenen Code auf `stdin`
    /// entgegen, tauscht ihn gegen den Token, speichert ihn (0600) und druckt
    /// die `export CLAUDE_CODE_OAUTH_TOKEN=…`-Zeile.
    Login {
        /// Provider (derzeit nur `anthropic`).
        #[arg(default_value = "anthropic")]
        provider: String,
    },
    /// Setup-Token direkt setzen (Alternative zum API-Key). Liest von `stdin`.
    Token {
        /// Provider (derzeit nur `anthropic`).
        #[arg(default_value = "anthropic")]
        provider: String,
    },
    /// Lokale Credentials importieren (z. B. `~/.codex/auth.json`).
    Import {
        /// Quelle: `codex` (OpenAI) oder `claude-cli` (Anthropic).
        #[arg(default_value = "codex")]
        source: String,
    },
    /// Vorhandene Credential-Quellen anzeigen (ohne Secrets).
    Status,
}

/// Aktionen für die dedizierte `harw-gateway.service`-Unit.
#[derive(Debug, Subcommand)]
pub enum GatewayAction {
    /// Unit schreiben, aktivieren und starten.
    Install,
    /// Gateway-Dienst starten.
    Start,
    /// Gateway-Dienst geordnet stoppen.
    Stop,
    /// Gateway-Dienst neu starten.
    Restart,
    /// Gateway-Dienst beim Login aktivieren.
    Enable,
    /// Gateway-Dienst beim Login deaktivieren.
    Disable,
}

/// Aktionen des `harw service`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum ServiceAction {
    /// Dienst-Unit rendern und installieren.
    Install {
        /// Nur die gerenderte Unit ausgeben, nichts installieren.
        #[arg(long)]
        dry_run: bool,
    },
    /// Dienststatus abfragen.
    Status,
    /// Dienst-Unit entfernen.
    Uninstall,
}

/// Aktionen des `harw project`-Subcommands.
///
/// Ohne `path` wirkt jede Aktion auf das aktuelle Arbeitsverzeichnis
/// (siehe `crate::project_trust`).
#[derive(Debug, Subcommand)]
pub enum ProjectAction {
    /// Gibt das Projekt frei (bzw. erneuert die Freigabe).
    Trust {
        /// Projekt-Root; ohne Angabe das aktuelle Arbeitsverzeichnis.
        #[arg(value_name = "DIR")]
        path: Option<PathBuf>,
    },
    /// Entzieht die Freigabe für das Projekt.
    Untrust {
        /// Projekt-Root; ohne Angabe das aktuelle Arbeitsverzeichnis.
        #[arg(value_name = "DIR")]
        path: Option<PathBuf>,
    },
    /// Zeigt den Vertrauensstatus des Projekts.
    Status {
        /// Projekt-Root; ohne Angabe das aktuelle Arbeitsverzeichnis.
        #[arg(value_name = "DIR")]
        path: Option<PathBuf>,
    },
}

/// Auszuführende Aktion unter `harw settings`.
///
/// Ohne diesen Subcommand (`Cli::command == Some(Command::Settings { action:
/// None })`) startet [`crate::settings::run`] das interaktive Menü.
#[derive(Debug, Subcommand)]
pub enum SettingsAction {
    /// Provider verwalten (`providers/<name>.toml` im aktiven Profil).
    Provider {
        #[command(subcommand)]
        action: SettingsProviderAction,
    },
    /// Standardmodell verwalten.
    Model {
        #[command(subcommand)]
        action: SettingsModelAction,
    },
    /// Freigabe-Standardmodus und Allow-/Deny-Regeln — derselbe Schreibpfad
    /// wie das `/permissions`-Panel der TUI.
    Permissions {
        #[command(subcommand)]
        action: SettingsPermissionsAction,
    },
    /// Liest einen einzelnen, punktgetrennten Konfigurationsschlüssel.
    Get {
        /// Punktgetrennter Schlüsselpfad, z. B. `permissions.default_mode`.
        key: String,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Setzt einen einzelnen, punktgetrennten Konfigurationsschlüssel.
    Set {
        /// Punktgetrennter Schlüsselpfad, z. B. `permissions.default_mode`.
        key: String,
        /// Neuer Wert als Text; wird als TOML-String geschrieben (siehe
        /// `crate::settings`). Ohne Wert wird der Schlüssel gelöscht.
        value: Option<String>,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
}

/// Ziel-Ebene (`SettingScope`) eines `get`/`set`/`permissions`-Aufrufs.
///
/// `--global` ist die Vorgabe, wenn keines der beiden Flags gesetzt ist;
/// `--global` und `--project` schließen sich gegenseitig aus, unabhängig von
/// der Reihenfolge (siehe `AnalyzeArgs::bottom_up`/`top_down` für dasselbe
/// Muster).
#[derive(Debug, Clone, Copy, Args)]
pub struct SettingsScopeArgs {
    /// Schreibt/liest die dauerhafte User-Ebene (`~/.harw/…`). Vorgabe.
    #[arg(long, conflicts_with = "project")]
    pub global: bool,
    /// Schreibt/liest die dauerhafte Projekt-Ebene
    /// (`~/.harw/profiles/<p>/projects/<key>/settings.toml`).
    #[arg(long)]
    pub project: bool,
}

impl SettingsScopeArgs {
    /// Löst die Flags in einen [`harw_config::SettingScope`] auf.
    ///
    /// # Returns
    /// [`harw_config::SettingScope::Project`], wenn `--project` gesetzt ist,
    /// sonst [`harw_config::SettingScope::Global`] (Vorgabe). Nie `Session`
    /// — die CLI-Grammatik kennt keinen Weg, eine reine Speicher-Ebene
    /// anzusprechen.
    #[must_use]
    pub fn resolve(self) -> harw_config::SettingScope {
        if self.project {
            harw_config::SettingScope::Project
        } else {
            harw_config::SettingScope::Global
        }
    }
}

/// Aktionen des `harw settings provider`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum SettingsProviderAction {
    /// Listet alle Provider des aktiven Profils.
    List,
    /// Legt einen neuen Provider an (oder überschreibt einen gleichnamigen).
    Add {
        /// Provider-Name (Dateiname `providers/<name>.toml`).
        name: String,
        /// API-Dialekt, z. B. `openai-chat`, `openai-responses`,
        /// `anthropic-messages`, `ollama`.
        #[arg(long)]
        api: String,
        /// Basis-URL des Providers.
        #[arg(long = "base-url", value_name = "URL")]
        base_url: String,
        /// Secret-Referenz (`env:VAR` oder `secrets:NAME`); ein Klartext-Key
        /// wird abgelehnt.
        #[arg(long)]
        auth: Option<String>,
        /// Modell-IDs dieses Providers, kommagetrennt.
        #[arg(long, value_delimiter = ',')]
        models: Vec<String>,
    },
    /// Entfernt einen Provider.
    Remove {
        /// Provider-Name.
        name: String,
    },
    /// Aktiviert einen zuvor deaktivierten Provider.
    Enable {
        /// Provider-Name.
        name: String,
    },
    /// Deaktiviert einen Provider, ohne ihn zu löschen.
    Disable {
        /// Provider-Name.
        name: String,
    },
}

/// Aktionen des `harw settings model`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum SettingsModelAction {
    /// Setzt das Standardmodell (`default_model`) der globalen Ebene.
    Default {
        /// Modell-ID, wie in `models/<id>.toml` deklariert.
        id: String,
    },
}

/// Aktionen des `harw models`-Subcommands (Addendum C).
///
/// Ohne diesen Subcommand (`Cli::command == Some(Command::Models { action:
/// None })`) entspricht das dem `list`-Zweig (siehe `crate::models::run`).
#[derive(Debug, Subcommand)]
pub enum ModelsAction {
    /// Listet aktivierte Provider, konfigurierte Modelle (mit markiertem
    /// Standardmodell) und die internen Modellstellen samt Auflösung.
    List,
    /// Fragt die `/models`-Endpunkte konfigurierter Provider ab
    /// (`harw_provider_http::discovery::list_models`).
    Scan {
        /// Nur diesen Provider abfragen; ohne Angabe alle aktivierten
        /// Provider.
        provider: Option<String>,
        /// Legt für jedes entdeckte Modell eine `models/<id>.toml` im
        /// aktiven Profil an (bereits vorhandene Dateien bleiben
        /// unverändert).
        #[arg(long)]
        add: bool,
        /// Zeigt/übernimmt nur kostenlose Modelle (Preis 0 oder
        /// `:free`-Suffix der Modell-ID).
        #[arg(long)]
        free_only: bool,
    },
    /// Interne Modellstellen verwalten (Session-Titel, Kompaktierungs-
    /// Zusammenfassung, Speicher-Konsolidierung, Traumreflexion, Explorer,
    /// Recherche). Ohne Unterbefehl entspricht dies `internal show`.
    Internal {
        #[command(subcommand)]
        action: Option<InternalAction>,
    },
    /// Setzt das globale Standardmodell — derselbe Schreibpfad wie `harw
    /// settings model default`.
    Default {
        /// Modell-ID, wie in `models/<id>.toml` deklariert.
        id: String,
    },
}

/// Aktionen des `harw models internal`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum InternalAction {
    /// Zeigt jede interne Modellstelle mit ihrer effektiven Auflösung
    /// (explizit / OpenRouter-Standard / Hauptmodell).
    Show,
    /// Setzt eine interne Modellstelle explizit auf `model`, optional bei
    /// einem anderen Provider als `harness.default_provider`.
    Set {
        /// Stellen-Schlüssel, z. B. `session_title` oder `session-title`
        /// (siehe `harw_config::InternalModelPoint::parse`).
        point: String,
        /// Modell-ID beim gewählten Provider.
        model: String,
        /// Provider-Name; ohne Angabe `harness.default_provider`.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Erzwingt für diese Stelle das Hauptmodell der Sitzung (leere Wahl).
    Main {
        /// Stellen-Schlüssel.
        point: String,
    },
    /// Entfernt eine explizite Wahl für diese Stelle; die Auflösung fällt
    /// zurück auf den OpenRouter-Standard bzw. das Hauptmodell.
    Reset {
        /// Stellen-Schlüssel.
        point: String,
    },
    /// Schaltet `use_openrouter_defaults` global an (`on`) oder aus (`off`).
    OpenrouterDefaults {
        /// `on` oder `off`.
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
}

/// Aktionen des `harw settings permissions`-Subcommands — derselbe
/// Schreibpfad wie das `/permissions`-Panel (Contract §2/§5 Zeile A2).
#[derive(Debug, Subcommand)]
pub enum SettingsPermissionsAction {
    /// Zeigt Standardmodus, Timeout und Allow-/Deny-Regeln der gewählten
    /// Ebene.
    Get {
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Setzt den Standard-Freigabemodus (`ask`, `auto` oder `full`).
    SetMode {
        /// `ask`, `auto` oder `full`.
        mode: String,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Hängt eine Allow-Regel an.
    Allow {
        /// Werkzeugname, z. B. `shell.exec`.
        tool: String,
        /// Optionales Muster (Shell-Präfix bzw. Pfad-Glob).
        #[arg(long)]
        pattern: Option<String>,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Hängt eine Deny-Regel an.
    Deny {
        /// Werkzeugname, z. B. `fs.write`.
        tool: String,
        /// Optionales Muster (Shell-Präfix bzw. Pfad-Glob).
        #[arg(long)]
        pattern: Option<String>,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Entfernt eine Allow-Regel per Index (siehe `permissions get`).
    Unallow {
        /// Index in der Allow-Liste, 0-basiert.
        index: usize,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Entfernt eine Deny-Regel per Index (siehe `permissions get`).
    Undeny {
        /// Index in der Deny-Liste, 0-basiert.
        index: usize,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_telegram_pairing_parses() {
        let cli = Cli::try_parse_from(["harw", "connect", "--channel", "telegram", "--pair", "ABCD-EFGH"])
            .expect("Telegram connect must parse");
        let Some(Command::Connect { channel, pair }) = cli.command else {
            panic!("expected connect command");
        };
        assert_eq!(channel, "telegram");
        assert_eq!(pair.as_deref(), Some("ABCD-EFGH"));
    }

    #[test]
    fn resume_is_absent_without_the_flag() {
        let cli = Cli::try_parse_from(["harw"]).expect("bare harw parses");

        assert_eq!(cli.chat.resume, None);
    }

    #[test]
    fn resume_without_a_value_requests_interactive_selection() {
        let short = Cli::try_parse_from(["harw", "-r"]).expect("short resume parses");
        let long = Cli::try_parse_from(["harw", "--resume"]).expect("long resume parses");

        assert_eq!(short.chat.resume, Some(None));
        assert_eq!(long.chat.resume, Some(None));
    }

    #[test]
    fn resume_with_a_value_targets_that_session() {
        let cli = Cli::try_parse_from(["harw", "--resume", "session-42"])
            .expect("resume selector parses");

        assert_eq!(cli.chat.resume, Some(Some("session-42".to_owned())));
    }

    #[test]
    fn resume_preserves_a_positional_prompt_when_explicitly_separated() {
        let cli = Cli::try_parse_from(["harw", "-r", "--", "continue this"])
            .expect("resume with prompt parses");

        assert_eq!(cli.chat.resume, Some(None));
        assert_eq!(cli.chat.prompt.as_deref(), Some("continue this"));
    }

    #[test]
    fn resume_does_not_consume_a_subcommand_name() {
        let cli = Cli::try_parse_from(["harw", "--resume", "doctor"])
            .expect("resume before subcommand parses");

        assert_eq!(cli.chat.resume, Some(None));
        assert!(matches!(cli.command, Some(Command::Doctor { .. })));
    }

    #[test]
    fn test_mode_flag_sets_the_requested_mode() {
        let cli = match Cli::try_parse_from(["harw", "--mode", "explore"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw --mode explore` sollte parsen: {err}"),
        };

        assert_eq!(cli.mode.as_deref(), Some("explore"));
    }

    #[test]
    fn test_mode_flag_accepts_an_unknown_value_without_validation() {
        // Bewusst: die Gültigkeit der Modusliste wird hier NICHT geprüft (siehe
        // Doc-Kommentar an `Cli::mode`). Dieser Test hält die Entscheidung fest,
        // damit sie nicht versehentlich "repariert" wird.
        let cli = match Cli::try_parse_from(["harw", "--mode", "voelliger-unsinn"]) {
            Ok(cli) => cli,
            Err(err) => panic!("ein unbekannter --mode-Wert muss hier durchgehen: {err}"),
        };

        assert_eq!(cli.mode.as_deref(), Some("voelliger-unsinn"));
    }

    #[test]
    fn test_goal_flag_sets_the_goal_statement() {
        let cli = match Cli::try_parse_from(["harw", "--goal", "Alle Tests grün"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw --goal ...` sollte parsen: {err}"),
        };

        assert_eq!(cli.goal.as_deref(), Some("Alle Tests grün"));
    }

    #[test]
    fn test_analyze_without_arguments_defaults_to_bottom_up_whole_workspace() {
        let cli = match Cli::try_parse_from(["harw", "analyze"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw analyze` sollte parsen: {err}"),
        };

        let Some(Command::Analyze(args)) = cli.command else {
            panic!("erwartete Command::Analyze, bekam {:?}", cli.command);
        };
        assert_eq!(args.crate_name, None);
        assert!(args.bottom_up);
    }

    #[test]
    fn test_analyze_with_crate_dry_run_and_max_parallel() {
        let cli = match Cli::try_parse_from([
            "harw",
            "analyze",
            "harw-plan",
            "--dry-run",
            "--max-parallel",
            "4",
        ]) {
            Ok(cli) => cli,
            Err(err) => {
                panic!("`harw analyze harw-plan --dry-run --max-parallel 4` sollte parsen: {err}")
            }
        };

        let Some(Command::Analyze(args)) = cli.command else {
            panic!("erwartete Command::Analyze, bekam {:?}", cli.command);
        };
        assert_eq!(args.crate_name.as_deref(), Some("harw-plan"));
        assert!(args.dry_run);
        assert_eq!(args.max_parallel, Some(4));
    }

    #[test]
    fn test_bottom_up_and_top_down_conflict_the_same_way_in_both_orders() {
        let forward = Cli::try_parse_from(["harw", "analyze", "--bottom-up", "--top-down"]);
        let backward = Cli::try_parse_from(["harw", "analyze", "--top-down", "--bottom-up"]);

        // Dokumentiertes Ergebnis: beide Flags gemeinsam sind ein Parse-Fehler,
        // unabhängig von der Reihenfolge — kein stiller Vorrang eines Flags.
        match (forward, backward) {
            (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind()),
            (a, b) => panic!("beide Reihenfolgen müssen gleich scheitern, bekam {a:?} / {b:?}"),
        }
    }

    #[test]
    fn test_project_trust_subcommands_parse() {
        let trust = match Cli::try_parse_from(["harw", "project", "trust", "/tmp/proj"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw project trust /tmp/proj` sollte parsen: {err}"),
        };
        let Some(Command::Project {
            action: ProjectAction::Trust { path },
        }) = trust.command
        else {
            panic!("erwartete Command::Project(Trust), bekam {:?}", trust.command);
        };
        assert_eq!(path, Some(PathBuf::from("/tmp/proj")));

        let untrust = match Cli::try_parse_from(["harw", "project", "untrust"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw project untrust` sollte parsen: {err}"),
        };
        let Some(Command::Project {
            action: ProjectAction::Untrust { path },
        }) = untrust.command
        else {
            panic!(
                "erwartete Command::Project(Untrust), bekam {:?}",
                untrust.command
            );
        };
        assert_eq!(path, None);

        let status = match Cli::try_parse_from(["harw", "project", "status", "."]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw project status .` sollte parsen: {err}"),
        };
        let Some(Command::Project {
            action: ProjectAction::Status { path },
        }) = status.command
        else {
            panic!(
                "erwartete Command::Project(Status), bekam {:?}",
                status.command
            );
        };
        assert_eq!(path, Some(PathBuf::from(".")));
    }

    #[test]
    fn test_verbose_and_add_dir_flags_parse_and_repeat() {
        let cli = match Cli::try_parse_from([
            "harw",
            "--verbose",
            "--add-dir",
            "/tmp/a",
            "--add-dir",
            "/tmp/b",
        ]) {
            Ok(cli) => cli,
            Err(err) => panic!("`--verbose --add-dir ...` sollte parsen: {err}"),
        };

        assert!(cli.chat.verbose);
        assert_eq!(
            cli.chat.add_dir,
            vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
        );
    }

    #[test]
    fn test_verbose_and_add_dir_default_to_empty() {
        let cli = Cli::try_parse_from(["harw"]).expect("bare harw parses");

        assert!(!cli.chat.verbose);
        assert!(cli.chat.add_dir.is_empty());
    }

    #[test]
    fn test_settings_without_action_parses_for_interactive_menu() {
        let cli = match Cli::try_parse_from(["harw", "settings"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw settings` sollte parsen: {err}"),
        };

        assert!(matches!(
            cli.command,
            Some(Command::Settings { action: None })
        ));
    }

    #[test]
    fn test_settings_provider_add_parses_all_flags() {
        let cli = match Cli::try_parse_from([
            "harw",
            "settings",
            "provider",
            "add",
            "test",
            "--api",
            "openai-chat",
            "--base-url",
            "http://localhost:1",
            "--auth",
            "env:X",
            "--models",
            "a,b",
        ]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw settings provider add ...` sollte parsen: {err}"),
        };

        let Some(Command::Settings {
            action:
                Some(SettingsAction::Provider {
                    action: SettingsProviderAction::Add {
                        name,
                        api,
                        base_url,
                        auth,
                        models,
                    },
                }),
        }) = cli.command
        else {
            panic!("erwartete Settings(Provider(Add)), bekam {:?}", cli.command);
        };
        assert_eq!(name, "test");
        assert_eq!(api, "openai-chat");
        assert_eq!(base_url, "http://localhost:1");
        assert_eq!(auth.as_deref(), Some("env:X"));
        assert_eq!(models, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn test_settings_model_default_parses() {
        let cli = match Cli::try_parse_from(["harw", "settings", "model", "default", "gpt-5.4"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw settings model default ...` sollte parsen: {err}"),
        };

        let Some(Command::Settings {
            action:
                Some(SettingsAction::Model {
                    action: SettingsModelAction::Default { id },
                }),
        }) = cli.command
        else {
            panic!("erwartete Settings(Model(Default)), bekam {:?}", cli.command);
        };
        assert_eq!(id, "gpt-5.4");
    }

    #[test]
    fn test_settings_get_set_default_to_global_scope() {
        let get = match Cli::try_parse_from(["harw", "settings", "get", "default_model"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw settings get ...` sollte parsen: {err}"),
        };
        let Some(Command::Settings {
            action: Some(SettingsAction::Get { key, scope }),
        }) = get.command
        else {
            panic!("erwartete Settings(Get), bekam {:?}", get.command);
        };
        assert_eq!(key, "default_model");
        assert!(!scope.global);
        assert!(!scope.project);

        let set = match Cli::try_parse_from([
            "harw",
            "settings",
            "set",
            "default_model",
            "gpt-5.4",
            "--project",
        ]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw settings set ... --project` sollte parsen: {err}"),
        };
        let Some(Command::Settings {
            action: Some(SettingsAction::Set { key, value, scope }),
        }) = set.command
        else {
            panic!("erwartete Settings(Set), bekam {:?}", set.command);
        };
        assert_eq!(key, "default_model");
        assert_eq!(value.as_deref(), Some("gpt-5.4"));
        assert!(scope.project);
        assert!(!scope.global);
    }

    #[test]
    fn test_settings_scope_flags_conflict_in_both_orders() {
        let forward = Cli::try_parse_from([
            "harw", "settings", "get", "default_model", "--global", "--project",
        ]);
        let backward = Cli::try_parse_from([
            "harw", "settings", "get", "default_model", "--project", "--global",
        ]);

        match (forward, backward) {
            (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind()),
            (a, b) => panic!("beide Reihenfolgen müssen gleich scheitern, bekam {a:?} / {b:?}"),
        }
    }

    #[test]
    fn test_settings_permissions_allow_and_deny_parse() {
        let allow = match Cli::try_parse_from([
            "harw",
            "settings",
            "permissions",
            "allow",
            "shell.exec",
            "--pattern",
            "cargo check",
        ]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw settings permissions allow ...` sollte parsen: {err}"),
        };
        assert!(matches!(
            allow.command,
            Some(Command::Settings {
                action: Some(SettingsAction::Permissions {
                    action: SettingsPermissionsAction::Allow { .. },
                }),
            })
        ));

        let deny = match Cli::try_parse_from([
            "harw",
            "settings",
            "permissions",
            "deny",
            "fs.write",
        ]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw settings permissions deny ...` sollte parsen: {err}"),
        };
        assert!(matches!(
            deny.command,
            Some(Command::Settings {
                action: Some(SettingsAction::Permissions {
                    action: SettingsPermissionsAction::Deny { .. },
                }),
            })
        ));
    }

    #[test]
    fn test_models_without_action_parses_for_list() {
        let cli = match Cli::try_parse_from(["harw", "models"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models` sollte parsen: {err}"),
        };
        assert!(matches!(cli.command, Some(Command::Models { action: None })));
    }

    #[test]
    fn test_models_scan_parses_provider_and_flags() {
        let cli = match Cli::try_parse_from([
            "harw", "models", "scan", "openrouter", "--add", "--free-only",
        ]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models scan ...` sollte parsen: {err}"),
        };
        let Some(Command::Models {
            action: Some(ModelsAction::Scan { provider, add, free_only }),
        }) = cli.command
        else {
            panic!("erwartete Models(Scan), bekam {:?}", cli.command);
        };
        assert_eq!(provider.as_deref(), Some("openrouter"));
        assert!(add);
        assert!(free_only);
    }

    #[test]
    fn test_models_scan_without_provider_defaults_to_all() {
        let cli = match Cli::try_parse_from(["harw", "models", "scan"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models scan` sollte parsen: {err}"),
        };
        let Some(Command::Models {
            action: Some(ModelsAction::Scan { provider, add, free_only }),
        }) = cli.command
        else {
            panic!("erwartete Models(Scan), bekam {:?}", cli.command);
        };
        assert_eq!(provider, None);
        assert!(!add);
        assert!(!free_only);
    }

    #[test]
    fn test_models_internal_show_parses() {
        let cli = match Cli::try_parse_from(["harw", "models", "internal"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models internal` sollte parsen: {err}"),
        };
        assert!(matches!(
            cli.command,
            Some(Command::Models {
                action: Some(ModelsAction::Internal { action: None }),
            })
        ));

        let cli = match Cli::try_parse_from(["harw", "models", "internal", "show"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models internal show` sollte parsen: {err}"),
        };
        assert!(matches!(
            cli.command,
            Some(Command::Models {
                action: Some(ModelsAction::Internal {
                    action: Some(InternalAction::Show),
                }),
            })
        ));
    }

    #[test]
    fn test_models_internal_set_parses_point_model_and_provider() {
        let cli = match Cli::try_parse_from([
            "harw",
            "models",
            "internal",
            "set",
            "session-title",
            "nvidia/nemotron-3.5-lightning",
            "--provider",
            "openrouter",
        ]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models internal set ...` sollte parsen: {err}"),
        };
        let Some(Command::Models {
            action: Some(ModelsAction::Internal {
                action: Some(InternalAction::Set { point, model, provider }),
            }),
        }) = cli.command
        else {
            panic!("erwartete Models(Internal(Set)), bekam {:?}", cli.command);
        };
        assert_eq!(point, "session-title");
        assert_eq!(model, "nvidia/nemotron-3.5-lightning");
        assert_eq!(provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn test_models_internal_main_and_reset_parse() {
        let main = match Cli::try_parse_from(["harw", "models", "internal", "main", "explorer"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models internal main ...` sollte parsen: {err}"),
        };
        assert!(matches!(
            main.command,
            Some(Command::Models {
                action: Some(ModelsAction::Internal {
                    action: Some(InternalAction::Main { .. }),
                }),
            })
        ));

        let reset = match Cli::try_parse_from(["harw", "models", "internal", "reset", "research"])
        {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models internal reset ...` sollte parsen: {err}"),
        };
        assert!(matches!(
            reset.command,
            Some(Command::Models {
                action: Some(ModelsAction::Internal {
                    action: Some(InternalAction::Reset { .. }),
                }),
            })
        ));
    }

    #[test]
    fn test_models_internal_openrouter_defaults_accepts_on_off_only() {
        let on = Cli::try_parse_from(["harw", "models", "internal", "openrouter-defaults", "on"])
            .expect("`on` sollte parsen");
        assert!(matches!(
            on.command,
            Some(Command::Models {
                action: Some(ModelsAction::Internal {
                    action: Some(InternalAction::OpenrouterDefaults { .. }),
                }),
            })
        ));

        let invalid = Cli::try_parse_from([
            "harw",
            "models",
            "internal",
            "openrouter-defaults",
            "maybe",
        ]);
        assert!(invalid.is_err(), "ungültiger Zustand muss scheitern");
    }

    #[test]
    fn test_models_default_parses() {
        let cli = match Cli::try_parse_from(["harw", "models", "default", "gpt-5.4"]) {
            Ok(cli) => cli,
            Err(err) => panic!("`harw models default ...` sollte parsen: {err}"),
        };
        let Some(Command::Models {
            action: Some(ModelsAction::Default { id }),
        }) = cli.command
        else {
            panic!("erwartete Models(Default), bekam {:?}", cli.command);
        };
        assert_eq!(id, "gpt-5.4");
    }

    #[test]
    fn test_web_rejects_config_dir_flag() {
        let result = Cli::try_parse_from(["harw", "web", "--config-dir", "/tmp/cfg"]);

        assert!(
            result.is_err(),
            "`harw web --config-dir` darf nicht mehr parsen, bekam {result:?}"
        );
    }
}
