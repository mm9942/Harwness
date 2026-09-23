//! Kommandozeilen-Grammatik von `harw-sentinel`.
//!
//! # Verantwortungsbereich
//! [`Cli`] bündelt jeden Startparameter dieses Binaries: das verbindliche
//! `--log`-Flag (siehe Moduldoku von [`LogLevel`] für die Abweichung von
//! `harw-cli`s Fehlerverhalten), Wurzelverzeichnisse für die sechs
//! eingebetteten Dateisystem-Sensoren (`--proc-root`, `--thermal-root`,
//! `--workspace-root`, `--blockio-root`, `--gpu-root`, `--cgroup-root`), den
//! Pfad des IPC-Empfangssockets und die Poll-Taktung. Diese Datei parst
//! nur — sie kennt weder `Sentinel` noch einen Sensor.
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.
//!
//! # Fehler
//! Keine eigenen — [`Cli::try_parse_from`]/`clap::Parser::try_parse` liefern
//! `clap::Error`, das `main` in [`crate::error::SentinelBinError::Cli`]
//! überführt.

use std::path::PathBuf;

use clap::Parser;

/// Vorgabe: das IPC-Empfangssocket liegt unter `<home>/sentinel.sock`.
///
/// Kein `harw_home::paths`-Eintrag definiert diesen Pfad — die
/// IPC-Empfangsentscheidung ist Gegenstand dieses Knotens (AW2-19), nicht
/// von `harw-home`. `--socket` überschreibt diesen Namen vollständig.
pub const DEFAULT_SOCKET_FILE_NAME: &str = "sentinel.sock";

/// Vorgabe-Wurzel des unprivilegierten `/proc`-Lesebereichs
/// ([`harw_dod_listener::ListenerSensor`], [`harw_dod_cpu::CpuSensor`]).
pub const DEFAULT_PROC_ROOT: &str = "/proc";

/// Vorgabe-Wurzel des unprivilegierten `/sys/class/thermal`-Lesebereichs
/// ([`harw_dod_thermal::ThermalSensor`]).
pub const DEFAULT_THERMAL_ROOT: &str = "/sys/class/thermal";

/// Vorgabe-Wurzel des unprivilegierten `/sys/block`-Lesebereichs
/// ([`harw_dod_blockio::BlockioSensor`]) — der Pfad, den
/// [`harw_dod_cap::Capability::ReadSysfsBlock`]`::probe()` nennt.
pub const DEFAULT_BLOCKIO_ROOT: &str = "/sys/block";

/// Vorgabe-Wurzel des unprivilegierten `/sys/class/drm`-Lesebereichs
/// ([`harw_dod_gpu::GpuSensor`]) — der Pfad, den
/// [`harw_dod_cap::Capability::ReadSysfsDrm`]`::probe()` nennt.
pub const DEFAULT_GPU_ROOT: &str = "/sys/class/drm";

/// Vorgabe-Wurzel des unprivilegierten `/sys/fs/cgroup`-Lesebereichs
/// ([`harw_dod_cgroup::CgroupSensor`]) — der Pfad, den
/// [`harw_dod_cap::Capability::ReadCgroupV2`]`::probe()` nennt.
pub const DEFAULT_CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Vorgabe-Taktung der Sammelschleife in Sekunden.
///
/// Dieser Wert (bzw. `--interval-secs`) ist der programmweite Poll-Abstand:
/// die Sammelschleife des Sentinels schläft genau so lange, und Sensoren mit
/// Rückschaufenster bekommen mindestens das Doppelte (`PollTiming` in
/// `main.rs`). Fünf Sekunden sind ein für interaktive Beobachtung
/// brauchbarer Standard.
pub const DEFAULT_INTERVAL_SECS: u64 = 5;

/// Gültige Werte für `--log`, in `tracing_subscriber::EnvFilter`-Syntax.
///
/// # Description
/// Anders als `harw-cli` (dessen `--log` ein unvalidierter `String` ist, der
/// bei einem ungültigen Wert **still** auf `"warn"` zurückfällt — siehe
/// `harw-cli/src/main.rs::init_tracing`) ist dieses Flag ein
/// `clap::ValueEnum`: ein nicht in dieser Liste enthaltener Wert ist ein
/// **Parse-Fehler** (`clap::Error`, Exitcode ungleich `0`), keine stille
/// Voreinstellung. Für einen dauerhaft laufenden Sammler, der nie eine
/// interaktive Sitzung vor sich hat, die einen falschen Start bemerken
/// würde, ist eine stillschweigend andere Log-Stufe als die angeforderte
/// ein Betriebsrisiko, kein kosmetischer Unterschied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum LogLevel {
    /// Feinste Stufe: einzelne Sensor-Polls, Feldwerte, Zwischenschritte.
    Trace,
    /// Entscheidungsverzweigungen, Zwischenwerte je Poll-Runde.
    Debug,
    /// Phasengrenzen: Start, Sensor-Registrierung, Poll-Rundenzusammenfassung.
    Info,
    /// Wiederherstellbare Abweichung: Landlock degradiert, IPC-Socket nicht
    /// bindbar, ein Sensor geht nach `Degraded` über.
    Warn,
    /// Nicht behebbarer Fehlschlag vor einem `Err`-Rückgabewert.
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
    /// ```rust
    /// use harw_sentinel::cli::LogLevel;
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

/// Vollständige Kommandozeilen-Grammatik von `harw-sentinel`.
///
/// # Description
/// Jedes Feld hat eine betriebsfähige Voreinstellung außer `--home` und
/// `--socket`, die von `main` gegen `harw_home::paths` aufgelöst werden,
/// falls nicht gesetzt (ein Vorgabewert ließe sich hier nicht ohne den
/// bereits aufgelösten Root-Space bilden).
#[derive(Debug, Parser)]
#[command(
    name = "harw-sentinel",
    bin_name = "harw-sentinel",
    version,
    about = "Unprivilegierte Sammelstelle: ruft Sensoren, führt den Degradationsautomaten, puffert.",
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

    /// Root-Space überschreiben (Vorrang vor `HARW_HOME`/`$HOME/.harw`).
    #[arg(long, value_name = "DIR")]
    pub home: Option<PathBuf>,

    /// Expliziter, administrativ vertrauter Pfad der DoD-Konfiguration
    /// (absolut, root-eigen). Ohne Angabe gilt `/etc/harw-dod/config.toml`.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Wurzel des `/proc`-Lesebereichs für [`harw_dod_listener::ListenerSensor`]
    /// und [`harw_dod_cpu::CpuSensor`]. Nur zum Testen gegen ein
    /// Fixture-Verzeichnis zu überschreiben — in Produktion bleibt es bei
    /// [`DEFAULT_PROC_ROOT`].
    #[arg(long, value_name = "DIR", default_value = DEFAULT_PROC_ROOT)]
    pub proc_root: PathBuf,

    /// Wurzel des `/sys/class/thermal`-Lesebereichs für
    /// [`harw_dod_thermal::ThermalSensor`]. Nur zum Testen zu überschreiben.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_THERMAL_ROOT)]
    pub thermal_root: PathBuf,

    /// Wurzel des Cargo-Workspace, den
    /// [`harw_dod_workspace::WorkspaceDriftSensor`] beobachtet.
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub workspace_root: PathBuf,

    /// Wurzel des `/sys/block`-Lesebereichs für
    /// [`harw_dod_blockio::BlockioSensor`]. Nur zum Testen gegen ein
    /// Fixture-Verzeichnis zu überschreiben — in Produktion bleibt es bei
    /// [`DEFAULT_BLOCKIO_ROOT`].
    #[arg(long, value_name = "DIR", default_value = DEFAULT_BLOCKIO_ROOT)]
    pub blockio_root: PathBuf,

    /// Wurzel des `/sys/class/drm`-Lesebereichs für
    /// [`harw_dod_gpu::GpuSensor`]. Nur zum Testen zu überschreiben.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_GPU_ROOT)]
    pub gpu_root: PathBuf,

    /// Wurzel des `/sys/fs/cgroup`-Lesebereichs für
    /// [`harw_dod_cgroup::CgroupSensor`]. Nur zum Testen zu überschreiben.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_CGROUP_ROOT)]
    pub cgroup_root: PathBuf,

    /// Pfad des `SOCK_SEQPACKET`-Empfangssockets für die privilegierten
    /// Sonden (`harw-probe-fs`, `harw-probe-bpf`). Ohne Angabe:
    /// `<home>/sentinel.sock` (siehe [`DEFAULT_SOCKET_FILE_NAME`]).
    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Taktung der Sammelschleife in Sekunden zwischen zwei Poll-Runden.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_INTERVAL_SECS)]
    pub interval_secs: u64,

    /// Genau eine Poll-Runde ausführen, einen Beleg einfrieren und beenden,
    /// statt endlos zu laufen. Für Diagnose und Tests am realen Host.
    #[arg(long, default_value_t = false)]
    pub once: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{Cli, LogLevel};
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_log_level_as_filter_directive_matches_every_variant() {
        assert_eq!(LogLevel::Trace.as_filter_directive(), "trace");
        assert_eq!(LogLevel::Debug.as_filter_directive(), "debug");
        assert_eq!(LogLevel::Info.as_filter_directive(), "info");
        assert_eq!(LogLevel::Warn.as_filter_directive(), "warn");
        assert_eq!(LogLevel::Error.as_filter_directive(), "error");
    }

    #[test]
    fn test_log_defaults_to_info_when_omitted() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(cli.log, LogLevel::Info);
        assert!(!cli.once);
        assert_eq!(cli.interval_secs, super::DEFAULT_INTERVAL_SECS);
        Ok(())
    }

    #[test]
    fn test_log_accepts_every_documented_value() -> TestResult {
        for value in ["trace", "debug", "info", "warn", "error"] {
            let cli = Cli::try_parse_from(["harw-sentinel", "--log", value]).map_err(|err| {
                TestError::Unexpected(format!("expected {value} to parse, got {err}"))
            })?;
            assert_eq!(cli.log.as_filter_directive(), value);
        }
        Ok(())
    }

    #[test]
    fn test_log_rejects_invalid_value_as_a_hard_error() {
        let result = Cli::try_parse_from(["harw-sentinel", "--log", "verbose"]);
        assert!(
            result.is_err(),
            "an unrecognised --log value must be a parse error, not a silent default"
        );
    }

    #[test]
    fn test_workspace_root_defaults_to_current_directory_marker() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(cli.workspace_root, std::path::PathBuf::from("."));
        Ok(())
    }

    #[test]
    fn test_socket_override_is_honoured() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel", "--socket", "/tmp/custom.sock"])
            .map_err(ctx("valid override"))?;
        assert_eq!(
            cli.socket,
            Some(std::path::PathBuf::from("/tmp/custom.sock"))
        );
        Ok(())
    }

    #[test]
    fn test_proc_root_defaults_to_the_declared_constant() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(
            cli.proc_root,
            std::path::PathBuf::from(super::DEFAULT_PROC_ROOT)
        );
        Ok(())
    }

    #[test]
    fn test_thermal_root_defaults_to_the_declared_constant() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(
            cli.thermal_root,
            std::path::PathBuf::from(super::DEFAULT_THERMAL_ROOT)
        );
        Ok(())
    }

    #[test]
    fn test_blockio_root_defaults_to_the_declared_constant() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(
            cli.blockio_root,
            std::path::PathBuf::from(super::DEFAULT_BLOCKIO_ROOT)
        );
        Ok(())
    }

    #[test]
    fn test_blockio_root_override_is_honoured() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel", "--blockio-root", "/tmp/fixture-block"])
            .map_err(ctx("valid override"))?;
        assert_eq!(
            cli.blockio_root,
            std::path::PathBuf::from("/tmp/fixture-block")
        );
        Ok(())
    }

    #[test]
    fn test_gpu_root_defaults_to_the_declared_constant() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(
            cli.gpu_root,
            std::path::PathBuf::from(super::DEFAULT_GPU_ROOT)
        );
        Ok(())
    }

    #[test]
    fn test_gpu_root_override_is_honoured() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel", "--gpu-root", "/tmp/fixture-drm"])
            .map_err(ctx("valid override"))?;
        assert_eq!(cli.gpu_root, std::path::PathBuf::from("/tmp/fixture-drm"));
        Ok(())
    }

    #[test]
    fn test_cgroup_root_defaults_to_the_declared_constant() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(
            cli.cgroup_root,
            std::path::PathBuf::from(super::DEFAULT_CGROUP_ROOT)
        );
        Ok(())
    }

    #[test]
    fn test_cgroup_root_override_is_honoured() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel", "--cgroup-root", "/tmp/fixture-cgroup"])
            .map_err(ctx("valid override"))?;
        assert_eq!(
            cli.cgroup_root,
            std::path::PathBuf::from("/tmp/fixture-cgroup")
        );
        Ok(())
    }
}
