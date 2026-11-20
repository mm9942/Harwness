//! Beobachtungsbereich der Sonde: Flag vor `[probe_fs]` vor Vorgabe
//! (PL-100 WP1.6).
//!
//! # Verantwortungsbereich
//! [`resolve_watch_plan`] entscheidet, welche Wurzeln und Home-Einträge
//! diese Sonde beobachtet, und [`config_from_loaded`], ob die
//! DoD-Konfiguration dafür taugt. Früher stand in der Unit ein Platzhalter
//! (`--scope-root /srv/harw/watched`), der nichts Sicherheitsrelevantes
//! abdeckte und die Sonde deshalb aus `harw-dod.target` heraushielt.
//!
//! # Die Vorgaben decken die Orte von `persistence-write`
//! - [`DEFAULT_SCOPE_ROOTS`]: `/etc` (systemd, cron, `ld.so.preload`,
//!   Profile, ssh, udev, motd, Umgebung …), die Spools von cron und at sowie
//!   die systemd- und udev-Verzeichnisse unter `/usr`, `/usr/local` und
//!   `/lib` — jede Schreibänderung darunter zählt.
//! - [`DEFAULT_HOME_ROOTS`]: `/home`, `/var/home` (ostree-Hosts, wo `/home`
//!   ein Symlink ist und der Kernel den aufgelösten Pfad meldet), `/root`,
//!   `/var/roothome`.
//! - [`DEFAULT_HOME_ENTRIES`]: `.ssh`, die systemd-User-Units, Autostart,
//!   `environment.d`, `.pam_environment` und die Shell-Startdateien — relativ
//!   zu jedem Home-Verzeichnis, ohne einen einzigen Benutzernamen
//!   (`harw_dod_fsmon::filter::WatchFilter`).
//!
//! Die ausgelieferten Dateien `harw-dod/config.toml` und
//! `harw-dod/packaging/config.example.toml` zeigen genau diese Werte
//! auskommentiert; ein Test hier hält beide gleich.
//!
//! # Rangfolge
//! - Wurzeln: ist `--scope-root` oder `--home-root` angegeben, gelten genau
//!   die Flags (eine nicht angegebene Liste ist leer). Sonst je Liste
//!   `[probe_fs]`, sonst die Vorgabe.
//! - Home-Einträge: `--home-entry`, sonst `[probe_fs] home_entries`, sonst
//!   die Vorgabe.
//!
//! # Konfiguration: fail closed
//! Ein ausdrücklich angegebener `--config` muss lesbar, vertrauenswürdig und
//! gültig sein. Ohne `--config` gilt `/etc/harw-dod/config.toml`; nur wenn
//! diese Datei fehlt, gelten die Vorgaben. Jeder andere Fehler bricht den
//! Start ab — eine privilegierte Sonde läuft nie mit einer Datei, die sie
//! nicht prüfen konnte.
//!
//! # Exportierte Elemente
//! [`DEFAULT_SCOPE_ROOTS`], [`DEFAULT_HOME_ROOTS`], [`DEFAULT_HOME_ENTRIES`],
//! [`WatchSource`], [`WatchPlan`], [`resolve_watch_plan`],
//! [`config_from_loaded`].
//!
//! # Nebenläufigkeit
//! Reine Funktionen und Werttypen.
//!
//! # Fehler
//! [`ProbeError::EmptyWatchScope`], [`ProbeError::Config`].
//!
//! # Examples
//! ```rust,ignore
//! use crate::watch::resolve_watch_plan;
//!
//! let plan = resolve_watch_plan(&[], &[], &[], None)?;
//! assert!(plan.filter.read_scope().allows(std::path::Path::new("/etc/hosts")));
//! # Ok::<(), crate::error::ProbeError>(())
//! ```

use std::path::{Path, PathBuf};

use harw_dod_config::{Config, ConfigError, ProbeFsSettings};
use harw_dod_fsmon::WatchFilter;

use crate::error::ProbeError;

/// Vorgabe von `[probe_fs] scope_roots`: Verzeichnisse, unter denen jede
/// Schreibänderung zählt (siehe Moduldoku).
pub const DEFAULT_SCOPE_ROOTS: [&str; 11] = [
    "/etc",
    "/var/spool/cron",
    "/var/spool/at",
    "/usr/lib/systemd",
    "/usr/local/lib/systemd",
    "/lib/systemd",
    "/usr/share/systemd",
    "/usr/local/share/systemd",
    "/usr/lib/udev/rules.d",
    "/usr/local/lib/udev/rules.d",
    "/lib/udev/rules.d",
];

/// Vorgabe von `[probe_fs] home_roots` (siehe Moduldoku).
pub const DEFAULT_HOME_ROOTS: [&str; 4] = ["/home", "/var/home", "/root", "/var/roothome"];

/// Vorgabe von `[probe_fs] home_entries`: die Home-Spalte der Regel
/// `persistence-write` plus `.ssh` (`authorized_keys`, `rc`).
pub const DEFAULT_HOME_ENTRIES: [&str; 16] = [
    ".ssh",
    ".config/systemd",
    ".local/share/systemd",
    ".config/autostart",
    ".config/environment.d",
    ".pam_environment",
    ".bashrc",
    ".bash_aliases",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
];

/// Woher eine Liste des Beobachtungsbereichs stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchSource {
    /// Ein Kommandozeilenflag.
    CommandLine,
    /// `[probe_fs]` der DoD-Konfiguration.
    DodConfig,
    /// Die eingebaute Vorgabe.
    BuiltIn,
}

impl WatchSource {
    /// Stabiler Bezeichner für das Journal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CommandLine => "command-line",
            Self::DodConfig => "dod-config",
            Self::BuiltIn => "built-in",
        }
    }
}

/// Der aufgelöste Beobachtungsbereich samt Herkunft je Liste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchPlan {
    /// Wurzeln und Home-Einträge.
    pub filter: WatchFilter,
    /// Herkunft von `scope_roots`.
    pub scope_roots_source: WatchSource,
    /// Herkunft von `home_roots`.
    pub home_roots_source: WatchSource,
    /// Herkunft von `home_entries`.
    pub home_entries_source: WatchSource,
}

/// Löst den Beobachtungsbereich auf (siehe Moduldoku, „Rangfolge").
///
/// # Arguments
/// - `cli_scope_roots`, `cli_home_roots`, `cli_home_entries`
///   (`&[PathBuf]`): die Flags (leer, wenn nicht angegeben; ihre Form hat
///   `clap` bereits geprüft).
/// - `config` (`Option<&ProbeFsSettings>`): `[probe_fs]` der geladenen
///   Konfiguration; `None` ohne Konfiguration.
///
/// # Returns
/// Den [`WatchPlan`].
///
/// # Errors
/// [`ProbeError::EmptyWatchScope`], wenn danach weder eine `scope_root`
/// noch eine `home_root` bleibt — eine Sonde ohne Wurzel wäre still blind.
pub fn resolve_watch_plan(
    cli_scope_roots: &[PathBuf],
    cli_home_roots: &[PathBuf],
    cli_home_entries: &[PathBuf],
    config: Option<&ProbeFsSettings>,
) -> Result<WatchPlan, ProbeError> {
    let roots_from_cli = !cli_scope_roots.is_empty() || !cli_home_roots.is_empty();
    let (scope_roots, scope_roots_source) = if roots_from_cli {
        (cli_scope_roots.to_vec(), WatchSource::CommandLine)
    } else {
        pick(
            config.and_then(ProbeFsSettings::scope_roots),
            &DEFAULT_SCOPE_ROOTS,
        )
    };
    let (home_roots, home_roots_source) = if roots_from_cli {
        (cli_home_roots.to_vec(), WatchSource::CommandLine)
    } else {
        pick(
            config.and_then(ProbeFsSettings::home_roots),
            &DEFAULT_HOME_ROOTS,
        )
    };
    let (home_entries, home_entries_source) = if cli_home_entries.is_empty() {
        pick(
            config.and_then(ProbeFsSettings::home_entries),
            &DEFAULT_HOME_ENTRIES,
        )
    } else {
        (cli_home_entries.to_vec(), WatchSource::CommandLine)
    };
    let filter = WatchFilter::new(scope_roots, home_roots, home_entries);
    if filter.has_no_roots() {
        return Err(ProbeError::EmptyWatchScope);
    }
    Ok(WatchPlan {
        filter,
        scope_roots_source,
        home_roots_source,
        home_entries_source,
    })
}

// Konfigurierte Liste, sonst die Vorgabe.
fn pick(configured: Option<&[PathBuf]>, defaults: &[&str]) -> (Vec<PathBuf>, WatchSource) {
    match configured {
        Some(paths) => (paths.to_vec(), WatchSource::DodConfig),
        None => (
            defaults.iter().map(PathBuf::from).collect(),
            WatchSource::BuiltIn,
        ),
    }
}

/// Bewertet ein Ladeergebnis der DoD-Konfiguration (siehe Moduldoku,
/// „Konfiguration: fail closed").
///
/// # Arguments
/// - `loaded` (`Result<Config, ConfigError>`): das Ergebnis von
///   `load_config` (`explicit`) bzw. `load_system_config`.
/// - `explicit` (`bool`): ob der Pfad über `--config` verlangt wurde.
///
/// # Returns
/// `Some(config)` bei Erfolg; `None` nur, wenn die Systemkonfiguration
/// fehlt (dann gelten die Vorgaben).
///
/// # Errors
/// [`ProbeError::Config`] für jeden anderen Fehler, auch eine fehlende
/// ausdrücklich angegebene Datei.
pub fn config_from_loaded(
    loaded: Result<Config, ConfigError>,
    explicit: bool,
) -> Result<Option<Config>, ProbeError> {
    match loaded {
        Ok(config) => Ok(Some(config)),
        Err(ConfigError::Io(error))
            if !explicit && error.kind() == std::io::ErrorKind::NotFound =>
        {
            tracing::warn!(
                "DoD system configuration not found; watching the built-in persistence locations"
            );
            Ok(None)
        }
        Err(error) => Err(ProbeError::Config(error)),
    }
}

/// Lädt die DoD-Konfiguration für diesen Start.
///
/// # Arguments
/// - `explicit` (`Option<&Path>`): `--config`, falls angegeben.
///
/// # Returns
/// Siehe [`config_from_loaded`].
///
/// # Errors
/// Siehe [`config_from_loaded`].
pub fn load_probe_config(explicit: Option<&Path>) -> Result<Option<Config>, ProbeError> {
    match explicit {
        Some(path) => config_from_loaded(harw_dod_config::load_config(path), true),
        None => config_from_loaded(harw_dod_config::load_system_config(), false),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use harw_dod_config::{Config, ConfigError};

    use super::{
        DEFAULT_HOME_ENTRIES, DEFAULT_HOME_ROOTS, DEFAULT_SCOPE_ROOTS, WatchSource,
        config_from_loaded, resolve_watch_plan,
    };
    use crate::error::ProbeError;
    use crate::test_support::{TestError, TestResult, ctx};

    const HOST_PROFILE: &str = "schema_version = 1\nmode = \"observe\"\n\n\
                                [profiles.host]\nscope = \"host\"\nsensors = [\"exec\"]\n";

    fn paths(raw: &[&str]) -> Vec<PathBuf> {
        raw.iter().map(PathBuf::from).collect()
    }

    /// Ohne Flag und ohne Konfiguration gelten die Vorgaben; sie decken die
    /// Orte von `persistence-write` ohne Benutzernamen.
    #[test]
    fn test_builtin_plan_covers_the_persistence_locations() -> TestResult {
        let plan = resolve_watch_plan(&[], &[], &[], None).map_err(ctx("built-in plan"))?;
        assert_eq!(plan.scope_roots_source, WatchSource::BuiltIn);
        assert_eq!(plan.home_roots_source, WatchSource::BuiltIn);
        assert_eq!(plan.home_entries_source, WatchSource::BuiltIn);
        for path in [
            "/etc/systemd/system/evil.service",
            "/etc/cron.d/evil",
            "/etc/crontab",
            "/etc/ld.so.preload",
            "/etc/profile.d/evil.sh",
            "/var/spool/cron/crontabs/root",
            "/usr/lib/systemd/system/evil.service",
            "/home/alice/.ssh/authorized_keys",
            "/home/alice/.config/systemd/user/evil.service",
            "/home/alice/.config/autostart/evil.desktop",
            "/home/alice/.bashrc",
            "/var/home/alice/.zshrc",
            "/root/.ssh/authorized_keys",
            "/root/.profile",
            "/var/roothome/.bash_profile",
        ] {
            assert!(plan.filter.admits(Path::new(path)), "{path}");
        }
        for path in ["/home/alice/.cache/x", "/var/log/syslog", "/tmp/x"] {
            assert!(!plan.filter.admits(Path::new(path)), "{path}");
        }
        Ok(())
    }

    /// Ein Wurzelflag ersetzt beide Wurzellisten; die Einträge bleiben bei
    /// der Vorgabe, solange kein `--home-entry` angegeben ist.
    #[test]
    fn test_a_root_flag_replaces_both_root_lists() -> TestResult {
        let config = Config::from_toml(&format!(
            "{HOST_PROFILE}\n[probe_fs]\nscope_roots = [\"/opt/a\"]\nhome_roots = [\"/srv/homes\"]\n"
        ))
        .map_err(ctx("config"))?;
        let plan = resolve_watch_plan(&paths(&["/srv/data"]), &[], &[], Some(config.probe_fs()))
            .map_err(ctx("cli plan"))?;
        assert_eq!(plan.filter.scope_roots(), paths(&["/srv/data"]).as_slice());
        assert!(plan.filter.home_roots().is_empty());
        assert_eq!(plan.scope_roots_source, WatchSource::CommandLine);
        assert_eq!(plan.home_roots_source, WatchSource::CommandLine);
        assert_eq!(plan.home_entries_source, WatchSource::BuiltIn);
        assert_eq!(plan.filter.home_entries().len(), DEFAULT_HOME_ENTRIES.len());
        Ok(())
    }

    /// Ohne Flag gilt `[probe_fs]` je Schlüssel, ein fehlender Schlüssel
    /// fällt auf die Vorgabe; `--home-entry` gewinnt über die Datei.
    #[test]
    fn test_config_keys_apply_per_key_and_the_entry_flag_wins() -> TestResult {
        let config = Config::from_toml(&format!(
            "{HOST_PROFILE}\n[probe_fs]\nscope_roots = [\"/opt/a\"]\nhome_entries = [\".x\"]\n"
        ))
        .map_err(ctx("config"))?;
        let plan = resolve_watch_plan(&[], &[], &[], Some(config.probe_fs()))
            .map_err(ctx("config plan"))?;
        assert_eq!(plan.filter.scope_roots(), paths(&["/opt/a"]).as_slice());
        assert_eq!(plan.scope_roots_source, WatchSource::DodConfig);
        assert_eq!(
            plan.filter.home_roots(),
            paths(&DEFAULT_HOME_ROOTS).as_slice()
        );
        assert_eq!(plan.home_roots_source, WatchSource::BuiltIn);
        assert_eq!(plan.filter.home_entries(), paths(&[".x"]).as_slice());
        assert_eq!(plan.home_entries_source, WatchSource::DodConfig);

        let flagged = resolve_watch_plan(&[], &[], &paths(&[".y"]), Some(config.probe_fs()))
            .map_err(ctx("entry flag"))?;
        assert_eq!(flagged.filter.home_entries(), paths(&[".y"]).as_slice());
        assert_eq!(flagged.home_entries_source, WatchSource::CommandLine);
        Ok(())
    }

    /// Bleibt keine Wurzel, startet die Sonde nicht (still blind wäre
    /// schlimmer).
    #[test]
    fn test_a_plan_without_roots_is_refused() -> TestResult {
        let config = Config::from_toml(&format!(
            "{HOST_PROFILE}\n[probe_fs]\nscope_roots = []\nhome_roots = []\n"
        ))
        .map_err(ctx("config"))?;
        let Err(error) = resolve_watch_plan(&[], &[], &[], Some(config.probe_fs())) else {
            return Err(TestError::Unexpected(
                "an empty plan must be refused".to_owned(),
            ));
        };
        assert!(matches!(error, ProbeError::EmptyWatchScope));
        Ok(())
    }

    /// Nur eine fehlende Systemkonfiguration fällt auf die Vorgaben; eine
    /// fehlende ausdrückliche Datei und jeder andere Fehler brechen ab.
    #[test]
    fn test_config_loading_fails_closed_except_for_a_missing_system_file() -> TestResult {
        let missing = || ConfigError::Io(std::io::ErrorKind::NotFound.into());
        assert!(
            config_from_loaded(Err(missing()), false)
                .map_err(ctx("missing system config"))?
                .is_none()
        );
        assert!(matches!(
            config_from_loaded(Err(missing()), true),
            Err(ProbeError::Config(_))
        ));
        assert!(matches!(
            config_from_loaded(
                Err(ConfigError::UntrustedSystemPath { path: "/etc" }),
                false
            ),
            Err(ProbeError::Config(_))
        ));
        assert!(matches!(
            config_from_loaded(
                Err(ConfigError::Io(std::io::ErrorKind::PermissionDenied.into())),
                false
            ),
            Err(ProbeError::Config(_))
        ));
        let valid = Config::from_toml(HOST_PROFILE).map_err(ctx("valid config"))?;
        assert!(
            config_from_loaded(Ok(valid), true)
                .map_err(ctx("valid"))?
                .is_some()
        );
        Ok(())
    }

    /// Die ausgelieferten Dateien zeigen auskommentiert genau die Vorgaben
    /// dieser Sonde.
    #[test]
    fn test_shipped_config_files_document_exactly_the_built_in_defaults() -> TestResult {
        for (name, input) in [
            ("harw-dod/config.toml", include_str!("../../../config.toml")),
            (
                "harw-dod/packaging/config.example.toml",
                include_str!("../../../packaging/config.example.toml"),
            ),
        ] {
            let enabled = input
                .replace("\n# [probe_fs]\n", "\n[probe_fs]\n")
                .replace("\n# scope_roots = ", "\nscope_roots = ")
                .replace("\n# home_roots = ", "\nhome_roots = ")
                .replace("\n# home_entries = ", "\nhome_entries = ");
            let config = Config::from_toml(&enabled).map_err(ctx(name))?;
            let settings = config.probe_fs();
            assert_eq!(
                settings.scope_roots(),
                Some(paths(&DEFAULT_SCOPE_ROOTS).as_slice()),
                "{name}"
            );
            assert_eq!(
                settings.home_roots(),
                Some(paths(&DEFAULT_HOME_ROOTS).as_slice()),
                "{name}"
            );
            assert_eq!(
                settings.home_entries(),
                Some(paths(&DEFAULT_HOME_ENTRIES).as_slice()),
                "{name}"
            );
        }
        Ok(())
    }

    /// Jede Vorgabe besteht dieselbe Grammatik wie Datei und Flag.
    #[test]
    fn test_builtin_defaults_pass_the_config_grammar() {
        for root in DEFAULT_SCOPE_ROOTS.iter().chain(DEFAULT_HOME_ROOTS.iter()) {
            assert!(harw_dod_config::is_valid_probe_fs_root(root), "{root}");
        }
        for entry in DEFAULT_HOME_ENTRIES {
            assert!(
                harw_dod_config::is_valid_probe_fs_home_entry(entry),
                "{entry}"
            );
        }
    }
}
