//! Beobachtungsbereich mit Home-Einträgen — PL-100 WP1.6.
//!
//! # Warum ein Filter neben dem `ReadScope`
//! Die Regel `persistence-write` braucht Schreibzugriffe auf Orte wie
//! `~/.ssh`, `~/.config/systemd/user`, `~/.config/autostart` und die
//! Shell-Startdateien — für **jedes** Home-Verzeichnis, ohne dass ein
//! Benutzername in der Konfiguration steht. Ganz `/home` als Wurzel zu
//! melden hieße, jeden Browser-Cache-Schreibzugriff an den Sentinel zu
//! schicken (Flut, Bedrohung B2/B10). Einzelne Pfade wie `/home/a/.ssh` als
//! Wurzeln zu nehmen hieße, Benutzernamen festzuschreiben und Verzeichnisse
//! zu übersehen, die ein Angreifer erst anlegt (`~/.config/autostart` fehlt
//! oft).
//!
//! [`WatchFilter`] trennt deshalb zwei Arten von Wurzeln:
//! - **`scope_roots`**: jede Schreibänderung darunter zählt (z. B. `/etc`).
//! - **`home_roots`**: Verzeichnisse mit Home-Verzeichnissen (`/home`,
//!   `/var/home`) oder selbst ein Home-Verzeichnis (`/root`). Darunter zählt
//!   ein Pfad nur, wenn er nach höchstens [`MAX_HOME_DEPTH`] Komponenten
//!   (etwa dem Benutzernamen, bei verschachtelten Layouts auch einer Domäne)
//!   mit einem Eintrag aus `home_entries` beginnt — komponentenweise,
//!   `.bashrc` trifft also nie `.bashrc.bak`.
//!
//! Der `ReadScope` der Sonde (und damit Landlock und die fanotify-Marken)
//! umfasst beide Wurzelarten vollständig, damit auch ein erst später
//! angelegtes Verzeichnis gemeldet werden kann; nur die Weitergabe wird
//! gefiltert.
//!
//! # Exportierte Elemente
//! [`WatchFilter`], [`MAX_HOME_DEPTH`].
//!
//! # Nebenläufigkeit
//! Unveränderlicher Werttyp, `Send + Sync`; [`WatchFilter::admits`] macht
//! keine Ein-/Ausgabe.
//!
//! # Fehler
//! Keine.
//!
//! # Examples
//! ```rust
//! use harw_dod_fsmon::filter::WatchFilter;
//! use std::path::{Path, PathBuf};
//!
//! let filter = WatchFilter::new(
//!     vec![PathBuf::from("/etc")],
//!     vec![PathBuf::from("/home"), PathBuf::from("/root")],
//!     vec![PathBuf::from(".ssh"), PathBuf::from(".bashrc")],
//! );
//! assert!(filter.admits(Path::new("/etc/cron.d/job")));
//! assert!(filter.admits(Path::new("/home/alice/.ssh/authorized_keys")));
//! assert!(filter.admits(Path::new("/root/.bashrc")));
//! assert!(!filter.admits(Path::new("/home/alice/.cache/x")));
//! assert!(!filter.admits(Path::new("/var/log/syslog")));
//! ```

use std::path::{Component, Path, PathBuf};

use harw_dod_cap::ReadScope;

/// Wie viele führende Komponenten unter einer Home-Wurzel höchstens
/// übersprungen werden, bevor ein Home-Eintrag passen muss: `0` für
/// `/root/.ssh`, `1` für `/home/<name>/.ssh`, `2` für
/// `/home/<domäne>/<name>/.ssh`.
pub const MAX_HOME_DEPTH: usize = 2;

/// Beobachtungsbereich aus vollständigen Wurzeln, Home-Wurzeln und
/// Home-Einträgen.
///
/// # Description
/// Siehe Moduldoku. Die Pfade werden so übernommen, wie sie übergeben
/// werden; die Formprüfung (absolute Wurzeln ohne `.`/`..`, nie `/`,
/// relative, nicht leere Einträge) macht der Aufrufer
/// (`harw_dod_config::ProbeFsSettings` bzw. die Kommandozeile der Sonde).
/// Ein leerer Eintrag passt defensiv nie.
///
/// # Concurrency
/// `Send + Sync`.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchFilter {
    // Wurzeln, unter denen jede Schreibänderung zählt.
    scope_roots: Vec<PathBuf>,
    // Wurzeln, unter denen nur Home-Einträge zählen.
    home_roots: Vec<PathBuf>,
    // Pfade relativ zu einem Home-Verzeichnis.
    home_entries: Vec<PathBuf>,
}

impl WatchFilter {
    /// Baut den Filter.
    ///
    /// # Arguments
    /// - `scope_roots` (`Vec<PathBuf>`): vollständig beobachtete Wurzeln.
    /// - `home_roots` (`Vec<PathBuf>`): Home-Wurzeln.
    /// - `home_entries` (`Vec<PathBuf>`): relative Home-Einträge.
    ///
    /// # Returns
    /// Den Filter.
    #[must_use]
    pub fn new(
        scope_roots: Vec<PathBuf>,
        home_roots: Vec<PathBuf>,
        home_entries: Vec<PathBuf>,
    ) -> Self {
        Self {
            scope_roots,
            home_roots,
            home_entries,
        }
    }

    /// Die vollständig beobachteten Wurzeln.
    #[must_use]
    pub fn scope_roots(&self) -> &[PathBuf] {
        &self.scope_roots
    }

    /// Die Home-Wurzeln.
    #[must_use]
    pub fn home_roots(&self) -> &[PathBuf] {
        &self.home_roots
    }

    /// Die relativen Home-Einträge.
    #[must_use]
    pub fn home_entries(&self) -> &[PathBuf] {
        &self.home_entries
    }

    /// Ob der Filter keine einzige Wurzel hat (dann gäbe es nichts zu
    /// beobachten).
    #[must_use]
    pub fn has_no_roots(&self) -> bool {
        self.scope_roots.is_empty() && self.home_roots.is_empty()
    }

    /// Der Lesebereich aus beiden Wurzelarten — die Grundlage für den
    /// `SensorHandle`, die Landlock-Regel und die fanotify-Marken der Sonde.
    ///
    /// # Returns
    /// `ReadScope::from_roots(scope_roots ∪ home_roots)`.
    #[must_use]
    pub fn read_scope(&self) -> ReadScope {
        ReadScope::from_roots(
            self.scope_roots
                .iter()
                .chain(self.home_roots.iter())
                .cloned(),
        )
    }

    /// Ob ein geschriebener Pfad weitergereicht wird.
    ///
    /// # Arguments
    /// - `path` (`&Path`): der vom Kernel aufgelöste, absolute Pfad.
    ///
    /// # Returns
    /// `true` unter einer `scope_root`; unter einer `home_root` nur, wenn
    /// der Rest nach höchstens [`MAX_HOME_DEPTH`] Komponenten mit einem
    /// Home-Eintrag beginnt; sonst und bei jeder `..`-Komponente `false`.
    #[must_use]
    pub fn admits(&self, path: &Path) -> bool {
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return false;
        }
        if self.scope_roots.iter().any(|root| path.starts_with(root)) {
            return true;
        }
        self.home_roots.iter().any(|root| {
            path.strip_prefix(root)
                .is_ok_and(|rest| self.matches_home_entry(rest))
        })
    }

    // Ob `rest` (relativ zur Home-Wurzel) nach höchstens `MAX_HOME_DEPTH`
    // Komponenten mit einem nicht leeren Home-Eintrag beginnt.
    fn matches_home_entry(&self, rest: &Path) -> bool {
        (0..=MAX_HOME_DEPTH).any(|skip| {
            let tail: PathBuf = rest.components().skip(skip).collect();
            self.home_entries
                .iter()
                .any(|entry| !entry.as_os_str().is_empty() && tail.starts_with(entry))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::WatchFilter;

    fn filter() -> WatchFilter {
        WatchFilter::new(
            vec![PathBuf::from("/etc"), PathBuf::from("/var/spool/cron")],
            vec![
                PathBuf::from("/home"),
                PathBuf::from("/var/home"),
                PathBuf::from("/root"),
            ],
            vec![
                PathBuf::from(".ssh"),
                PathBuf::from(".config/systemd"),
                PathBuf::from(".config/autostart"),
                PathBuf::from(".bashrc"),
            ],
        )
    }

    #[test]
    fn test_scope_roots_admit_everything_below_them() {
        let filter = filter();
        for path in [
            "/etc/ld.so.preload",
            "/etc/systemd/system/x.service",
            "/var/spool/cron/root",
        ] {
            assert!(filter.admits(Path::new(path)), "{path}");
        }
        assert!(!filter.admits(Path::new("/etcetera/x")), "component-wise");
        assert!(!filter.admits(Path::new("/var/log/syslog")));
    }

    /// Home-Einträge passen unter jedem Home-Verzeichnis, ohne Benutzernamen
    /// in der Konfiguration, auch auf ostree-Hosts und für root.
    #[test]
    fn test_home_entries_match_below_any_home_directory() {
        let filter = filter();
        for path in [
            "/home/alice/.ssh/authorized_keys",
            "/home/bob/.config/systemd/user/evil.service",
            "/home/bob/.config/autostart/evil.desktop",
            "/var/home/carol/.bashrc",
            "/root/.ssh/authorized_keys",
            "/root/.bashrc",
            "/home/corp.example/dave/.ssh/rc",
        ] {
            assert!(filter.admits(Path::new(path)), "{path}");
        }
    }

    /// Alles andere unter einer Home-Wurzel bleibt draußen (keine Flut aus
    /// Caches), auch Namen, die nur mit einem Eintrag anfangen.
    #[test]
    fn test_other_home_paths_are_not_admitted() {
        let filter = filter();
        for path in [
            "/home/alice/.cache/mozilla/x",
            "/home/alice/.bashrc.bak",
            "/home/alice/.config/systemd-backup/x",
            "/home/alice/a/b/c/.ssh/x",
            "/home",
            "/root/notes.txt",
        ] {
            assert!(!filter.admits(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn test_parent_dir_components_and_empty_entries_never_match() {
        let filter = WatchFilter::new(
            vec![PathBuf::from("/etc")],
            vec![PathBuf::from("/home")],
            vec![PathBuf::new()],
        );
        assert!(!filter.admits(Path::new("/etc/../root/.ssh/x")));
        assert!(!filter.admits(Path::new("/home/alice/anything")));
    }

    #[test]
    fn test_read_scope_covers_both_root_kinds_and_detects_an_empty_filter() {
        let filter = filter();
        let scope = filter.read_scope();
        assert!(scope.allows(Path::new("/etc/hosts")));
        assert!(scope.allows(Path::new("/home/alice/.cache/x")));
        assert!(!scope.allows(Path::new("/var/log/syslog")));
        assert!(!filter.has_no_roots());
        assert!(WatchFilter::default().has_no_roots());
    }
}
