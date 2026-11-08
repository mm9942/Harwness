//! Die Regelkonfiguration dieses Hosts (PL-100 WP1.6): baut die
//! [`harw_dod_rules::RuleConfig`], die der Sentinel in jedem Zyklus als
//! `rule_config` an die Regeln gibt und mit der er die Listener-Historie
//! lernt.
//!
//! # Quellen
//! - **Workspace-Wurzel:** `--workspace-root`, falls angegeben, kanonisiert
//!   (`std::fs::canonicalize`; ist das nicht möglich, die angegebene
//!   Schreibweise). `exec-writable-path` meldet Programmstarts darunter.
//! - **Home-Verzeichnisse:** das sechste Feld jeder Zeile von `/etc/passwd`
//!   ([`crate::peer_allow::PASSWD_PATH`]), je verschiedenem Verzeichnis
//!   einmal, höchstens [`MAX_HOME_DIRS`]. Ausgelassen werden die Orte, die
//!   die Regeln ohnehin kennen (`/home/<name>`, `/root`, `/var/home/<name>`,
//!   `/var/roothome`). Jedes übrige Verzeichnis geht in der Schreibweise
//!   von `passwd` ein und, falls seine **erste** Komponente ein Symlink ist
//!   (ein direkter Eintrag unter `/`, etwa `/home -> var/home` oder
//!   `/u -> /export/home`), zusätzlich mit genau dieser einen Ebene
//!   lexikalisch aufgelöst ([`resolve_first_component`]; Sensoren melden
//!   teils aufgelöste, teils ausgeführte Pfade). Was
//!   `RuleConfig::with_home_dir` ablehnt (`/`, relativ, unter `/bin`,
//!   `/usr`, `/etc`, …), wird übersprungen und nur gezählt — das ist der
//!   Normalfall für Systemkonten.
//! - **Flüchtiger Portbereich:** `<--proc-root>/`[`PORT_RANGE_FILE`]
//!   (`net.ipv4.ip_local_port_range`); ist die Datei nicht lesbar oder der
//!   Bereich unzulässig, gilt `RuleConfig::DEFAULT_EPHEMERAL_PORTS`.
//!
//! # Wann
//! [`build`] läuft in `crate::run` **vor** der Landlock-Selbstbeschränkung,
//! weil `/etc` danach nicht mehr lesbar ist, und damit vor dem ersten
//! Poll. Home-Verzeichnisse werden deshalb nie kanonisiert:
//! `canonicalize` liefe durch jedes Zwischenverzeichnis und könnte für bis
//! zu [`MAX_HOME_DIRS`] Einträge einen Automount auslösen
//! (`/net/<host>/…`) oder an einem hängenden NFS-Server blockieren — die
//! Unit verbirgt mit `ProtectHome=yes` nur `/home`, `/root` und
//! `/run/user`. [`resolve_first_component`] liest höchstens einen Symlink
//! direkt unter `/` (`readlink` folgt nichts und betritt keinen Mount).
//! Nur die ausdrücklich angegebene Workspace-Wurzel wird kanonisiert.
//!
//! # Journal-Hygiene
//! Home-Verzeichnisse enthalten Kontonamen: [`build`] meldet nur Zahlen
//! (`info`) und nie einen Pfad. Eine unbrauchbare Workspace-Wurzel steht auf
//! `warn` nur mit dem inhaltsfreien Grund.
//!
//! # Nebenläufigkeit
//! Reine Startfunktion, einmal im Hauptthread.
//!
//! # Fehler
//! Keine nach außen: jede unbrauchbare Quelle fällt auf die Vorgabe zurück
//! und wird in [`RuleConfigSummary`] gezählt.
//!
//! # Examples
//! ```rust,ignore
//! let (rule_config, summary) = rule_config::build(
//!     cli.workspace_root.as_deref(),
//!     Path::new(peer_allow::PASSWD_PATH),
//!     &cli.proc_root,
//! );
//! ```

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use harw_dod_rules::RuleConfig;

use crate::peer_allow;

/// Pfad des flüchtigen Portbereichs relativ zu `--proc-root`.
pub const PORT_RANGE_FILE: &str = "sys/net/ipv4/ip_local_port_range";

/// Höchstzahl der aus `/etc/passwd` übernommenen Home-Verzeichnisse (je
/// Schreibweise); weitere werden nur gezählt. Begrenzt den Aufwand je
/// Ereignis in den Pfadregeln.
pub const MAX_HOME_DIRS: usize = 256;

/// Höchstgröße der gelesenen Portbereichsdatei in Bytes.
const MAX_PORT_RANGE_BYTES: u64 = 64;

/// Woher der flüchtige Portbereich stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortRangeSource {
    /// Aus `ip_local_port_range` des Hosts.
    Host,
    /// Die Datei war nicht lesbar oder nicht parsbar.
    DefaultUnreadable,
    /// Der Bereich des Hosts ist für die Regeln unzulässig (Anfang unter
    /// 1024 oder Anfang nach Ende).
    DefaultRejected,
}

impl PortRangeSource {
    /// Stabiler Bezeichner für das Journal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::DefaultUnreadable => "default-unreadable",
            Self::DefaultRejected => "default-rejected",
        }
    }
}

/// Was [`build`] übernommen hat — nur Zahlen und feste Bezeichner, keine
/// Pfade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleConfigSummary {
    /// Ob eine Workspace-Wurzel übernommen wurde.
    pub workspace_root: bool,
    /// Inhaltsfreier Grund, falls die Workspace-Wurzel abgelehnt wurde.
    pub workspace_root_refused: Option<&'static str>,
    /// Übernommene Home-Verzeichnisse (je Schreibweise).
    pub home_dirs: usize,
    /// Von `RuleConfig::with_home_dir` abgelehnte Einträge.
    pub home_dirs_refused: usize,
    /// Wegen [`MAX_HOME_DIRS`] nicht übernommene Einträge.
    pub home_dirs_truncated: usize,
    /// Ob `/etc/passwd` lesbar war.
    pub passwd_readable: bool,
    /// Der wirksame flüchtige Portbereich.
    pub ephemeral_ports: (u16, u16),
    /// Woher er stammt.
    pub ephemeral_source: PortRangeSource,
}

/// Baut die Regelkonfiguration dieses Hosts (siehe Moduldoku).
///
/// # Arguments
/// - `workspace_root` (`Option<&Path>`): `--workspace-root`, falls
///   angegeben (absolut, von `clap` geprüft).
/// - `passwd_path` (`&Path`): die Nutzerdatenbank (in Produktion
///   [`peer_allow::PASSWD_PATH`]).
/// - `proc_root` (`&Path`): `--proc-root`.
///
/// # Returns
/// Die Konfiguration und eine pfadfreie Zusammenfassung; beides wird auch
/// auf `info` gemeldet (nur Zahlen).
///
/// # Concurrency
/// Liest höchstens drei Dateien; nur vor der Landlock-Selbstbeschränkung
/// aufrufen.
#[must_use]
pub fn build(
    workspace_root: Option<&Path>,
    passwd_path: &Path,
    proc_root: &Path,
) -> (RuleConfig, RuleConfigSummary) {
    let (config, ephemeral_ports, ephemeral_source) =
        with_ephemeral_ports(RuleConfig::new(), read_port_range(proc_root));

    let (config, workspace_root_refused) = match workspace_root {
        Some(root) => {
            let canonical = std::fs::canonicalize(root).unwrap_or_else(|error| {
                tracing::debug!(error = %error, "workspace root not canonicalisable; using it as given");
                root.to_path_buf()
            });
            match config.clone().with_workspace_root(&canonical) {
                Ok(config) => (config, None),
                Err(error) => {
                    // Inhaltsfrei (`RuleConfigError` interpoliert keinen Pfad).
                    tracing::warn!(error = %error, "workspace root unusable for the rules; ignored");
                    (config, Some(workspace_refusal_label(&error)))
                }
            }
        }
        None => (config, None),
    };

    let passwd = peer_allow::read_passwd(passwd_path).ok();
    let (config, homes) = with_home_dirs(config, passwd.as_deref().unwrap_or(""), |path| {
        resolve_first_component(path, Path::new("/"))
    });

    let summary = RuleConfigSummary {
        workspace_root: workspace_root.is_some() && workspace_root_refused.is_none(),
        workspace_root_refused,
        home_dirs: homes.added,
        home_dirs_refused: homes.refused,
        home_dirs_truncated: homes.truncated,
        passwd_readable: passwd.is_some(),
        ephemeral_ports,
        ephemeral_source,
    };
    tracing::info!(
        workspace_root = summary.workspace_root,
        workspace_root_refused = summary.workspace_root_refused,
        home_dirs = summary.home_dirs,
        home_dirs_refused = summary.home_dirs_refused,
        home_dirs_truncated = summary.home_dirs_truncated,
        passwd_readable = summary.passwd_readable,
        ephemeral_first = summary.ephemeral_ports.0,
        ephemeral_last = summary.ephemeral_ports.1,
        ephemeral_source = summary.ephemeral_source.label(),
        "rule configuration resolved"
    );
    (config, summary)
}

/// Inhaltsfreier Bezeichner einer abgelehnten Workspace-Wurzel.
const fn workspace_refusal_label(error: &harw_dod_rules::RuleConfigError) -> &'static str {
    match error {
        harw_dod_rules::RuleConfigError::WorkspaceRootNotUtf8 => "not-utf8",
        harw_dod_rules::RuleConfigError::WorkspaceRootNotAbsolute => "not-absolute",
        harw_dod_rules::RuleConfigError::WorkspaceRootIsFilesystemRoot => "filesystem-root",
        _ => "refused",
    }
}

/// Setzt den flüchtigen Portbereich aus `range`, sonst die Vorgabe.
///
/// # Returns
/// Die Konfiguration, den wirksamen Bereich und seine Herkunft.
fn with_ephemeral_ports(
    config: RuleConfig,
    range: Option<(u16, u16)>,
) -> (RuleConfig, (u16, u16), PortRangeSource) {
    let default = (
        *RuleConfig::DEFAULT_EPHEMERAL_PORTS.start(),
        *RuleConfig::DEFAULT_EPHEMERAL_PORTS.end(),
    );
    let Some((first, last)) = range else {
        return (config, default, PortRangeSource::DefaultUnreadable);
    };
    match config.clone().with_ephemeral_ports(first, last) {
        Ok(config) => (config, (first, last), PortRangeSource::Host),
        Err(error) => {
            tracing::warn!(
                error = %error,
                first,
                last,
                "host ephemeral port range unusable for the rules; using the default"
            );
            (config, default, PortRangeSource::DefaultRejected)
        }
    }
}

/// Liest `<proc_root>/`[`PORT_RANGE_FILE`] begrenzt und parst ihn.
fn read_port_range(proc_root: &Path) -> Option<(u16, u16)> {
    let path = proc_root.join(PORT_RANGE_FILE);
    let mut text = String::new();
    let read = std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_PORT_RANGE_BYTES).read_to_string(&mut text));
    match read {
        Ok(_) => parse_port_range(&text),
        Err(error) => {
            tracing::debug!(error = %error, "ephemeral port range unreadable");
            None
        }
    }
}

/// Parst `ip_local_port_range`: genau zwei durch Leerraum getrennte Ports.
#[must_use]
fn parse_port_range(text: &str) -> Option<(u16, u16)> {
    let mut parts = text.split_whitespace();
    let first = parts.next()?.parse::<u16>().ok()?;
    let last = parts.next()?.parse::<u16>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((first, last))
}

/// Zähler von [`with_home_dirs`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HomeCounts {
    // Übernommene Schreibweisen.
    added: usize,
    // Von `with_home_dir` abgelehnte Einträge.
    refused: usize,
    // Wegen `MAX_HOME_DIRS` ausgelassene Schreibweisen.
    truncated: usize,
}

/// Übernimmt die Home-Verzeichnisse aus dem `passwd(5)`-Text (siehe
/// Moduldoku).
///
/// # Description
/// Je verschiedener Schreibweise genau ein Versuch. Aufgelöst (über
/// `canonicalize`, in Produktion [`resolve_first_component`]) wird nur eine
/// Schreibweise, die `RuleConfig::with_home_dir` angenommen hat — ein
/// Systemverzeichnis oder `/` wird so nie berührt.
fn with_home_dirs(
    mut config: RuleConfig,
    passwd: &str,
    canonicalize: impl Fn(&Path) -> Option<PathBuf>,
) -> (RuleConfig, HomeCounts) {
    let mut counts = HomeCounts::default();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for home in passwd_home_dirs(passwd) {
        let raw = PathBuf::from(home);
        if is_builtin_home(&raw) || !seen.insert(raw.clone()) {
            continue;
        }
        if !add_home(&mut config, &raw, &mut counts) {
            continue;
        }
        let Some(canonical) = canonicalize(&raw) else {
            continue;
        };
        if canonical != raw && !is_builtin_home(&canonical) && seen.insert(canonical.clone()) {
            add_home(&mut config, &canonical, &mut counts);
        }
    }
    (config, counts)
}

/// Löst einen Symlink in der ersten Komponente von `path` lexikalisch auf.
///
/// # Description
/// Liest nur `<fs_root>/<erste Komponente>` mit `readlink` — einen
/// direkten Eintrag unter `/`, der selbst nie gefolgt wird; kein tieferes
/// Verzeichnis wird betreten, also löst das nie einen Automount aus und
/// blockiert nie an einem entfernten Dateisystem (siehe Moduldoku, „Wann").
/// Das Ziel wird relativ zu `/` (ein relatives Ziel) bzw. absolut
/// komponentenweise eingesetzt (`.` entfällt, `..` entfernt eine Ebene),
/// der Rest von `path` unverändert angehängt. Weitere Symlinks im Ziel
/// oder tiefer bleiben unaufgelöst.
///
/// # Arguments
/// - `path` (`&Path`): ein absolutes Home-Verzeichnis aus `passwd`.
/// - `fs_root` (`&Path`): die Dateisystemwurzel, unter der gelesen wird
///   (`/` in Produktion, ein Verzeichnis in Tests); das Ergebnis ist in der
///   Schreibweise von `path` (ohne `fs_root`).
///
/// # Returns
/// Die aufgelöste Schreibweise, oder `None`, wenn die erste Komponente kein
/// Symlink ist, `path` nicht absolut oder nicht normal ist (`.`, `..`) oder
/// das Ziel über `/` hinaus zeigt.
fn resolve_first_component(path: &Path, fs_root: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return None;
    }
    let Some(Component::Normal(first)) = components.next() else {
        return None;
    };
    let target = std::fs::read_link(fs_root.join(first)).ok()?;
    let mut resolved = PathBuf::from("/");
    for component in target.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => resolved.push(part),
            Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            Component::Prefix(_) => return None,
        }
    }
    for component in components {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

/// Ein Versuch von `RuleConfig::with_home_dir` samt Zählung.
///
/// # Returns
/// `true`, wenn `home` übernommen wurde.
fn add_home(config: &mut RuleConfig, home: &Path, counts: &mut HomeCounts) -> bool {
    if counts.added >= MAX_HOME_DIRS {
        counts.truncated = counts.truncated.saturating_add(1);
        return false;
    }
    match config.clone().with_home_dir(home) {
        Ok(extended) => {
            *config = extended;
            counts.added = counts.added.saturating_add(1);
            true
        }
        Err(_) => {
            counts.refused = counts.refused.saturating_add(1);
            false
        }
    }
}

/// Das Home-Feld (sechstes Feld) jeder wohlgeformten `passwd(5)`-Zeile, in
/// Dateireihenfolge.
///
/// # Description
/// Leere Zeilen, Kommentare (`#`) und NIS-Kompatibilitätszeilen (`+`/`-`)
/// werden übersprungen, ebenso Zeilen mit weniger als sieben Feldern oder
/// leerem Home-Feld (wie `crate::peer_allow`).
fn passwd_home_dirs(passwd: &str) -> impl Iterator<Item = &str> {
    passwd.lines().filter_map(|line| {
        if line.is_empty() || line.starts_with(['#', '+', '-']) {
            return None;
        }
        let mut fields = line.split(':');
        let home = fields.nth(5)?;
        // Die Shell muss als siebtes Feld folgen (wohlgeformte Zeile).
        fields.next()?;
        (!home.is_empty()).then_some(home)
    })
}

/// Ob `path` ein Ort ist, den die Regeln ohnehin als Home-Verzeichnis
/// kennen: `/home/<name>`, `/root`, `/var/home/<name>`, `/var/roothome`.
fn is_builtin_home(path: &Path) -> bool {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => parts.push(part),
            // `.`/`..` oder ein Präfix: nicht als fester Ort erkennen; die
            // Regelkonfiguration normalisiert selbst.
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => return false,
        }
    }
    let parts: Vec<Option<&str>> = parts.iter().map(|part| part.to_str()).collect();
    matches!(
        parts.as_slice(),
        [Some("home"), Some(_)]
            | [Some("root")]
            | [Some("var"), Some("home"), Some(_)]
            | [Some("var"), Some("roothome")]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin
nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin
# comment:x:0:0::/srv/commented:/bin/sh
+nisuser::::::
harw-sentinel:x:990:990:Harw-DoD sentinel:/:/usr/sbin/nologin
git:x:997:997:git:/var/lib/git:/usr/bin/git-shell
alice:x:1000:1000:Alice:/home/alice:/bin/bash
bob:x:1001:1001:Bob:/home/corp/bob:/bin/bash
carol:x:1002:1002:Carol:/var/home/carol:/bin/bash
dave:x:1003:1003:Dave:/home/corp/bob:/bin/bash
short:x:1004:1004
empty:x:1005:1005:Empty::/bin/sh
";

    fn proc_with_range(range: &str) -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let file = dir.path().join(PORT_RANGE_FILE);
        let parent = file
            .parent()
            .ok_or(crate::test_support::TestError::Missing("port range parent"))?;
        std::fs::create_dir_all(parent).map_err(ctx("proc fixture"))?;
        std::fs::write(&file, range).map_err(ctx("write port range"))?;
        Ok(dir)
    }

    #[test]
    fn test_passwd_home_dirs_skip_comments_nis_and_malformed_lines() {
        let homes: Vec<&str> = passwd_home_dirs(PASSWD).collect();
        assert_eq!(
            homes,
            vec![
                "/root",
                "/usr/sbin",
                "/nonexistent",
                "/",
                "/var/lib/git",
                "/home/alice",
                "/home/corp/bob",
                "/var/home/carol",
                "/home/corp/bob",
            ]
        );
    }

    #[test]
    fn test_builtin_homes_are_recognised_by_component() {
        for builtin in ["/root", "/home/alice", "/var/home/carol", "/var/roothome"] {
            assert!(is_builtin_home(Path::new(builtin)), "{builtin}");
        }
        for other in [
            "/home",
            "/home/corp/bob",
            "/var/lib/git",
            "/rootfs",
            "/var/home",
            "/home/../etc",
        ] {
            assert!(!is_builtin_home(Path::new(other)), "{other}");
        }
    }

    /// Jedes verschiedene, nicht abgelehnte und nicht ohnehin bekannte Home
    /// geht genau einmal ein; Systemverzeichnisse und `/` nur als Zähler.
    #[test]
    fn test_home_dirs_take_distinct_extra_homes_and_count_the_refused() {
        let (config, counts) = with_home_dirs(RuleConfig::new(), PASSWD, |_| None);
        assert_eq!(
            config.home_dirs().to_vec(),
            vec![
                "/nonexistent".to_owned(),
                "/var/lib/git".to_owned(),
                "/home/corp/bob".to_owned(),
            ]
        );
        assert_eq!(counts.added, 3);
        // `/usr/sbin` (Systemverzeichnis) und `/` (Dateisystemwurzel).
        assert_eq!(counts.refused, 2);
        assert_eq!(counts.truncated, 0);
    }

    #[test]
    fn test_home_dirs_are_bounded() {
        let passwd: String = (0..MAX_HOME_DIRS.saturating_add(10))
            .map(|index| format!("svc{index}:x:{index}:{index}::/srv/svc{index}:/bin/false\n"))
            .collect();
        let (config, counts) = with_home_dirs(RuleConfig::new(), &passwd, |_| None);
        assert_eq!(config.home_dirs().len(), MAX_HOME_DIRS);
        assert_eq!(counts.added, MAX_HOME_DIRS);
        assert_eq!(counts.truncated, 10);
    }

    /// Ein symlinktes Home geht in beiden Schreibweisen ein.
    #[cfg(unix)]
    #[test]
    fn test_symlinked_home_is_added_in_both_spellings() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let real = dir.path().join("data").join("svc");
        std::fs::create_dir_all(&real).map_err(ctx("real home"))?;
        let link = dir.path().join("svc-link");
        std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink"))?;
        let link_text = link
            .to_str()
            .ok_or(crate::test_support::TestError::Missing("utf-8 link"))?;
        let passwd = format!("svc:x:900:900::{link_text}:/bin/false\n");
        let (config, counts) = with_home_dirs(RuleConfig::new(), &passwd, |path| {
            std::fs::canonicalize(path).ok()
        });
        let canonical = std::fs::canonicalize(&link).map_err(ctx("canonical home"))?;
        let canonical_text = canonical
            .to_str()
            .ok_or(crate::test_support::TestError::Missing("utf-8 canonical"))?;
        assert_eq!(
            config.home_dirs().to_vec(),
            vec![link_text.to_owned(), canonical_text.to_owned()]
        );
        assert_eq!(counts.added, 2);
        Ok(())
    }

    /// Review Runde 1: nur ein Symlink direkt unter der Dateisystemwurzel
    /// wird aufgelöst (relativ und absolut, `..` lexikalisch); ein Symlink
    /// tiefer im Pfad, ein Nicht-Symlink und ein Ziel über `/` hinaus
    /// ergeben `None`.
    #[cfg(unix)]
    #[test]
    fn test_resolve_first_component_reads_one_top_level_symlink_only() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::os::unix::fs::symlink("var/home", root.path().join("home"))
            .map_err(ctx("relative top-level link"))?;
        std::os::unix::fs::symlink("/export/./home", root.path().join("u"))
            .map_err(ctx("absolute top-level link"))?;
        std::os::unix::fs::symlink("srv/../data", root.path().join("d"))
            .map_err(ctx("dotdot link"))?;
        std::os::unix::fs::symlink("../../escape", root.path().join("bad"))
            .map_err(ctx("escaping link"))?;
        std::fs::create_dir(root.path().join("plain")).map_err(ctx("plain dir"))?;
        std::os::unix::fs::symlink("/elsewhere", root.path().join("plain").join("deep"))
            .map_err(ctx("deep link"))?;

        let resolve = |path: &str| resolve_first_component(Path::new(path), root.path());
        assert_eq!(
            resolve("/home/corp/bob"),
            Some(PathBuf::from("/var/home/corp/bob"))
        );
        assert_eq!(
            resolve("/u/alice"),
            Some(PathBuf::from("/export/home/alice"))
        );
        assert_eq!(resolve("/d/svc"), Some(PathBuf::from("/data/svc")));
        assert_eq!(resolve("/bad/svc"), None);
        assert_eq!(resolve("/plain/deep/svc"), None, "only the first component");
        assert_eq!(resolve("/missing/svc"), None);
        assert_eq!(resolve("relative/svc"), None);
        assert_eq!(resolve("/home/../etc"), None);
        Ok(())
    }

    #[test]
    fn test_parse_port_range_takes_exactly_two_ports() {
        assert_eq!(parse_port_range("32768\t60999\n"), Some((32_768, 60_999)));
        assert_eq!(parse_port_range("  40000   50000 "), Some((40_000, 50_000)));
        for bad in ["", "32768", "32768 60999 1", "a b", "70000 80000", "-1 5"] {
            assert_eq!(parse_port_range(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn test_build_takes_the_host_port_range() -> TestResult {
        let proc = proc_with_range("40000\t50000\n")?;
        let (config, summary) = build(None, Path::new("/nonexistent-harw-passwd"), proc.path());
        assert_eq!(config.ephemeral_ports(), 40_000..=50_000);
        assert_eq!(summary.ephemeral_source, PortRangeSource::Host);
        assert!(!summary.passwd_readable);
        assert_eq!(summary.home_dirs, 0);
        assert!(!summary.workspace_root);
        Ok(())
    }

    /// Unlesbar oder unzulässig: die Vorgabe, nie ein halber Bereich.
    #[test]
    fn test_build_falls_back_to_the_default_port_range() -> TestResult {
        let empty = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (config, summary) = build(None, Path::new("/nonexistent-harw-passwd"), empty.path());
        assert_eq!(
            config.ephemeral_ports(),
            RuleConfig::DEFAULT_EPHEMERAL_PORTS
        );
        assert_eq!(summary.ephemeral_source, PortRangeSource::DefaultUnreadable);

        let low = proc_with_range("80 50000\n")?;
        let (config, summary) = build(None, Path::new("/nonexistent-harw-passwd"), low.path());
        assert_eq!(
            config.ephemeral_ports(),
            RuleConfig::DEFAULT_EPHEMERAL_PORTS
        );
        assert_eq!(summary.ephemeral_source, PortRangeSource::DefaultRejected);
        assert_eq!(
            summary.ephemeral_ports,
            (
                *RuleConfig::DEFAULT_EPHEMERAL_PORTS.start(),
                *RuleConfig::DEFAULT_EPHEMERAL_PORTS.end()
            )
        );
        Ok(())
    }

    /// Die Workspace-Wurzel geht kanonisiert ein; `/` wird abgelehnt.
    #[cfg(unix)]
    #[test]
    fn test_build_canonicalises_the_workspace_root_and_refuses_the_root() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let real = dir.path().join("ws");
        std::fs::create_dir(&real).map_err(ctx("workspace"))?;
        let link = dir.path().join("ws-link");
        std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink"))?;
        let empty = tempfile::tempdir().map_err(ctx("tempdir"))?;

        let (config, summary) = build(
            Some(link.as_path()),
            Path::new("/nonexistent-harw-passwd"),
            empty.path(),
        );
        let canonical = std::fs::canonicalize(&real).map_err(ctx("canonical workspace"))?;
        let canonical_text = canonical
            .to_str()
            .ok_or(crate::test_support::TestError::Missing("utf-8 workspace"))?;
        assert_eq!(
            config.workspace_roots().to_vec(),
            vec![canonical_text.to_owned()]
        );
        assert!(summary.workspace_root);

        let (config, summary) = build(
            Some(Path::new("/")),
            Path::new("/nonexistent-harw-passwd"),
            empty.path(),
        );
        assert!(config.workspace_roots().is_empty());
        assert!(!summary.workspace_root);
        assert_eq!(summary.workspace_root_refused, Some("filesystem-root"));
        Ok(())
    }

    #[test]
    fn test_build_reads_the_homes_from_passwd() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let passwd = dir.path().join("passwd");
        std::fs::write(&passwd, PASSWD).map_err(ctx("write passwd"))?;
        let (config, summary) = build(None, &passwd, dir.path());
        assert!(summary.passwd_readable);
        assert_eq!(summary.home_dirs, config.home_dirs().len());
        assert!(config.home_dirs().contains(&"/var/lib/git".to_owned()));
        assert!(config.home_dirs().contains(&"/home/corp/bob".to_owned()));
        // `/usr/sbin` und `/` (abgelehnte Schreibweisen werden nie
        // kanonisiert, die Zahl hängt also nicht vom Testhost ab).
        assert_eq!(summary.home_dirs_refused, 2);
        assert!(
            !config.home_dirs().contains(&"/home/alice".to_owned()),
            "a home the rules know anyway is not added"
        );
        Ok(())
    }
}
