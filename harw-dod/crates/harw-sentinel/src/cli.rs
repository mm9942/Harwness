//! Kommandozeilen-Grammatik von `harw-sentinel`.
//!
//! # Verantwortungsbereich
//! [`Cli`] bündelt jeden Startparameter dieses Binaries: das verbindliche
//! `--log`-Flag (siehe Moduldoku von [`LogLevel`] für die Abweichung von
//! `harw-cli`s Fehlerverhalten), Wurzelverzeichnisse für die sechs
//! eingebetteten Dateisystem-Sensoren (`--proc-root`, `--thermal-root`,
//! `--workspace-root`, `--blockio-root`, `--gpu-root`, `--cgroup-root`), den
//! Pfad des IPC-Empfangssockets, die Poll-Taktung, den expliziten
//! Konfigurationspfad (`--config`), die beiden optionalen Exporte
//! (`--findings-export`, `--health-export`), die Sonden-Konten der
//! `SO_PEERCRED`-Zulassung (`--probe-peer-user`, PL-100 WP0.5), den
//! Container-Schalter (`--container-sensor`, PL-100 WP1.8) und den
//! Diagnosemodus `--print-sandbox` (PL-100 WP0.7). Diese Datei parst
//! nur — sie kennt weder `Sentinel` noch einen Sensor.
//!
//! # Kein Arbeitsverzeichnis als Lesewurzel (PL-100 WP0.7)
//! `--workspace-root` hat **keine** Vorgabe mehr: früher stand sie auf `.`,
//! und weil die Unit kein `WorkingDirectory=` setzte, lag das
//! Arbeitsverzeichnis von systemd (`/`) als Leseregel in der
//! Landlock-Sandbox — die Lese-Sandbox umfasste damit das ganze
//! Dateisystem. Der Workspace-Sensor läuft jetzt nur, wenn
//! `--workspace-root` ausdrücklich und absolut angegeben ist.
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
use harw_types::HostId;

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
    about = "Harw-DoD Sentinel — unprivilegierte Sammelstelle: ruft Sensoren, führt den Degradationsautomaten, puffert.",
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
    /// [`harw_dod_workspace::WorkspaceDriftSensor`] beobachtet. Muss absolut
    /// sein. Ohne Angabe läuft kein Workspace-Sensor und keine
    /// Workspace-Leseregel entsteht (PL-100 WP0.7: nie das
    /// Arbeitsverzeichnis).
    #[arg(long, value_name = "DIR", value_parser = parse_absolute_path)]
    pub workspace_root: Option<PathBuf>,

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

    /// Höchstzahl rotierter Telemetrie-Dateien (`rotated-*.jsonl` samt
    /// `.blake3`-Beidatei) unter `<home>/telemetry`; die ältesten Paare
    /// werden nach einer Rotation entfernt. Ohne Angabe gilt das Limit der
    /// Retention-Klasse `telemetry_rotated` (`harw-retention`).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub telemetry_max_files: Option<u64>,

    /// Optionaler JSON-Lines-Export jedes zertifizierten Befundes für
    /// `harw-security-hub` und harw (siehe `crate::export`), z. B.
    /// `/var/lib/harw-dod/export/findings.jsonl` (die ausgelieferte Unit).
    /// Muss absolut sein. Ohne Angabe wird nichts exportiert.
    #[arg(long, value_name = "PATH", value_parser = parse_absolute_path)]
    pub findings_export: Option<PathBuf>,

    /// Optional health document, atomically rewritten every poll cycle
    /// (see `crate::health_export`). Must be absolute. Without it, the
    /// default is the sibling `health.json` of `--findings-export`; with
    /// neither flag nothing is written.
    #[arg(long, value_name = "PATH", value_parser = parse_absolute_path)]
    pub health_export: Option<PathBuf>,

    /// `host`-Feld jeder Exportzeile — muss `server.host` des Hubs
    /// entsprechen. Ohne Angabe: der Kernel-Hostname
    /// (`/proc/sys/kernel/hostname`). Nur mit `--findings-export` wirksam.
    #[arg(long, value_name = "ID", value_parser = parse_host_id)]
    pub findings_host: Option<HostId>,

    /// Anzahl aufbewahrter rotierter Exportdateien (`<pfad>.1` … `<pfad>.N`).
    /// Vorgabe 1. Der Export belegt höchstens `(N + 1) × 16 MiB`. Nur mit
    /// `--findings-export` wirksam; passt zur Retention-Klasse
    /// `sentinel_export` (`harw-retention`).
    #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=64))]
    pub findings_export_keep: u32,

    /// Abklingzeit des Befund-Throttles in Sekunden (PL-100 WP0.4): ein
    /// Befund mit demselben `dedup_key` (Regel, uid, Cgroup, grobes Subjekt)
    /// wird innerhalb dieser Zeit nur gezählt, nicht erneut gemeldet — außer
    /// bei stärkerer Härte oder höherer Schwere. `0` meldet jede
    /// Wiederholung, höchstens 604800 (7 Tage). Ohne Angabe gilt
    /// `[sentinel] finding_cooldown_secs` der DoD-Konfiguration, sonst die
    /// Vorgabe 600 (10 min; `crate::throttle::DEFAULT_COOLDOWN_SECS`) — das
    /// Flag gewinnt immer. Siehe `crate::throttle`.
    #[arg(
        long,
        value_name = "SECS",
        value_parser = clap::value_parser!(u64).range(0..=crate::throttle::MAX_COOLDOWN_SECS)
    )]
    pub finding_cooldown_secs: Option<u64>,

    /// Konto, dessen Prozesse Ereignisse über den IPC-Socket einreichen
    /// dürfen (`SO_PEERCRED`, PL-100 WP0.5): ein Kontoname (aufgelöst beim
    /// Start über `/etc/passwd`) oder eine numerische uid. Mehrfach
    /// angebbar; eine Angabe ersetzt die Konfiguration und die Vorgabe
    /// vollständig. uid `0` ist immer zugelassen. Ohne Angabe gilt
    /// `[sentinel] probe_peer_users` der DoD-Konfiguration, sonst die
    /// Sonden-Konten aus `deploy/sysusers.d/harw.conf`
    /// (`crate::peer_allow::DEFAULT_PROBE_PEER_USERS`).
    #[arg(
        long = "probe-peer-user",
        value_name = "USER|UID",
        value_parser = parse_peer_user
    )]
    pub probe_peer_users: Option<Vec<String>>,

    /// Schalter des Container-Sensors (PL-100 WP1.8): `auto` registriert je
    /// bekannter Anordnung (`libpod-*.scope` unter `machine.slice`,
    /// `docker-*.scope` unter `system.slice`) einen Sensor, auch ohne
    /// erkannte Laufzeit (ein fehlendes Verzeichnis ist auf cgroup v2 eine
    /// leere Messung), `on` ebenso, meldet aber das fehlende Verzeichnis einer
    /// beim Start nicht erkannten Laufzeit als beeinträchtigt, `off` keinen.
    /// Ohne cgroup v2 melden sich die Sensoren als beeinträchtigt. Ohne Angabe
    /// gilt `[sentinel] container_sensor` der DoD-Konfiguration, sonst `auto`
    /// — das Flag gewinnt immer. Siehe `crate::container_plan`.
    #[arg(long, value_enum, value_name = "MODE")]
    pub container_sensor: Option<ContainerSensorArg>,

    /// Gibt die wirksamen Landlock-Lese-/Schreibwurzeln, die abgewiesenen
    /// Wurzeln und die zugelassenen IPC-Peer-uids auf stdout aus und beendet
    /// sich, ohne Socket, Sink oder Landlock anzufassen (Host-Verifikation,
    /// PL-100 WP0.7). Wertet dieselben Flags aus wie der normale Start.
    #[arg(long, default_value_t = false)]
    pub print_sandbox: bool,
}

/// Werte von `--container-sensor` (dieselben wie `[sentinel]
/// container_sensor`, [`harw_dod_config::ContainerSensorMode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum ContainerSensorArg {
    /// Je bekannter Anordnung ein Sensor; fehlendes Verzeichnis leer.
    Auto,
    /// Je bekannter Anordnung ein Sensor; fehlendes Verzeichnis einer nicht
    /// erkannten Laufzeit beeinträchtigt.
    On,
    /// Kein Container-Sensor.
    Off,
}

impl From<ContainerSensorArg> for harw_dod_config::ContainerSensorMode {
    fn from(arg: ContainerSensorArg) -> Self {
        match arg {
            ContainerSensorArg::Auto => Self::Auto,
            ContainerSensorArg::On => Self::On,
            ContainerSensorArg::Off => Self::Off,
        }
    }
}

impl Cli {
    /// The effective health export path.
    ///
    /// # Returns
    /// `--health-export` when given; otherwise, when `--findings-export` is
    /// set, its sibling [`crate::health_export::DEFAULT_HEALTH_FILE_NAME`];
    /// otherwise `None` (no health export).
    #[must_use]
    pub fn resolved_health_export(&self) -> Option<PathBuf> {
        self.health_export.clone().or_else(|| {
            self.findings_export
                .as_deref()
                .and_then(crate::health_export::default_path_for)
        })
    }
}

/// `clap`-Wertparser für `--findings-export`/`--health-export`/
/// `--workspace-root`: nur absolute Pfade.
fn parse_absolute_path(raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("'{raw}' is not an absolute path"))
    }
}

/// Höchstlänge eines `--probe-peer-user`-Eintrags in Bytes.
const MAX_PEER_USER_LEN: usize = 256;

/// `clap`-Wertparser für `--probe-peer-user`: nicht leer, höchstens
/// [`MAX_PEER_USER_LEN`] Bytes, kein `:` (Feldtrenner von `passwd(5)`),
/// kein Leer- oder Steuerzeichen.
fn parse_peer_user(raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err("probe peer user must not be empty".to_owned());
    }
    if raw.len() > MAX_PEER_USER_LEN {
        return Err(format!(
            "probe peer user is longer than {MAX_PEER_USER_LEN} bytes"
        ));
    }
    if raw
        .chars()
        .any(|c| c == ':' || c.is_whitespace() || c.is_control())
    {
        return Err(
            "probe peer user must not contain ':', whitespace or control characters".to_owned(),
        );
    }
    Ok(raw.to_owned())
}

/// `clap`-Wertparser für `--findings-host`: nicht leer, keine Steuerzeichen.
fn parse_host_id(raw: &str) -> Result<HostId, String> {
    if raw.chars().any(char::is_control) {
        return Err("host id must not contain control characters".to_owned());
    }
    HostId::try_from_str(raw.trim()).map_err(|error| error.to_string())
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

    /// Ohne Flag bleibt der Wert offen: `main` nimmt dann die
    /// DoD-Konfiguration oder die Vorgabe (CLI gewinnt, PL-100).
    #[test]
    fn test_finding_cooldown_is_unset_without_the_flag_and_bounded() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(cli.finding_cooldown_secs, None);
        let zero = Cli::try_parse_from(["harw-sentinel", "--finding-cooldown-secs", "0"])
            .map_err(ctx("zero cooldown"))?;
        assert_eq!(zero.finding_cooldown_secs, Some(0));
        let most = Cli::try_parse_from(["harw-sentinel", "--finding-cooldown-secs", "604800"])
            .map_err(ctx("seven days"))?;
        assert_eq!(most.finding_cooldown_secs, Some(604_800));
        assert!(
            Cli::try_parse_from(["harw-sentinel", "--finding-cooldown-secs", "604801"]).is_err(),
            "more than seven days is rejected"
        );
        Ok(())
    }

    /// PL-100 WP1.8: ohne Flag bleibt der Container-Schalter offen (dann
    /// Konfiguration bzw. `auto`); jeder der drei Werte wird angenommen.
    #[test]
    fn test_container_sensor_is_unset_without_the_flag_and_takes_three_values() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(cli.container_sensor, None);
        for (raw, mode) in [
            ("auto", harw_dod_config::ContainerSensorMode::Auto),
            ("on", harw_dod_config::ContainerSensorMode::On),
            ("off", harw_dod_config::ContainerSensorMode::Off),
        ] {
            let cli = Cli::try_parse_from(["harw-sentinel", "--container-sensor", raw])
                .map_err(ctx("container mode"))?;
            let arg = cli
                .container_sensor
                .ok_or(TestError::Missing("container mode"))?;
            assert_eq!(harw_dod_config::ContainerSensorMode::from(arg), mode);
        }
        assert!(Cli::try_parse_from(["harw-sentinel", "--container-sensor", "yes"]).is_err());
        Ok(())
    }

    /// PL-100 WP0.7: ohne Flag gibt es keine Workspace-Wurzel — nie das
    /// Arbeitsverzeichnis.
    #[test]
    fn test_workspace_root_has_no_default() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(cli.workspace_root, None);
        Ok(())
    }

    #[test]
    fn test_workspace_root_accepts_an_absolute_path_only() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel", "--workspace-root", "/srv/harw/ws"])
            .map_err(ctx("absolute workspace root"))?;
        assert_eq!(
            cli.workspace_root,
            Some(std::path::PathBuf::from("/srv/harw/ws"))
        );
        for relative in [".", "ws", "./ws", ""] {
            assert!(
                Cli::try_parse_from(["harw-sentinel", "--workspace-root", relative]).is_err(),
                "{relative:?} must not parse as a workspace root"
            );
        }
        Ok(())
    }

    /// Ohne Flag bleibt die Liste offen (Konfiguration oder Vorgabe, siehe
    /// `main`); die Vorgabe selbst prüft `main`.
    #[test]
    fn test_probe_peer_users_are_unset_without_the_flag() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert_eq!(cli.probe_peer_users, None);
        Ok(())
    }

    #[test]
    fn test_explicit_probe_peer_users_replace_the_default() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-sentinel",
            "--probe-peer-user",
            "harw-probe-netlink",
            "--probe-peer-user",
            "991",
        ])
        .map_err(ctx("explicit peer users"))?;
        assert_eq!(
            cli.probe_peer_users,
            Some(vec!["harw-probe-netlink".to_owned(), "991".to_owned()])
        );
        Ok(())
    }

    #[test]
    fn test_probe_peer_user_rejects_empty_separator_and_control_characters() {
        let too_long = "a".repeat(257);
        for bad in ["", "a:b", "two words", "evil\n", too_long.as_str()] {
            assert!(
                Cli::try_parse_from(["harw-sentinel", "--probe-peer-user", bad]).is_err(),
                "{bad:?} must not parse as a probe peer user"
            );
        }
    }

    #[test]
    fn test_print_sandbox_is_off_by_default_and_parses() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert!(!cli.print_sandbox);
        let cli = Cli::try_parse_from(["harw-sentinel", "--print-sandbox"])
            .map_err(ctx("--print-sandbox"))?;
        assert!(cli.print_sandbox);
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
    fn test_findings_export_is_disabled_when_not_configured() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert!(cli.findings_export.is_none());
        assert!(cli.findings_host.is_none());
        Ok(())
    }

    #[test]
    fn test_findings_export_accepts_an_absolute_path_and_host() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-sentinel",
            "--findings-export",
            "/var/lib/harw-sentinel/findings.jsonl",
            "--findings-host",
            "host-a",
        ])
        .map_err(ctx("valid export flags"))?;
        assert_eq!(
            cli.findings_export,
            Some(std::path::PathBuf::from(
                "/var/lib/harw-sentinel/findings.jsonl"
            ))
        );
        assert_eq!(
            cli.findings_host.as_ref().map(harw_types::HostId::as_str),
            Some("host-a")
        );
        Ok(())
    }

    #[test]
    fn test_config_flag_accepts_the_shipped_system_path() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel", "--config", "/etc/harw-dod/config.toml"])
            .map_err(ctx("valid --config"))?;
        assert_eq!(
            cli.config,
            Some(std::path::PathBuf::from("/etc/harw-dod/config.toml"))
        );
        Ok(())
    }

    #[test]
    fn test_health_export_is_disabled_without_either_export_flag() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("no required args"))?;
        assert!(cli.health_export.is_none());
        assert!(cli.resolved_health_export().is_none());
        Ok(())
    }

    #[test]
    fn test_health_export_defaults_to_the_findings_export_sibling() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-sentinel",
            "--findings-export",
            "/var/lib/harw-dod/export/findings.jsonl",
        ])
        .map_err(ctx("valid export flag"))?;
        assert_eq!(
            cli.resolved_health_export(),
            Some(std::path::PathBuf::from(
                "/var/lib/harw-dod/export/health.json"
            ))
        );
        Ok(())
    }

    #[test]
    fn test_explicit_health_export_wins_over_the_default() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-sentinel",
            "--findings-export",
            "/var/lib/harw-dod/export/findings.jsonl",
            "--health-export",
            "/run/harw-dod/health.json",
        ])
        .map_err(ctx("valid export flags"))?;
        assert_eq!(
            cli.resolved_health_export(),
            Some(std::path::PathBuf::from("/run/harw-dod/health.json"))
        );
        let alone = Cli::try_parse_from(["harw-sentinel", "--health-export", "/tmp/h.json"])
            .map_err(ctx("health export alone"))?;
        assert_eq!(
            alone.resolved_health_export(),
            Some(std::path::PathBuf::from("/tmp/h.json"))
        );
        Ok(())
    }

    #[test]
    fn test_health_export_rejects_a_relative_path() {
        let result = Cli::try_parse_from(["harw-sentinel", "--health-export", "health.json"]);
        assert!(
            result.is_err(),
            "a relative health export path must not parse"
        );
    }

    #[test]
    fn test_findings_export_keep_defaults_to_one_and_is_bounded() {
        let cli = Cli::try_parse_from(["harw-sentinel"]);
        assert_eq!(cli.map(|c| c.findings_export_keep).ok(), Some(1));
        assert!(Cli::try_parse_from(["harw-sentinel", "--findings-export-keep", "0"]).is_err());
        assert!(Cli::try_parse_from(["harw-sentinel", "--findings-export-keep", "65"]).is_err());
        let ok = Cli::try_parse_from(["harw-sentinel", "--findings-export-keep", "5"]);
        assert_eq!(ok.map(|c| c.findings_export_keep).ok(), Some(5));
    }

    #[test]
    fn test_findings_export_rejects_a_relative_path() {
        let result = Cli::try_parse_from(["harw-sentinel", "--findings-export", "findings.jsonl"]);
        assert!(result.is_err(), "a relative export path must not parse");
    }

    #[test]
    fn test_findings_host_rejects_blank_and_control_characters() {
        for bad in ["", "   ", "host\nb"] {
            let result = Cli::try_parse_from(["harw-sentinel", "--findings-host", bad]);
            assert!(result.is_err(), "{bad:?} must not parse as a host id");
        }
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
