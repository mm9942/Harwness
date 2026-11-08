//! `ExecWritablePathRule`: ein Programmstart aus einem Ort, an den
//! jedermann oder ein gewöhnlicher Benutzer schreiben kann (PL-100 WP1.6).
//!
//! # Verantwortungsbereich
//! Prüft jedes `EventKind::ProcessExec`-Ereignis: liegt der ausgeführte Pfad
//! (lexikalisch normalisiert, siehe `path_class`) an einem beschreibbaren
//! Ort oder führt er über einen Datei-Deskriptor, entsteht ein Befund. Orte,
//! nach absteigender Schwere geprüft:
//!
//! | Ort | Beispiele | Schwere | Art |
//! |---|---|---|---|
//! | weltbeschreibbares Temp-Verzeichnis | `/tmp`, `/var/tmp` (`/usr/tmp`), `/dev/shm`, `/run/shm` (`/var/run/shm`), `/run/lock` (`/var/lock`) | [`Severity::High`] | `RuleTriggered` |
//! | Datei-Deskriptor | `/dev/fd/<n>`, `/proc/<pid>/fd/<n>` (`fexecve`, `memfd`) | [`Severity::Medium`] | `Anomaly` |
//! | Arbeitsverzeichnis eines Prozesses (ungelöster Link) | `/proc/<pid>/cwd/…`, `/proc/self/cwd/…`, `/proc/<pid>/task/<tid>/cwd/…` | [`Severity::Medium`] | `Anomaly` |
//! | Workspace-Wurzel aus [`crate::rule::RuleConfig`] | `/srv/agents/ws/target/debug/x` | [`Severity::Medium`] | `Anomaly` |
//! | Home-Verzeichnis | `/home/<name>/…`, `/root/…`, `/var/home/<name>/…`, `/var/roothome/…`, Home-Verzeichnisse aus [`crate::rule::RuleConfig::with_home_dir`] | [`Severity::Medium`] | `Anomaly` |
//! | Laufzeitverzeichnis eines Benutzers | `/run/user/<uid>/…` (`/var/run/user/…`) | [`Severity::Medium`] | `Anomaly` |
//!
//! Ein Start aus einem Temp-Verzeichnis ist das klassische Muster eines
//! nachgeladenen Werkzeugs und eskaliert deshalb (`RuleTriggered`). Aus dem
//! eigenen Home- oder Workspace-Bereich laufen dagegen ständig legitime
//! Programme (`~/.cargo/bin`, `~/.local/bin`, Testprogramme unter
//! `target/`); diese Orte ergeben nur eine `Anomaly` zur Kenntnisnahme, die
//! die Eskalationsleiter nie einstuft. Ebenso ein Start über einen
//! Datei-Deskriptor: so startet ein `memfd`-Schadprogramm ohne Datei, aber
//! auch systemd (≥ 255) seinen `systemd-executor` bei jedem Unit-Start
//! (`/proc/self/fd/<n>`) und runc seine eigene Kopie. Ohne den Ort der Datei
//! (das Ereignis trägt ihn nicht) wäre `High` hier eine Befund-Flut. Ein
//! Start über `/proc/<pid>/cwd/…` führt ebenso an einen Ort, den das Ereignis
//! nicht verrät (`chdir("/tmp"); execve("/proc/self/cwd/payload")`), und
//! wird deshalb wie ein Deskriptor gemeldet statt übergangen.
//!
//! # Normalisierung und Grenzen
//! - `..` wird lexikalisch aufgelöst; feste Links (`/proc/<pid>/root`,
//!   `/var/run`, `/var/lock`, `/usr/tmp`, `/dev/fd`) löst `path_class` an Ort
//!   und Stelle auf, sodass `/proc/self/root/../tmp/x` als `/tmp/x` gilt.
//! - Ein Pfad, der durch einen beschreibbaren Ort **führt** und ihn über
//!   `..` wieder verlässt (`/tmp/l/../../usr/bin/x`), zählt wie ein Start aus
//!   diesem Ort: ist `/tmp/l` ein Symlink des Angreifers, liegt das Programm
//!   ganz woanders. Gemeldet wird die schwerste berührte Ortsklasse (bei
//!   gleicher Schwere der Zielort).
//! - Ein Pfad über `/proc/<pid>/cwd` (auch nur durchquert, etwa
//!   `/proc/self/cwd/../../tmp/x`) ergibt die Ortsklasse „unresolved process
//!   link": `..` hinter dem Link führt in das unbekannte Arbeitsverzeichnis,
//!   nicht nach `/proc/<pid>`, und der lexikalische Zielort ist bedeutungslos.
//! - **Relative Pfade** (`./payload`) haben keinen bekannten Ort: das
//!   Arbeitsverzeichnis des Prozesses steht nicht im Ereignis, und Regeln
//!   führen kein I/O aus. Solche Starts bewertet die Regel nicht; ein Sensor,
//!   der den aufgelösten Programmpfad liefert, schließt diese Lücke.
//! - Andere Symlinks werden nicht aufgelöst.
//!
//! # Prozesskontext
//! Trägt das Ereignis einen [`harw_dod_signals::Actor`], nennt die
//! Zusammenfassung dessen UID und — aus `Actor.process`, falls vorhanden —
//! den escapten Prozessnamen `comm`: ein `comm`, der nicht zum Pfad passt,
//! ist für die Triage ein Hinweis auf Tarnung. Keine pid (sie macht die
//! Zusammenfassung pro Start verschieden, siehe `path_class`).
//!
//! # Zeitstempel, Härte, Zusammenfassung
//! Ereigniszeit, [`Hardness::Observed`]. Der Pfad ist angreiferkontrolliert
//! und erscheint nur escapt und gekürzt; Cargos Build-Hash
//! (`<name>-<16 Hexziffern>`) erscheint als `<name>-<hash>`, damit zwei
//! Builds desselben Programms dieselbe Zusammenfassung ergeben.
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
//! use harw_dod_rules::rules::ExecWritablePathRule;
//! use harw_dod_signals::{EventKind, SecurityEvent, Severity};
//! use harw_types::{ContentDigest, SensorId};
//!
//! let exec = |path: &str| SecurityEvent {
//!     sensor: SensorId::from_str("procmon-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: None,
//!     kind: EventKind::ProcessExec {
//!         path: path.to_owned(),
//!         argv_digest: ContentDigest::of(b"argv"),
//!     },
//! };
//! let scope = NetworkScope::empty();
//! let events = vec![exec("/usr/bin/ls"), exec("/dev/shm/.x")];
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &events,
//!     baselines: &[],
//!     network_scope: &scope,
//!     listener_history: None,
//!     rule_config: None,
//! };
//! let severities: Vec<Severity> = ExecWritablePathRule
//!     .evaluate(&ctx)
//!     .iter()
//!     .map(|finding| finding.severity())
//!     .collect();
//! assert_eq!(severities, vec![Severity::High]);
//! ```

use harw_dod_signals::{EventKind, Hardness, Severity};

use crate::finding::{Finding, FindingKind, Raw};
use crate::rule::{Rule, RuleConfig, RuleContext};
use crate::rules::path_class::{
    actor_suffix, home_relatives, is_file_descriptor_path, is_process_cwd_path,
    normalized_components, stable_summary_path, walk_components,
};

/// Meldet einen Programmstart aus einem welt- oder benutzerbeschreibbaren
/// Ort.
///
/// # Description
/// Siehe Moduldoku für Orte, Schwere, Befund-Art und Grenzen.
///
/// # Errors
/// Keine eigenen Fehler; siehe [`crate::rule`]-Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecWritablePathRule;

impl Rule for ExecWritablePathRule {
    /// # Returns
    /// `"exec-writable-path"`.
    fn id(&self) -> &'static str {
        "exec-writable-path"
    }

    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): siehe [`crate::rule::RuleContext`];
    ///   die Workspace-Wurzeln und zusätzlichen Home-Verzeichnisse kommen aus
    ///   `ctx.rule_config`.
    ///
    /// # Returns
    /// Je `ProcessExec`-Ereignis aus einem beschreibbaren Ort einen Befund,
    /// gestempelt mit dessen `observed_at`.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        ctx.events
            .iter()
            .filter_map(|event| {
                let EventKind::ProcessExec { path, .. } = &event.kind else {
                    return None;
                };
                let location = classify(path, ctx.rule_config)?;
                Some(Finding::raw(
                    self.id(),
                    location.kind(),
                    location.severity(),
                    Hardness::Observed,
                    format!(
                        "process executed from {} location: {}{}",
                        location.label(),
                        stable_summary_path(path),
                        actor_suffix(event.actor.as_ref()),
                    ),
                    // Ereigniszeit, nicht `ctx.now`.
                    event.observed_at,
                ))
            })
            .collect()
    }
}

// Die Ortsklassen dieser Regel, siehe Tabelle in der Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritableLocation {
    // `/tmp`, `/var/tmp`, `/dev/shm`, `/run/shm`, `/run/lock`.
    WorldWritableTemp,
    // `/proc/<pid>/fd/<n>` (auch `/dev/fd/<n>`): Ort der Datei unbekannt.
    FileDescriptor,
    // `/proc/<pid>/cwd/…`: Arbeitsverzeichnis des Prozesses unbekannt.
    ProcessCwd,
    // Unterhalb einer konfigurierten Workspace-Wurzel.
    Workspace,
    // `/home/<name>/…`, `/root/…`, `/var/home/<name>/…`, `/var/roothome/…`
    // oder ein konfiguriertes Home-Verzeichnis.
    Home,
    // `/run/user/<uid>/…`.
    UserRuntime,
}

impl WritableLocation {
    // Schwere eines Starts aus diesem Ort.
    fn severity(self) -> Severity {
        match self {
            Self::WorldWritableTemp => Severity::High,
            Self::FileDescriptor
            | Self::ProcessCwd
            | Self::Workspace
            | Self::Home
            | Self::UserRuntime => Severity::Medium,
        }
    }

    // Befund-Art: nur das Temp-Verzeichnis eskaliert, die übrigen Orte sind
    // auf Entwickler-Hosts Alltag (siehe Moduldoku).
    fn kind(self) -> FindingKind {
        match self {
            Self::WorldWritableTemp => FindingKind::RuleTriggered,
            Self::FileDescriptor
            | Self::ProcessCwd
            | Self::Workspace
            | Self::Home
            | Self::UserRuntime => FindingKind::Anomaly,
        }
    }

    // Feste Kennung für die Zusammenfassung.
    fn label(self) -> &'static str {
        match self {
            Self::WorldWritableTemp => "world-writable temporary",
            Self::FileDescriptor => "file descriptor",
            Self::ProcessCwd => "unresolved process link",
            Self::Workspace => "workspace",
            Self::Home => "home directory",
            Self::UserRuntime => "user runtime directory",
        }
    }
}

// Weltbeschreibbare Temp-Verzeichnisse (sticky, 1777). `/var/lock`,
// `/var/run/shm` und `/usr/tmp` sind nach `path_class` bereits auf ihr Ziel
// abgebildet.
const WORLD_WRITABLE_TEMP: &[&[&str]] = &[
    &["tmp"],
    &["var", "tmp"],
    &["dev", "shm"],
    &["run", "shm"],
    &["run", "lock"],
];

// Ordnet einen ausgeführten Pfad einer Ortsklasse zu: die schwerste Klasse
// aus Zielort und jedem über `..` verlassenen Verzeichnis, bei gleicher
// Schwere der Zielort (siehe Moduldoku). `None` für jeden anderen Ort und
// für relative Pfade.
fn classify(path: &str, config: Option<&RuleConfig>) -> Option<WritableLocation> {
    let mut departed: Option<WritableLocation> = None;
    let components = walk_components(path, |stack| {
        if let Some(found) = classify_components(stack, config) {
            departed = more_severe(departed, found);
        }
    })?;
    match (classify_components(&components, config), departed) {
        (Some(target), Some(walked)) if walked.severity() > target.severity() => Some(walked),
        (Some(target), _) => Some(target),
        (None, walked) => walked,
    }
}

// Die schwerere zweier Klassen; bei gleicher Schwere bleibt die erste.
fn more_severe(
    current: Option<WritableLocation>,
    found: WritableLocation,
) -> Option<WritableLocation> {
    match current {
        Some(current) if current.severity() >= found.severity() => Some(current),
        _ => Some(found),
    }
}

// Ortsklasse eines bereits normalisierten Komponentenstapels.
fn classify_components(
    components: &[&str],
    config: Option<&RuleConfig>,
) -> Option<WritableLocation> {
    if WORLD_WRITABLE_TEMP
        .iter()
        .any(|prefix| strictly_below(components, prefix))
    {
        return Some(WritableLocation::WorldWritableTemp);
    }
    if is_file_descriptor_path(components) {
        return Some(WritableLocation::FileDescriptor);
    }
    if is_process_cwd_path(components) {
        return Some(WritableLocation::ProcessCwd);
    }
    if config.is_some_and(|config| below_workspace_root(components, config)) {
        return Some(WritableLocation::Workspace);
    }
    if home_relatives(components, config)
        .iter()
        .any(|rest| !rest.is_empty())
    {
        return Some(WritableLocation::Home);
    }
    // `/run/user/<uid>/` und mindestens eine Komponente darunter.
    if matches!(components, ["run", "user", _, _, ..]) {
        return Some(WritableLocation::UserRuntime);
    }
    None
}

// Ob `components` echt unterhalb von `prefix` liegt (nicht gleich).
fn strictly_below(components: &[&str], prefix: &[&str]) -> bool {
    components.len() > prefix.len() && components.starts_with(prefix)
}

// Ob `components` echt unterhalb einer Workspace-Wurzel liegt.
fn below_workspace_root(components: &[&str], config: &RuleConfig) -> bool {
    config.workspace_roots().iter().any(|root| {
        normalized_components(root).is_some_and(|root| strictly_below(components, &root))
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
    use std::path::Path;

    fn exec_by(path: &str, actor: Option<Actor>) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("procmon-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor,
            kind: EventKind::ProcessExec {
                path: path.to_owned(),
                argv_digest: ContentDigest::of(b"argv"),
            },
        }
    }

    fn exec(path: &str) -> SecurityEvent {
        exec_by(path, None)
    }

    fn ctx_with<'a>(
        events: &'a [SecurityEvent],
        scope: &'a NetworkScope,
        config: Option<&'a RuleConfig>,
    ) -> RuleContext<'a> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events,
            baselines: &[],
            network_scope: scope,
            listener_history: None,
            rule_config: config,
        }
    }

    fn severities(events: &[SecurityEvent], config: Option<&RuleConfig>) -> Vec<Severity> {
        let scope = NetworkScope::empty();
        ExecWritablePathRule
            .evaluate(&ctx_with(events, &scope, config))
            .iter()
            .map(Finding::severity)
            .collect()
    }

    #[test]
    fn test_exec_from_world_writable_temp_is_high() {
        for path in [
            "/tmp/x",
            "/tmp/.hidden/payload",
            "/var/tmp/x",
            "/dev/shm/x",
            "/run/shm/x",
            "/run/lock/x",
            "/var/lock/x",
            "/var/run/shm/x",
            "/var/run/lock/x",
            "/usr/tmp/x",
        ] {
            assert_eq!(
                severities(&[exec(path)], None),
                vec![Severity::High],
                "{path}"
            );
        }
    }

    #[test]
    fn test_exec_from_home_or_user_runtime_is_medium() {
        for path in [
            "/home/alice/.local/bin/tool",
            "/home/alice/x",
            "/root/x",
            "/run/user/1000/x",
            "/var/run/user/1000/x",
        ] {
            assert_eq!(
                severities(&[exec(path)], None),
                vec![Severity::Medium],
                "{path}"
            );
        }
    }

    #[test]
    fn test_exec_from_system_locations_does_not_trigger() {
        for path in [
            "/usr/bin/ls",
            "/usr/local/bin/tool",
            "/opt/app/bin/app",
            "/tmpfoo/x",
            "/var/tmpx/y",
            "/run/user/1000",
            "/home",
            "/proc/self/exe",
            "/var/runner/x",
            "/srv/proc/self/root/tmp/x",
        ] {
            assert!(severities(&[exec(path)], None).is_empty(), "{path}");
        }
    }

    // (Art, Schwere, Zusammenfassung) aller Befunde.
    fn shapes(
        events: &[SecurityEvent],
        config: Option<&RuleConfig>,
    ) -> Vec<(FindingKind, Severity, String)> {
        let scope = NetworkScope::empty();
        ExecWritablePathRule
            .evaluate(&ctx_with(events, &scope, config))
            .iter()
            .map(|finding| {
                (
                    finding.kind(),
                    finding.severity(),
                    finding.summary().to_owned(),
                )
            })
            .collect()
    }

    /// Review-Befund: auf Entwickler-Hosts laufen ständig Programme aus Home
    /// und Workspace. Nur das Temp-Verzeichnis eskaliert (`RuleTriggered`),
    /// die übrigen Orte sind `Anomaly`.
    #[test]
    fn test_only_world_writable_temp_is_rule_triggered() -> TestResult {
        let config = RuleConfig::new()
            .with_workspace_root(Path::new("/srv/agents/ws"))
            .map_err(ctx("Workspace-Wurzel"))?;
        let cases = [
            ("/tmp/x", FindingKind::RuleTriggered, Severity::High),
            ("/dev/fd/3", FindingKind::Anomaly, Severity::Medium),
            (
                "/srv/agents/ws/bin/x",
                FindingKind::Anomaly,
                Severity::Medium,
            ),
            (
                "/home/alice/.cargo/bin/cargo",
                FindingKind::Anomaly,
                Severity::Medium,
            ),
            ("/run/user/1000/x", FindingKind::Anomaly, Severity::Medium),
        ];
        for (path, kind, severity) in cases {
            let found: Vec<(FindingKind, Severity)> = shapes(&[exec(path)], Some(&config))
                .into_iter()
                .map(|(kind, severity, _)| (kind, severity))
                .collect();
            assert_eq!(found, vec![(kind, severity)], "{path}");
        }
        Ok(())
    }

    /// Review-Befund: ein Start über einen Datei-Deskriptor (`fexecve`,
    /// `memfd`) wurde nie bewertet.
    #[test]
    fn test_exec_through_a_file_descriptor_is_reported() {
        for path in [
            "/dev/fd/3",
            "/proc/self/fd/7",
            "/proc/thread-self/fd/7",
            "/proc/4242/fd/3",
            "/proc/4242/task/4243/fd/3",
            "/dev/fd/3/../../usr/bin/ls",
        ] {
            let found = shapes(&[exec(path)], None);
            assert_eq!(found.len(), 1, "{path}");
            assert!(
                found
                    .iter()
                    .all(|(kind, severity, summary)| *kind == FindingKind::Anomaly
                        && *severity == Severity::Medium
                        && summary.starts_with("process executed from file descriptor location:")),
                "{path}: {found:?}"
            );
        }
    }

    /// Review-Befund: zwei Builds desselben Testprogramms unterscheiden sich
    /// nur im Cargo-Hash und müssen dieselbe Zusammenfassung ergeben.
    #[test]
    fn test_build_hash_does_not_change_the_summary() -> TestResult {
        let config = RuleConfig::new()
            .with_workspace_root(Path::new("/srv/agents/ws"))
            .map_err(ctx("Workspace-Wurzel"))?;
        let first = shapes(
            &[exec(
                "/srv/agents/ws/target/debug/deps/harw_x-0123456789abcdef",
            )],
            Some(&config),
        );
        let second = shapes(
            &[exec(
                "/srv/agents/ws/target/debug/deps/harw_x-fedcba9876543210",
            )],
            Some(&config),
        );
        assert_eq!(first, second);
        let (_, _, summary) = first.first().ok_or_else(missing("Befund"))?;
        assert_eq!(
            summary,
            "process executed from workspace location: /srv/agents/ws/target/debug/deps/harw_x-<hash>"
        );
        Ok(())
    }

    /// Review-Befund (Runde 2): ostree-Hosts melden aufgelöste Pfade unter
    /// `/var/home` und `/var/roothome`; konfigurierte Home-Verzeichnisse
    /// (AD-Layout, Dienstkonto) zählen ebenso.
    #[test]
    fn test_exec_from_ostree_and_configured_homes_is_medium() -> TestResult {
        for path in ["/var/home/alice/x", "/var/roothome/.local/bin/tool"] {
            assert_eq!(
                shapes(&[exec(path)], None),
                vec![(
                    FindingKind::Anomaly,
                    Severity::Medium,
                    format!("process executed from home directory location: {path}")
                )],
                "{path}"
            );
        }
        // `/var/home` selbst ist kein Ort darin.
        assert!(severities(&[exec("/var/home/alice")], None).is_empty());

        let config = RuleConfig::new()
            .with_home_dir(Path::new("/srv/git"))
            .map_err(ctx("Dienstkonto"))?;
        assert_eq!(
            severities(&[exec("/srv/git/hooks/post-receive")], Some(&config)),
            vec![Severity::Medium]
        );
        assert!(severities(&[exec("/srv/git/hooks/post-receive")], None).is_empty());
        Ok(())
    }

    /// Review-Befund (Runde 2): `chdir("/tmp"); execve("/proc/self/cwd/x")`
    /// umging die Regel ohne relativen Pfad. Der Ort ist unbekannt wie bei
    /// einem Datei-Deskriptor — auch, wenn `..` den Link wieder verlässt.
    #[test]
    fn test_exec_through_process_cwd_is_reported() {
        for path in [
            "/proc/self/cwd/x",
            "/proc/4242/cwd/x",
            "/proc/thread-self/cwd/payload",
            "/proc/4242/task/4243/cwd/x",
            "/proc/self/cwd/../../tmp/x",
            "/proc/self/cwd/../../usr/bin/ls",
        ] {
            let found = shapes(&[exec(path)], None);
            assert_eq!(
                found,
                vec![(
                    FindingKind::Anomaly,
                    Severity::Medium,
                    format!("process executed from unresolved process link location: {path}")
                )],
                "{path}"
            );
        }
        // Ein Temp-Verzeichnis vor dem Link bleibt die schwerere Klasse.
        assert_eq!(
            severities(&[exec("/tmp/a/../../proc/self/cwd/x")], None),
            vec![Severity::High]
        );
        assert!(severities(&[exec("/proc/sys/cwd/x")], None).is_empty());
    }

    #[test]
    fn test_relative_exec_path_is_not_judged() {
        assert!(severities(&[exec("./payload"), exec("payload")], None).is_empty());
    }

    /// `..` und `/proc/self/root/` umgehen die Regel nicht.
    #[test]
    fn test_path_tricks_are_normalized() {
        for path in [
            "/usr/bin/../../tmp/x",
            "/proc/self/root/tmp/x",
            "//dev//shm/./x",
            // Review-Befund: `..` hinter der Prozesswurzel bleibt an der
            // Wurzel (der Kernel startet `/tmp/x`).
            "/proc/self/root/../tmp/x",
            "/proc/4242/task/4243/root/../tmp/x",
            // Durch ein Temp-Verzeichnis und wieder hinaus: `/tmp/l` kann ein
            // Symlink des Angreifers sein.
            "/tmp/l/../../usr/bin/x",
            "/home/alice/../../tmp/x",
        ] {
            assert_eq!(
                severities(&[exec(path)], None),
                vec![Severity::High],
                "{path}"
            );
        }
        // Umgekehrt: aus `/tmp` selbst herausnormalisiert ist kein Treffer
        // (`/tmp` ist kein Ort darin).
        assert!(severities(&[exec("/tmp/../usr/bin/ls")], None).is_empty());
    }

    /// Die schwerste berührte Klasse gewinnt, bei gleicher Schwere der
    /// Zielort.
    #[test]
    fn test_most_severe_touched_location_is_reported() {
        let found = shapes(&[exec("/home/alice/bin/../../../tmp/x")], None);
        assert_eq!(
            found,
            vec![(
                FindingKind::RuleTriggered,
                Severity::High,
                "process executed from world-writable temporary location: /home/alice/bin/../../../tmp/x"
                    .to_owned()
            )]
        );
        let found = shapes(&[exec("/tmp/a/../../home/alice/x")], None);
        assert_eq!(
            found
                .iter()
                .map(|(_, severity, _)| *severity)
                .collect::<Vec<_>>(),
            vec![Severity::High]
        );
        let found = shapes(&[exec("/run/user/1000/a/../../../../home/alice/x")], None);
        assert!(
            found.iter().all(|(_, _, summary)| summary
                .starts_with("process executed from home directory location:")),
            "{found:?}"
        );
    }

    #[test]
    fn test_configured_workspace_root_triggers_medium() -> TestResult {
        let config = RuleConfig::new()
            .with_workspace_root(Path::new("/srv/agents/ws"))
            .map_err(ctx("Workspace-Wurzel"))?;
        let events = vec![exec("/srv/agents/ws/target/debug/app")];
        assert_eq!(severities(&events, Some(&config)), vec![Severity::Medium]);
        // Ohne Konfiguration ist derselbe Ort kein Treffer.
        assert!(severities(&events, None).is_empty());
        // Nachbarverzeichnisse mit gleichem Präfix zählen nicht.
        assert!(severities(&[exec("/srv/agents/wsx/app")], Some(&config)).is_empty());
        Ok(())
    }

    #[test]
    fn test_workspace_label_wins_over_home() -> TestResult {
        let config = RuleConfig::new()
            .with_workspace_root(Path::new("/home/dev/project"))
            .map_err(ctx("Workspace-Wurzel"))?;
        let scope = NetworkScope::empty();
        let events = vec![exec("/home/dev/project/target/debug/app")];
        let findings = ExecWritablePathRule.evaluate(&ctx_with(&events, &scope, Some(&config)));
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert!(
            finding
                .summary()
                .starts_with("process executed from workspace location:")
        );
        Ok(())
    }

    #[test]
    fn test_finding_shape_and_summary_with_process_context() -> TestResult {
        let actor = Actor {
            uid: 1000,
            auid: None,
            cgroup: None,
            process: ProcessContext::from_pid(4242).map(|process| ProcessContext {
                ppid: Some(1),
                comm: Some("kworker".to_owned()),
                ..process
            }),
        };
        let scope = NetworkScope::empty();
        let events = vec![exec_by("/tmp/.x", Some(actor))];
        let findings = ExecWritablePathRule.evaluate(&ctx_with(&events, &scope, None));

        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert_eq!(finding.rule_id(), "exec-writable-path");
        assert_eq!(finding.kind(), FindingKind::RuleTriggered);
        assert_eq!(finding.hardness(), Hardness::Observed);
        assert_eq!(
            finding.summary(),
            "process executed from world-writable temporary location: /tmp/.x (uid 1000, comm 'kworker')"
        );
        assert!(!finding.summary().contains("4242"), "keine pid");
        Ok(())
    }

    #[test]
    fn test_summary_without_actor_has_no_suffix() -> TestResult {
        let scope = NetworkScope::empty();
        let events = vec![exec("/root/x")];
        let findings = ExecWritablePathRule.evaluate(&ctx_with(&events, &scope, None));
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert_eq!(
            finding.summary(),
            "process executed from home directory location: /root/x"
        );
        Ok(())
    }

    /// Angreiferkontrollierte Pfade erscheinen nur escapt in der
    /// Zusammenfassung.
    #[test]
    fn test_attacker_controlled_path_is_escaped() -> TestResult {
        let scope = NetworkScope::empty();
        let events = vec![exec("/tmp/a\u{202e}gpj.sh\nIGNORE PREVIOUS")];
        let findings = ExecWritablePathRule.evaluate(&ctx_with(&events, &scope, None));
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert!(!finding.summary().contains('\u{202e}'));
        assert!(!finding.summary().contains('\n'));
        assert!(finding.summary().contains("\\u{202e}"));
        assert!(finding.summary().contains("\\u{000a}"));
        Ok(())
    }

    #[test]
    fn test_finding_is_stamped_with_the_event_time_not_ctx_now() -> TestResult {
        let event_time: Timestamp = "2026-11-08T10:00:00Z"
            .parse()
            .map_err(ctx("Ereigniszeit"))?;
        let mut event = exec("/tmp/x");
        event.observed_at = event_time;
        let events = vec![event];
        let scope = NetworkScope::empty();
        let mut rule_ctx = ctx_with(&events, &scope, None);
        rule_ctx.now = "2026-11-08T10:05:00Z"
            .parse()
            .map_err(ctx("Auswertungszeit"))?;

        let stamps: Vec<Timestamp> = ExecWritablePathRule
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
            sensor: SensorId::from_str("fsmon-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::FileWrite {
                path: "/tmp/x".to_owned(),
            },
        }];
        assert!(severities(&events, None).is_empty());
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() {
        let scope = NetworkScope::empty();
        let events = vec![exec("/tmp/x"), exec("/home/a/b"), exec("/usr/bin/ls")];
        let ctx = ctx_with(&events, &scope, None);
        assert_eq!(
            ExecWritablePathRule.evaluate(&ctx),
            ExecWritablePathRule.evaluate(&ctx)
        );
    }
}
