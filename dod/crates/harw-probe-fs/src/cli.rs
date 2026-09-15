//! Kommandozeilen-Grammatik von `harw-probe-fs` — `clap`, wie jedes andere
//! Binary dieses Projekts.
//!
//! # Verantwortungsbereich
//! [`Cli`] bündelt jeden Startparameter dieser Sonde: das verbindliche
//! `--log`-Flag (siehe [`LogLevel`]-Moduldoku für die Abweichung von
//! `harw-cli`s Fehlerverhalten), den `ReadScope` (`--scope-root`,
//! wiederholbar, Pflicht), die loginuid-Wurzel (`--proc-root`), die
//! Sensor-Kennung (`--sensor-id`) und den Sentinel-Socket
//! (`--sentinel-socket`, Pflicht). Diese Datei parst nur — sie kennt weder
//! `ReadScope` noch `FsMonSensor`.
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
//! ])
//! .expect("minimal valid arguments");
//! assert_eq!(cli.log, crate::cli::LogLevel::Info);
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
    about = "fanotify-Sonde, Push-Only: sammelt Dateisystem-Ereignisse und sendet sie an den Sentinel."
)]
pub struct Cli {
    /// Log-Stufe für `tracing`. Siehe [`LogLevel`] für die Begründung, warum
    /// ein ungültiger Wert hier ein Fehler ist statt einer stillen
    /// Voreinstellung.
    #[arg(long, value_enum, default_value = "info")]
    pub log: LogLevel,

    /// Erlaubte Wurzelverzeichnisse für den `ReadScope` dieser Sonde.
    /// Wiederholbar; mindestens eine Angabe ist Pflicht.
    #[arg(long = "scope-root", value_name = "DIR", required = true)]
    pub scope_roots: Vec<PathBuf>,

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
    /// einer laufenden Sentinel-Instanz abhinge.
    #[arg(long, value_name = "PATH")]
    pub sentinel_socket: PathBuf,
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{Cli, LogLevel, DEFAULT_PROC_ROOT, DEFAULT_SENSOR_ID};

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
    fn test_log_defaults_to_info_when_omitted() {
        let cli = Cli::try_parse_from(minimal_args()).expect("minimal valid arguments");
        assert_eq!(cli.log, LogLevel::Info);
    }

    #[test]
    fn test_log_accepts_every_documented_value() {
        for value in ["trace", "debug", "info", "warn", "error"] {
            let mut args = minimal_args();
            args.push("--log");
            args.push(value);
            let cli = Cli::try_parse_from(args)
                .unwrap_or_else(|err| panic!("expected {value} to parse, got {err}"));
            assert_eq!(cli.log.as_filter_directive(), value);
        }
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

    #[test]
    fn test_scope_root_is_required() {
        let result = Cli::try_parse_from([
            "harw-probe-fs",
            "--sentinel-socket",
            "/run/harw-sentinel.sock",
        ]);
        assert!(result.is_err(), "missing --scope-root must be rejected");
    }

    #[test]
    fn test_sentinel_socket_is_required() {
        let result = Cli::try_parse_from(["harw-probe-fs", "--scope-root", "/srv/data"]);
        assert!(result.is_err(), "missing --sentinel-socket must be rejected");
    }

    #[test]
    fn test_scope_root_collects_multiple_occurrences() {
        let cli = Cli::try_parse_from([
            "harw-probe-fs",
            "--scope-root",
            "/srv/data",
            "--scope-root",
            "/srv/other",
            "--sentinel-socket",
            "/run/harw-sentinel.sock",
        ])
        .expect("two scope roots");
        assert_eq!(
            cli.scope_roots,
            vec![std::path::PathBuf::from("/srv/data"), std::path::PathBuf::from("/srv/other")]
        );
    }

    #[test]
    fn test_proc_root_and_sensor_id_use_documented_defaults() {
        let cli = Cli::try_parse_from(minimal_args()).expect("minimal valid arguments");
        assert_eq!(cli.proc_root, std::path::PathBuf::from(DEFAULT_PROC_ROOT));
        assert_eq!(cli.sensor_id, DEFAULT_SENSOR_ID);
    }
}
