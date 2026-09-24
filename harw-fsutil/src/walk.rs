//! Begrenzter, symlinkfester Verzeichnis-Walk.
//!
//! [`walk_beneath`] liefert einen [`WalkBeneath`]-Iterator in
//! Tiefensuche-Vorordnung (ein Verzeichnis-Eintrag erscheint direkt vor
//! seinem Inhalt). Innerhalb eines Verzeichnisses sind die Namen bytewise
//! sortiert, die Reihenfolge ist also unabhängig von der `readdir`-Ordnung
//! des Dateisystems.
//!
//! # Sicherheitseigenschaften
//! - Symlinks werden **nie** gefolgt, sondern als [`EntryType::Symlink`]
//!   gemeldet (`fstatat(AT_SYMLINK_NOFOLLOW)`).
//! - Unterverzeichnisse werden relativ zum Eltern-Deskriptor mit
//!   `openat(O_DIRECTORY | O_NOFOLLOW)` geöffnet; wird ein Verzeichnis
//!   zwischen `fstatat` und `openat` gegen einen Symlink getauscht, scheitert
//!   das Öffnen (Fehler-Item), statt dem Symlink zu folgen.
//! - Zyklenschutz über `(st_dev, st_ino)` aller betretenen Verzeichnisse
//!   (z. B. Bind-Mounts); ein bereits betretenes Verzeichnis wird still
//!   übersprungen (sein Eintrag wird trotzdem gemeldet).
//! - Speicher je Verzeichnis ist durch das verbleibende Entry-Budget begrenzt:
//!   es werden nur die `rest + 1` kleinsten Namen gehalten.

use std::collections::{BinaryHeap, HashSet};
use std::ffi::OsString;
use std::io;
use std::iter::FusedIterator;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rustix::fs::{AtFlags, Dir, FileType, Mode, Stat};
use rustix::io::retry_on_intr;

use crate::open::{READABLE_DIR_FLAGS, open_dir_nofollow};

/// Wie oft (in `readdir`-Einträgen) die Deadline beim Einlesen eines
/// Verzeichnisses geprüft wird.
const DEADLINE_CHECK_INTERVAL: usize = 64;

/// Grenzen eines [`walk_beneath`]-Laufs.
///
/// # Examples
/// ```rust
/// use harw_fsutil::WalkLimits;
/// use std::time::{Duration, Instant};
///
/// let limits = WalkLimits {
///     max_depth: 8,
///     max_entries: 10_000,
///     deadline: Some(Instant::now() + Duration::from_secs(2)),
/// };
/// assert_eq!(limits.max_depth, 8);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkLimits {
    /// Maximale Tiefe gemeldeter Einträge; direkte Kinder der Wurzel haben
    /// Tiefe 1. `0` liefert nichts.
    pub max_depth: usize,
    /// Maximale Anzahl gelieferter Items (Einträge **und** Fehler, auch
    /// Fehler beim Betreten eines Unterverzeichnisses).
    pub max_entries: usize,
    /// Zeitpunkt, ab dem der Walk abbricht (`Instant::now() >= deadline`).
    pub deadline: Option<Instant>,
}

/// Typ eines Eintrags laut `fstatat(AT_SYMLINK_NOFOLLOW)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryType {
    /// Reguläre Datei.
    File,
    /// Verzeichnis.
    Dir,
    /// Symbolischer Link (nie gefolgt).
    Symlink,
    /// FIFO, Socket, Gerät o. ä.
    Other,
}

/// Ein Eintrag des Walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    /// Pfad relativ zur Wurzel (ohne führendes `./`).
    pub rel_path: PathBuf,
    /// Typ des Eintrags selbst (bei Symlinks nicht der des Ziels).
    pub entry_type: EntryType,
    /// `st_size` für [`EntryType::File`] und [`EntryType::Symlink`] (Länge
    /// des Link-Ziels), `0` für Verzeichnisse und sonstige Typen.
    pub len: u64,
}

/// Grund, aus dem ein Walk unvollständig ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WalkStop {
    /// Mindestens ein Verzeichnis wurde wegen `max_depth` nicht betreten
    /// (auch wenn es leer ist). Der Walk läuft für den Rest weiter.
    DepthLimit,
    /// `max_entries` war erreicht und es gab weitere Einträge; Walk beendet.
    EntryLimit,
    /// Die Deadline ist abgelaufen; Walk beendet.
    Deadline,
}

/// Iterator von [`walk_beneath`].
///
/// Liefert `Ok(WalkEntry)` oder `Err` für einzelne Einträge/Verzeichnisse,
/// die nicht gelesen werden konnten (der Walk läuft danach weiter). Fehler
/// behalten ihren `errno`, tragen aber keinen Pfad.
#[derive(Debug)]
pub struct WalkBeneath {
    limits: WalkLimits,
    stack: Vec<Frame>,
    pending: Option<PendingDir>,
    visited: HashSet<(u64, u64)>,
    yielded: usize,
    stopped: Option<WalkStop>,
    finished: bool,
}

/// Ein geöffnetes, eingelesenes Verzeichnis.
#[derive(Debug)]
struct Frame {
    fd: OwnedFd,
    rel: PathBuf,
    depth: usize,
    names: Vec<OsString>,
    next: usize,
}

/// Ein gemeldetes Verzeichnis, das beim nächsten `next()` betreten wird.
#[derive(Debug)]
struct PendingDir {
    name: OsString,
    rel: PathBuf,
    depth: usize,
}

/// Startet einen begrenzten Walk unter `root`.
///
/// `root` selbst wird mit [`open_dir_nofollow`] geöffnet (letztes Glied kein
/// Symlink) und nicht als Eintrag gemeldet.
///
/// # Errors
/// Fehler beim Öffnen, `fstat` oder Einlesen der Wurzel.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{EntryType, WalkLimits, walk_beneath};
/// use std::path::Path;
///
/// let limits = WalkLimits { max_depth: 4, max_entries: 1_000, deadline: None };
/// let mut walk = walk_beneath(Path::new("/home/user/.harw/memory"), limits)?;
/// for entry in walk.by_ref() {
///     let entry = entry?;
///     if entry.entry_type == EntryType::File {
///         println!("{} ({} Bytes)", entry.rel_path.display(), entry.len);
///     }
/// }
/// if let Some(stop) = walk.stopped() {
///     eprintln!("unvollständig: {stop:?}");
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn walk_beneath(root: &Path, limits: WalkLimits) -> io::Result<WalkBeneath> {
    let fd = open_dir_nofollow(root)?;
    let stat = rustix::fs::fstat(&fd)?;
    let mut walk = WalkBeneath {
        limits,
        stack: Vec::new(),
        pending: None,
        visited: HashSet::new(),
        yielded: 0,
        stopped: None,
        finished: false,
    };
    walk.visited.insert(dev_ino(&stat));

    if deadline_passed(limits.deadline) {
        walk.finish(Some(WalkStop::Deadline));
        return Ok(walk);
    }
    if limits.max_depth == 0 {
        walk.finish(Some(WalkStop::DepthLimit));
        return Ok(walk);
    }
    match read_sorted_names(fd.as_fd(), walk.name_capacity(), limits.deadline)? {
        Some(names) => walk.stack.push(Frame {
            fd,
            rel: PathBuf::new(),
            depth: 0,
            names,
            next: 0,
        }),
        None => walk.finish(Some(WalkStop::Deadline)),
    }
    Ok(walk)
}

impl WalkBeneath {
    /// Grund, aus dem der Walk unvollständig ist, oder `None`, wenn er (bis
    /// zur aktuellen Position) vollständig war.
    ///
    /// [`WalkStop::EntryLimit`] und [`WalkStop::Deadline`] beenden die
    /// Iteration und haben Vorrang vor [`WalkStop::DepthLimit`]. Endgültig
    /// aussagekräftig erst, nachdem der Iterator `None` geliefert hat.
    #[must_use]
    pub fn stopped(&self) -> Option<WalkStop> {
        self.stopped
    }

    /// Wie viele Namen ein Verzeichnis höchstens im Speicher halten muss.
    fn name_capacity(&self) -> usize {
        self.limits
            .max_entries
            .saturating_sub(self.yielded)
            .saturating_add(1)
    }

    /// Beendet den Walk; `Some(stop)` überschreibt einen früheren Grund.
    fn finish(&mut self, stop: Option<WalkStop>) {
        if stop.is_some() {
            self.stopped = stop;
        }
        self.stack.clear();
        self.pending = None;
        self.finished = true;
    }

    /// Öffnet und liest ein gemeldetes Unterverzeichnis ein.
    fn enter(&mut self, pending: PendingDir) -> io::Result<()> {
        let Some(parent) = self.stack.last() else {
            return Ok(());
        };
        let fd = retry_on_intr(|| {
            rustix::fs::openat(&parent.fd, &pending.name, READABLE_DIR_FLAGS, Mode::empty())
        })?;
        let stat = rustix::fs::fstat(&fd)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(io::Error::from(rustix::io::Errno::NOTDIR));
        }
        if !self.visited.insert(dev_ino(&stat)) {
            // Zyklus oder bereits betreten (Bind-Mount): nicht erneut absteigen.
            return Ok(());
        }
        match read_sorted_names(fd.as_fd(), self.name_capacity(), self.limits.deadline)? {
            Some(names) => self.stack.push(Frame {
                fd,
                rel: pending.rel,
                depth: pending.depth,
                names,
                next: 0,
            }),
            None => self.finish(Some(WalkStop::Deadline)),
        }
        Ok(())
    }
}

impl Iterator for WalkBeneath {
    type Item = io::Result<WalkEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.finished {
                return None;
            }
            if deadline_passed(self.limits.deadline) {
                self.finish(Some(WalkStop::Deadline));
                return None;
            }
            if let Some(pending) = self.pending.take() {
                match self.enter(pending) {
                    Ok(()) => continue,
                    Err(err) => {
                        // Fehler-Items zählen wie Einträge gegen `max_entries`
                        // (siehe `WalkLimits::max_entries`). Ist das Budget
                        // bereits verbraucht, wäre dieser Fehler ein weiteres
                        // Item → `EntryLimit` statt Auslieferung.
                        if self.yielded >= self.limits.max_entries {
                            self.finish(Some(WalkStop::EntryLimit));
                            return None;
                        }
                        self.yielded += 1;
                        return Some(Err(err));
                    }
                }
            }

            let top_exhausted = match self.stack.last() {
                Some(frame) => frame.next >= frame.names.len(),
                None => {
                    self.finish(None);
                    return None;
                }
            };
            if top_exhausted {
                self.stack.pop();
                continue;
            }
            if self.yielded >= self.limits.max_entries {
                self.finish(Some(WalkStop::EntryLimit));
                return None;
            }

            let frame = self.stack.last_mut()?;
            let name = std::mem::take(&mut frame.names[frame.next]);
            frame.next += 1;
            let rel_path = frame.rel.join(&name);
            let depth = frame.depth + 1;
            let stat =
                retry_on_intr(|| rustix::fs::statat(&frame.fd, &name, AtFlags::SYMLINK_NOFOLLOW));
            self.yielded += 1;

            let stat = match stat {
                Ok(stat) => stat,
                Err(errno) => return Some(Err(errno.into())),
            };
            let size = u64::try_from(stat.st_size).unwrap_or(0);
            let (entry_type, len) = match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => (EntryType::File, size),
                FileType::Symlink => (EntryType::Symlink, size),
                FileType::Directory => (EntryType::Dir, 0),
                _ => (EntryType::Other, 0),
            };
            if entry_type == EntryType::Dir {
                if depth < self.limits.max_depth {
                    self.pending = Some(PendingDir {
                        name,
                        rel: rel_path.clone(),
                        depth,
                    });
                } else if self.stopped.is_none() {
                    self.stopped = Some(WalkStop::DepthLimit);
                }
            }
            return Some(Ok(WalkEntry {
                rel_path,
                entry_type,
                len,
            }));
        }
    }
}

impl FusedIterator for WalkBeneath {}

/// Liest die Namen eines Verzeichnisses (ohne `.`/`..`) sortiert ein und
/// behält nur die `capacity` kleinsten. `Ok(None)` bei abgelaufener Deadline.
fn read_sorted_names(
    fd: BorrowedFd<'_>,
    capacity: usize,
    deadline: Option<Instant>,
) -> io::Result<Option<Vec<OsString>>> {
    let dir = Dir::read_from(fd)?;
    let mut heap: BinaryHeap<OsString> = BinaryHeap::new();
    for (index, entry) in dir.enumerate() {
        if index % DEADLINE_CHECK_INTERVAL == 0 && deadline_passed(deadline) {
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

/// `true`, wenn eine Deadline gesetzt und erreicht ist.
fn deadline_passed(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// `(st_dev, st_ino)` als `u64`-Paar.
///
/// Die Feldtypen von `Stat` sind architektur- und backendabhängig
/// (`c_ulong`, `u64`, auf macOS `i32` für `st_dev`); die Casts sind je nach
/// Ziel verlustfrei oder no-op.
#[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
fn dev_ino(stat: &Stat) -> (u64, u64) {
    (stat.st_dev as u64, stat.st_ino as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    const UNBEGRENZT: WalkLimits = WalkLimits {
        max_depth: usize::MAX,
        max_entries: usize::MAX,
        deadline: None,
    };

    fn collect(walk: &mut WalkBeneath) -> TestResult<Vec<(String, EntryType)>> {
        walk.by_ref()
            .map(|entry| {
                let entry = entry?;
                Ok((
                    entry.rel_path.to_string_lossy().into_owned(),
                    entry.entry_type,
                ))
            })
            .collect()
    }

    #[test]
    fn sortierte_vorordnung_mit_typen_und_laengen() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        std::fs::create_dir_all(root.join("b/inner"))?;
        std::fs::write(root.join("c.txt"), b"12345")?;
        std::fs::write(root.join("a.txt"), b"1")?;
        std::fs::write(root.join("b/inner/z"), b"")?;
        symlink("c.txt", root.join("b/link"))?;

        let mut walk = walk_beneath(root, UNBEGRENZT)?;
        let entries: Vec<WalkEntry> = walk.by_ref().collect::<io::Result<Vec<_>>>()?;
        let summary: Vec<(&str, EntryType, u64)> = entries
            .iter()
            .map(|entry| {
                Ok::<_, TestError>((
                    entry.rel_path.to_str().ok_or(TestError::Missing("utf8"))?,
                    entry.entry_type,
                    entry.len,
                ))
            })
            .collect::<TestResult<Vec<_>>>()?;
        assert_eq!(
            summary,
            vec![
                ("a.txt", EntryType::File, 1),
                ("b", EntryType::Dir, 0),
                ("b/inner", EntryType::Dir, 0),
                ("b/inner/z", EntryType::File, 0),
                ("b/link", EntryType::Symlink, 5),
                ("c.txt", EntryType::File, 5),
            ]
        );
        assert_eq!(walk.stopped(), None);
        assert!(walk.next().is_none(), "fused");
        Ok(())
    }

    #[test]
    fn schleife_a_zeigt_auf_punkt_terminiert() -> TestResult {
        let tmp = tempfile::tempdir()?;
        symlink(".", tmp.path().join("a"))?;
        std::fs::create_dir(tmp.path().join("d"))?;
        symlink("..", tmp.path().join("d/up"))?;

        let mut walk = walk_beneath(tmp.path(), UNBEGRENZT)?;
        assert_eq!(
            collect(&mut walk)?,
            vec![
                ("a".to_owned(), EntryType::Symlink),
                ("d".to_owned(), EntryType::Dir),
                ("d/up".to_owned(), EntryType::Symlink),
            ]
        );
        assert_eq!(walk.stopped(), None);
        Ok(())
    }

    #[test]
    fn dangling_symlink_wird_gemeldet() -> TestResult {
        let tmp = tempfile::tempdir()?;
        symlink("fehlt/nirgends", tmp.path().join("dangling"))?;
        let mut walk = walk_beneath(tmp.path(), UNBEGRENZT)?;
        let entries: Vec<WalkEntry> = walk.by_ref().collect::<io::Result<Vec<_>>>()?;
        assert_eq!(
            entries,
            vec![WalkEntry {
                rel_path: PathBuf::from("dangling"),
                entry_type: EntryType::Symlink,
                len: "fehlt/nirgends".len() as u64,
            }]
        );
        Ok(())
    }

    #[test]
    fn tiefengrenze() -> TestResult {
        let tmp = tempfile::tempdir()?;
        std::fs::create_dir_all(tmp.path().join("d1/d2"))?;
        std::fs::write(tmp.path().join("d1/d2/f"), b"x")?;
        std::fs::write(tmp.path().join("top"), b"x")?;

        let limits = WalkLimits {
            max_depth: 1,
            ..UNBEGRENZT
        };
        let mut walk = walk_beneath(tmp.path(), limits)?;
        assert_eq!(
            collect(&mut walk)?,
            vec![
                ("d1".to_owned(), EntryType::Dir),
                ("top".to_owned(), EntryType::File),
            ]
        );
        assert_eq!(walk.stopped(), Some(WalkStop::DepthLimit));

        let limits = WalkLimits {
            max_depth: 3,
            ..UNBEGRENZT
        };
        let mut walk = walk_beneath(tmp.path(), limits)?;
        assert_eq!(collect(&mut walk)?.len(), 4);
        assert_eq!(walk.stopped(), None);

        let limits = WalkLimits {
            max_depth: 0,
            ..UNBEGRENZT
        };
        let mut walk = walk_beneath(tmp.path(), limits)?;
        assert!(collect(&mut walk)?.is_empty());
        assert_eq!(walk.stopped(), Some(WalkStop::DepthLimit));
        Ok(())
    }

    #[test]
    fn entry_grenze() -> TestResult {
        let tmp = tempfile::tempdir()?;
        for name in ["e", "d", "c", "b", "a"] {
            std::fs::write(tmp.path().join(name), b"x")?;
        }
        let limits = WalkLimits {
            max_entries: 3,
            ..UNBEGRENZT
        };
        let mut walk = walk_beneath(tmp.path(), limits)?;
        let names: Vec<String> = collect(&mut walk)?
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(walk.stopped(), Some(WalkStop::EntryLimit));

        let limits = WalkLimits {
            max_entries: 5,
            ..UNBEGRENZT
        };
        let mut walk = walk_beneath(tmp.path(), limits)?;
        assert_eq!(collect(&mut walk)?.len(), 5);
        assert_eq!(walk.stopped(), None);
        Ok(())
    }

    #[test]
    fn entry_grenze_hat_vorrang_vor_tiefengrenze() -> TestResult {
        let tmp = tempfile::tempdir()?;
        std::fs::create_dir_all(tmp.path().join("a/b"))?;
        std::fs::write(tmp.path().join("z"), b"x")?;
        let limits = WalkLimits {
            max_depth: 1,
            max_entries: 1,
            deadline: None,
        };
        let mut walk = walk_beneath(tmp.path(), limits)?;
        assert_eq!(collect(&mut walk)?, vec![("a".to_owned(), EntryType::Dir)]);
        assert_eq!(walk.stopped(), Some(WalkStop::EntryLimit));
        Ok(())
    }

    #[test]
    fn deadline_grenze() -> TestResult {
        let tmp = tempfile::tempdir()?;
        std::fs::write(tmp.path().join("f"), b"x")?;

        let limits = WalkLimits {
            deadline: Some(Instant::now()),
            ..UNBEGRENZT
        };
        let mut walk = walk_beneath(tmp.path(), limits)?;
        assert!(collect(&mut walk)?.is_empty());
        assert_eq!(walk.stopped(), Some(WalkStop::Deadline));

        let limits = WalkLimits {
            deadline: Some(Instant::now() + Duration::from_secs(3600)),
            ..UNBEGRENZT
        };
        let mut walk = walk_beneath(tmp.path(), limits)?;
        assert_eq!(collect(&mut walk)?.len(), 1);
        assert_eq!(walk.stopped(), None);
        Ok(())
    }

    /// R2-03: Ein nicht betretbares Unterverzeichnis liefert ein Fehler-Item,
    /// das gegen `max_entries` zählt.
    #[test]
    fn fehler_beim_betreten_zaehlt_gegen_entry_grenze() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        if rustix::process::geteuid().is_root() {
            // root ignoriert `0o000` (CAP_DAC_OVERRIDE); das Öffnen gelänge.
            eprintln!("übersprungen: läuft als root, 0o000 sperrt nicht");
            return Ok(());
        }
        let tmp = tempfile::tempdir()?;
        let locked = tmp.path().join("a");
        std::fs::create_dir(&locked)?;
        std::fs::write(tmp.path().join("b"), b"x")?;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))?;

        // Stellt die Rechte auch bei einem Test-Fehlschlag wieder her, damit
        // `TempDir` aufräumen kann.
        struct Restore<'a>(&'a Path);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
        let _restore = Restore(&locked);

        // Lokaler Alias, damit die Closure-Signatur nicht clippy::type_complexity auslöst.
        type WalkRunOutcome = TestResult<(Vec<Result<String, Option<i32>>>, Option<WalkStop>)>;
        let run = |max_entries: usize| -> WalkRunOutcome {
            let limits = WalkLimits {
                max_entries,
                ..UNBEGRENZT
            };
            let mut walk = walk_beneath(tmp.path(), limits)?;
            let items: Vec<Result<String, Option<i32>>> = walk
                .by_ref()
                .map(|item| match item {
                    Ok(entry) => Ok(entry.rel_path.to_string_lossy().into_owned()),
                    Err(err) => Err(err.raw_os_error()),
                })
                .collect();
            Ok((items, walk.stopped()))
        };
        let eacces = Some(rustix::io::Errno::ACCESS.raw_os_error());

        let (items, stopped) = run(1)?;
        let (items2, stopped2) = run(2)?;
        let (items_all, stopped_all) = run(usize::MAX)?;

        let a: Result<String, Option<i32>> = Ok("a".to_owned());
        let b: Result<String, Option<i32>> = Ok("b".to_owned());
        let denied: Result<String, Option<i32>> = Err(eacces);
        assert_eq!(items, vec![a.clone()]);
        assert_eq!(stopped, Some(WalkStop::EntryLimit));
        assert_eq!(items2, vec![a.clone(), denied.clone()]);
        assert_eq!(stopped2, Some(WalkStop::EntryLimit));
        assert_eq!(items_all, vec![a, denied, b]);
        assert_eq!(stopped_all, None);
        Ok(())
    }

    #[test]
    fn wurzel_symlink_wird_abgelehnt() -> TestResult {
        let tmp = tempfile::tempdir()?;
        std::fs::create_dir(tmp.path().join("real"))?;
        symlink("real", tmp.path().join("link"))?;
        let Err(_) = walk_beneath(&tmp.path().join("link"), UNBEGRENZT) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        Ok(())
    }

    #[test]
    fn begrenzter_namensspeicher_bleibt_deterministisch() -> TestResult {
        let tmp = tempfile::tempdir()?;
        for index in (0..200).rev() {
            std::fs::write(tmp.path().join(format!("n{index:03}")), b"")?;
        }
        let fd = open_dir_nofollow(tmp.path())?;
        let names =
            read_sorted_names(fd.as_fd(), 4, None)?.ok_or(TestError::Missing("keine deadline"))?;
        assert_eq!(names, ["n000", "n001", "n002", "n003"]);
        Ok(())
    }
}
