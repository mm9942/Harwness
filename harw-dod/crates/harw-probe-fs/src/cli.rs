//! Kommandozeilen-Grammatik von `harw-probe-fs` — `clap`, wie jedes andere
//! Binary dieses Projekts.
//!
//! # Verantwortungsbereich
//! [`Cli`] bündelt jeden Startparameter dieser Sonde: das verbindliche
//! `--log`-Flag (siehe [`LogLevel`]-Moduldoku für die Abweichung von
//! `harw-cli`s Fehlerverhalten), die DoD-Konfiguration (`--config`), den
//! Beobachtungsbereich (`--scope-root`, `--home-root`, `--home-entry`, je
//! wiederholbar und optional, PL-100 WP1.6), die loginuid-Wurzel
//! (`--proc-root`), die Sensor-Kennung (`--sensor-id`) und den
//! Sentinel-Socket (`--sentinel-socket`, Pflicht). Diese Datei parst nur —
//! sie kennt weder `ReadScope` noch `FsMonSensor`; welcher Bereich gilt
//! (Flag vor `[probe_fs]` vor Vorgabe), entscheidet [`crate::watch`].
//!
//! # Pfadgrammatik der Bereichsflags
//! `--scope-root`/`--home-root` verlangen dieselbe Form wie `[probe_fs]`
//! (`harw_dod_config::is_valid_probe_fs_root`: absolut, nie `/`, ohne `.`,
//! `..`, leere Komponente oder Steuerzeichen), `--home-entry` die relative
//! Form (`harw_dod_config::is_valid_probe_fs_home_entry`). Eine Wurzel `/`
//! weitete die Landlock-Leseregel dieser privilegierten Sonde auf das ganze
//! Dateisystem.
//!
//! # Unterbefehl `completions`
//! Optional: `harw-probe-fs completions [SHELL] [--install|--uninstall]`
//! (`harw_completions::CompletionsSubcommand`). `subcommand_negates_reqs`
//! hebt die Pflichtangaben für diesen Pfad auf. Deshalb ist
//! [`Cli::sentinel_socket`] ein `Option<PathBuf>` mit `required = true`:
//! ein nacktes `PathBuf` würde im abgeleiteten `FromArgMatches` auch beim
//! Unterbefehl einen `MissingRequiredArgument`-Fehler auslösen. Ohne
//! Unterbefehl erzwingt `clap` die Angabe weiterhin; `main` prüft sie
//! zusätzlich defensiv.
//!
//! # `--log`: ein ungültiger Wert ist ein Fehler
//! Wie `harw-sentinel` (und anders als `harw-cli`, dessen `--log` ein
//! unvalidierter `String` ist, der bei einem ungültigen Wert **still** auf
//! `"warn"` zurückfällt) ist [`LogLevel`] ein `clap::ValueEnum`: ein nicht
//! in dieser Liste enthaltener Wert ist ein **Parse-Fehler**
//! (`clap::Error`, `main` beendet mit `ExitCode::FAILURE`), keine stille
//! Voreinstellung. Ein Prozess mit `CAP_SYS_ADMIN`, der eine falsch
//! getippte Kommandozeile still ignoriert, verhält sich anders, als der
//! Aufrufer erwartet hat — bei dieser Berechtigungsklasse kein akzeptables
//! Verhalten.
//!
//! # Exportierte Typen
//! [`Cli`], [`LogLevel`].
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.
//!
//! # Fehler
//! Keine eigenen — `clap::Parser::try_parse`/`try_parse_from` liefern
//! `clap::Error`, das `main` direkt behandelt (siehe `crate`-Moduldoku).
//!
//! # Examples
//! ```rust,ignore
//! use crate::cli::Cli;
//! use clap::Parser as _;
//!
//! let cli = Cli::try_parse_from([
//!     "harw-probe-fs",
//!     "--scope-root", "/srv/data",
//!     "--sentinel-socket", "/run/harw-sentinel.sock",
//! ])?;
//! assert_eq!(cli.log, crate::cli::LogLevel::Info);
//! # Ok::<(), clap::Error>(())
//! ```

use std::path::PathBuf;

use clap::Parser;

/// Vorgabe-Wurzel für die loginuid-Auflösung
/// ([`harw_dod_fsmon::loginuid::resolve_loginuid`]).
pub const DEFAULT_PROC_ROOT: &str = "/proc";

/// Vorgabe-Kennung dieses Sensors, sofern `--sensor-id` nicht gesetzt ist.
pub const DEFAULT_SENSOR_ID: &str = "fsmon-0";

/// Gültige Werte für `--log`, in `tracing_subscriber::EnvFilter`-Syntax.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „--log: ein ungültiger Wert ist ein Fehler".
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum LogLevel {
    /// Feinste Stufe: einzelne Rohereignisse, Zwischenschritte der Formung.
    Trace,
    /// Entscheidungsverzweigungen (z. B. „außerhalb des Bereichs verworfen").
    Debug,
    /// Phasengrenzen: Start, Landlock-Ausgang, Sendebestätigung je Runde.
    Info,
    /// Wiederherstellbare Abweichung: ein transienter Sensorfehler.
    Warn,
    /// Nicht behebbarer Fehlschlag vor einem harten Start- oder
    /// Laufzeitabbruch.
    Error,
}

impl LogLevel {
    /// Wandelt diese Stufe in eine `tracing_subscriber::EnvFilter`-Direktive.
    ///
    /// # Returns
    /// Einen statischen Bezeichner (`"trace"`, `"debug"`, `"info"`, `"warn"`
    /// oder `"error"`), den [`tracing_subscriber::EnvFilter::try_new`] ohne
    /// weitere Übersetzung akzeptiert.
    ///
    /// # Examples
    /// ```rust,ignore
    /// use crate::cli::LogLevel;
    ///
    /// assert_eq!(LogLevel::Warn.as_filter_directive(), "warn");
    /// ```
    #[must_use]
    pub const fn as_filter_directive(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Vollständige Kommandozeilen-Grammatik von `harw-probe-fs`.
#[derive(Debug, Parser)]
#[command(
    name = "harw-probe-fs",
    bin_name = "harw-probe-fs",
    version,
    about = "fanotify-Sonde, Push-Only: sammelt Dateisystem-Ereignisse und sendet sie an den Sentinel.",
    subcommand_negates_reqs = true
)]
pub struct Cli {
    /// Log-Stufe für `tracing`. Siehe [`LogLevel`] für die Begründung, warum
    /// ein ungültiger Wert hier ein Fehler ist statt einer stillen
    /// Voreinstellung.
    #[arg(long, value_enum, default_value = "info")]
    pub log: LogLevel,

    /// Expliziter, administrativ vertrauter Pfad der DoD-Konfiguration
    /// (absolut, root-eigen; fehlt sie, scheitert der Start). Ohne Angabe
    /// gilt `/etc/harw-dod/config.toml`; fehlt diese Datei, gelten die
    /// eingebauten Vorgaben (siehe [`crate::watch`]).
    #[arg(long, value_name = "PATH", value_parser = parse_absolute_path)]
    pub config: Option<PathBuf>,

    /// Verzeichnis, unter dem jede Schreibänderung gemeldet wird.
    /// Wiederholbar. Eine Angabe von `--scope-root` oder `--home-root`
    /// ersetzt **beide** Wurzellisten von `[probe_fs]` und der Vorgabe
    /// (eine nicht angegebene Liste ist dann leer).
    #[arg(long = "scope-root", value_name = "DIR", value_parser = parse_watch_root)]
    pub scope_roots: Vec<PathBuf>,

    /// Verzeichnis mit Home-Verzeichnissen (z. B. `/home`) oder selbst ein
    /// Home-Verzeichnis (z. B. `/root`): darunter werden nur die
    /// Home-Einträge gemeldet. Wiederholbar; siehe `--scope-root`.
    #[arg(long = "home-root", value_name = "DIR", value_parser = parse_watch_root)]
    pub home_roots: Vec<PathBuf>,

    /// Pfad relativ zu einem Home-Verzeichnis, dessen Schreibänderungen
    /// gemeldet werden (z. B. `.ssh`, `.bashrc`). Wiederholbar; eine Angabe
    /// ersetzt `[probe_fs] home_entries` und die Vorgabe.
    #[arg(long = "home-entry", value_name = "PATH", value_parser = parse_home_entry)]
    pub home_entries: Vec<PathBuf>,

    /// Wurzel für die loginuid-Auflösung. Nur zum Testen gegen ein
    /// Fixture-Verzeichnis zu überschreiben — in Produktion bleibt es bei
    /// [`DEFAULT_PROC_ROOT`].
    #[arg(long, value_name = "DIR", default_value = DEFAULT_PROC_ROOT)]
    pub proc_root: PathBuf,

    /// Kennung, unter der diese Sonde ihre Ereignisse meldet
    /// (`SecurityEvent::sensor`).
    #[arg(long, value_name = "ID", default_value = DEFAULT_SENSOR_ID)]
    pub sensor_id: String,

    /// Pfad des `SOCK_SEQPACKET`-Unix-Sockets, über den der Sentinel hört.
    /// Pflicht — es gibt keinen betriebsfähigen Vorgabewert, der nicht von
    /// einer laufenden Sentinel-Instanz abhinge. `Option` nur wegen des
    /// `completions`-Unterbefehls (siehe Moduldoku); ohne Unterbefehl ist
    /// der Wert immer gesetzt.
    #[arg(long, value_name = "PATH", required = true)]
    pub sentinel_socket: Option<PathBuf>,

    /// Optional subcommand; without it the binary runs normally.
    #[command(subcommand)]
    pub command: Option<harw_completions::CompletionsSubcommand>,
}

/// `clap`-Wertparser für `--config`: nur absolute Pfade.
fn parse_absolute_path(raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{raw:?} is not an absolute path"))
    }
}

/// `clap`-Wertparser für `--scope-root`/`--home-root` (siehe Moduldoku,
/// „Pfadgrammatik der Bereichsflags").
fn parse_watch_root(raw: &str) -> Result<PathBuf, String> {
    if harw_dod_config::is_valid_probe_fs_root(raw) {
        Ok(PathBuf::from(raw))
    } else {
        Err(format!(
            "{raw:?} must be an absolute directory other than '/' without '.', '..', empty \
             components, a trailing '/' or control characters"
        ))
    }
}

/// `clap`-Wertparser für `--home-entry` (siehe Moduldoku, „Pfadgrammatik
/// der Bereichsflags").
fn parse_home_entry(raw: &str) -> Result<PathBuf, String> {
    if harw_dod_config::is_valid_probe_fs_home_entry(raw) {
        Ok(PathBuf::from(raw))
    } else {
        Err(format!(
            "{raw:?} must be a relative path without '.', '..', empty components, a trailing \
             '/' or control characters"
        ))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{Cli, DEFAULT_PROC_ROOT, DEFAULT_SENSOR_ID, LogLevel};
    use crate::test_support::{TestResult, ctx};

    fn minimal_args() -> Vec<&'static str> {
        vec![
            "harw-probe-fs",
            "--scope-root",
            "/srv/data",
            "--sentinel-socket",
            "/run/harw-sentinel.sock",
        ]
    }

    #[test]
    fn test_log_defaults_to_info_when_omitted() -> TestResult {
        let cli = Cli::try_parse_from(minimal_args()).map_err(ctx("minimal valid arguments"))?;
        assert_eq!(cli.log, LogLevel::Info);
        Ok(())
    }

    #[test]
    fn test_log_accepts_every_documented_value() -> TestResult {
        for value in ["trace", "debug", "info", "warn", "error"] {
            let mut args = minimal_args();
            args.push("--log");
            args.push(value);
            let cli = Cli::try_parse_from(args)
                .map_err(ctx("erwartete, dass der Wert erfolgreich geparst wird"))?;
            assert_eq!(cli.log.as_filter_directive(), value);
        }
        Ok(())
    }

    #[test]
    fn test_log_rejects_invalid_value_as_a_hard_error() {
        let mut args = minimal_args();
        args.push("--log");
        args.push("verbose");
        let result = Cli::try_parse_from(args);
        assert!(
            result.is_err(),
            "an unrecognised --log value must be a parse error, not a silent default"
        );
    }

    /// PL-100 WP1.6: ohne Bereichsflag kommt der Bereich aus `[probe_fs]`
    /// bzw. der Vorgabe (`crate::watch`), nicht aus einem Platzhalter.
    #[test]
    fn test_scope_flags_and_config_are_optional() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-probe-fs",
            "--sentinel-socket",
            "/run/harw-sentinel.sock",
        ])
        .map_err(ctx("no scope flag"))?;
        assert!(cli.scope_roots.is_empty());
        assert!(cli.home_roots.is_empty());
        assert!(cli.home_entries.is_empty());
        assert_eq!(cli.config, None);
        Ok(())
    }

    #[test]
    fn test_home_root_entry_and_config_flags_parse() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-probe-fs",
            "--config",
            "/etc/harw-dod/config.toml",
            "--home-root",
            "/home",
            "--home-root",
            "/root",
            "--home-entry",
            ".ssh",
            "--home-entry",
            ".config/systemd",
            "--sentinel-socket",
            "/run/harw-sentinel.sock",
        ])
        .map_err(ctx("home flags"))?;
        assert_eq!(
            cli.config,
            Some(std::path::PathBuf::from("/etc/harw-dod/config.toml"))
        );
        assert_eq!(
            cli.home_roots,
            vec![
                std::path::PathBuf::from("/home"),
                std::path::PathBuf::from("/root")
            ]
        );
        assert_eq!(
            cli.home_entries,
            vec![
                std::path::PathBuf::from(".ssh"),
                std::path::PathBuf::from(".config/systemd")
            ]
        );
        Ok(())
    }

    /// Eine Wurzel `/`, eine relative Wurzel, ein absoluter Home-Eintrag
    /// und ein relativer `--config` sind Parsefehler.
    #[test]
    fn test_scope_flags_refuse_paths_that_would_widen_the_sandbox() {
        for args in [
            ["--scope-root", "/"],
            ["--scope-root", "srv/data"],
            ["--home-root", "/home/../etc"],
            ["--home-entry", "/root/.ssh"],
            ["--home-entry", "../.ssh"],
            ["--config", "config.toml"],
        ] {
            let mut argv = vec![
                "harw-probe-fs",
                "--sentinel-socket",
                "/run/harw-sentinel.sock",
            ];
            argv.extend(args);
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "{args:?} must be rejected"
            );
        }
    }

    #[test]
    fn test_sentinel_socket_is_required() {
        let result = Cli::try_parse_from(["harw-probe-fs", "--scope-root", "/srv/data"]);
        assert!(
            result.is_err(),
            "missing --sentinel-socket must be rejected"
        );
    }

    #[test]
    fn test_scope_root_collects_multiple_occurrences() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-probe-fs",
            "--scope-root",
            "/srv/data",
            "--scope-root",
            "/srv/other",
            "--sentinel-socket",
            "/run/harw-sentinel.sock",
        ])
        .map_err(ctx("two scope roots"))?;
        assert_eq!(
            cli.scope_roots,
            vec![
                std::path::PathBuf::from("/srv/data"),
                std::path::PathBuf::from("/srv/other")
            ]
        );
        Ok(())
    }

    #[test]
    fn test_proc_root_and_sensor_id_use_documented_defaults() -> TestResult {
        let cli = Cli::try_parse_from(minimal_args()).map_err(ctx("minimal valid arguments"))?;
        assert_eq!(cli.proc_root, std::path::PathBuf::from(DEFAULT_PROC_ROOT));
        assert_eq!(cli.sensor_id, DEFAULT_SENSOR_ID);
        Ok(())
    }
}
