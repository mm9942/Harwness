//! Kommandozeilenvertrag der privilegierten BPF-Probe.
//!
//! Profil, Sensorwahl, CIDRs und Scope werden nie als lose Flags akzeptiert:
//! sie kommen ausschliesslich aus `harw-dod-config`. `--config` ist nur der
//! explizite administrative/Test-Override des selben vertrauenswuerdigen
//! Laders; ohne Flag wird `/etc/harw-dod/config.toml` verwendet.
//!
//! `--dev-allow-missing-bpf` ist ein reiner Entwicklungsschalter (nie in der
//! systemd-Unit): ein gewählter Sensor mit fehlendem oder ungültigem Objekt
//! degradiert dann, statt den Start mit Exit-Status 78 abzulehnen. `clap`
//! nimmt ihn nur zusammen mit `--bpf-manifest` an (`requires`): jedes
//! aufgelöste Objekt wird auch im Entwicklungsmodus gegen das Manifest
//! geprüft, ein Teilbau (z. B. nur die Prozess-Objekte) lädt also seine
//! vollständigen Sensoren, aber nie ungeprüfte Bytes. Ohne den Schalter gilt
//! die produktive Regel (siehe `main::verify_selected_objects`, Owner-Review
//! zu PR #168).
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
    /// test mount; the profile paths remain mount-relative. Read only for a
    /// cgroup-scoped profile: it must then be absolute, not `/` and without
    /// `..`, and with `include_descendants = true` it becomes the one
    /// list-only Landlock root of the descendant rescan.
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

    /// SHA-256 manifest of the BPF build (`manifest.json`, TOML despite the
    /// extension, written by `harw-dod/bpf/scripts/write-manifest.sh`).
    /// Production setting: when set, the manifest is always read and checked
    /// (even if no object resolves), every resolved object is verified
    /// against it before loading, and the file plus every directory above it
    /// must be root-owned and neither group- nor world-writable (see
    /// `bpf_manifest`). Every selected object must resolve: a missing, empty,
    /// truncated or non-ELF object refuses the start with exit status 78.
    /// Without this flag any resolved object is refused as well. No default
    /// and no discovery next to the objects.
    #[arg(long, value_name = "PATH")]
    pub bpf_manifest: Option<PathBuf>,

    /// Development only, never in production or in the systemd unit: a
    /// selected sensor whose objects are missing or invalid degrades (one
    /// `SensorDegraded`) instead of refusing the start with exit status 78.
    /// Requires `--bpf-manifest`: every object that does resolve (including
    /// the resolved sibling of a missing one) is still verified against the
    /// manifest, so this mode never loads unverified bytes; a partial build
    /// runs its complete sensors and degrades the rest. Logs one warning at
    /// startup.
    #[arg(long = "dev-allow-missing-bpf", requires = "bpf_manifest")]
    pub dev_allow_missing_bpf: bool,

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
    use crate::test_support::{TestError, TestResult, ctx};

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
            "--bpf-manifest",
            "/usr/local/lib/harw-dod/bpf/manifest.json",
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
        assert_eq!(
            cli.bpf_manifest,
            Some(std::path::PathBuf::from(
                "/usr/local/lib/harw-dod/bpf/manifest.json"
            ))
        );
        Ok(())
    }

    #[test]
    fn bpf_manifest_has_no_default() -> TestResult {
        // Kein Rückfall auf ein Manifest neben den Objekten: ohne Flag bleibt
        // es leer, und `run` verweigert dann das Laden aufgelöster Objekte.
        let cli = Cli::try_parse_from(minimal_args()).map_err(ctx("minimal arguments parse"))?;
        assert!(cli.bpf_manifest.is_none());
        Ok(())
    }

    #[test]
    fn dev_allow_missing_bpf_defaults_to_false() -> TestResult {
        let cli = Cli::try_parse_from(minimal_args()).map_err(ctx("minimal arguments parse"))?;
        assert!(!cli.dev_allow_missing_bpf);
        Ok(())
    }

    #[test]
    fn dev_allow_missing_bpf_without_bpf_manifest_is_refused() -> TestResult {
        // Ohne Manifest gäbe es keine Prüfung der aufgelösten Objekte.
        match Cli::try_parse_from([
            "harw-probe-bpf",
            "--sentinel-socket",
            "/run/harw-dod/sentinel.sock",
            "--dev-allow-missing-bpf",
        ]) {
            Err(err) if err.kind() == clap::error::ErrorKind::MissingRequiredArgument => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn dev_allow_missing_bpf_parses_together_with_bpf_manifest() -> TestResult {
        // In beiden Reihenfolgen; das Manifest bleibt gesetzt.
        let manifest = "/usr/local/lib/harw-dod/bpf/manifest.json";
        for args in [
            [
                "harw-probe-bpf",
                "--sentinel-socket",
                "/run/harw-dod/sentinel.sock",
                "--bpf-manifest",
                manifest,
                "--dev-allow-missing-bpf",
            ],
            [
                "harw-probe-bpf",
                "--sentinel-socket",
                "/run/harw-dod/sentinel.sock",
                "--dev-allow-missing-bpf",
                "--bpf-manifest",
                manifest,
            ],
        ] {
            let cli = Cli::try_parse_from(args)
                .map_err(ctx("the development flag parses with a manifest"))?;
            assert!(cli.dev_allow_missing_bpf);
            assert_eq!(
                cli.bpf_manifest.as_deref(),
                Some(std::path::Path::new(manifest))
            );
        }
        Ok(())
    }

    #[test]
    fn missing_sentinel_socket_is_rejected() {
        assert!(Cli::try_parse_from(["harw-probe-bpf"]).is_err());
    }
}
