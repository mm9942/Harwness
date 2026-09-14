//! Symlinkfestes Öffnen von Dateien und Verzeichnissen.
//!
//! Alle Funktionen setzen `O_NOFOLLOW` und `O_CLOEXEC` über
//! [`rustix::fs::OFlags`] — nie über eine hartkodierte Zahl, weil der Wert
//! architekturabhängig ist.
//!
//! - [`open_nofollow`]: nur das **letzte** Pfadglied darf kein Symlink sein
//!   (Elternverzeichnisse werden wie bei `open(2)` aufgelöst).
//! - [`open_dir_nofollow`]: dasselbe für Verzeichnisse (`O_DIRECTORY`).
//! - [`open_beneath`]: **kein** Pfadglied darf ein Symlink sein, und die
//!   Auflösung bleibt unterhalb eines Wurzel-Deskriptors.
//!
//! # FIFOs und Geräte
//! [`open_nofollow`] und [`open_beneath`] öffnen immer mit `O_NONBLOCK`, damit
//! ein FIFO oder Terminal als letztes Glied `open(2)` nicht unbegrenzt
//! blockiert. Direkt danach prüft `fstat` den Typ: nur reguläre Dateien und
//! Verzeichnisse werden zurückgegeben, alles andere (FIFO, Socket, Zeichen-
//! und Blockgerät) liefert `InvalidInput`. Für die zurückgegebene Datei wird
//! `O_NONBLOCK` per `fcntl(F_SETFL)` wieder entfernt — sie verhält sich also
//! wie ein normal geöffneter Deskriptor. Nur `O_NONBLOCK` zu entfernen, ohne
//! den Typ zu prüfen, würde den Hänger bloß vom `open` in das erste `read`
//! verschieben (ein Angreifer hält das FIFO schreibend offen, schreibt aber
//! nie). [`open_dir_nofollow`] braucht das nicht: mit `O_DIRECTORY` lehnt der
//! Kernel Nicht-Verzeichnisse mit `ENOTDIR` ab, bevor ein FIFO geöffnet wird.
//! Das Öffnen selbst kann bei Geräten Seiteneffekte haben (der Deskriptor wird
//! sofort wieder geschlossen); `O_NONBLOCK` bewirkt zudem, dass eine Datei mit
//! fremdem Lease (`fcntl(F_SETLEASE)`) mit `EAGAIN` scheitert, statt auf das
//! Lease-Ende zu warten.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use rustix::fs::{CWD, FileType, Mode, OFlags};
use rustix::io::retry_on_intr;

/// Zugriffs- und Anlegemodus für die `*_nofollow`/`*_beneath`-Öffner.
///
/// Die Semantik folgt [`std::fs::OpenOptions`]: `append` impliziert
/// Schreibzugriff; `create`, `create_new` und `truncate` verlangen
/// Schreibzugriff; `truncate` und `append` schließen sich aus;
/// `create_new` (`O_CREAT | O_EXCL`) hat Vorrang vor `create`.
///
/// `mode` sind die Rechte-Bits (`& 0o7777`) für eine neu angelegte Datei;
/// der Prozess-`umask` wird vom Kernel noch abgezogen. Ohne `create`/
/// `create_new` ist `mode` bedeutungslos.
///
/// # Examples
/// ```rust
/// use harw_fsutil::OpenMode;
///
/// let ro = OpenMode::read_only();
/// assert!(ro.read && !ro.write);
/// let neu = OpenMode::write_create_new(0o600);
/// assert!(neu.create_new && neu.mode == 0o600);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenMode {
    /// Lesezugriff.
    pub read: bool,
    /// Schreibzugriff.
    pub write: bool,
    /// Anlegen, falls nicht vorhanden (`O_CREAT`).
    pub create: bool,
    /// Nur neu anlegen, sonst Fehler `EEXIST` (`O_CREAT | O_EXCL`).
    pub create_new: bool,
    /// Auf Länge 0 kürzen (`O_TRUNC`).
    pub truncate: bool,
    /// Anhängen (`O_APPEND`), impliziert Schreibzugriff.
    pub append: bool,
    /// Rechte-Bits für neu angelegte Dateien.
    pub mode: u32,
}

impl OpenMode {
    /// Nur lesen, nichts anlegen.
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            create: false,
            create_new: false,
            truncate: false,
            append: false,
            mode: 0o600,
        }
    }

    /// Lesen und schreiben einer existierenden Datei, nichts anlegen.
    #[must_use]
    pub const fn read_write() -> Self {
        Self {
            write: true,
            ..Self::read_only()
        }
    }

    /// Nur schreiben; Datei muss neu sein (`O_CREAT | O_EXCL`), Rechte `mode`.
    #[must_use]
    pub const fn write_create_new(mode: u32) -> Self {
        Self {
            read: false,
            write: true,
            create_new: true,
            mode,
            ..Self::read_only()
        }
    }

    /// Nur schreiben; anlegen falls nötig und auf 0 kürzen, Rechte `mode`.
    #[must_use]
    pub const fn write_truncate(mode: u32) -> Self {
        Self {
            read: false,
            write: true,
            create: true,
            truncate: true,
            mode,
            ..Self::read_only()
        }
    }

    /// Anhängen; anlegen falls nötig, Rechte `mode`.
    #[must_use]
    pub const fn append_create(mode: u32) -> Self {
        Self {
            read: false,
            append: true,
            create: true,
            mode,
            ..Self::read_only()
        }
    }

    /// Übersetzt den Modus in `OFlags` inklusive `NOFOLLOW | CLOEXEC |
    /// NONBLOCK` (siehe Modul-Doku „FIFOs und Geräte“; [`finish_open`]
    /// entfernt `NONBLOCK` nach der Typprüfung wieder).
    pub(crate) fn oflags(&self) -> io::Result<OFlags> {
        let write = self.write || self.append;
        let mut flags = match (self.read, write) {
            (true, false) => OFlags::RDONLY,
            (false, true) => OFlags::WRONLY,
            (true, true) => OFlags::RDWR,
            (false, false) => {
                return Err(invalid_input("OpenMode ohne Lese- oder Schreibzugriff"));
            }
        };
        if (self.create || self.create_new || self.truncate) && !write {
            return Err(invalid_input(
                "OpenMode: create/create_new/truncate verlangen Schreibzugriff",
            ));
        }
        if self.truncate && self.append {
            return Err(invalid_input("OpenMode: truncate und append schließen sich aus"));
        }
        if self.create_new {
            flags |= OFlags::CREATE | OFlags::EXCL;
        } else if self.create {
            flags |= OFlags::CREATE;
        }
        if self.truncate {
            flags |= OFlags::TRUNC;
        }
        if self.append {
            flags |= OFlags::APPEND;
        }
        Ok(flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK)
    }

    /// Rechte-Bits als rustix-`Mode` (Dateityp-Bits werden verworfen).
    ///
    /// Ohne `create`/`create_new` immer leer: `openat2(2)` lehnt einen
    /// Modus ungleich 0 ohne `O_CREAT` mit `EINVAL` ab (anders als `openat`).
    pub(crate) fn create_mode(&self) -> Mode {
        if self.create || self.create_new {
            mode_from_bits(self.mode)
        } else {
            Mode::empty()
        }
    }
}

/// `u32`-Rechte-Bits → `Mode`. Unter Linux ist `RawMode` immer `u32`.
#[cfg(target_os = "linux")]
pub(crate) fn mode_from_bits(bits: u32) -> Mode {
    Mode::from_raw_mode(bits & 0o7777)
}

/// `u32`-Rechte-Bits → `Mode`. `RawMode` ist hier plattformabhängig
/// (z. B. `u16` auf macOS); `& 0o7777` passt in jede Breite.
#[cfg(not(target_os = "linux"))]
#[allow(clippy::cast_possible_truncation, clippy::unnecessary_cast)]
pub(crate) fn mode_from_bits(bits: u32) -> Mode {
    Mode::from_raw_mode((bits & 0o7777) as rustix::fs::RawMode)
}

/// Flags für Zwischenverzeichnisse beim komponentenweisen Abstieg.
///
/// Unter Linux `O_PATH`, damit auch nur-durchsuchbare Verzeichnisse
/// (`--x`) passierbar sind — wie bei `openat2`. `O_PATH | O_NOFOLLOW` würde
/// einen Symlink selbst öffnen; `O_DIRECTORY` macht daraus `ENOTDIR`, und
/// [`ensure_directory`] prüft zusätzlich per `fstat`.
#[cfg(target_os = "linux")]
const INTERMEDIATE_DIR_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Flags für Zwischenverzeichnisse beim komponentenweisen Abstieg.
#[cfg(not(target_os = "linux"))]
const INTERMEDIATE_DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Flags für lesbare Verzeichnis-Deskriptoren (für `readdir`/`fsync`).
pub(crate) const READABLE_DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Prüft, ob `error` ein `ELOOP` ist (Symlink als letztes Pfadglied bei
/// [`open_nofollow`]).
///
/// Ersetzt `io::ErrorKind::FilesystemLoop`, das hinter dem instabilen
/// Feature `io_error_more` liegt; der Vergleich läuft über den rohen `errno`.
///
/// # Examples
/// ```rust
/// use std::io;
/// assert!(!harw_fsutil::is_symlink_loop(&io::Error::from(io::ErrorKind::NotFound)));
/// ```
pub fn is_symlink_loop(error: &io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
}

/// Öffnet `path`; das letzte Pfadglied darf kein Symlink sein.
///
/// Elternverzeichnisse werden normal aufgelöst (auch über Symlinks). Ist das
/// letzte Glied ein Symlink, schlägt der Aufruf fehl (`ELOOP`; bei
/// `create_new` auf einen Symlink `EEXIST`) — auch wenn der Symlink ins
/// Leere zeigt, wird nichts angelegt.
///
/// Geöffnet wird mit `O_NONBLOCK`; nur reguläre Dateien und Verzeichnisse
/// werden zurückgegeben, danach ohne `O_NONBLOCK` (siehe Modul-Doku „FIFOs
/// und Geräte“).
///
/// # Errors
/// `InvalidInput` bei widersprüchlichem [`OpenMode`] und wenn das Ziel weder
/// reguläre Datei noch Verzeichnis ist (FIFO, Gerät, Socket); sonst der
/// `errno` von `openat(2)`/`fstat(2)`/`fcntl(2)` (z. B. `ENXIO` beim
/// Schreib-Öffnen eines FIFOs ohne Leser).
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{OpenMode, open_nofollow};
/// use std::io::Read;
/// use std::path::Path;
///
/// let mut file = open_nofollow(Path::new("/etc/hostname"), OpenMode::read_only())?;
/// let mut text = String::new();
/// file.read_to_string(&mut text)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn open_nofollow(path: &Path, mode: OpenMode) -> io::Result<File> {
    let flags = mode.oflags()?;
    let create_mode = mode.create_mode();
    let fd = retry_on_intr(|| rustix::fs::openat(CWD, path, flags, create_mode))?;
    finish_open(fd)
}

/// Öffnet das Verzeichnis `path` lesbar (`O_RDONLY | O_DIRECTORY`); das letzte
/// Pfadglied darf kein Symlink sein.
///
/// Der Deskriptor eignet sich als Wurzel für [`open_beneath`] und für
/// `readdir`/`fsync`.
///
/// # Errors
/// `ELOOP`/`ENOTDIR`, wenn das letzte Glied ein Symlink bzw. kein
/// Verzeichnis ist, sonst der `errno` von `openat(2)`.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::open_dir_nofollow;
/// use std::path::Path;
///
/// let root = open_dir_nofollow(Path::new("/home/mia/.harw"))?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn open_dir_nofollow(path: &Path) -> io::Result<OwnedFd> {
    let fd = retry_on_intr(|| rustix::fs::openat(CWD, path, READABLE_DIR_FLAGS, Mode::empty()))?;
    Ok(fd)
}

/// Öffnet `rel` relativ zu `root`, ohne je einem Symlink zu folgen und ohne
/// `root` zu verlassen.
///
/// `rel` muss relativ sein und darf keine `..`-Komponente enthalten
/// (`InvalidInput`, auch wenn `..` die Wurzel nicht verließe) — so ist die
/// Semantik auf beiden Implementierungspfaden identisch. `.`-Komponenten
/// werden ignoriert; ein reines `.` öffnet `root` selbst.
///
/// **Abschließendes `/` oder `/.`** (`file.txt/`, `dir/`, `dir/.`, `./`) wird
/// ebenfalls mit `InvalidInput` abgelehnt: `openat2` verlangt dafür ein
/// Verzeichnis (`ENOTDIR` bei Dateien), [`Path::components`] verwirft den Zusatz
/// aber, sodass der Fallback die Datei öffnen würde. Die Ablehnung auf beiden
/// Pfaden hält die Semantik identisch; ein Verzeichnis öffnet man ohne `/`.
///
/// Wie bei [`open_nofollow`] wird mit `O_NONBLOCK` geöffnet und nur eine
/// reguläre Datei oder ein Verzeichnis zurückgegeben (ohne `O_NONBLOCK`).
///
/// Unter Linux: `openat2(root, rel, flags | O_NOFOLLOW, mode,
/// RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)`; `EAGAIN`
/// (Umbenennungs-Wettlauf im Kernel) wird begrenzt wiederholt. Liefert
/// `openat2` `ENOSYS` (Kernel < 5.6) oder `EPERM` (seccomp-Filter älterer
/// Container-Runtimes), folgt der komponentenweise Fallback: jedes
/// Zwischenglied mit `openat(O_DIRECTORY | O_NOFOLLOW)`, das letzte mit
/// `openat(flags | O_NOFOLLOW)`. Der Fallback ist gleich streng (jeder
/// Symlink, auch Magic-Links unter `/proc`, führt zum Fehler) und liefert bei
/// echten Rechteproblemen den eigentlichen `errno`. Auf anderen
/// Unix-Systemen wird nur der Fallback verwendet.
///
/// # Errors
/// `InvalidInput` für leere, absolute, `..`-haltige oder auf `/` bzw. `/.`
/// endende Pfade, widersprüchliche [`OpenMode`] und Ziele, die weder reguläre
/// Datei noch Verzeichnis sind; ein Symlink in irgendeinem Glied liefert
/// `ELOOP` (bzw. `ENOTDIR` bei Zwischengliedern im Fallback); sonst der
/// `errno` des Syscalls.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{OpenMode, open_beneath, open_dir_nofollow};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let root = open_dir_nofollow(Path::new("/home/mia/.harw"))?;
/// let file = open_beneath(root.as_fd(), Path::new("sessions/abc.jsonl"), OpenMode::read_only())?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn open_beneath(root: BorrowedFd<'_>, rel: &Path, mode: OpenMode) -> io::Result<File> {
    validate_beneath_path(rel)?;
    let flags = mode.oflags()?;
    beneath_impl(root, rel, mode, flags)
}

/// Linux: `openat2` mit Fallback.
#[cfg(target_os = "linux")]
fn beneath_impl(
    root: BorrowedFd<'_>,
    rel: &Path,
    mode: OpenMode,
    flags: OFlags,
) -> io::Result<File> {
    use rustix::fs::ResolveFlags;
    use rustix::io::Errno;

    /// Obergrenze für `EAGAIN`-Wiederholungen von `openat2`.
    const MAX_EAGAIN_RETRIES: usize = 16;

    let resolve = ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS;
    let create_mode = mode.create_mode();
    let mut attempts = 0;
    loop {
        match retry_on_intr(|| rustix::fs::openat2(root, rel, flags, create_mode, resolve)) {
            Ok(fd) => return finish_open(fd),
            Err(Errno::AGAIN) if attempts < MAX_EAGAIN_RETRIES => attempts += 1,
            Err(Errno::NOSYS | Errno::PERM) => {
                return open_beneath_componentwise(root, rel, mode);
            }
            Err(errno) => return Err(errno.into()),
        }
    }
}

/// Nicht-Linux: kein `openat2`, nur komponentenweise.
#[cfg(not(target_os = "linux"))]
fn beneath_impl(
    root: BorrowedFd<'_>,
    rel: &Path,
    mode: OpenMode,
    _flags: OFlags,
) -> io::Result<File> {
    open_beneath_componentwise(root, rel, mode)
}

/// Komponentenweiser `open_beneath`-Pfad (ENOSYS-Fallback, Nicht-Linux).
///
/// Eigenständig testbar; validiert `rel` und `mode` selbst.
pub(crate) fn open_beneath_componentwise(
    root: BorrowedFd<'_>,
    rel: &Path,
    mode: OpenMode,
) -> io::Result<File> {
    let parts = validate_beneath_path(rel)?;
    let flags = mode.oflags()?;
    let create_mode = mode.create_mode();

    let Some((last, intermediates)) = parts.split_last() else {
        // Nur `.`-Komponenten: die Wurzel selbst.
        let fd = retry_on_intr(|| rustix::fs::openat(root, ".", flags, create_mode))?;
        return finish_open(fd);
    };

    let mut current: Option<OwnedFd> = None;
    for part in intermediates {
        let dirfd = current.as_ref().map_or(root, AsFd::as_fd);
        let next = retry_on_intr(|| {
            rustix::fs::openat(dirfd, *part, INTERMEDIATE_DIR_FLAGS, Mode::empty())
        })?;
        ensure_directory(&next)?;
        current = Some(next);
    }
    let dirfd = current.as_ref().map_or(root, AsFd::as_fd);
    let fd = retry_on_intr(|| rustix::fs::openat(dirfd, *last, flags, create_mode))?;
    finish_open(fd)
}

/// Abschluss von [`open_nofollow`]/[`open_beneath`]: nur reguläre Dateien und
/// Verzeichnisse zulassen, dann `O_NONBLOCK` entfernen.
///
/// Die Typprüfung läuft auf dem bereits geöffneten Deskriptor (`fstat`), also
/// ohne Wettlauf zwischen Prüfen und Öffnen.
fn finish_open(fd: OwnedFd) -> io::Result<File> {
    let stat = rustix::fs::fstat(&fd)?;
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::RegularFile | FileType::Directory => {}
        _ => {
            return Err(invalid_input(
                "Ziel ist weder reguläre Datei noch Verzeichnis (FIFO, Gerät, Socket)",
            ));
        }
    }
    let status = rustix::fs::fcntl_getfl(&fd)?;
    if status.contains(OFlags::NONBLOCK) {
        rustix::fs::fcntl_setfl(&fd, status.difference(OFlags::NONBLOCK))?;
    }
    Ok(File::from(fd))
}

/// Verteidigung in der Tiefe: ein Zwischenglied muss wirklich ein
/// Verzeichnis sein (kein per `O_PATH` geöffneter Symlink).
fn ensure_directory(fd: &OwnedFd) -> io::Result<()> {
    let stat = rustix::fs::fstat(fd)?;
    if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
        Ok(())
    } else {
        Err(io::Error::from(rustix::io::Errno::NOTDIR))
    }
}

/// Prüft `rel` für [`open_beneath`] und liefert die normalen Komponenten.
///
/// Abgelehnt (`InvalidInput`): leer, absolut (Wurzel/Präfix), `..`,
/// abschließendes `/` oder `/.` (siehe [`open_beneath`]).
pub(crate) fn validate_beneath_path(rel: &Path) -> io::Result<Vec<&OsStr>> {
    if rel.as_os_str().is_empty() {
        return Err(invalid_input("open_beneath: leerer Pfad"));
    }
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(invalid_input("open_beneath: `..` ist nicht erlaubt"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_input("open_beneath: absoluter Pfad ist nicht erlaubt"));
            }
        }
    }
    // `components()` verwirft ein abschließendes `/` bzw. `/.`, `openat2` nicht.
    let bytes = rel.as_os_str().as_bytes();
    if bytes.ends_with(b"/") || bytes.ends_with(b"/.") {
        return Err(invalid_input("open_beneath: abschließendes `/` ist nicht erlaubt"));
    }
    Ok(parts)
}

/// `InvalidInput`-Fehler mit statischer Meldung.
pub(crate) fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::io::Errno;
    use std::io::{Read, Write};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};

    fn is_errno(err: &io::Error, candidates: &[Errno]) -> bool {
        candidates
            .iter()
            .any(|errno| err.raw_os_error() == Some(errno.raw_os_error()))
    }

    /// Baut `root/{dir/inner.txt, top.txt, link_file -> top.txt,
    /// link_dir -> dir, dangling -> fehlt}`.
    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir(root.join("dir")).expect("mkdir");
        std::fs::write(root.join("dir/inner.txt"), b"inner").expect("write");
        std::fs::write(root.join("top.txt"), b"top").expect("write");
        symlink("top.txt", root.join("link_file")).expect("symlink");
        symlink("dir", root.join("link_dir")).expect("symlink");
        symlink("fehlt", root.join("dangling")).expect("symlink");
        tmp
    }

    fn read_all(mut file: File) -> String {
        let mut text = String::new();
        file.read_to_string(&mut text).expect("read");
        text
    }

    #[test]
    fn oflags_validierung_wie_openoptions() {
        let none = OpenMode {
            read: false,
            ..OpenMode::read_only()
        };
        assert_eq!(none.oflags().unwrap_err().kind(), io::ErrorKind::InvalidInput);

        let trunc_ro = OpenMode {
            truncate: true,
            ..OpenMode::read_only()
        };
        assert_eq!(trunc_ro.oflags().unwrap_err().kind(), io::ErrorKind::InvalidInput);

        let trunc_append = OpenMode {
            truncate: true,
            ..OpenMode::append_create(0o600)
        };
        assert_eq!(trunc_append.oflags().unwrap_err().kind(), io::ErrorKind::InvalidInput);

        let flags = OpenMode::write_create_new(0o600).oflags().expect("gültig");
        let expected = OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::CREATE | OFlags::EXCL;
        assert!(flags.contains(expected));
        assert!(flags.contains(OFlags::WRONLY));
        let flags = OpenMode::read_only().oflags().expect("gültig");
        assert!(flags.contains(OFlags::NOFOLLOW));
        assert!(flags.contains(OFlags::NONBLOCK), "R2-10: nie blockierend öffnen");
        assert!(!flags.contains(OFlags::CREATE));
    }

    #[test]
    fn open_nofollow_liest_regulaere_datei() {
        let tmp = fixture();
        let file = open_nofollow(&tmp.path().join("top.txt"), OpenMode::read_only()).expect("open");
        assert_eq!(read_all(file), "top");
    }

    #[test]
    fn open_nofollow_symlink_als_letztes_glied_scheitert() {
        let tmp = fixture();
        let err = open_nofollow(&tmp.path().join("link_file"), OpenMode::read_only()).unwrap_err();
        assert!(is_errno(&err, &[Errno::LOOP]), "{err:?}");
    }

    #[test]
    fn open_nofollow_legt_nichts_hinter_dangling_symlink_an() {
        let tmp = fixture();
        let dangling = tmp.path().join("dangling");
        let err = open_nofollow(&dangling, OpenMode::write_truncate(0o600)).unwrap_err();
        assert!(is_errno(&err, &[Errno::LOOP, Errno::EXIST]), "{err:?}");
        let err = open_nofollow(&dangling, OpenMode::write_create_new(0o600)).unwrap_err();
        assert!(is_errno(&err, &[Errno::LOOP, Errno::EXIST]), "{err:?}");
        assert!(!tmp.path().join("fehlt").exists());
    }

    #[test]
    fn open_dir_nofollow_lehnt_symlink_ab() {
        let tmp = fixture();
        open_dir_nofollow(&tmp.path().join("dir")).expect("verzeichnis");
        let err = open_dir_nofollow(&tmp.path().join("link_dir")).unwrap_err();
        assert!(is_errno(&err, &[Errno::LOOP, Errno::NOTDIR]), "{err:?}");
        let err = open_dir_nofollow(&tmp.path().join("top.txt")).unwrap_err();
        assert!(is_errno(&err, &[Errno::NOTDIR]), "{err:?}");
    }

    /// Gemeinsame Prüfungen für beide `open_beneath`-Pfade.
    fn check_beneath(open: fn(BorrowedFd<'_>, &Path, OpenMode) -> io::Result<File>) {
        let tmp = fixture();
        let root = open_dir_nofollow(tmp.path()).expect("root");
        let root = root.as_fd();

        let file = open(root, Path::new("dir/inner.txt"), OpenMode::read_only()).expect("nested");
        assert_eq!(read_all(file), "inner");
        let file =
            open(root, Path::new("./dir/./inner.txt"), OpenMode::read_only()).expect("curdir");
        assert_eq!(read_all(file), "inner");

        // Symlink als letztes Glied.
        let err = open(root, Path::new("link_file"), OpenMode::read_only()).unwrap_err();
        assert!(is_errno(&err, &[Errno::LOOP]), "{err:?}");
        // Symlink als mittleres Glied.
        let err = open(root, Path::new("link_dir/inner.txt"), OpenMode::read_only()).unwrap_err();
        assert!(is_errno(&err, &[Errno::LOOP, Errno::NOTDIR]), "{err:?}");
        // Dangling Symlink mit Anlegen: nichts wird angelegt.
        let err = open(root, Path::new("dangling"), OpenMode::write_truncate(0o600)).unwrap_err();
        assert!(is_errno(&err, &[Errno::LOOP, Errno::EXIST]), "{err:?}");
        assert!(!tmp.path().join("fehlt").exists());

        // `..`, absolute Pfade und (R2-04) abschließendes `/` bzw. `/.` —
        // auf beiden Pfaden gleich abgelehnt, auch für Verzeichnisse.
        let bad_paths = [
            "..",
            "dir/../top.txt",
            "/etc/hostname",
            "",
            "top.txt/",
            "top.txt/.",
            "top.txt//",
            "dir/",
            "dir/.",
            "./",
            "dir/inner.txt/",
        ];
        for bad in bad_paths {
            let err = open(root, Path::new(bad), OpenMode::read_only()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        // Ohne abschließendes `/` bleiben Verzeichnis und Wurzel öffenbar.
        let dir = open(root, Path::new("dir"), OpenMode::read_only()).expect("dir");
        assert!(dir.metadata().expect("meta").is_dir());
        let dot = open(root, Path::new("."), OpenMode::read_only()).expect("wurzel");
        assert!(dot.metadata().expect("meta").is_dir());
        let file = open(root, Path::new("dir//./inner.txt"), OpenMode::read_only()).expect("mitte");
        assert_eq!(read_all(file), "inner");

        // Anlegen unterhalb der Wurzel mit Rechten ohne Gruppe/Andere.
        let neu = OpenMode::write_create_new(0o600);
        let mut file = open(root, Path::new("dir/neu.txt"), neu).expect("create");
        file.write_all(b"neu").expect("write");
        let meta = std::fs::symlink_metadata(tmp.path().join("dir/neu.txt")).expect("meta");
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o077, 0);
        let err = open(root, Path::new("dir/neu.txt"), neu).unwrap_err();
        assert!(is_errno(&err, &[Errno::EXIST]), "{err:?}");
    }

    #[test]
    fn open_beneath_oeffentlicher_pfad() {
        check_beneath(open_beneath);
    }

    #[test]
    fn open_beneath_enosys_fallback_komponentenweise() {
        check_beneath(open_beneath_componentwise);
    }

    #[test]
    fn fallback_punkt_oeffnet_wurzel() {
        let tmp = fixture();
        let root = open_dir_nofollow(tmp.path()).expect("root");
        let dot = Path::new(".");
        let file =
            open_beneath_componentwise(root.as_fd(), dot, OpenMode::read_only()).expect("wurzel");
        assert!(file.metadata().expect("meta").is_dir());
    }

    /// Führt `f` in einem eigenen Thread aus. Blockiert es länger als 10 s,
    /// schlägt der Test fehl, statt die ganze Testsuite aufzuhängen.
    fn within_timeout<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Der Empfänger existiert nach einem Timeout nicht mehr; dann ist
            // der Test ohnehin schon fehlgeschlagen.
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("Öffnen blockiert (FIFO ohne O_NONBLOCK?)")
    }

    /// Erfolg wird zu `None`, ein Fehler zu seiner `ErrorKind`.
    fn outcome(result: io::Result<File>) -> Option<io::ErrorKind> {
        result.err().map(|err| err.kind())
    }

    /// Legt ein FIFO per `mkfifo(1)` an (rustix bietet `mknodat`, das Crate
    /// braucht es aber sonst nicht). `false`, wenn das Programm fehlt.
    fn make_fifo(path: &Path) -> bool {
        match std::process::Command::new("mkfifo").arg(path).status() {
            Ok(status) if status.success() => true,
            other => {
                eprintln!("übersprungen: `mkfifo` nicht verfügbar ({other:?})");
                false
            }
        }
    }

    /// R2-10: Ein FIFO als letztes Glied blockiert nicht und wird abgelehnt.
    #[test]
    fn fifo_blockiert_nicht_und_wird_abgelehnt() {
        let tmp = fixture();
        let fifo = tmp.path().join("fifo");
        if !make_fifo(&fifo) {
            return;
        }
        let invalid = Some(io::ErrorKind::InvalidInput);

        let path = fifo.clone();
        let ro = within_timeout(move || outcome(open_nofollow(&path, OpenMode::read_only())));
        assert_eq!(ro, invalid);
        let path = fifo.clone();
        let rw = within_timeout(move || outcome(open_nofollow(&path, OpenMode::read_write())));
        assert_eq!(rw, invalid);
        // Schreiben ohne Leser: `ENXIO` statt Warten.
        let path = fifo.clone();
        let write = OpenMode::write_truncate(0o600);
        let wo = within_timeout(move || outcome(open_nofollow(&path, write)));
        assert!(wo.is_some(), "Schreib-Öffnen eines FIFOs muss scheitern");

        let openers: [fn(BorrowedFd<'_>, &Path, OpenMode) -> io::Result<File>; 2] =
            [open_beneath, open_beneath_componentwise];
        for open in openers {
            let root_path = tmp.path().to_path_buf();
            let result = within_timeout(move || {
                let root = open_dir_nofollow(&root_path).expect("root");
                outcome(open(root.as_fd(), Path::new("fifo"), OpenMode::read_only()))
            });
            assert_eq!(result, invalid);
        }
        assert!(std::fs::symlink_metadata(&fifo).expect("meta").file_type().is_fifo());
    }

    /// R2-10: Geräte werden ebenfalls abgelehnt.
    #[test]
    fn zeichengeraet_wird_abgelehnt() {
        let null = Path::new("/dev/null");
        if !null.exists() {
            eprintln!("übersprungen: /dev/null fehlt");
            return;
        }
        let err = open_nofollow(null, OpenMode::read_only()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// R2-10: Zurückgegebene Deskriptoren sind wieder blockierend; `O_APPEND`
    /// bleibt beim Entfernen von `O_NONBLOCK` erhalten.
    #[test]
    fn nonblock_wird_nach_dem_oeffnen_entfernt() {
        let tmp = fixture();
        let file = open_nofollow(&tmp.path().join("top.txt"), OpenMode::read_only()).expect("open");
        let status = rustix::fs::fcntl_getfl(file.as_fd()).expect("getfl");
        assert!(!status.contains(OFlags::NONBLOCK));

        let log = tmp.path().join("log");
        let file = open_nofollow(&log, OpenMode::append_create(0o600)).expect("append");
        let status = rustix::fs::fcntl_getfl(file.as_fd()).expect("getfl");
        assert!(status.contains(OFlags::APPEND));
        assert!(!status.contains(OFlags::NONBLOCK));

        let root = open_dir_nofollow(tmp.path()).expect("root");
        let openers: [fn(BorrowedFd<'_>, &Path, OpenMode) -> io::Result<File>; 2] =
            [open_beneath, open_beneath_componentwise];
        for open in openers {
            let file = open(root.as_fd(), Path::new("dir/inner.txt"), OpenMode::read_only())
                .expect("open");
            let status = rustix::fs::fcntl_getfl(file.as_fd()).expect("getfl");
            assert!(!status.contains(OFlags::NONBLOCK));
            let dir = open(root.as_fd(), Path::new("."), OpenMode::read_only()).expect("dir");
            let status = rustix::fs::fcntl_getfl(dir.as_fd()).expect("getfl");
            assert!(!status.contains(OFlags::NONBLOCK));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fallback_passiert_nur_durchsuchbares_verzeichnis() {
        let tmp = fixture();
        let dir = tmp.path().join("dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o100)).expect("chmod");
        let root = open_dir_nofollow(tmp.path()).expect("root");
        let rel = Path::new("dir/inner.txt");
        let result = open_beneath_componentwise(root.as_fd(), rel, OpenMode::read_only());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert_eq!(read_all(result.expect("O_PATH-Abstieg")), "inner");
    }
}
