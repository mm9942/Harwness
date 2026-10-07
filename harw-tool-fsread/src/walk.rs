//! Begrenzter, symlinkfester Verzeichnis-Walk mit vollständigen Metadaten.
//!
//! # Verantwortung
//! Wie `harw_fsutil::walk_beneath`, aber (1) beginnt an einem **Unterpfad
//! unterhalb der Scope-Wurzel**, der selbst symlinkfrei geöffnet wird, und
//! (2) liefert je Eintrag ein vollständiges [`Meta`] (`mtime`, Rechte, Inode, …),
//! das `find`/`du`/`ls -R`/`tree` brauchen.
//!
//! # Sicherheit und Grenzen
//! - Symlinks werden nie gefolgt (`statat(SYMLINK_NOFOLLOW)`,
//!   `openat(O_NOFOLLOW | O_DIRECTORY)` relativ zum Eltern-Deskriptor).
//! - Zyklenschutz über `(st_dev, st_ino)` (Bind-Mounts).
//! - Geheimnis-Verzeichnisse ([`crate::scope::is_secret_path`]) werden gemeldet,
//!   aber nie betreten.
//! - Pro Verzeichnis werden nur die `rest + 1` kleinsten Namen gehalten:
//!   deterministische Reihenfolge und beschränkter Speicher auch bei
//!   Verzeichnissen mit Millionen Einträgen.
//! - Harte Grenzen für Tiefe, besuchte Einträge und Zeit
//!   ([`HARD_MAX_DEPTH`], [`HARD_MAX_VISITED`], [`WALK_TIMEOUT`]).
//! - Verschwindende Einträge (Wettlauf) zählen als `errors`, nie als Panic.
//!
//! # Nebenläufigkeit
//! Synchron, ohne geteilten Zustand; vom Aufrufer in `spawn_blocking` auszuführen.

use crate::meta::{Kind, Meta};
use crate::scope::{RelPath, Scope, is_secret_path};
use rustix::fs::{AtFlags, Dir, Mode, OFlags};
use rustix::io::retry_on_intr;
use std::collections::{BinaryHeap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Größte erlaubte Tiefe.
pub const HARD_MAX_DEPTH: usize = 32;

/// Größte Zahl besuchter Einträge je Walk.
pub const HARD_MAX_VISITED: usize = 100_000;

/// Zeitlimit eines Walks.
pub const WALK_TIMEOUT: Duration = Duration::from_secs(5);

/// Wie oft die Deadline beim Einlesen eines Verzeichnisses geprüft wird.
const DEADLINE_CHECK_INTERVAL: usize = 64;

/// Flags zum Betreten eines Unterverzeichnisses.
const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Grenzen eines Walks.
#[derive(Debug, Clone, Copy)]
pub struct WalkOpts {
    /// Tiefe der gemeldeten Einträge (Kinder der Startwurzel haben Tiefe 1).
    pub max_depth: usize,
    /// Höchstzahl besuchter Einträge.
    pub max_entries: usize,
    /// Abbruchzeitpunkt.
    pub deadline: Instant,
}

impl WalkOpts {
    /// Grenzen mit Obergrenzen ([`HARD_MAX_DEPTH`], [`HARD_MAX_VISITED`]) und
    /// Frist [`WALK_TIMEOUT`].
    #[must_use]
    pub fn bounded(max_depth: usize, max_entries: usize) -> Self {
        Self {
            max_depth: max_depth.min(HARD_MAX_DEPTH),
            max_entries: max_entries.clamp(1, HARD_MAX_VISITED),
            deadline: Instant::now() + WALK_TIMEOUT,
        }
    }
}

/// Ein besuchter Eintrag.
#[derive(Debug)]
pub struct Entry<'a> {
    /// Pfad relativ zur **Scope-Wurzel**.
    pub rel: &'a Path,
    /// Name im Elternverzeichnis.
    pub name: &'a OsStr,
    /// Tiefe (Kinder der Startwurzel = 1).
    pub depth: usize,
    /// Metadaten des Eintrags selbst.
    pub meta: &'a Meta,
}

/// Entscheidung des Besuchers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Weiter, Verzeichnisse betreten.
    Continue,
    /// Weiter, dieses Verzeichnis nicht betreten.
    SkipDir,
    /// Walk beenden.
    Stop,
}

/// Grund, aus dem ein Walk vorzeitig endete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Höchstzahl besuchter Einträge erreicht.
    EntryLimit,
    /// Frist abgelaufen.
    Deadline,
    /// Der Besucher hat abgebrochen.
    Caller,
}

impl StopReason {
    /// Stabiler Name für das Feld `stopped`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EntryLimit => "walk_entry_limit",
            Self::Deadline => "walk_deadline",
            Self::Caller => "result_limit",
        }
    }
}

/// Bericht über einen Walk.
#[derive(Debug, Default, Clone, Copy)]
pub struct Report {
    /// Vorzeitiges Ende, falls eines.
    pub stopped: Option<StopReason>,
    /// Mindestens ein Verzeichnis wurde wegen der Tiefe nicht betreten.
    pub depth_limited: bool,
    /// Zahl nicht betretener Geheimnis-Verzeichnisse.
    pub denied_dirs: usize,
    /// Zahl nicht lesbarer/verschwundener Einträge.
    pub errors: usize,
    /// Besuchte Einträge.
    pub visited: usize,
}

struct Ctx<'v> {
    opts: WalkOpts,
    report: Report,
    seen: HashSet<(u64, u64)>,
    visit: &'v mut dyn FnMut(&Entry<'_>) -> Flow,
}

/// Führt einen Walk ab `start` (relativ zur Scope-Wurzel) aus.
///
/// `start` selbst wird nicht gemeldet.
///
/// # Errors
/// Wenn `start` kein symlinkfrei erreichbares Verzeichnis ist.
pub fn walk<F>(scope: &Scope, start: &RelPath, opts: WalkOpts, mut visit: F) -> io::Result<Report>
where
    F: FnMut(&Entry<'_>) -> Flow,
{
    let fd = scope.open_dir(start)?;
    let stat = rustix::fs::fstat(&fd)?;
    let meta = Meta::from_stat(&stat);
    let mut ctx = Ctx {
        opts,
        report: Report::default(),
        seen: HashSet::new(),
        visit: &mut visit,
    };
    ctx.seen.insert((meta.dev, meta.ino));
    if opts.max_depth > 0 {
        ctx.read_dir(&fd, start.as_path(), 0);
    } else {
        ctx.report.depth_limited = true;
    }
    Ok(ctx.report)
}

impl Ctx<'_> {
    fn deadline_passed(&self) -> bool {
        Instant::now() >= self.opts.deadline
    }

    /// Liest und besucht ein Verzeichnis. `false` = Walk beendet.
    fn read_dir(&mut self, fd: &OwnedFd, rel: &Path, depth: usize) -> bool {
        let capacity = self
            .opts
            .max_entries
            .saturating_sub(self.report.visited)
            .saturating_add(1);
        let names = match smallest_names(fd, capacity, self.opts.deadline) {
            Ok(Some(names)) => names,
            Ok(None) => {
                self.report.stopped = Some(StopReason::Deadline);
                return false;
            }
            Err(_) => {
                self.report.errors += 1;
                return true;
            }
        };
        for name in names {
            if self.report.visited >= self.opts.max_entries {
                self.report.stopped = Some(StopReason::EntryLimit);
                return false;
            }
            if self.deadline_passed() {
                self.report.stopped = Some(StopReason::Deadline);
                return false;
            }
            self.report.visited += 1;
            let stat = match retry_on_intr(|| {
                rustix::fs::statat(fd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
            }) {
                Ok(stat) => stat,
                Err(_) => {
                    self.report.errors += 1;
                    continue;
                }
            };
            let meta = Meta::from_stat(&stat);
            let child_rel: PathBuf = rel.join(&name);
            let entry = Entry {
                rel: &child_rel,
                name: &name,
                depth: depth + 1,
                meta: &meta,
            };
            match (self.visit)(&entry) {
                Flow::Stop => {
                    self.report.stopped = Some(StopReason::Caller);
                    return false;
                }
                Flow::SkipDir => continue,
                Flow::Continue => {}
            }
            if meta.kind != Kind::Dir {
                continue;
            }
            if is_secret_path(&child_rel) {
                self.report.denied_dirs += 1;
                continue;
            }
            if depth + 1 >= self.opts.max_depth {
                self.report.depth_limited = true;
                continue;
            }
            let child_fd = match retry_on_intr(|| {
                rustix::fs::openat(fd, name.as_os_str(), DIR_FLAGS, Mode::empty())
            }) {
                Ok(child_fd) => child_fd,
                Err(_) => {
                    self.report.errors += 1;
                    continue;
                }
            };
            let Ok(child_stat) = rustix::fs::fstat(&child_fd) else {
                self.report.errors += 1;
                continue;
            };
            let child_meta = Meta::from_stat(&child_stat);
            if !self.seen.insert((child_meta.dev, child_meta.ino)) {
                continue;
            }
            if !self.read_dir(&child_fd, &child_rel, depth + 1) {
                return false;
            }
        }
        true
    }
}

/// Liest die Namen eines Verzeichnisses (ohne `.`/`..`) und behält die
/// `capacity` kleinsten, sortiert. `Ok(None)` bei abgelaufener Frist.
pub fn smallest_names(
    fd: &OwnedFd,
    capacity: usize,
    deadline: Instant,
) -> io::Result<Option<Vec<OsString>>> {
    let dir = Dir::read_from(fd.as_fd())?;
    let mut heap: BinaryHeap<OsString> = BinaryHeap::new();
    for (index, entry) in dir.enumerate() {
        if index % DEADLINE_CHECK_INTERVAL == 0 && Instant::now() >= deadline {
            return Ok(None);
        }
        let entry = entry?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        heap.push(OsString::from_vec(bytes.to_vec()));
        if heap.len() > capacity {
            heap.pop();
        }
    }
    Ok(Some(heap.into_sorted_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult};

    fn collect(fx: &Fixture, start: &str, opts: WalkOpts) -> TestResult<(Vec<String>, Report)> {
        let scope = fx.scope()?;
        let mut out = Vec::new();
        let report = walk(&scope, &scope.rel(start)?, opts, |entry| {
            out.push(entry.rel.to_string_lossy().into_owned());
            Flow::Continue
        })?;
        Ok((out, report))
    }

    #[test]
    fn sorted_preorder_without_following_symlinks() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("b/inner/z", b"")?;
        fx.write("c.txt", b"12345")?;
        fx.write("a.txt", b"1")?;
        fx.plant_escapes()?;
        let (names, report) = collect(&fx, ".", WalkOpts::bounded(10, 1000))?;
        assert_eq!(
            names,
            vec![
                "a.txt",
                "b",
                "b/inner",
                "b/inner/z",
                "c.txt",
                "link_dir",
                "link_file",
                "loop",
                "nested",
                "nested/up"
            ]
        );
        assert_eq!(report.stopped, None);
        assert_eq!(report.errors, 0);
        Ok(())
    }

    #[test]
    fn depth_limit_is_reported() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a/b/c/d.txt", b"")?;
        let (names, report) = collect(&fx, ".", WalkOpts::bounded(2, 1000))?;
        assert_eq!(names, vec!["a", "a/b"]);
        assert!(report.depth_limited);
        assert_eq!(report.stopped, None);
        Ok(())
    }

    #[test]
    fn entry_limit_keeps_the_smallest_names_deterministically() -> TestResult {
        let fx = Fixture::new()?;
        for index in 0..300 {
            fx.write(&format!("many/f{index:04}"), b"")?;
        }
        let (names, report) = collect(&fx, "many", WalkOpts::bounded(3, 5))?;
        assert_eq!(
            names,
            vec![
                "many/f0000",
                "many/f0001",
                "many/f0002",
                "many/f0003",
                "many/f0004"
            ]
        );
        assert_eq!(report.stopped, Some(StopReason::EntryLimit));
        Ok(())
    }

    #[test]
    fn secret_directories_are_listed_but_not_entered() -> TestResult {
        let fx = Fixture::new()?;
        fx.write(".ssh/id_ed25519", b"KEY")?;
        fx.write("src/lib.rs", b"")?;
        let (names, report) = collect(&fx, ".", WalkOpts::bounded(5, 100))?;
        assert!(names.contains(&".ssh".to_owned()));
        assert!(
            !names.iter().any(|name| name.contains("id_ed25519")),
            "{names:?}"
        );
        assert_eq!(report.denied_dirs, 1);
        Ok(())
    }

    #[test]
    fn start_must_be_symlink_free_directory() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        let scope = fx.scope()?;
        let opts = WalkOpts::bounded(3, 100);
        assert!(walk(&scope, &scope.rel("link_dir")?, opts, |_| Flow::Continue).is_err());
        assert!(
            walk(&scope, &scope.rel("link_dir/deep")?, opts, |_| {
                Flow::Continue
            })
            .is_err()
        );
        assert!(walk(&scope, &scope.rel("missing")?, opts, |_| Flow::Continue).is_err());
        Ok(())
    }

    #[test]
    fn caller_can_stop_and_skip() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a/x", b"")?;
        fx.write("b/y", b"")?;
        let scope = fx.scope()?;
        let mut seen = Vec::new();
        let report = walk(
            &scope,
            &RelPath::root(),
            WalkOpts::bounded(5, 100),
            |entry| {
                seen.push(entry.rel.to_string_lossy().into_owned());
                if entry.name == "a" {
                    Flow::SkipDir
                } else if entry.name == "y" {
                    Flow::Stop
                } else {
                    Flow::Continue
                }
            },
        )?;
        assert_eq!(seen, vec!["a", "b", "b/y"]);
        assert_eq!(report.stopped, Some(StopReason::Caller));
        Ok(())
    }
}
