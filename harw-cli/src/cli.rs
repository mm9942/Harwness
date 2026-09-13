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
    /// (Telegram) + Knowledge (Workbench/Dream/Diary). Bleibt dauerhaft am
    /// Leben — der Weg, wie `harw.service` läuft.
    Gateway {
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
    /// Auth-/Credential-Verwaltung (Claude-Setup-Token, Import, Status).
    Auth {
        /// Auszuführende Auth-Aktion.
        #[command(subcommand)]
        action: AuthAction,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_web_rejects_config_dir_flag() {
        let result = Cli::try_parse_from(["harw", "web", "--config-dir", "/tmp/cfg"]);

        assert!(
            result.is_err(),
            "`harw web --config-dir` darf nicht mehr parsen, bekam {result:?}"
        );
    }
}
