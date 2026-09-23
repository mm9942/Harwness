//! Kommandozeilenvertrag der privilegierten BPF-Probe.
//!
//! Profil, Sensorwahl, CIDRs und Scope werden nie als lose Flags akzeptiert:
//! sie kommen ausschliesslich aus `harw-dod-config`. `--config` ist nur der
//! explizite administrative/Test-Override des selben vertrauenswuerdigen
//! Laders; ohne Flag wird `/etc/harw-dod/config.toml` verwendet.
//!
//! Optionaler Unterbefehl `completions` (`harw_completions::CompletionsSubcommand`).
//! `subcommand_negates_reqs` hebt die Pflichtangaben für diesen Pfad auf;
//! deshalb ist `sentinel_socket` ein `Option<PathBuf>` mit `required = true`
//! (ein nacktes `PathBuf` würde im abgeleiteten `FromArgMatches` auch beim
//! Unterbefehl scheitern). Ohne Unterbefehl erzwingt `clap` die Angabe.

use std::path::PathBuf;

use clap::Parser;

pub const DEFAULT_SENSOR_ID_PROCMON: &str = "probe-bpf-procmon-0";
pub const DEFAULT_SENSOR_ID_FLOW: &str = "probe-bpf-flow-0";
pub const DEFAULT_CGROUP_ROOT: &str = "/sys/fs/cgroup";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
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

#[derive(Debug, Parser)]
#[command(
    name = "harw-probe-bpf",
    bin_name = "harw-probe-bpf",
    version,
    about = "Push-only eBPF probe bound to the trusted DoD observation profile.",
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[arg(long, value_enum, default_value = "info")]
    pub log: LogLevel,

    /// Trusted configuration source. Relative paths are rejected by
    /// `harw_dod_config::load_config`; this flag never enables home/repo
    /// discovery.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Sentinel socket. `Option` only because of the `completions`
    /// subcommand; always set when no subcommand is given.
    #[arg(long, value_name = "PATH", required = true)]
    pub sentinel_socket: Option<PathBuf>,

    #[arg(long = "sensor-id-procmon", value_name = "ID", default_value = DEFAULT_SENSOR_ID_PROCMON)]
    pub sensor_id_procmon: String,

    #[arg(long = "sensor-id-flow", value_name = "ID", default_value = DEFAULT_SENSOR_ID_FLOW)]
    pub sensor_id_flow: String,

    /// Root of the cgroup-v2 hierarchy. It exists only for an administrative
    /// test mount; the profile paths remain mount-relative.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_CGROUP_ROOT)]
    pub cgroup_root: PathBuf,

    #[arg(long, value_name = "PATH")]
    pub exec_program_path: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    pub exit_program_path: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    pub tcp_v4_program_path: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    pub tcp_v6_program_path: Option<PathBuf>,

    /// Optional subcommand; without it the binary runs normally.
    #[command(subcommand)]
    pub command: Option<harw_completions::CompletionsSubcommand>,
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{
        Cli, DEFAULT_CGROUP_ROOT, DEFAULT_SENSOR_ID_FLOW, DEFAULT_SENSOR_ID_PROCMON, LogLevel,
    };
    use crate::test_support::{TestResult, ctx};

    fn minimal_args() -> [&'static str; 3] {
        [
            "harw-probe-bpf",
            "--sentinel-socket",
            "/run/harw-dod/sentinel.sock",
        ]
    }

    #[test]
    fn defaults_to_the_system_config_and_has_no_profile_or_cidr_flag() -> TestResult {
        let cli = Cli::try_parse_from(minimal_args()).map_err(ctx("minimal arguments parse"))?;
        assert_eq!(cli.log, LogLevel::Info);
        assert!(cli.config.is_none());
        assert_eq!(cli.sensor_id_procmon, DEFAULT_SENSOR_ID_PROCMON);
        assert_eq!(cli.sensor_id_flow, DEFAULT_SENSOR_ID_FLOW);
        assert_eq!(
            cli.cgroup_root,
            std::path::PathBuf::from(DEFAULT_CGROUP_ROOT)
        );
        Ok(())
    }

    #[test]
    fn config_and_all_contract_object_paths_are_explicit() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-probe-bpf",
            "--sentinel-socket",
            "/run/harw-dod/sentinel.sock",
            "--config",
            "/etc/harw-dod/config.toml",
            "--exec-program-path",
            "/usr/local/lib/harw-dod/bpf/exec.bpf.o",
            "--exit-program-path",
            "/usr/local/lib/harw-dod/bpf/exit.bpf.o",
            "--tcp-v4-program-path",
            "/usr/local/lib/harw-dod/bpf/tcp-v4.bpf.o",
            "--tcp-v6-program-path",
            "/usr/local/lib/harw-dod/bpf/tcp-v6.bpf.o",
        ])
        .map_err(ctx("all explicit runtime paths parse"))?;
        assert_eq!(
            cli.config,
            Some(std::path::PathBuf::from("/etc/harw-dod/config.toml"))
        );
        assert!(cli.exec_program_path.is_some());
        assert!(cli.exit_program_path.is_some());
        assert!(cli.tcp_v4_program_path.is_some());
        assert!(cli.tcp_v6_program_path.is_some());
        Ok(())
    }

    #[test]
    fn missing_sentinel_socket_is_rejected() {
        assert!(Cli::try_parse_from(["harw-probe-bpf"]).is_err());
    }
}
