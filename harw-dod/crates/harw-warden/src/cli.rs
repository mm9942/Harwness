//! Kommandozeilen-Grammatik von `harw-warden`.
//!
//! # Verantwortungsbereich
//! [`Cli`] bündelt die Startparameter dieses Binaries: das verbindliche
//! `--log`-Flag (siehe [`LogLevel`]-Moduldoku für die Begründung — identisch
//! zu `harw-sentinel`/`harw-probe-fs`), `--cgroup-root`, das
//! Wurzelverzeichnis für [`harw_dod_warden::CgroupV2Executor`] (Produktion:
//! `/sys/fs/cgroup`), `--policy`, die Warden-eigene, root-eigene Policy
//! (Vorgabe `/etc/harw-dod/warden.toml`, PL-100 WP 3.1), und `--state-dir`,
//! das StateDirectory für Nonce-Ledger, Aktions-Ledger und Hold-Timer
//! (Vorgabe `/var/lib/harw-warden`, systemd `StateDirectory=harw-warden`),
//! und `--holds-dir`, das Indexverzeichnis der Eindämmungsmarker für harw
//! (Vorgabe `/run/harw-warden/holds`, tmpfiles; Owner-Review 4505015305).
//! Unterkommandos ([`Command`]): `completions` und `keygen` (neuer
//! Proof-v2-Schlüssel, [`KeygenArgs`]). Der
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

/// Vorgabepfad der Warden-Policy (root-eigen, nicht gruppen- oder
/// weltschreibbar; siehe `harw_dod_warden::config`).
pub const DEFAULT_POLICY_PATH: &str = harw_dod_warden::config::DEFAULT_POLICY_PATH;

/// Vorgabepfad des StateDirectory (systemd `StateDirectory=harw-warden`).
pub const DEFAULT_STATE_DIR: &str = harw_dod_warden::state::DEFAULT_STATE_DIR;

/// Vorgabe-Schlüsselordner für `keygen` (`proof_key_dir` der Vorgabe-Policy).
pub const DEFAULT_KEY_DIR: &str = harw_dod_warden::config::DEFAULT_PROOF_KEY_DIR;

/// Vorgabepfad des Indexverzeichnisses der Eindämmungsmarker (gleich
/// `harw_tool_job::CONTAINMENT_HOLDS_DIR`, dem Pfad, den harw liest).
pub const DEFAULT_HOLDS_DIR: &str = harw_dod_warden::DEFAULT_HOLDS_DIR;

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
    pub command: Option<Command>,

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

    /// Warden-eigene Policy (`warden.toml`, Schema 1). Muss root gehören und
    /// darf weder gruppen- noch weltschreibbar sein.
    #[arg(long, value_name = "FILE", default_value = DEFAULT_POLICY_PATH)]
    pub policy: PathBuf,

    /// StateDirectory für Nonce-Ledger, Aktions-Ledger und Hold-Timer (muss
    /// existieren, Modus `0700`).
    #[arg(long, value_name = "DIR", default_value = DEFAULT_STATE_DIR)]
    pub state_dir: PathBuf,

    /// Indexverzeichnis der Eindämmungsmarker für harw (muss existieren,
    /// dem Warden-Nutzer gehören und darf weder gruppen- noch weltschreibbar
    /// sein). Nur zum Testen zu überschreiben: harw liest immer
    /// [`DEFAULT_HOLDS_DIR`].
    #[arg(long, value_name = "DIR", default_value = DEFAULT_HOLDS_DIR)]
    pub holds_dir: PathBuf,
}

/// Unterkommandos von `harw-warden`; ohne Unterkommando läuft der
/// Durchsetzer am systemd-Socket.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum Command {
    /// Generate or install shell completions for this binary.
    #[command(alias = "completion")]
    Completions(harw_completions::CompletionsArgs),
    /// Erzeugt einen neuen Proof-v2-Schlüssel `<key-dir>/<key-id>.key`
    /// (Modus 0600, als root ausführen). Gibt nur die Key-ID aus, nie
    /// Schlüsselmaterial.
    Keygen(KeygenArgs),
}

/// Argumente von `harw-warden keygen` (PL-100 WP 3.1, „Schlüssel“).
#[derive(Debug, Clone, clap::Args)]
pub struct KeygenArgs {
    /// Key-ID des neuen Schlüssels (ASCII-Buchstaben, Ziffern und `._-`;
    /// zugleich Dateiname `<key-id>.key`).
    #[arg(long, value_name = "ID")]
    pub key_id: String,

    /// Schlüsselordner (`proof_key_dir` aus `warden.toml`); muss root
    /// gehören und darf weder gruppen- noch weltschreibbar sein.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_KEY_DIR)]
    pub key_dir: PathBuf,
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{
        Cli, Command, DEFAULT_CGROUP_ROOT, DEFAULT_HOLDS_DIR, DEFAULT_KEY_DIR, DEFAULT_POLICY_PATH,
        DEFAULT_STATE_DIR, LogLevel,
    };
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
        assert_eq!(
            cli.policy,
            std::path::PathBuf::from("/etc/harw-dod/warden.toml")
        );
        assert_eq!(cli.policy, std::path::PathBuf::from(DEFAULT_POLICY_PATH));
        assert_eq!(
            cli.state_dir,
            std::path::PathBuf::from("/var/lib/harw-warden")
        );
        assert_eq!(cli.state_dir, std::path::PathBuf::from(DEFAULT_STATE_DIR));
        // Derselbe Pfad, den harw (`harw_tool_job::CONTAINMENT_HOLDS_DIR`) liest.
        assert_eq!(
            cli.holds_dir,
            std::path::PathBuf::from("/run/harw-warden/holds")
        );
        assert_eq!(cli.holds_dir, std::path::PathBuf::from(DEFAULT_HOLDS_DIR));
        Ok(())
    }

    #[test]
    fn test_holds_dir_override_is_honoured() -> TestResult {
        let cli = Cli::try_parse_from(["harw-warden", "--holds-dir", "/tmp/fixture/holds"])
            .map_err(ctx("valid override"))?;
        assert_eq!(
            cli.holds_dir,
            std::path::PathBuf::from("/tmp/fixture/holds")
        );
        Ok(())
    }

    #[test]
    fn test_policy_and_state_dir_overrides_are_honoured() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-warden",
            "--policy",
            "/tmp/fixture/warden.toml",
            "--state-dir",
            "/tmp/fixture/state",
        ])
        .map_err(ctx("valid overrides"))?;
        assert_eq!(
            cli.policy,
            std::path::PathBuf::from("/tmp/fixture/warden.toml")
        );
        assert_eq!(
            cli.state_dir,
            std::path::PathBuf::from("/tmp/fixture/state")
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
    fn test_keygen_subcommand_parses_with_default_key_dir() -> TestResult {
        let cli = Cli::try_parse_from(["harw-warden", "keygen", "--key-id", "k-2026"])
            .map_err(ctx("keygen parses"))?;
        let Some(Command::Keygen(args)) = cli.command else {
            return Err(crate::test_support::TestError::Unexpected(
                "expected the keygen subcommand".to_owned(),
            ));
        };
        assert_eq!(args.key_id, "k-2026");
        assert_eq!(args.key_dir, std::path::PathBuf::from(DEFAULT_KEY_DIR));
        assert_eq!(
            args.key_dir,
            std::path::PathBuf::from("/etc/harw-dod/warden-keys")
        );
        assert!(Cli::try_parse_from(["harw-warden", "keygen"]).is_err());
        let completions = Cli::try_parse_from(["harw-warden", "completions", "bash"])
            .map_err(ctx("completions parses"))?;
        assert!(matches!(completions.command, Some(Command::Completions(_))));
        Ok(())
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
