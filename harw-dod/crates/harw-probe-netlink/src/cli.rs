//! Kommandozeilenvertrag der Audit-Netlink-Sonde.
//!
//! # Verantwortungsbereich
//! Drei Angaben, mehr nicht: `--log` (Stufe des stderr-Subscribers, wie bei
//! `harw-probe-bpf`), `--sentinel-socket` (Pflicht) und `--sensor-id`
//! (Vorgabe [`DEFAULT_SENSOR_ID`]). Welche Records gemeldet werden, ist
//! **keine** Option: die Regel steht in `harw_dod_authlog`
//! (`LOGIN_RECORD_TYPES`), nicht in einer Startzeile.
//!
//! Optionaler Unterbefehl `completions`
//! (`harw_completions::CompletionsSubcommand`). `subcommand_negates_reqs`
//! hebt die Pflichtangaben für diesen Pfad auf; deshalb ist
//! `sentinel_socket` ein `Option<PathBuf>` mit `required = true` (ein nacktes
//! `PathBuf` würde im abgeleiteten `FromArgMatches` auch beim Unterbefehl
//! scheitern). Ohne Unterbefehl erzwingt `clap` die Angabe.
//!
//! # Das `SensorId`-Schema
//! `probe-netlink-auth-0`: Muster `<binary>-<teilsensor>-<instanz>` wie
//! `probe-bpf-procmon-0`. Das Präfix hält die Kennung von einer künftigen
//! eigenständigen Sensor-Crate gleichen Namens unterscheidbar.
//!
//! # Exportierte Typen
//! [`Cli`], [`LogLevel`], [`DEFAULT_SENSOR_ID`].
//!
//! # Nebenläufigkeit
//! Reine Werttypen.
//!
//! # Fehler
//! `clap::Error`, behandelt in `main`.
//!
//! # Examples
//! ```text
//! $ harw-probe-netlink --log info --sentinel-socket /run/harw/sentinel.sock
//! ```

use std::path::PathBuf;

use clap::Parser;

/// Vorgabe für `--sensor-id` (siehe Moduldoku, „Das `SensorId`-Schema").
pub const DEFAULT_SENSOR_ID: &str = "probe-netlink-auth-0";

/// Stufe des `tracing`-Subscribers (`--log`).
///
/// # Description
/// Geschlossene Menge: `clap` lehnt jeden anderen Wert ab, bevor
/// `main::init_tracing` ihn sieht.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum LogLevel {
    /// Everything, including per-record details.
    Trace,
    /// Per-cycle counters.
    Debug,
    /// Phase boundaries and periodic statistics.
    Info,
    /// Lost records and fallbacks.
    Warn,
    /// Unrecoverable errors only.
    Error,
}

impl LogLevel {
    /// Die `EnvFilter`-Direktive dieser Stufe.
    ///
    /// # Returns
    /// `"trace"`, `"debug"`, `"info"`, `"warn"` oder `"error"`.
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

/// Die geparste Kommandozeile.
#[derive(Debug, Parser)]
#[command(
    name = "harw-probe-netlink",
    bin_name = "harw-probe-netlink",
    version,
    about = "Push-only audit netlink probe: forwards login records (USER_AUTH, USER_LOGIN) to harw-sentinel.",
    subcommand_negates_reqs = true
)]
pub struct Cli {
    /// Log level of the stderr subscriber.
    #[arg(long, value_enum, default_value = "info")]
    pub log: LogLevel,

    /// Sentinel socket (SOCK_SEQPACKET). `Option` only because of the
    /// `completions` subcommand; always set when no subcommand is given.
    #[arg(long, value_name = "PATH", required = true)]
    pub sentinel_socket: Option<PathBuf>,

    /// Sensor id carried by every forwarded event.
    #[arg(long = "sensor-id", value_name = "ID", default_value = DEFAULT_SENSOR_ID)]
    pub sensor_id: String,

    /// Optional subcommand; without it the binary runs normally.
    #[command(subcommand)]
    pub command: Option<harw_completions::CompletionsSubcommand>,
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{Cli, DEFAULT_SENSOR_ID, LogLevel};
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_defaults_need_only_the_sentinel_socket() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-probe-netlink",
            "--sentinel-socket",
            "/run/harw/sentinel.sock",
        ])
        .map_err(ctx("minimale Argumente parsen"))?;
        assert_eq!(cli.log, LogLevel::Info);
        assert_eq!(cli.sensor_id, DEFAULT_SENSOR_ID);
        assert_eq!(
            cli.sentinel_socket,
            Some(std::path::PathBuf::from("/run/harw/sentinel.sock"))
        );
        assert!(cli.command.is_none());
        Ok(())
    }

    #[test]
    fn test_log_and_sensor_id_are_overridable() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-probe-netlink",
            "--log",
            "debug",
            "--sensor-id",
            "probe-netlink-auth-1",
            "--sentinel-socket",
            "/run/harw/sentinel.sock",
        ])
        .map_err(ctx("alle Argumente parsen"))?;
        assert_eq!(cli.log, LogLevel::Debug);
        assert_eq!(cli.log.as_filter_directive(), "debug");
        assert_eq!(cli.sensor_id, "probe-netlink-auth-1");
        Ok(())
    }

    #[test]
    fn test_missing_sentinel_socket_is_rejected() {
        assert!(Cli::try_parse_from(["harw-probe-netlink"]).is_err());
    }

    #[test]
    fn test_unknown_log_level_is_rejected() {
        assert!(
            Cli::try_parse_from([
                "harw-probe-netlink",
                "--log",
                "verbose",
                "--sentinel-socket",
                "/run/harw/sentinel.sock",
            ])
            .is_err()
        );
    }

    #[test]
    fn test_completions_subcommand_needs_no_sentinel_socket() -> TestResult {
        let cli = Cli::try_parse_from(["harw-probe-netlink", "completions", "bash"])
            .map_err(ctx("completions ohne Socket parsen"))?;
        assert!(cli.command.is_some());
        assert!(cli.sentinel_socket.is_none());
        Ok(())
    }

    #[test]
    fn test_every_log_level_has_its_directive() {
        let levels = [
            (LogLevel::Trace, "trace"),
            (LogLevel::Debug, "debug"),
            (LogLevel::Info, "info"),
            (LogLevel::Warn, "warn"),
            (LogLevel::Error, "error"),
        ];
        for (level, directive) in levels {
            assert_eq!(level.as_filter_directive(), directive);
        }
    }
}
