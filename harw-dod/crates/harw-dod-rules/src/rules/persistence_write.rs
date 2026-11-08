//! `PersistenceWriteRule`: ein Schreibzugriff auf einen Ort, über den sich
//! Code dauerhaft einnistet (PL-100 WP1.6).
//!
//! # Verantwortungsbereich
//! Prüft jedes `EventKind::FileWrite`-Ereignis: liegt der geschriebene Pfad
//! (lexikalisch normalisiert, siehe `path_class`) an einem
//! Persistenz-Ort, entsteht ein Befund. Orte:
//!
//! | Klasse | System | Je Benutzer (Home-Verzeichnis, siehe unten) |
//! |---|---|---|
//! | systemd-Unit | jede Kombination aus Wurzel `/etc/systemd`, `/etc/xdg/systemd`, `/usr/lib/systemd`, `/usr/local/lib/systemd`, `/lib/systemd`, `/usr/share/systemd`, `/usr/local/share/systemd` und Unterverzeichnis `system`, `user`, `system.control`, `user.control`, `system.attached`, `system-generators`, `user-generators` | `~/.config/systemd/user`, `~/.config/systemd/user.control`, `~/.local/share/systemd/user` |
//! | cron | `/etc/crontab`, `/etc/anacrontab`, `/etc/cron.{d,hourly,daily,weekly,monthly}`, `/var/spool/cron` | — |
//! | at-Job | `/var/spool/at` | — |
//! | SysV-Init | `/etc/rc.local`, `/etc/rc.d`, `/etc/init.d`, `/etc/rc{0..6,S}.d` (über `systemd-sysv-generator` weiterhin ausgeführt) | — |
//! | udev-Regel | `/etc/udev/rules.d`, `/usr/lib/udev/rules.d`, `/lib/udev/rules.d`, `/usr/local/lib/udev/rules.d` (`RUN+=`) | — |
//! | update-motd | `/etc/update-motd.d` (bei jedem Login als root ausgeführt) | — |
//! | Shell-Profil | `/etc/profile`, `/etc/profile.d`, `/etc/bash.bashrc`, `/etc/bashrc`, `/etc/zshrc`, `/etc/zprofile`, `/etc/zshenv`, `/etc/zsh` | `~/.bashrc`, `~/.bash_aliases` (von Debians/Ubuntus `.bashrc` eingelesen), `~/.bash_profile`, `~/.bash_login`, `~/.bash_logout`, `~/.profile`, `~/.zshrc`, `~/.zprofile`, `~/.zshenv`, `~/.zlogin` |
//! | Login-Umgebung | `/etc/environment`, `/etc/environment.d` (`LD_PRELOAD` u. a. für jede Sitzung) | `~/.config/environment.d`, `~/.pam_environment` |
//! | Autostart | `/etc/xdg/autostart` | `~/.config/autostart` |
//! | Loader-Preload | `/etc/ld.so.preload` | — |
//! | Loader-Konfiguration | `/etc/ld.so.conf`, `/etc/ld.so.conf.d` (Suchpfad für Bibliotheken) | — |
//! | SSH | `/etc/ssh/authorized_keys/…` und **jeder** absolute Pfad, der auf `.ssh/authorized_keys` oder `.ssh/authorized_keys2` endet (auch Dienstkonten wie `/var/lib/git`) | (in der Spalte links enthalten) |
//! | SSH-rc | `/etc/ssh/sshrc` und **jeder** absolute Pfad, der auf `.ssh/rc` endet (bei jedem SSH-Login ausgeführt) | (in der Spalte links enthalten) |
//! | sshd-Konfiguration | `/etc/ssh/sshd_config`, `/etc/ssh/sshd_config.d` (`AuthorizedKeysFile`, `ForceCommand`, `AuthorizedKeysCommand`) | — |
//!
//! Home-Verzeichnisse sind `/home/<name>`, `/root`, auf ostree-Hosts
//! (Fedora Silverblue/Kinoite u. a., wo `/home` und `/root` Symlinks sind und
//! der Sensor den aufgelösten Pfad meldet) `/var/home/<name>` und
//! `/var/roothome`, sowie jedes über
//! [`crate::rule::RuleConfig::with_home_dir`] konfigurierte Verzeichnis
//! (verschachtelte oder AD-Layouts wie `/home/<domäne>/<name>`).
//!
//! Laufzeit-Units unter `/run/systemd` überleben keinen Neustart und zählen
//! nicht.
//!
//! # Schwere und Folgeschritte
//! Immer [`Severity::High`], `FindingKind::RuleTriggered`,
//! [`Hardness::Observed`]: die Routing-Tabelle (README §3.3) eskaliert
//! `persistence-write` an einen Menschen; Dateien werden nie angefasst.
//!
//! # Grenzen
//! - Ein relativer Pfad hat keinen bekannten Ort und wird nicht bewertet.
//! - Symlinks werden nicht aufgelöst (kein I/O in Regeln).
//! - Ein Schreiben über eine temporäre Datei mit anschließendem Umbenennen
//!   trifft eine Einzeldatei wie `~/.bashrc` nur, wenn der Sensor das
//!   Umbenennen als Schreibzugriff auf das Ziel meldet.
//!
//! # Zeitstempel und Zusammenfassung
//! Ereigniszeit, nie [`crate::rule::RuleContext::now`]. Der Pfad ist
//! angreiferkontrolliert und erscheint nur escapt und gekürzt; dazu UID und
//! `comm` des Akteurs, falls bekannt (keine pid, siehe `path_class`).
//!
//! # Nebenläufigkeit
//! Zustandsloser Unit-Struct: `Send + Sync`.
//!
//! # Fehler
//! Keine.
//!
//! # Examples
//! ```rust
//! use harw_authority::NetworkScope;
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::PersistenceWriteRule;
//! use harw_dod_signals::{EventKind, SecurityEvent};
//! use harw_types::SensorId;
//!
//! let write = |path: &str| SecurityEvent {
//!     sensor: SensorId::from_str("fsmon-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: None,
//!     kind: EventKind::FileWrite { path: path.to_owned() },
//! };
//! let scope = NetworkScope::empty();
//! let events = vec![write("/var/log/syslog"), write("/home/alice/.ssh/authorized_keys")];
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &events,
//!     baselines: &[],
//!     network_scope: &scope,
//!     listener_history: None,
//!     rule_config: None,
//! };
//! let findings = PersistenceWriteRule.evaluate(&ctx);
//! assert_eq!(findings.len(), 1);
//! ```

use harw_dod_signals::{EventKind, Hardness, Severity};

use crate::finding::{Finding, FindingKind, Raw};
use crate::rule::{Rule, RuleConfig, RuleContext};
use crate::rules::path_class::{actor_suffix, home_relatives, normalized_components, summary_path};

/// Meldet einen Schreibzugriff auf einen Persistenz-Ort.
///
/// # Description
/// Siehe Moduldoku für Orte, Schwere und Grenzen.
///
/// # Errors
/// Keine eigenen Fehler; siehe [`crate::rule`]-Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistenceWriteRule;

impl Rule for PersistenceWriteRule {
    /// # Returns
    /// `"persistence-write"`.
    fn id(&self) -> &'static str {
        "persistence-write"
    }

    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): siehe [`crate::rule::RuleContext`];
    ///   zusätzliche Home-Verzeichnisse kommen aus `ctx.rule_config`.
    ///
    /// # Returns
    /// Je `FileWrite`-Ereignis auf einen Persistenz-Ort einen Befund,
    /// gestempelt mit dessen `observed_at`.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        ctx.events
            .iter()
            .filter_map(|event| {
                let EventKind::FileWrite { path } = &event.kind else {
                    return None;
                };
                let class = classify(path, ctx.rule_config)?;
                Some(Finding::raw(
                    self.id(),
                    FindingKind::RuleTriggered,
                    Severity::High,
                    Hardness::Observed,
                    format!(
                        "write to {} persistence location: {}{}",
                        class.label(),
                        summary_path(path),
                        actor_suffix(event.actor.as_ref()),
                    ),
                    // Ereigniszeit, nicht `ctx.now`.
                    event.observed_at,
                ))
            })
            .collect()
    }
}

// Die Klassen der Persistenz-Orte, siehe Tabelle in der Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistenceClass {
    // systemd-Units und -Generatoren (System und Benutzer).
    SystemdUnit,
    // cron/anacron.
    Cron,
    // at-Jobs (`/var/spool/at`).
    AtJob,
    // SysV-Init-Skripte und `rc.local`.
    SysvInit,
    // udev-Regeln (`RUN+=`).
    UdevRule,
    // `/etc/update-motd.d`.
    UpdateMotd,
    // Shell-Start- und Profildateien.
    ShellProfile,
    // Umgebung jeder Login-Sitzung (`/etc/environment`, `environment.d`).
    LoginEnvironment,
    // XDG-Autostart.
    Autostart,
    // `/etc/ld.so.preload`.
    LoaderPreload,
    // `/etc/ld.so.conf` und `/etc/ld.so.conf.d`.
    LoaderConfig,
    // `authorized_keys` eines Benutzers.
    SshAuthorizedKeys,
    // `~/.ssh/rc` bzw. `/etc/ssh/sshrc`.
    SshRc,
    // `/etc/ssh/sshd_config` und `/etc/ssh/sshd_config.d`.
    SshdConfig,
}

impl PersistenceClass {
    // Feste Kennung für die Zusammenfassung.
    fn label(self) -> &'static str {
        match self {
            Self::SystemdUnit => "systemd unit",
            Self::Cron => "cron",
            Self::AtJob => "at job",
            Self::SysvInit => "sysv init script",
            Self::UdevRule => "udev rule",
            Self::UpdateMotd => "update-motd script",
            Self::ShellProfile => "shell profile",
            Self::LoginEnvironment => "login environment",
            Self::Autostart => "autostart",
            Self::LoaderPreload => "dynamic loader preload",
            Self::LoaderConfig => "dynamic loader configuration",
            Self::SshAuthorizedKeys => "ssh authorized_keys",
            Self::SshRc => "ssh rc",
            Self::SshdConfig => "sshd configuration",
        }
    }
}

// Wurzeln der systemd-Suchpfade (systemd.unit(5)), kombiniert mit
// `SYSTEMD_UNIT_DIRS`. Nicht jede Kombination existiert; ein Schreibzugriff
// auf eine nicht vorgesehene wäre trotzdem verdächtig.
const SYSTEMD_ROOTS: &[&[&str]] = &[
    &["etc", "systemd"],
    &["etc", "xdg", "systemd"],
    &["usr", "lib", "systemd"],
    &["usr", "local", "lib", "systemd"],
    &["lib", "systemd"],
    &["usr", "share", "systemd"],
    &["usr", "local", "share", "systemd"],
];

// Unterverzeichnisse einer `SYSTEMD_ROOTS`-Wurzel mit Units oder
// Generatoren (Pfad selbst oder darunter).
const SYSTEMD_UNIT_DIRS: &[&str] = &[
    "system",
    "user",
    "system.control",
    "user.control",
    "system.attached",
    "system-generators",
    "user-generators",
];

// Systemweite Persistenz-Orte außer systemd (Pfad selbst oder darunter).
const SYSTEM_LOCATIONS: &[(&[&str], PersistenceClass)] = &[
    (&["etc", "crontab"], PersistenceClass::Cron),
    (&["etc", "anacrontab"], PersistenceClass::Cron),
    (&["etc", "cron.d"], PersistenceClass::Cron),
    (&["etc", "cron.hourly"], PersistenceClass::Cron),
    (&["etc", "cron.daily"], PersistenceClass::Cron),
    (&["etc", "cron.weekly"], PersistenceClass::Cron),
    (&["etc", "cron.monthly"], PersistenceClass::Cron),
    (&["var", "spool", "cron"], PersistenceClass::Cron),
    (&["var", "spool", "at"], PersistenceClass::AtJob),
    (&["etc", "rc.local"], PersistenceClass::SysvInit),
    (&["etc", "rc.d"], PersistenceClass::SysvInit),
    (&["etc", "init.d"], PersistenceClass::SysvInit),
    (&["etc", "rc0.d"], PersistenceClass::SysvInit),
    (&["etc", "rc1.d"], PersistenceClass::SysvInit),
    (&["etc", "rc2.d"], PersistenceClass::SysvInit),
    (&["etc", "rc3.d"], PersistenceClass::SysvInit),
    (&["etc", "rc4.d"], PersistenceClass::SysvInit),
    (&["etc", "rc5.d"], PersistenceClass::SysvInit),
    (&["etc", "rc6.d"], PersistenceClass::SysvInit),
    (&["etc", "rcS.d"], PersistenceClass::SysvInit),
    (&["etc", "udev", "rules.d"], PersistenceClass::UdevRule),
    (
        &["usr", "lib", "udev", "rules.d"],
        PersistenceClass::UdevRule,
    ),
    (&["lib", "udev", "rules.d"], PersistenceClass::UdevRule),
    (
        &["usr", "local", "lib", "udev", "rules.d"],
        PersistenceClass::UdevRule,
    ),
    (&["etc", "update-motd.d"], PersistenceClass::UpdateMotd),
    (&["etc", "profile"], PersistenceClass::ShellProfile),
    (&["etc", "profile.d"], PersistenceClass::ShellProfile),
    (&["etc", "bash.bashrc"], PersistenceClass::ShellProfile),
    (&["etc", "bashrc"], PersistenceClass::ShellProfile),
    (&["etc", "zshrc"], PersistenceClass::ShellProfile),
    (&["etc", "zprofile"], PersistenceClass::ShellProfile),
    (&["etc", "zshenv"], PersistenceClass::ShellProfile),
    (&["etc", "zsh"], PersistenceClass::ShellProfile),
    (&["etc", "environment"], PersistenceClass::LoginEnvironment),
    (
        &["etc", "environment.d"],
        PersistenceClass::LoginEnvironment,
    ),
    (&["etc", "xdg", "autostart"], PersistenceClass::Autostart),
    (&["etc", "ld.so.preload"], PersistenceClass::LoaderPreload),
    (&["etc", "ld.so.conf"], PersistenceClass::LoaderConfig),
    (&["etc", "ld.so.conf.d"], PersistenceClass::LoaderConfig),
    // `AuthorizedKeysFile /etc/ssh/authorized_keys/%u`.
    (
        &["etc", "ssh", "authorized_keys"],
        PersistenceClass::SshAuthorizedKeys,
    ),
    (&["etc", "ssh", "sshrc"], PersistenceClass::SshRc),
    (&["etc", "ssh", "sshd_config"], PersistenceClass::SshdConfig),
    (
        &["etc", "ssh", "sshd_config.d"],
        PersistenceClass::SshdConfig,
    ),
];

// Persistenz-Orte relativ zu einem Home-Verzeichnis (Pfad selbst oder
// darunter).
const HOME_LOCATIONS: &[(&[&str], PersistenceClass)] = &[
    (
        &[".config", "systemd", "user"],
        PersistenceClass::SystemdUnit,
    ),
    (
        &[".config", "systemd", "user.control"],
        PersistenceClass::SystemdUnit,
    ),
    (
        &[".local", "share", "systemd", "user"],
        PersistenceClass::SystemdUnit,
    ),
    (&[".bashrc"], PersistenceClass::ShellProfile),
    (&[".bash_aliases"], PersistenceClass::ShellProfile),
    (&[".bash_profile"], PersistenceClass::ShellProfile),
    (&[".bash_login"], PersistenceClass::ShellProfile),
    (&[".bash_logout"], PersistenceClass::ShellProfile),
    (&[".profile"], PersistenceClass::ShellProfile),
    (&[".zshrc"], PersistenceClass::ShellProfile),
    (&[".zprofile"], PersistenceClass::ShellProfile),
    (&[".zshenv"], PersistenceClass::ShellProfile),
    (&[".zlogin"], PersistenceClass::ShellProfile),
    (&[".config", "autostart"], PersistenceClass::Autostart),
    (
        &[".config", "environment.d"],
        PersistenceClass::LoginEnvironment,
    ),
    (&[".pam_environment"], PersistenceClass::LoginEnvironment),
];

// Dateinamen von `authorized_keys` unterhalb eines `.ssh`-Verzeichnisses.
const AUTHORIZED_KEYS_FILES: &[&str] = &["authorized_keys", "authorized_keys2"];

// Ordnet einen geschriebenen Pfad einer Persistenz-Klasse zu; `None` für
// jeden anderen Ort und für relative Pfade. `config` liefert zusätzliche
// Home-Verzeichnisse.
fn classify(path: &str, config: Option<&RuleConfig>) -> Option<PersistenceClass> {
    let components = normalized_components(path)?;
    if is_systemd_unit_location(&components) {
        return Some(PersistenceClass::SystemdUnit);
    }
    // `…/.ssh/authorized_keys[2]` und `…/.ssh/rc` an jedem Ort: das Home
    // eines Dienstkontos liegt oft außerhalb von `/home` (z. B.
    // `/var/lib/git`).
    match components.as_slice() {
        [.., ".ssh", file] if AUTHORIZED_KEYS_FILES.contains(file) => {
            return Some(PersistenceClass::SshAuthorizedKeys);
        }
        [.., ".ssh", "rc"] => return Some(PersistenceClass::SshRc),
        _ => {}
    }
    let system = SYSTEM_LOCATIONS
        .iter()
        .find(|(prefix, _)| components.starts_with(prefix))
        .map(|(_, class)| *class);
    if system.is_some() {
        return system;
    }
    home_relatives(&components, config)
        .into_iter()
        .find_map(|rest| {
            HOME_LOCATIONS
                .iter()
                .find(|(prefix, _)| rest.starts_with(prefix))
                .map(|(_, class)| *class)
        })
}

// Ob `components` an oder unter `<SYSTEMD_ROOTS>/<SYSTEMD_UNIT_DIRS>` liegt.
fn is_systemd_unit_location(components: &[&str]) -> bool {
    SYSTEMD_ROOTS.iter().any(|root| {
        components
            .strip_prefix(*root)
            .and_then(|rest| rest.first())
            .is_some_and(|dir| SYSTEMD_UNIT_DIRS.contains(dir))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx, missing};
    use harw_authority::NetworkScope;
    use harw_dod_signals::{Actor, ProcessContext, SecurityEvent};
    use harw_types::{ContentDigest, SensorId};
    use jiff::Timestamp;

    fn write_by(path: &str, actor: Option<Actor>) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("fsmon-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor,
            kind: EventKind::FileWrite {
                path: path.to_owned(),
            },
        }
    }

    fn write(path: &str) -> SecurityEvent {
        write_by(path, None)
    }

    fn ctx_with<'a>(events: &'a [SecurityEvent], scope: &'a NetworkScope) -> RuleContext<'a> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events,
            baselines: &[],
            network_scope: scope,
            listener_history: None,
            rule_config: None,
        }
    }

    fn summaries(events: &[SecurityEvent]) -> Vec<String> {
        let scope = NetworkScope::empty();
        PersistenceWriteRule
            .evaluate(&ctx_with(events, &scope))
            .iter()
            .map(|finding| finding.summary().to_owned())
            .collect()
    }

    #[test]
    fn test_every_location_class_triggers() {
        let cases = [
            ("/etc/systemd/system/evil.service", "systemd unit"),
            (
                "/etc/systemd/system/multi-user.target.wants/evil.service",
                "systemd unit",
            ),
            ("/usr/lib/systemd/user/evil.service", "systemd unit"),
            ("/lib/systemd/system/evil.service", "systemd unit"),
            ("/etc/systemd/system-generators/evil", "systemd unit"),
            (
                "/home/alice/.config/systemd/user/evil.service",
                "systemd unit",
            ),
            (
                "/root/.local/share/systemd/user/evil.service",
                "systemd unit",
            ),
            ("/etc/crontab", "cron"),
            ("/etc/cron.d/evil", "cron"),
            ("/etc/cron.daily/evil", "cron"),
            ("/var/spool/cron/crontabs/alice", "cron"),
            ("/etc/profile", "shell profile"),
            ("/etc/profile.d/evil.sh", "shell profile"),
            ("/etc/bash.bashrc", "shell profile"),
            ("/etc/zsh/zshrc", "shell profile"),
            ("/home/alice/.bashrc", "shell profile"),
            ("/root/.bashrc", "shell profile"),
            ("/home/alice/.profile", "shell profile"),
            ("/home/alice/.zshrc", "shell profile"),
            ("/etc/xdg/autostart/evil.desktop", "autostart"),
            ("/home/alice/.config/autostart/evil.desktop", "autostart"),
            ("/etc/ld.so.preload", "dynamic loader preload"),
            ("/home/alice/.ssh/authorized_keys", "ssh authorized_keys"),
            ("/root/.ssh/authorized_keys2", "ssh authorized_keys"),
        ];
        for (path, label) in cases {
            let found = summaries(&[write(path)]);
            assert_eq!(
                found,
                vec![format!("write to {label} persistence location: {path}")],
                "{path}"
            );
        }
    }

    /// Review-Befund: fehlende Standardorte des systemd-Suchpfads und
    /// `authorized_keys` von Dienstkonten außerhalb von `/home`.
    #[test]
    fn test_additional_standard_locations_trigger() {
        let cases = [
            ("/usr/local/lib/systemd/system/evil.service", "systemd unit"),
            ("/usr/local/lib/systemd/user/evil.service", "systemd unit"),
            ("/etc/systemd/system.control/evil.service", "systemd unit"),
            ("/etc/systemd/system.attached/evil.service", "systemd unit"),
            ("/usr/share/systemd/user/evil.service", "systemd unit"),
            ("/usr/local/share/systemd/user/evil.service", "systemd unit"),
            ("/etc/xdg/systemd/user/evil.service", "systemd unit"),
            (
                "/usr/local/lib/systemd/system-generators/evil",
                "systemd unit",
            ),
            (
                "/home/alice/.config/systemd/user.control/evil.service",
                "systemd unit",
            ),
            ("/var/lib/git/.ssh/authorized_keys", "ssh authorized_keys"),
            ("/srv/backup/.ssh/authorized_keys2", "ssh authorized_keys"),
            ("/etc/ssh/authorized_keys/alice", "ssh authorized_keys"),
        ];
        for (path, label) in cases {
            let found = summaries(&[write(path)]);
            assert_eq!(
                found,
                vec![format!("write to {label} persistence location: {path}")],
                "{path}"
            );
        }
    }

    /// Review-Befund (Runde 2): weitere bekannte Linux-Persistenz-Orte
    /// (SysV-Init, udev, update-motd, Loader-Konfiguration, Login-Umgebung,
    /// at, sshd-Konfiguration, `~/.ssh/rc`, `~/.bash_aliases`).
    #[test]
    fn test_further_well_known_locations_trigger() {
        let cases = [
            ("/etc/rc.local", "sysv init script"),
            ("/etc/rc.d/rc.local", "sysv init script"),
            ("/etc/init.d/evil", "sysv init script"),
            ("/etc/rc2.d/S99evil", "sysv init script"),
            ("/etc/rcS.d/S01evil", "sysv init script"),
            ("/etc/udev/rules.d/99-evil.rules", "udev rule"),
            ("/usr/lib/udev/rules.d/99-evil.rules", "udev rule"),
            ("/lib/udev/rules.d/99-evil.rules", "udev rule"),
            ("/etc/update-motd.d/99-evil", "update-motd script"),
            ("/etc/ld.so.conf", "dynamic loader configuration"),
            (
                "/etc/ld.so.conf.d/evil.conf",
                "dynamic loader configuration",
            ),
            ("/etc/environment", "login environment"),
            ("/etc/environment.d/90-evil.conf", "login environment"),
            (
                "/home/alice/.config/environment.d/90-evil.conf",
                "login environment",
            ),
            ("/home/alice/.pam_environment", "login environment"),
            ("/var/spool/at/a0001", "at job"),
            ("/etc/ssh/sshd_config", "sshd configuration"),
            ("/etc/ssh/sshd_config.d/x.conf", "sshd configuration"),
            ("/etc/ssh/sshrc", "ssh rc"),
            ("/home/alice/.ssh/rc", "ssh rc"),
            ("/var/lib/git/.ssh/rc", "ssh rc"),
            ("/home/alice/.bash_aliases", "shell profile"),
        ];
        for (path, label) in cases {
            let found = summaries(&[write(path)]);
            assert_eq!(
                found,
                vec![format!("write to {label} persistence location: {path}")],
                "{path}"
            );
        }
    }

    /// Review-Befund (Runde 2): auf ostree-Hosts meldet der Sensor die
    /// aufgelösten Pfade unter `/var/home` und `/var/roothome`; jede
    /// Home-Klasse muss dort ebenso greifen.
    #[test]
    fn test_ostree_home_locations_trigger() {
        let cases = [
            ("/var/home/alice/.bashrc", "shell profile"),
            ("/var/home/alice/.profile", "shell profile"),
            (
                "/var/home/alice/.config/systemd/user/evil.service",
                "systemd unit",
            ),
            (
                "/var/home/alice/.local/share/systemd/user/evil.service",
                "systemd unit",
            ),
            ("/var/roothome/.config/autostart/x.desktop", "autostart"),
            ("/var/roothome/.bashrc", "shell profile"),
            (
                "/var/home/alice/.ssh/authorized_keys",
                "ssh authorized_keys",
            ),
        ];
        for (path, label) in cases {
            let found = summaries(&[write(path)]);
            assert_eq!(
                found,
                vec![format!("write to {label} persistence location: {path}")],
                "{path}"
            );
        }
        assert!(summaries(&[write("/var/home/.bashrc")]).is_empty());
    }

    /// Konfigurierte Home-Verzeichnisse (verschachtelt/AD) greifen nur mit
    /// der `rule_config`.
    #[test]
    fn test_configured_home_dir_locations_trigger() -> TestResult {
        let config = RuleConfig::new()
            .with_home_dir(std::path::Path::new("/home/corp/alice"))
            .map_err(ctx("AD-Home"))?;
        let scope = NetworkScope::empty();
        let events = vec![write("/home/corp/alice/.bashrc")];
        let mut rule_ctx = ctx_with(&events, &scope);
        assert!(PersistenceWriteRule.evaluate(&rule_ctx).is_empty());
        rule_ctx.rule_config = Some(&config);
        let found: Vec<String> = PersistenceWriteRule
            .evaluate(&rule_ctx)
            .iter()
            .map(|finding| finding.summary().to_owned())
            .collect();
        assert_eq!(
            found,
            vec![
                "write to shell profile persistence location: /home/corp/alice/.bashrc".to_owned()
            ]
        );
        Ok(())
    }

    #[test]
    fn test_other_locations_do_not_trigger() {
        for path in [
            "/var/log/syslog",
            "/etc/passwd",
            "/etc/systemd/system.conf",
            "/etc/profile.bak",
            "/home/alice/.bashrc.bak",
            "/home/alice/notes/.bashrc",
            "/home/alice/.ssh/known_hosts",
            "/home/.bashrc",
            "/tmp/.bashrc",
            "/run/systemd/transient/x.service",
            "/usr/lib/systemd/systemd-logind",
            "/usr/share/systemd/kbd-model-map",
            "/var/lib/git/.ssh/authorized_keys.bak",
            "/var/lib/git/authorized_keys",
            "/etc/ssh/ssh_config",
            "/etc/ssh/ssh_host_ed25519_key.pub",
            "/etc/rc.localx",
            "/etc/environmentx",
            "/run/udev/rules.d/99-x.rules",
            "/home/alice/.ssh/rc.bak",
            "/var/spool/atx/a",
        ] {
            assert!(summaries(&[write(path)]).is_empty(), "{path}");
        }
    }

    #[test]
    fn test_relative_paths_are_not_judged() {
        assert!(summaries(&[write(".bashrc"), write("etc/ld.so.preload")]).is_empty());
    }

    /// `..`, doppelte Trenner und `/proc/self/root/` umgehen die Regel nicht.
    #[test]
    fn test_path_tricks_are_normalized() {
        for path in [
            "/etc/./cron.d/../cron.d/evil",
            "//etc//ld.so.preload",
            "/proc/self/root/etc/ld.so.preload",
            "/proc/self/root/../etc/ld.so.preload",
            "/home/alice/x/../.bashrc",
        ] {
            assert_eq!(summaries(&[write(path)]).len(), 1, "{path}");
        }
    }

    #[test]
    fn test_finding_shape_with_process_context() -> TestResult {
        let actor = Actor {
            uid: 0,
            auid: Some(1000),
            cgroup: None,
            process: ProcessContext::from_pid(77).map(|process| ProcessContext {
                comm: Some("sh".to_owned()),
                ..process
            }),
        };
        let scope = NetworkScope::empty();
        let events = vec![write_by("/etc/cron.d/evil", Some(actor))];
        let findings = PersistenceWriteRule.evaluate(&ctx_with(&events, &scope));

        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert_eq!(finding.rule_id(), "persistence-write");
        assert_eq!(finding.kind(), FindingKind::RuleTriggered);
        assert_eq!(finding.severity(), Severity::High);
        assert_eq!(finding.hardness(), Hardness::Observed);
        assert_eq!(
            finding.summary(),
            "write to cron persistence location: /etc/cron.d/evil (uid 0, comm 'sh')"
        );
        Ok(())
    }

    #[test]
    fn test_attacker_controlled_path_is_escaped() -> TestResult {
        let scope = NetworkScope::empty();
        let events = vec![write("/etc/cron.d/x\u{2066}\u{1b}[2J")];
        let findings = PersistenceWriteRule.evaluate(&ctx_with(&events, &scope));
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert!(!finding.summary().contains('\u{2066}'));
        assert!(!finding.summary().contains('\u{1b}'));
        assert!(finding.summary().contains("\\u{2066}\\u{001b}[2J"));
        Ok(())
    }

    #[test]
    fn test_finding_is_stamped_with_the_event_time_not_ctx_now() -> TestResult {
        let event_time: Timestamp = "2026-11-08T10:00:00Z"
            .parse()
            .map_err(ctx("Ereigniszeit"))?;
        let mut event = write("/root/.ssh/authorized_keys");
        event.observed_at = event_time;
        let events = vec![event];
        let scope = NetworkScope::empty();
        let mut rule_ctx = ctx_with(&events, &scope);
        rule_ctx.now = "2026-11-08T10:05:00Z"
            .parse()
            .map_err(ctx("Auswertungszeit"))?;

        let stamps: Vec<Timestamp> = PersistenceWriteRule
            .evaluate(&rule_ctx)
            .iter()
            .map(Finding::observed_at)
            .collect();
        assert_eq!(stamps, vec![event_time]);
        Ok(())
    }

    #[test]
    fn test_other_event_kinds_are_ignored() {
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("procmon-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::ProcessExec {
                path: "/etc/cron.d/evil".to_owned(),
                argv_digest: ContentDigest::of(b"argv"),
            },
        }];
        assert!(summaries(&events).is_empty());
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() {
        let scope = NetworkScope::empty();
        let events = vec![
            write("/etc/ld.so.preload"),
            write("/home/a/.zshrc"),
            write("/var/x"),
        ];
        let ctx = ctx_with(&events, &scope);
        assert_eq!(
            PersistenceWriteRule.evaluate(&ctx),
            PersistenceWriteRule.evaluate(&ctx)
        );
    }
}
