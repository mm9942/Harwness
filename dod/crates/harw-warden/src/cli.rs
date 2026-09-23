//! Kommandozeilen-Grammatik von `harw-warden`.
//!
//! # Verantwortungsbereich
//! [`Cli`] bündelt die einzigen zwei Startparameter dieses Binaries: das
//! verbindliche `--log`-Flag (siehe [`LogLevel`]-Moduldoku für die
//! Begründung — identisch zu `harw-sentinel`/`harw-probe-fs`) und
//! `--cgroup-root`, das Wurzelverzeichnis für
//! [`harw_dod_warden::CgroupV2Executor`] (Produktion: `/sys/fs/cgroup`). Der
//! Empfangssocket selbst ist **kein** Kommandozeilenparameter — er kommt
//! ausschließlich über die systemd-Socket-Aktivierung
//! (`LISTEN_FDS`/`LISTEN_PID`, siehe [`crate::systemd`]), niemals über einen
//! selbst geöffneten Pfad. Diese Datei parst nur — sie kennt weder
//! `Warden` noch `CgroupV2Executor`.
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.
//!
//! # Fehler
//! Keine eigenen — `clap::Parser::try_parse` liefert `clap::Error`, das
//! `main` direkt behandelt (Muster: `harw-sentinel`/`harw-probe-fs`).

use std::path::PathBuf;

use clap::Parser;

/// Vorgabe-Wurzel für [`harw_dod_warden::CgroupV2Executor`] in Produktion.
pub const DEFAULT_CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Gültige Werte für `--log`, in `tracing_subscriber::EnvFilter`-Syntax.
///
/// # Description
/// Wie bei `harw-sentinel`/`harw-probe-fs`: ein nicht in dieser Liste
/// enthaltener Wert ist ein **Parse-Fehler** (`clap::Error`, `main` beendet
/// mit `ExitCode::FAILURE`), keine stille Voreinstellung. Für einen
/// privilegierten Durchsetzer, der cgroups einfriert und Prozessbäume
/// beendet, ist eine stillschweigend andere Log-Stufe ein Betriebsrisiko,
/// kein kosmetischer Unterschied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum LogLevel {
    /// Feinste Stufe: jede empfangene Anfrage, jedes Feld, jeder Audit-Eintrag.
    Trace,
    /// Entscheidungsverzweigungen (Bindung, Zulässigkeit, Ausführung).
    Debug,
    /// Phasengrenzen: Start, Landlock-Ausgang, jede angenommene Verbindung.
    Info,
    /// Wiederherstellbare Abweichung: eine Verbindung ohne Anfrage, eine
    /// zu große Nachricht, ein Ausführungsfehler.
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

/// Vollständige Kommandozeilen-Grammatik von `harw-warden`.
#[derive(Debug, Parser)]
#[command(
    name = "harw-warden",
    bin_name = "harw-warden",
    version,
    about = "Durchsetzer-Binary: nimmt Anfragen am systemd-Socket entgegen, reicht sie an harw-dod-warden weiter.",
    subcommand_negates_reqs = true
)]
pub struct Cli {
    /// Optionales Unterkommando; ohne es läuft das Binary normal.
    #[command(subcommand)]
    pub command: Option<harw_completions::CompletionsSubcommand>,

    /// Log-Stufe für `tracing`. Siehe [`LogLevel`] für die Begründung, warum
    /// ein ungültiger Wert hier ein Fehler ist statt einer stillen
    /// Voreinstellung.
    #[arg(long, value_enum, default_value = "info")]
    pub log: LogLevel,

    /// Wurzelverzeichnis der cgroup-v2-Kontrolldateien. Nur zum Testen gegen
    /// ein Fixture-Verzeichnis zu überschreiben — in Produktion bleibt es
    /// bei [`DEFAULT_CGROUP_ROOT`].
    #[arg(long, value_name = "DIR", default_value = DEFAULT_CGROUP_ROOT)]
    pub cgroup_root: PathBuf,
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{Cli, DEFAULT_CGROUP_ROOT, LogLevel};
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_log_level_as_filter_directive_matches_every_variant() {
        assert_eq!(LogLevel::Trace.as_filter_directive(), "trace");
        assert_eq!(LogLevel::Debug.as_filter_directive(), "debug");
        assert_eq!(LogLevel::Info.as_filter_directive(), "info");
        assert_eq!(LogLevel::Warn.as_filter_directive(), "warn");
        assert_eq!(LogLevel::Error.as_filter_directive(), "error");
    }

    #[test]
    fn test_defaults_are_used_when_omitted() -> TestResult {
        let cli = Cli::try_parse_from(["harw-warden"]).map_err(ctx("no required args"))?;
        assert_eq!(cli.log, LogLevel::Info);
        assert_eq!(
            cli.cgroup_root,
            std::path::PathBuf::from(DEFAULT_CGROUP_ROOT)
        );
        Ok(())
    }

    #[test]
    fn test_log_accepts_every_documented_value() -> TestResult {
        for value in ["trace", "debug", "info", "warn", "error"] {
            let cli = Cli::try_parse_from(["harw-warden", "--log", value])
                .map_err(ctx("expected value to parse"))?;
            assert_eq!(cli.log.as_filter_directive(), value);
        }
        Ok(())
    }

    #[test]
    fn test_log_rejects_invalid_value_as_a_hard_error() {
        let result = Cli::try_parse_from(["harw-warden", "--log", "verbose"]);
        assert!(
            result.is_err(),
            "an unrecognised --log value must be a parse error, not a silent default"
        );
    }

    #[test]
    fn test_cgroup_root_override_is_honoured() -> TestResult {
        let cli = Cli::try_parse_from(["harw-warden", "--cgroup-root", "/tmp/fixture-cgroup"])
            .map_err(ctx("valid override"))?;
        assert_eq!(
            cli.cgroup_root,
            std::path::PathBuf::from("/tmp/fixture-cgroup")
        );
        Ok(())
    }
}
