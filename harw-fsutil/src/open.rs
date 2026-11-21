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
//! - Einzelne Einträge relativ zu einem Verzeichnis-Deskriptor, ohne je
//!   einem Symlink zu folgen: [`open_dir_at`], [`mkdir_at`], [`unlink_at`],
//!   [`rename_at`], [`entry_type_at`], [`same_file_at`] und [`read_dir_names`].
//!   Sie nehmen nur **einen** Namen (kein `/`, nicht `.`/`..`), damit kein
//!   Zwischenglied aufgelöst wird; [`ensure_owned_directory`] prüft einen geöffneten
//!   Verzeichnis-Deskriptor (Typ, Eigentümer, keine Schreibrechte für Gruppe
//!   und Andere). Zusammen mit [`open_beneath`] lässt sich so eine
//!   Verzeichnisgrenze einmal per Deskriptor festlegen und danach ohne
//!   erneute Pfadauflösung benutzen.
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
//! nie). [`open_dir_nofollow`] und [`open_dir_at`] brauchen das nicht: mit
//! `O_DIRECTORY` lehnt der Kernel Nicht-Verzeichnisse mit `ENOTDIR` ab, bevor
//! ein FIFO oder Gerät geöffnet wird.
//! Das Öffnen selbst kann bei Geräten Seiteneffekte haben (der Deskriptor wird
//! sofort wieder geschlossen); `O_NONBLOCK` bewirkt zudem, dass eine Datei mit
//! fremdem Lease (`fcntl(F_SETLEASE)`) mit `EAGAIN` scheitert, statt auf das
//! Lease-Ende zu warten.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};
use rustix::io::retry_on_intr;

use crate::walk::EntryType;

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
            return Err(invalid_input(
                "OpenMode: truncate und append schließen sich aus",
            ));
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
/// let root = open_dir_nofollow(Path::new("/home/user/.harw"))?;
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
/// let root = open_dir_nofollow(Path::new("/home/user/.harw"))?;
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
                return Err(invalid_input(
                    "open_beneath: absoluter Pfad ist nicht erlaubt",
                ));
            }
        }
    }
    // `components()` verwirft ein abschließendes `/` bzw. `/.`, `openat2` nicht.
    let bytes = rel.as_os_str().as_bytes();
    if bytes.ends_with(b"/") || bytes.ends_with(b"/.") {
        return Err(invalid_input(
            "open_beneath: abschließendes `/` ist nicht erlaubt",
        ));
    }
    Ok(parts)
}

/// `InvalidInput`-Fehler mit statischer Meldung.
pub(crate) fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Prüft `name` für die `*_at`-Helfer: genau **ein** Pfadglied.
///
/// Abgelehnt (`InvalidInput`): leer, `.`, `..` und alles mit `/`. Ein `/`
/// hieße, dass `*at(2)` ein Zwischenglied auflöst — und dabei einem Symlink
/// folgen könnte.
fn validate_entry_name(name: &Path) -> io::Result<&OsStr> {
    let bytes = name.as_os_str().as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(invalid_input(
            "Eintragsname muss genau ein Pfadglied sein (ohne `/`, nicht `.`/`..`)",
        ));
    }
    Ok(name.as_os_str())
}

/// Öffnet das Verzeichnis `name` direkt in `dir` lesbar, ohne einem Symlink
/// zu folgen (`openat(2)` mit `O_RDONLY | O_DIRECTORY | O_NOFOLLOW |
/// O_CLOEXEC`).
///
/// # Description
/// Das Gegenstück zu [`open_dir_nofollow`] relativ zu einem Deskriptor, für
/// einen Abstieg Glied für Glied. Anders als [`open_beneath`] mit
/// [`OpenMode::read_only`] verlangt der Kernel schon beim Öffnen ein
/// Verzeichnis: eine reguläre Datei, ein FIFO oder ein Gerät unter `name`
/// wird nie geöffnet (`ENOTDIR`, ohne Blockieren und ohne Seiteneffekte des
/// Geräts), ein Symlink nie verfolgt (`ELOOP` bzw. `ENOTDIR`, je nach
/// Kernel), auch wenn er auf ein Verzeichnis zeigt. Ob das Verzeichnis privat
/// genug ist, prüft der Aufrufer danach selbst, etwa per
/// [`ensure_owned_directory`].
///
/// # Arguments
/// - `dir` (`BorrowedFd`): das Elternverzeichnis.
/// - `name` (`&Path`): genau ein Pfadglied.
///
/// # Returns
/// Das geöffnete Verzeichnis (lesbar; geeignet für `readdir`, `fsync`,
/// `fchmod` und als `dir` der übrigen `*_at`-Helfer).
///
/// # Errors
/// `InvalidInput` für einen Namen mit `/`, `.`, `..` oder leer; `NotFound`,
/// wenn kein Eintrag existiert; `ELOOP`/`ENOTDIR` für einen Symlink oder ein
/// Nicht-Verzeichnis; sonst der `errno` von `openat(2)` (z. B. `EACCES`).
///
/// # Concurrency
/// Zustandslos. Der Deskriptor bleibt an das geöffnete Verzeichnis gebunden,
/// auch wenn `name` danach umbenannt oder durch etwas anderes ersetzt wird.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{ensure_owned_directory, open_dir_at, open_dir_nofollow};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let parent = open_dir_nofollow(Path::new("/home/user/.harw"))?;
/// let dod = open_dir_at(parent.as_fd(), Path::new("dod"))?;
/// ensure_owned_directory(&dod)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn open_dir_at(dir: BorrowedFd<'_>, name: &Path) -> io::Result<File> {
    let name = validate_entry_name(name)?;
    let fd = retry_on_intr(|| rustix::fs::openat(dir, name, READABLE_DIR_FLAGS, Mode::empty()))?;
    Ok(File::from(fd))
}

/// Legt das Verzeichnis `name` direkt in `dir` an (`mkdirat(2)`).
///
/// # Description
/// `mkdirat` folgt nie einem Symlink als letztem Glied: liegt unter `name`
/// schon irgendein Eintrag (auch ein Symlink, auch ein ins Leere zeigender),
/// wird nichts angelegt — weder hier noch am Ziel des Symlinks. Ob der
/// vorhandene Eintrag ein echtes Verzeichnis ist, prüft der Aufrufer danach
/// selbst, etwa per [`open_beneath`] und [`ensure_owned_directory`].
///
/// # Arguments
/// - `dir` (`BorrowedFd`): das Elternverzeichnis.
/// - `name` (`&Path`): genau ein Pfadglied.
/// - `mode` (`u32`): Rechte-Bits (`& 0o7777`); der `umask` wird noch
///   abgezogen.
///
/// # Returns
/// `true`, wenn dieser Aufruf das Verzeichnis angelegt hat; `false`, wenn
/// unter `name` bereits ein Eintrag lag (`EEXIST`).
///
/// # Errors
/// `InvalidInput` für einen Namen mit `/`, `.`, `..` oder leer; sonst der
/// `errno` von `mkdirat(2)` (z. B. `EACCES`).
///
/// # Concurrency
/// Zustandslos. Legen zwei Aufrufer gleichzeitig an, meldet genau einer
/// `true`.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{mkdir_at, open_dir_nofollow};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let parent = open_dir_nofollow(Path::new("/home/user/.harw"))?;
/// if mkdir_at(parent.as_fd(), Path::new("dod"), 0o700)? {
///     // Neu angelegt: den Eintrag im Elternverzeichnis dauerhaft machen.
///     std::fs::File::from(parent).sync_all()?;
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn mkdir_at(dir: BorrowedFd<'_>, name: &Path, mode: u32) -> io::Result<bool> {
    let name = validate_entry_name(name)?;
    match retry_on_intr(|| rustix::fs::mkdirat(dir, name, mode_from_bits(mode))) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::EXIST) => Ok(false),
        Err(errno) => Err(errno.into()),
    }
}

/// Entfernt den Eintrag `name` direkt in `dir` (`unlinkat(2)` ohne
/// `AT_REMOVEDIR`).
///
/// # Description
/// Ein Symlink wird selbst entfernt, nie sein Ziel. Verzeichnisse werden
/// nicht entfernt (`EISDIR` bzw. `EPERM`).
///
/// # Arguments
/// - `dir` (`BorrowedFd`): das Verzeichnis, aus dem entfernt wird.
/// - `name` (`&Path`): genau ein Pfadglied.
///
/// # Errors
/// `InvalidInput` für einen ungültigen Namen; sonst der `errno` von
/// `unlinkat(2)`, insbesondere `NotFound`, wenn der Eintrag fehlt (der
/// Aufrufer entscheidet, ob das ein Fehler ist).
///
/// # Concurrency
/// Zustandslos.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{open_dir_nofollow, unlink_at};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let dir = open_dir_nofollow(Path::new("/home/user/.harw/dod/verdicts"))?;
/// unlink_at(dir.as_fd(), Path::new("verdicts-00000000000000000001.jsonl"))?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn unlink_at(dir: BorrowedFd<'_>, name: &Path) -> io::Result<()> {
    let name = validate_entry_name(name)?;
    retry_on_intr(|| rustix::fs::unlinkat(dir, name, AtFlags::empty()))?;
    Ok(())
}

/// Benennt `from` in `to` um, beide direkt in `dir` (`renameat(2)` mit
/// demselben Deskriptor auf beiden Seiten).
///
/// # Description
/// Ein vorhandenes `to` wird atomar ersetzt; ist es ein Symlink, wird der
/// Symlink ersetzt, nie sein Ziel beschrieben. Zusammen mit [`open_beneath`]
/// (`OpenMode::write_create_new`) ergibt das ein atomares Ersetzen innerhalb
/// eines per Deskriptor festgelegten Verzeichnisses.
///
/// # Arguments
/// - `dir` (`BorrowedFd`): das Verzeichnis beider Namen.
/// - `from` (`&Path`), `to` (`&Path`): je genau ein Pfadglied.
///
/// # Errors
/// `InvalidInput` für einen ungültigen Namen; sonst der `errno` von
/// `renameat(2)`.
///
/// # Concurrency
/// Zustandslos; die Atomizität liefert der Kernel.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{open_dir_nofollow, rename_at};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let dir = open_dir_nofollow(Path::new("/home/user/.harw/dod/verdicts"))?;
/// rename_at(dir.as_fd(), Path::new("anchor.json.tmp"), Path::new("anchor.json"))?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn rename_at(dir: BorrowedFd<'_>, from: &Path, to: &Path) -> io::Result<()> {
    let from = validate_entry_name(from)?;
    let to = validate_entry_name(to)?;
    retry_on_intr(|| rustix::fs::renameat(dir, from, dir, to))?;
    Ok(())
}

/// Der Typ des Eintrags `name` direkt in `dir`, ohne einem Symlink zu folgen
/// (`fstatat(2)` mit `AT_SYMLINK_NOFOLLOW`).
///
/// # Arguments
/// - `dir` (`BorrowedFd`): das Verzeichnis.
/// - `name` (`&Path`): genau ein Pfadglied.
///
/// # Returns
/// `Some(typ)` (ein Symlink ist [`EntryType::Symlink`], auch wenn er ins
/// Leere zeigt) oder `None`, wenn kein Eintrag existiert.
///
/// # Errors
/// `InvalidInput` für einen ungültigen Namen; sonst der `errno` von
/// `fstatat(2)` außer `ENOENT`.
///
/// # Concurrency
/// Zustandslos; das Ergebnis ist eine Momentaufnahme.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{EntryType, entry_type_at, open_dir_nofollow};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let dir = open_dir_nofollow(Path::new("/home/user/.harw"))?;
/// let kind = entry_type_at(dir.as_fd(), Path::new("config.toml"))?;
/// assert_ne!(kind, Some(EntryType::Symlink));
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn entry_type_at(dir: BorrowedFd<'_>, name: &Path) -> io::Result<Option<EntryType>> {
    let name = validate_entry_name(name)?;
    match retry_on_intr(|| rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)) {
        Ok(stat) => Ok(Some(match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => EntryType::File,
            FileType::Directory => EntryType::Dir,
            FileType::Symlink => EntryType::Symlink,
            _ => EntryType::Other,
        })),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(errno) => Err(errno.into()),
    }
}

/// Ob der Eintrag `name` direkt in `dir` genau die geöffnete Datei `file` ist
/// (gleiches Gerät und gleiche Inode-Nummer), ohne einem Symlink zu folgen.
///
/// # Description
/// Vergleicht `fstat(file)` mit `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
/// Typischer Einsatz: eine Sperrdatei, die per `flock` gesperrt wurde, ist
/// nur dann die gültige Sperre, wenn ihr Name danach noch auf dieselbe Inode
/// zeigt. Wurde der Name inzwischen entfernt oder durch eine andere Datei
/// (oder einen Symlink) ersetzt, ist die Antwort `false`.
///
/// # Arguments
/// - `dir` (`BorrowedFd`): das Verzeichnis.
/// - `name` (`&Path`): genau ein Pfadglied.
/// - `file` (`&File`): die geöffnete Datei.
///
/// # Returns
/// `true`, wenn `name` existiert und dieselbe Inode wie `file` ist; `false`,
/// wenn kein Eintrag existiert oder er etwas anderes ist.
///
/// # Errors
/// `InvalidInput` für einen ungültigen Namen; sonst der `errno` von
/// `fstat(2)` bzw. `fstatat(2)` außer `ENOENT`.
///
/// # Concurrency
/// Zustandslos; das Ergebnis ist eine Momentaufnahme.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{OpenMode, open_beneath, open_dir_nofollow, same_file_at};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let dir = open_dir_nofollow(Path::new("/home/user/.harw/dod/verdicts"))?;
/// let lock = open_beneath(dir.as_fd(), Path::new(".lock"), OpenMode::read_only())?;
/// assert!(same_file_at(dir.as_fd(), Path::new(".lock"), &lock)?);
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn same_file_at(dir: BorrowedFd<'_>, name: &Path, file: &File) -> io::Result<bool> {
    let name = validate_entry_name(name)?;
    let opened = retry_on_intr(|| rustix::fs::fstat(file))?;
    match retry_on_intr(|| rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)) {
        Ok(entry) => Ok(entry.st_dev == opened.st_dev && entry.st_ino == opened.st_ino),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(errno) => Err(errno.into()),
    }
}

/// Die Namen aller Einträge des Verzeichnisses `dir` (ohne `.`/`..`),
/// bytewise sortiert.
///
/// # Description
/// Liest per `readdir` auf einem eigenen Duplikat des Deskriptors
/// (`openat(dir, ".")`, [`rustix::fs::Dir::read_from`]); `dir` selbst und
/// seine Leseposition bleiben unberührt, und kein Pfad wird aufgelöst. Typen
/// liefert [`entry_type_at`].
///
/// # Arguments
/// - `dir` (`BorrowedFd`): ein lesbar geöffnetes Verzeichnis.
///
/// # Returns
/// Die Namen, sortiert (unabhängig von der `readdir`-Ordnung).
///
/// # Errors
/// Der `errno` von `fcntl(2)`, `openat(2)` oder `getdents(2)`.
///
/// # Concurrency
/// Zustandslos; ein gleichzeitig angelegter oder entfernter Eintrag kann
/// fehlen bzw. noch erscheinen.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{open_dir_nofollow, read_dir_names};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let dir = open_dir_nofollow(Path::new("/home/user/.harw"))?;
/// let names = read_dir_names(dir.as_fd())?;
/// assert!(names.windows(2).all(|pair| pair.first() <= pair.last()));
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn read_dir_names(dir: BorrowedFd<'_>) -> io::Result<Vec<OsString>> {
    let entries = Dir::read_from(dir)?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(OsString::from_vec(bytes.to_vec()));
    }
    names.sort();
    Ok(names)
}

/// Prüft, dass `dir` ein Verzeichnis ist, der effektiven UID gehört und
/// weder für die Gruppe noch für Andere beschreibbar ist
/// (`mode & 0o022 == 0`).
///
/// # Description
/// Die Prüfung läuft per `fstat` auf dem bereits geöffneten Deskriptor (kein
/// Pfad, kein Wettlauf zwischen Prüfen und Benutzen). Lesen und Durchsuchen
/// für Gruppe/Andere ist erlaubt; wer ein privates Verzeichnis will, setzt
/// danach selbst `0700` per `fchmod`. Ein Verzeichnis, in das Dritte
/// schreiben dürfen, kann Einträge vertauscht bekommen (z. B. ein eigenes
/// Verzeichnis gegen ein anderes eigenes); deshalb gilt es nicht als Grenze.
///
/// # Arguments
/// - `dir` (`&File`): der geöffnete Deskriptor.
///
/// # Errors
/// - `NotADirectory` (`ENOTDIR`): kein Verzeichnis.
/// - `PermissionDenied`: Eigentümer ≠ `geteuid()` oder Gruppe/Andere dürfen
///   schreiben.
/// - sonst der `errno` von `fstat`.
///
/// # Concurrency
/// Zustandslos.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{OpenMode, ensure_owned_directory, open_beneath, open_dir_nofollow};
/// use std::os::fd::AsFd;
/// use std::path::Path;
///
/// let parent = open_dir_nofollow(Path::new("/home/user/.harw"))?;
/// let dod = open_beneath(parent.as_fd(), Path::new("dod"), OpenMode::read_only())?;
/// ensure_owned_directory(&dod)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn ensure_owned_directory(dir: &File) -> io::Result<()> {
    let meta = dir.metadata()?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::from(rustix::io::Errno::NOTDIR));
    }
    let euid = rustix::process::geteuid().as_raw();
    if meta.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "ensure_owned_directory: Eigentümer-UID {} ≠ effektive UID {euid}",
                meta.uid()
            ),
        ));
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "ensure_owned_directory: Rechte {mode:04o} erlauben Gruppe/Andere zu schreiben"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use rustix::io::Errno;
    use std::io::{Read, Write};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};

    // Signatur der beiden Beneath-Öffner, die die Tests gleich behandeln.
    type Opener = fn(BorrowedFd<'_>, &Path, OpenMode) -> io::Result<File>;

    fn is_errno(err: &io::Error, candidates: &[Errno]) -> bool {
        candidates
            .iter()
            .any(|errno| err.raw_os_error() == Some(errno.raw_os_error()))
    }

    /// Wie [`is_errno`], aber liefert einen [`TestError`] statt zu assert!en,
    /// damit Aufrufer per `?` propagieren können.
    fn expect_errno(err: &io::Error, candidates: &[Errno]) -> TestResult {
        if is_errno(err, candidates) {
            Ok(())
        } else {
            Err(TestError::Unexpected(format!("{err:?}")))
        }
    }

    /// Baut `root/{dir/inner.txt, top.txt, link_file -> top.txt,
    /// link_dir -> dir, dangling -> fehlt}`.
    fn fixture() -> TestResult<tempfile::TempDir> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        std::fs::create_dir(root.join("dir"))?;
        std::fs::write(root.join("dir/inner.txt"), b"inner")?;
        std::fs::write(root.join("top.txt"), b"top")?;
        symlink("top.txt", root.join("link_file"))?;
        symlink("dir", root.join("link_dir"))?;
        symlink("fehlt", root.join("dangling"))?;
        Ok(tmp)
    }

    fn read_all(mut file: File) -> TestResult<String> {
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    #[test]
    fn oflags_validierung_wie_openoptions() -> TestResult {
        let none = OpenMode {
            read: false,
            ..OpenMode::read_only()
        };
        let Err(err) = none.oflags() else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let trunc_ro = OpenMode {
            truncate: true,
            ..OpenMode::read_only()
        };
        let Err(err) = trunc_ro.oflags() else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let trunc_append = OpenMode {
            truncate: true,
            ..OpenMode::append_create(0o600)
        };
        let Err(err) = trunc_append.oflags() else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let flags = OpenMode::write_create_new(0o600)
            .oflags()
            .map_err(ctx("gültig"))?;
        let expected = OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::CREATE | OFlags::EXCL;
        assert!(flags.contains(expected));
        assert!(flags.contains(OFlags::WRONLY));
        let flags = OpenMode::read_only().oflags().map_err(ctx("gültig"))?;
        assert!(flags.contains(OFlags::NOFOLLOW));
        assert!(
            flags.contains(OFlags::NONBLOCK),
            "R2-10: nie blockierend öffnen"
        );
        assert!(!flags.contains(OFlags::CREATE));
        Ok(())
    }

    #[test]
    fn open_nofollow_liest_regulaere_datei() -> TestResult {
        let tmp = fixture()?;
        let file = open_nofollow(&tmp.path().join("top.txt"), OpenMode::read_only())?;
        assert_eq!(read_all(file)?, "top");
        Ok(())
    }

    #[test]
    fn open_nofollow_symlink_als_letztes_glied_scheitert() -> TestResult {
        let tmp = fixture()?;
        let Err(err) = open_nofollow(&tmp.path().join("link_file"), OpenMode::read_only()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::LOOP])
    }

    #[test]
    fn open_nofollow_legt_nichts_hinter_dangling_symlink_an() -> TestResult {
        let tmp = fixture()?;
        let dangling = tmp.path().join("dangling");
        let Err(err) = open_nofollow(&dangling, OpenMode::write_truncate(0o600)) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::LOOP, Errno::EXIST])?;
        let Err(err) = open_nofollow(&dangling, OpenMode::write_create_new(0o600)) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::LOOP, Errno::EXIST])?;
        assert!(!tmp.path().join("fehlt").exists());
        Ok(())
    }

    #[test]
    fn open_dir_nofollow_lehnt_symlink_ab() -> TestResult {
        let tmp = fixture()?;
        open_dir_nofollow(&tmp.path().join("dir")).map_err(ctx("verzeichnis"))?;
        let Err(err) = open_dir_nofollow(&tmp.path().join("link_dir")) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::LOOP, Errno::NOTDIR])?;
        let Err(err) = open_dir_nofollow(&tmp.path().join("top.txt")) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::NOTDIR])
    }

    /// Gemeinsame Prüfungen für beide `open_beneath`-Pfade.
    fn check_beneath(open: fn(BorrowedFd<'_>, &Path, OpenMode) -> io::Result<File>) -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let root = root.as_fd();

        let file =
            open(root, Path::new("dir/inner.txt"), OpenMode::read_only()).map_err(ctx("nested"))?;
        assert_eq!(read_all(file)?, "inner");
        let file = open(root, Path::new("./dir/./inner.txt"), OpenMode::read_only())
            .map_err(ctx("curdir"))?;
        assert_eq!(read_all(file)?, "inner");

        // Symlink als letztes Glied.
        let Err(err) = open(root, Path::new("link_file"), OpenMode::read_only()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::LOOP])?;
        // Symlink als mittleres Glied.
        let Err(err) = open(root, Path::new("link_dir/inner.txt"), OpenMode::read_only()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::LOOP, Errno::NOTDIR])?;
        // Dangling Symlink mit Anlegen: nichts wird angelegt.
        let Err(err) = open(root, Path::new("dangling"), OpenMode::write_truncate(0o600)) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::LOOP, Errno::EXIST])?;
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
            let Err(err) = open(root, Path::new(bad), OpenMode::read_only()) else {
                return Err(TestError::Unexpected(format!("Err erwartet für {bad:?}")));
            };
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        // Ohne abschließendes `/` bleiben Verzeichnis und Wurzel öffenbar.
        let dir = open(root, Path::new("dir"), OpenMode::read_only()).map_err(ctx("dir"))?;
        assert!(dir.metadata().map_err(ctx("meta"))?.is_dir());
        let dot = open(root, Path::new("."), OpenMode::read_only()).map_err(ctx("wurzel"))?;
        assert!(dot.metadata().map_err(ctx("meta"))?.is_dir());
        let file = open(root, Path::new("dir//./inner.txt"), OpenMode::read_only())
            .map_err(ctx("mitte"))?;
        assert_eq!(read_all(file)?, "inner");

        // Anlegen unterhalb der Wurzel mit Rechten ohne Gruppe/Andere.
        let neu = OpenMode::write_create_new(0o600);
        let mut file = open(root, Path::new("dir/neu.txt"), neu).map_err(ctx("create"))?;
        file.write_all(b"neu")?;
        let meta = std::fs::symlink_metadata(tmp.path().join("dir/neu.txt"))?;
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o077, 0);
        let Err(err) = open(root, Path::new("dir/neu.txt"), neu) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::EXIST])
    }

    #[test]
    fn open_beneath_oeffentlicher_pfad() -> TestResult {
        check_beneath(open_beneath)
    }

    #[test]
    fn open_beneath_enosys_fallback_komponentenweise() -> TestResult {
        check_beneath(open_beneath_componentwise)
    }

    #[test]
    fn fallback_punkt_oeffnet_wurzel() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let dot = Path::new(".");
        let file = open_beneath_componentwise(root.as_fd(), dot, OpenMode::read_only())
            .map_err(ctx("wurzel"))?;
        assert!(file.metadata().map_err(ctx("meta"))?.is_dir());
        Ok(())
    }

    /// Führt `f` in einem eigenen Thread aus. Blockiert es länger als 10 s,
    /// schlägt der Test mit einem `Err` fehl, statt die ganze Testsuite
    /// aufzuhängen.
    fn within_timeout<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> TestResult<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Der Empfänger existiert nach einem Timeout nicht mehr; dann ist
            // der Test ohnehin schon fehlgeschlagen.
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .map_err(ctx("Öffnen blockiert (FIFO ohne O_NONBLOCK?)"))
    }

    /// Erfolg wird zu `None`, ein Fehler zu seiner `ErrorKind`.
    fn outcome(result: io::Result<File>) -> Option<io::ErrorKind> {
        result.err().map(|err| err.kind())
    }

    /// Legt ein FIFO per `mkfifo(1)` an (rustix bietet `mknodat`, das Crate
    /// braucht es aber sonst nicht). `false`, wenn das Programm fehlt.
    fn make_fifo(path: &Path) -> bool {
        // Fehlendes Programm oder Fehlschlag: der Aufrufer überspringt den Test
        // still (kein `eprintln!` in Tests erlaubt).
        matches!(
            std::process::Command::new("mkfifo").arg(path).status(),
            Ok(status) if status.success()
        )
    }

    /// R2-10: Ein FIFO als letztes Glied blockiert nicht und wird abgelehnt.
    #[test]
    fn fifo_blockiert_nicht_und_wird_abgelehnt() -> TestResult {
        let tmp = fixture()?;
        let fifo = tmp.path().join("fifo");
        if !make_fifo(&fifo) {
            return Ok(());
        }
        let invalid = Some(io::ErrorKind::InvalidInput);

        let path = fifo.clone();
        let ro = within_timeout(move || outcome(open_nofollow(&path, OpenMode::read_only())))?;
        assert_eq!(ro, invalid);
        let path = fifo.clone();
        let rw = within_timeout(move || outcome(open_nofollow(&path, OpenMode::read_write())))?;
        assert_eq!(rw, invalid);
        // Schreiben ohne Leser: `ENXIO` statt Warten.
        let path = fifo.clone();
        let write = OpenMode::write_truncate(0o600);
        let wo = within_timeout(move || outcome(open_nofollow(&path, write)))?;
        assert!(wo.is_some(), "Schreib-Öffnen eines FIFOs muss scheitern");

        let openers: [Opener; 2] = [open_beneath, open_beneath_componentwise];
        for open in openers {
            let root_path = tmp.path().to_path_buf();
            let result = within_timeout(move || -> TestResult<Option<io::ErrorKind>> {
                let root = open_dir_nofollow(&root_path).map_err(ctx("root"))?;
                Ok(outcome(open(
                    root.as_fd(),
                    Path::new("fifo"),
                    OpenMode::read_only(),
                )))
            })??;
            assert_eq!(result, invalid);
        }
        assert!(std::fs::symlink_metadata(&fifo)?.file_type().is_fifo());
        Ok(())
    }

    /// R2-10: Geräte werden ebenfalls abgelehnt.
    ///
    /// In manchen Sandboxen ist `/dev/null` eine reguläre Datei; deshalb wird
    /// das erste echte Zeichengerät aus einer kleinen Kandidatenliste genommen
    /// und ohne Gerät übersprungen.
    #[test]
    fn zeichengeraet_wird_abgelehnt() -> TestResult {
        let devices = ["/dev/null", "/dev/zero", "/dev/urandom"];
        let Some(device) = devices.iter().map(|name| Path::new(*name)).find(|path| {
            std::fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_char_device())
                .unwrap_or(false)
        }) else {
            // Kein Zeichengerät verfügbar: Test wird still übersprungen.
            return Ok(());
        };
        let Err(err) = open_nofollow(device, OpenMode::read_only()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        Ok(())
    }

    /// R2-10: Zurückgegebene Deskriptoren sind wieder blockierend; `O_APPEND`
    /// bleibt beim Entfernen von `O_NONBLOCK` erhalten.
    #[test]
    fn nonblock_wird_nach_dem_oeffnen_entfernt() -> TestResult {
        let tmp = fixture()?;
        let file = open_nofollow(&tmp.path().join("top.txt"), OpenMode::read_only())?;
        let status = rustix::fs::fcntl_getfl(file.as_fd()).map_err(ctx("getfl"))?;
        assert!(!status.contains(OFlags::NONBLOCK));

        let log = tmp.path().join("log");
        let file = open_nofollow(&log, OpenMode::append_create(0o600)).map_err(ctx("append"))?;
        let status = rustix::fs::fcntl_getfl(file.as_fd()).map_err(ctx("getfl"))?;
        assert!(status.contains(OFlags::APPEND));
        assert!(!status.contains(OFlags::NONBLOCK));

        let root = open_dir_nofollow(tmp.path())?;
        let openers: [Opener; 2] = [open_beneath, open_beneath_componentwise];
        for open in openers {
            let file = open(
                root.as_fd(),
                Path::new("dir/inner.txt"),
                OpenMode::read_only(),
            )
            .map_err(ctx("open"))?;
            let status = rustix::fs::fcntl_getfl(file.as_fd()).map_err(ctx("getfl"))?;
            assert!(!status.contains(OFlags::NONBLOCK));
            let dir =
                open(root.as_fd(), Path::new("."), OpenMode::read_only()).map_err(ctx("dir"))?;
            let status = rustix::fs::fcntl_getfl(dir.as_fd()).map_err(ctx("getfl"))?;
            assert!(!status.contains(OFlags::NONBLOCK));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fallback_passiert_nur_durchsuchbares_verzeichnis() -> TestResult {
        let tmp = fixture()?;
        let dir = tmp.path().join("dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o100))?;
        let root = open_dir_nofollow(tmp.path())?;
        let rel = Path::new("dir/inner.txt");
        let result = open_beneath_componentwise(root.as_fd(), rel, OpenMode::read_only());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        assert_eq!(read_all(result.map_err(ctx("O_PATH-Abstieg"))?)?, "inner");
        Ok(())
    }

    /// Die `*_at`-Helfer nehmen genau ein Pfadglied; alles andere scheitert
    /// vor jedem Syscall.
    #[test]
    fn at_helfer_lehnen_mehr_als_ein_pfadglied_ab() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let root = root.as_fd();
        for bad in [
            "",
            ".",
            "..",
            "dir/inner.txt",
            "link_dir/x",
            "top.txt/",
            "/etc",
        ] {
            let name = Path::new(bad);
            let results = [
                open_dir_at(root, name).map(|_| ()),
                mkdir_at(root, name, 0o700).map(|_| ()),
                unlink_at(root, name),
                rename_at(root, name, Path::new("neu")),
                rename_at(root, Path::new("top.txt"), name),
                entry_type_at(root, name).map(|_| ()),
            ];
            for result in results {
                let Err(err) = result else {
                    return Err(TestError::Unexpected(format!("Err erwartet für {bad:?}")));
                };
                assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
            }
        }
        // Nichts wurde angelegt, entfernt oder umbenannt.
        assert_eq!(read_dir_names(root)?.len(), 5);
        assert_eq!(std::fs::read(tmp.path().join("dir/inner.txt"))?, b"inner");
        assert_eq!(std::fs::read(tmp.path().join("top.txt"))?, b"top");
        Ok(())
    }

    #[test]
    fn open_dir_at_oeffnet_nur_echte_verzeichnisse() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let root = root.as_fd();
        let dir = open_dir_at(root, Path::new("dir")).map_err(ctx("verzeichnis"))?;
        assert!(dir.metadata()?.is_dir());
        assert_eq!(read_dir_names(dir.as_fd())?, [OsString::from("inner.txt")]);
        // Ein Symlink (auf ein Verzeichnis oder ins Leere) wird nie verfolgt,
        // eine reguläre Datei nie geöffnet.
        for name in ["link_dir", "dangling"] {
            let Err(err) = open_dir_at(root, Path::new(name)) else {
                return Err(TestError::Unexpected(format!("Err erwartet für {name}")));
            };
            expect_errno(&err, &[Errno::LOOP, Errno::NOTDIR])?;
        }
        let Err(err) = open_dir_at(root, Path::new("top.txt")) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::NOTDIR])?;
        let Err(err) = open_dir_at(root, Path::new("fehlt")) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        Ok(())
    }

    /// Ein FIFO unter dem Namen wird mit `ENOTDIR` abgelehnt, ohne geöffnet
    /// zu werden (kein Blockieren, auch ohne `O_NONBLOCK`).
    #[test]
    fn open_dir_at_oeffnet_kein_fifo() -> TestResult {
        let tmp = fixture()?;
        let fifo = tmp.path().join("fifo");
        if !make_fifo(&fifo) {
            return Ok(());
        }
        let root_path = tmp.path().to_path_buf();
        let errno = within_timeout(move || -> TestResult<Option<i32>> {
            let root = open_dir_nofollow(&root_path).map_err(ctx("root"))?;
            Ok(open_dir_at(root.as_fd(), Path::new("fifo"))
                .err()
                .and_then(|err| err.raw_os_error()))
        })??;
        assert_eq!(errno, Some(Errno::NOTDIR.raw_os_error()));
        assert!(std::fs::symlink_metadata(&fifo)?.file_type().is_fifo());
        Ok(())
    }

    #[test]
    fn mkdir_at_legt_an_und_folgt_keinem_symlink() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let root = root.as_fd();
        assert!(mkdir_at(root, Path::new("neu"), 0o700).map_err(ctx("anlegen"))?);
        let meta = std::fs::symlink_metadata(tmp.path().join("neu"))?;
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o077, 0);
        // Vorhandene Einträge jeder Art, auch Symlinks (ins Leere): `false`,
        // und am Ziel eines Symlinks entsteht nichts.
        for existing in ["neu", "top.txt", "link_dir", "dangling"] {
            assert!(
                !mkdir_at(root, Path::new(existing), 0o700).map_err(ctx("vorhanden"))?,
                "{existing}"
            );
        }
        assert!(!tmp.path().join("fehlt").exists());
        let dir = open_dir_nofollow(&tmp.path().join("dir"))?;
        assert_eq!(read_dir_names(dir.as_fd())?, [OsString::from("inner.txt")]);
        Ok(())
    }

    #[test]
    fn entry_type_at_meldet_symlinks_statt_ihnen_zu_folgen() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let root = root.as_fd();
        let expected = [
            ("top.txt", Some(EntryType::File)),
            ("dir", Some(EntryType::Dir)),
            ("link_file", Some(EntryType::Symlink)),
            ("link_dir", Some(EntryType::Symlink)),
            ("dangling", Some(EntryType::Symlink)),
            ("fehlt", None),
        ];
        for (name, kind) in expected {
            assert_eq!(entry_type_at(root, Path::new(name))?, kind, "{name}");
        }
        Ok(())
    }

    #[test]
    fn unlink_at_entfernt_nur_den_eintrag() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let root = root.as_fd();
        // Ein Symlink wird selbst entfernt, sein Ziel bleibt.
        unlink_at(root, Path::new("link_file")).map_err(ctx("datei-symlink"))?;
        assert_eq!(entry_type_at(root, Path::new("link_file"))?, None);
        assert_eq!(std::fs::read(tmp.path().join("top.txt"))?, b"top");
        unlink_at(root, Path::new("link_dir")).map_err(ctx("verzeichnis-symlink"))?;
        assert_eq!(std::fs::read(tmp.path().join("dir/inner.txt"))?, b"inner");
        unlink_at(root, Path::new("top.txt")).map_err(ctx("datei"))?;
        assert_eq!(entry_type_at(root, Path::new("top.txt"))?, None);
        // Verzeichnisse nie; ein fehlender Eintrag meldet `NotFound`.
        let Err(err) = unlink_at(root, Path::new("dir")) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::ISDIR, Errno::PERM])?;
        let Err(err) = unlink_at(root, Path::new("fehlt")) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        Ok(())
    }

    #[test]
    fn rename_at_ersetzt_einen_ziel_symlink_ohne_ihm_zu_folgen() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let root = root.as_fd();
        std::fs::write(tmp.path().join("neu.tmp"), b"neu")?;
        // Das Ziel ist ein Symlink: ersetzt wird der Symlink, nicht `top.txt`.
        rename_at(root, Path::new("neu.tmp"), Path::new("link_file")).map_err(ctx("rename"))?;
        assert_eq!(
            entry_type_at(root, Path::new("link_file"))?,
            Some(EntryType::File)
        );
        assert_eq!(std::fs::read(tmp.path().join("link_file"))?, b"neu");
        assert_eq!(std::fs::read(tmp.path().join("top.txt"))?, b"top");
        assert_eq!(entry_type_at(root, Path::new("neu.tmp"))?, None);
        Ok(())
    }

    #[test]
    fn same_file_at_erkennt_entfernte_und_ersetzte_eintraege() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let root = root.as_fd();
        let opened = File::open(tmp.path().join("top.txt"))?;
        assert!(same_file_at(root, Path::new("top.txt"), &opened)?);
        // Ein Symlink auf dieselbe Datei ist ein anderer Eintrag.
        assert!(!same_file_at(root, Path::new("link_file"), &opened)?);
        // Ersetzt (neue Inode unter demselben Namen) und entfernt: `false`.
        std::fs::write(tmp.path().join("neu.tmp"), b"neu")?;
        rename_at(root, Path::new("neu.tmp"), Path::new("top.txt")).map_err(ctx("rename"))?;
        assert!(!same_file_at(root, Path::new("top.txt"), &opened)?);
        let current = File::open(tmp.path().join("top.txt"))?;
        assert!(same_file_at(root, Path::new("top.txt"), &current)?);
        unlink_at(root, Path::new("top.txt")).map_err(ctx("unlink"))?;
        assert!(!same_file_at(root, Path::new("top.txt"), &current)?);
        // Mehr als ein Pfadglied wird abgelehnt.
        let Err(err) = same_file_at(root, Path::new("dir/inner.txt"), &current) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        Ok(())
    }

    #[test]
    fn read_dir_names_liefert_sortierte_namen_ohne_punkte() -> TestResult {
        let tmp = fixture()?;
        let root = open_dir_nofollow(tmp.path())?;
        let expected: Vec<OsString> = ["dangling", "dir", "link_dir", "link_file", "top.txt"]
            .into_iter()
            .map(OsString::from)
            .collect();
        assert_eq!(read_dir_names(root.as_fd())?, expected);
        // `readdir` läuft auf einer eigenen Kopie: ein zweiter Aufruf auf
        // demselben Deskriptor sieht wieder alles.
        assert_eq!(read_dir_names(root.as_fd())?, expected);
        Ok(())
    }

    #[test]
    fn ensure_owned_directory_prueft_typ_und_schreibrechte() -> TestResult {
        let tmp = fixture()?;
        let dir = tmp.path().join("dir");
        for mode in [0o700, 0o750, 0o755] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode))?;
            ensure_owned_directory(&File::open(&dir)?).map_err(ctx("privat genug"))?;
        }
        for mode in [0o770, 0o702, 0o777] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode))?;
            let Err(err) = ensure_owned_directory(&File::open(&dir)?) else {
                return Err(TestError::Unexpected(format!("Err erwartet für {mode:o}")));
            };
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{mode:o}");
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let Err(err) = ensure_owned_directory(&File::open(tmp.path().join("top.txt"))?) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        expect_errno(&err, &[Errno::NOTDIR])
    }
}
