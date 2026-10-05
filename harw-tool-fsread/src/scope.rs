//! Workspace-Scope: die einzige Stelle, an der ein vom Modell gelieferter
//! Pfad in einen Dateisystemzugriff übersetzt wird.
//!
//! # Verantwortung
//! - [`RelPath`]: ein **lexikalisch normalisierter, relativer** Pfad ohne `..`
//!   (ein `..`, das die Wurzel verließe, ist ein Fehler).
//! - [`Scope`]: öffnet, stat-et und liest Links ausschließlich **relativ zum
//!   Deskriptor der Workspace-Wurzel**; jedes Pfadglied wird mit
//!   `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` bzw. `openat(O_NOFOLLOW)`
//!   geöffnet ([`harw_fsutil::open_beneath`]). Ein Symlink in irgendeinem
//!   Pfadglied ist damit ein Fehler, kein Ausbruch — auch nicht im Rennen
//!   zwischen Prüfen und Öffnen.
//! - [`Scope::resolve_follow`]: für `-L`/`--follow`: löst Symlinks selbst auf,
//!   Glied für Glied, und lehnt jedes Ziel außerhalb der Wurzel ab.
//! - [`is_secret_path`]: Schlüsselmaterial (`.ssh`, `.gnupg`, `auth.toml`,
//!   `*.pem`, `*.key`, `.env*`, …) wird nie gelesen, unabhängig vom Workspace.
//!
//! # Sicherheit
//! Die Wurzel kommt ausschließlich aus dem [`harw_tools::ToolExecutionContext`]
//! (kanonisch, vom Harness gesetzt), nie aus den Argumenten.
//!
//! # Nebenläufigkeit
//! [`Scope`] hält einen Deskriptor und ist `Send + Sync`.

use harw_fsutil::{OpenMode, open_beneath, open_dir_nofollow};
use rustix::fs::{AtFlags, FileType, Stat};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

/// Höchstzahl aufgelöster Symlinks in [`Scope::resolve_follow`].
pub const MAX_SYMLINK_HOPS: usize = 40;

/// Höchstlänge eines Pfad-Arguments in Bytes.
pub const MAX_PATH_BYTES: usize = 4096;

/// Fehler der Pfadauflösung.
#[derive(Debug)]
pub enum ScopeError {
    /// Der Pfad ist leer, zu lang oder enthält ein NUL-Byte.
    Invalid(&'static str),
    /// Der Pfad verließe den Workspace (`..` oder absolut außerhalb).
    Escapes(String),
    /// Der Pfad ist ein gesperrter Geheimnis-Pfad.
    Denied(String),
    /// Zu viele Symlinks (Schleife).
    SymlinkLoop(String),
    /// Ein Symlink im Pfad (nicht gefolgt).
    Symlink(String),
    /// Ein Ein-/Ausgabefehler.
    Io(io::Error),
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid path: {reason}"),
            Self::Escapes(path) => write!(f, "path '{path}' is outside the workspace"),
            Self::Denied(path) => write!(f, "path '{path}' is protected (secret material)"),
            Self::SymlinkLoop(path) => write!(f, "too many symbolic links resolving '{path}'"),
            Self::Symlink(path) => write!(
                f,
                "'{path}' passes through a symbolic link; use follow=true to resolve links inside the workspace"
            ),
            Self::Io(error) => write!(f, "{}", io_message(error)),
        }
    }
}

impl std::error::Error for ScopeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ScopeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rustix::io::Errno> for ScopeError {
    fn from(errno: rustix::io::Errno) -> Self {
        Self::Io(io::Error::from(errno))
    }
}

/// Kurze, pfadfreie Fehlermeldung zu einem I/O-Fehler.
#[must_use]
pub fn io_message(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "no such file or directory".to_owned(),
        io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        _ => match error
            .raw_os_error()
            .map(rustix::io::Errno::from_raw_os_error)
        {
            Some(rustix::io::Errno::LOOP) => "symbolic link not followed".to_owned(),
            Some(rustix::io::Errno::NOTDIR) => "not a directory".to_owned(),
            Some(rustix::io::Errno::ISDIR) => "is a directory".to_owned(),
            _ => error.to_string(),
        },
    }
}

/// Ein lexikalisch normalisierter, relativer Pfad innerhalb der Wurzel.
///
/// Der leere Pfad bezeichnet die Wurzel selbst. Garantien: keine absolute
/// Form, keine `.`- und keine `..`-Komponenten.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct RelPath(PathBuf);

impl RelPath {
    /// Die Wurzel selbst.
    #[must_use]
    pub fn root() -> Self {
        Self(PathBuf::new())
    }

    /// Der Pfad relativ zur Wurzel (leer für die Wurzel).
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// `true` für die Wurzel selbst.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.as_os_str().is_empty()
    }

    /// Anzeigeform: `.` für die Wurzel, sonst der relative Pfad.
    #[must_use]
    pub fn display(&self) -> String {
        if self.is_root() {
            ".".to_owned()
        } else {
            self.0.to_string_lossy().into_owned()
        }
    }

    /// Hängt einen einzelnen Namen an.
    #[must_use]
    pub fn join(&self, name: &OsStr) -> Self {
        Self(self.0.join(name))
    }

    /// Letzter Namensbestandteil.
    #[must_use]
    pub fn file_name(&self) -> Option<&OsStr> {
        self.0.file_name()
    }

    fn parent(&self) -> Self {
        Self(self.0.parent().map(Path::to_path_buf).unwrap_or_default())
    }

    /// Normalisiert `components` lexikalisch; `None`, wenn `..` die Wurzel verließe.
    fn from_components<'a>(components: impl Iterator<Item = Component<'a>>) -> Option<Self> {
        let mut out = PathBuf::new();
        for component in components {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() {
                        return None;
                    }
                }
                Component::Normal(name) => out.push(name),
                Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        Some(Self(out))
    }
}

/// Dateinamen, die nie gelesen werden.
const SECRET_NAMES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".netrc",
    ".pgpass",
    ".git-credentials",
    "auth.toml",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "secrets",
];

/// Endungen von Schlüsselmaterial.
const SECRET_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx", "kdbx", "jks", "keystore"];

/// `true`, wenn irgendein Glied von `rel` auf Schlüsselmaterial hindeutet.
///
/// Geprüft wird jede Komponente: `.ssh/known_hosts` ist genauso gesperrt wie
/// `.ssh` selbst. `.env` und `.env.*` zählen als Geheimnis, `.env.example`
/// und `.env.sample` nicht.
#[must_use]
pub fn is_secret_path(rel: &Path) -> bool {
    rel.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        let name = name.to_string_lossy().to_ascii_lowercase();
        if SECRET_NAMES.contains(&name.as_str()) {
            return true;
        }
        if name.starts_with("id_rsa")
            || name.starts_with("id_ed25519")
            || name.starts_with("id_ecdsa")
        {
            return !name.ends_with(".pub");
        }
        if name == ".env"
            || (name.starts_with(".env.")
                && !name.ends_with(".example")
                && !name.ends_with(".sample")
                && !name.ends_with(".template"))
        {
            return true;
        }
        Path::new(&name)
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| SECRET_EXTENSIONS.contains(&extension))
    })
}

/// Der Zugriff auf genau einen Workspace.
#[derive(Debug)]
pub struct Scope {
    root: PathBuf,
    root_fd: OwnedFd,
}

impl Scope {
    /// Öffnet die (kanonische) Workspace-Wurzel.
    ///
    /// # Errors
    /// Wenn die Wurzel kein lesbares Verzeichnis ist oder ihr letztes Glied
    /// ein Symlink ist.
    pub fn new(root: &Path) -> io::Result<Self> {
        let root_fd = open_dir_nofollow(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            root_fd,
        })
    }

    /// Die kanonische Wurzel.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Deskriptor der Wurzel (für dirfd-relative Aufrufe).
    #[must_use]
    pub fn root_fd(&self) -> BorrowedFd<'_> {
        self.root_fd.as_fd()
    }

    /// Normalisiert ein Pfad-Argument zu einem [`RelPath`].
    ///
    /// Leer und `.` bezeichnen die Wurzel. Absolute Pfade sind nur erlaubt,
    /// wenn sie unter der Wurzel liegen.
    ///
    /// # Errors
    /// Zu lange Pfade, NUL-Bytes, Ausbruch über `..` oder absolut außerhalb.
    pub fn rel(&self, input: &str) -> Result<RelPath, ScopeError> {
        if input.len() > MAX_PATH_BYTES {
            return Err(ScopeError::Invalid("path longer than 4096 bytes"));
        }
        if input.contains('\0') {
            return Err(ScopeError::Invalid("path contains a NUL byte"));
        }
        let path = Path::new(input);
        let escapes = || ScopeError::Escapes(truncate_for_error(input));
        if path.is_absolute() {
            let stripped = path.strip_prefix(&self.root).map_err(|_| escapes())?;
            return RelPath::from_components(stripped.components()).ok_or_else(escapes);
        }
        RelPath::from_components(path.components()).ok_or_else(escapes)
    }

    /// Wie [`Scope::rel`] und zusätzlich: Geheimnis-Pfade werden abgelehnt.
    ///
    /// # Errors
    /// Wie [`Scope::rel`] plus [`ScopeError::Denied`].
    pub fn rel_readable(&self, input: &str) -> Result<RelPath, ScopeError> {
        let rel = self.rel(input)?;
        if is_secret_path(rel.as_path()) {
            return Err(ScopeError::Denied(truncate_for_error(input)));
        }
        Ok(rel)
    }

    /// Öffnet ein Verzeichnis relativ zur Wurzel (kein Symlink im Pfad).
    ///
    /// # Errors
    /// I/O-Fehler; `ENOTDIR`, wenn es kein Verzeichnis ist.
    pub fn open_dir(&self, rel: &RelPath) -> io::Result<OwnedFd> {
        if rel.is_root() {
            return self.root_fd.try_clone();
        }
        let file = open_beneath(self.root_fd.as_fd(), rel.as_path(), OpenMode::read_only())?;
        let fd = OwnedFd::from(file);
        let stat = rustix::fs::fstat(&fd)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(io::Error::from(rustix::io::Errno::NOTDIR));
        }
        Ok(fd)
    }

    /// Öffnet eine reguläre Datei zum Lesen. Verweigert Geheimnis-Pfade.
    ///
    /// # Errors
    /// [`ScopeError::Denied`], [`ScopeError::Io`] (inkl. `EISDIR` für Verzeichnisse
    /// und `ELOOP` für Symlinks).
    pub fn open_read(&self, rel: &RelPath) -> Result<File, ScopeError> {
        if is_secret_path(rel.as_path()) {
            return Err(ScopeError::Denied(rel.display()));
        }
        if rel.is_root() {
            return Err(ScopeError::Io(io::Error::from(rustix::io::Errno::ISDIR)));
        }
        let file = open_beneath(self.root_fd.as_fd(), rel.as_path(), OpenMode::read_only())?;
        let stat = rustix::fs::fstat(&file)?;
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Ok(file),
            FileType::Directory => Err(ScopeError::Io(io::Error::from(rustix::io::Errno::ISDIR))),
            _ => Err(ScopeError::Io(io::Error::from(rustix::io::Errno::INVAL))),
        }
    }

    /// Führt `f` mit dem Deskriptor des Elternverzeichnisses und dem letzten
    /// Namen aus. Für die Wurzel selbst gibt es keinen Namen: dort `fstat`.
    fn with_parent<R>(
        &self,
        rel: &RelPath,
        f: impl FnOnce(BorrowedFd<'_>, &OsStr) -> io::Result<R>,
    ) -> io::Result<R> {
        let Some(name) = rel.file_name() else {
            return Err(io::Error::from(rustix::io::Errno::INVAL));
        };
        let parent = rel.parent();
        if parent.is_root() {
            return f(self.root_fd.as_fd(), name);
        }
        let dir = self.open_dir(&parent)?;
        f(dir.as_fd(), name)
    }

    /// `lstat`: Metadaten des Eintrags selbst, Symlinks werden nicht gefolgt
    /// (auch nicht in Zwischengliedern).
    ///
    /// # Errors
    /// I/O-Fehler.
    pub fn lstat(&self, rel: &RelPath) -> io::Result<Stat> {
        if rel.is_root() {
            return Ok(rustix::fs::fstat(&self.root_fd)?);
        }
        self.with_parent(rel, |dir, name| {
            Ok(rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?)
        })
    }

    /// Ziel eines Symlinks (`readlink`), Zwischenglieder ohne Symlink.
    ///
    /// # Errors
    /// I/O-Fehler; `EINVAL`, wenn es kein Symlink ist.
    pub fn read_link(&self, rel: &RelPath) -> io::Result<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        self.with_parent(rel, |dir, name| {
            let target = rustix::fs::readlinkat(dir, name, Vec::new())?;
            Ok(PathBuf::from(std::ffi::OsString::from_vec(
                target.into_bytes(),
            )))
        })
    }

    /// Löst alle Symlinks von `rel` auf (`-L`/`--follow`).
    ///
    /// Jedes Zwischenergebnis bleibt unter der Wurzel; ein Ziel außerhalb ist
    /// [`ScopeError::Escapes`], eine Schleife [`ScopeError::SymlinkLoop`].
    /// Ein nicht existierendes **letztes** Glied ist kein Fehler (der
    /// Aufrufer sieht es beim Öffnen).
    ///
    /// # Errors
    /// Siehe oben sowie I/O-Fehler für fehlende Zwischenglieder.
    pub fn resolve_follow(&self, rel: &RelPath) -> Result<RelPath, ScopeError> {
        let display = rel.display();
        let mut resolved = RelPath::root();
        let mut pending: VecDeque<Part> = parts_of(rel.as_path()).collect();
        let mut hops = 0usize;
        while let Some(part) = pending.pop_front() {
            let name = match part {
                Part::Up => {
                    if resolved.is_root() {
                        return Err(ScopeError::Escapes(display));
                    }
                    resolved = resolved.parent();
                    continue;
                }
                Part::Name(name) => name,
            };
            let candidate = resolved.join(&name);
            match self.lstat(&candidate) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
                    hops += 1;
                    if hops > MAX_SYMLINK_HOPS {
                        return Err(ScopeError::SymlinkLoop(display));
                    }
                    let target = self.read_link(&candidate)?;
                    let target = if target.is_absolute() {
                        let inside = target
                            .strip_prefix(&self.root)
                            .map_err(|_| ScopeError::Escapes(display.clone()))?
                            .to_path_buf();
                        resolved = RelPath::root();
                        inside
                    } else {
                        // Relative Ziele gelten ab dem Verzeichnis des Links,
                        // also ab dem unveränderten `resolved`.
                        target
                    };
                    for item in parts_of(&target).collect::<Vec<_>>().into_iter().rev() {
                        pending.push_front(item);
                    }
                }
                Ok(_) => resolved = candidate,
                Err(error) if error.kind() == io::ErrorKind::NotFound && pending.is_empty() => {
                    resolved = candidate;
                }
                Err(error) => return Err(ScopeError::Io(error)),
            }
        }
        Ok(resolved)
    }
}

/// Ein Glied eines noch aufzulösenden Pfads.
enum Part {
    /// `..`
    Up,
    /// Ein Name.
    Name(std::ffi::OsString),
}

/// Zerlegt einen Pfad in [`Part`]s (`.` entfällt, Wurzelzeichen ebenso).
fn parts_of(path: &Path) -> impl Iterator<Item = Part> + '_ {
    path.components().filter_map(|component| match component {
        Component::Normal(name) => Some(Part::Name(name.to_os_string())),
        Component::ParentDir => Some(Part::Up),
        Component::CurDir | Component::RootDir | Component::Prefix(_) => None,
    })
}

/// Kürzt ein Pfad-Argument für Fehlermeldungen.
fn truncate_for_error(input: &str) -> String {
    let shown: String = input.chars().take(120).collect();
    if shown.len() < input.len() {
        format!("{shown}…")
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, SECRET, TestError, TestResult};
    use std::io::Read;

    #[test]
    fn rel_normalizes_and_rejects_escape() -> TestResult {
        let fx = Fixture::new()?;
        let scope = fx.scope()?;
        assert!(scope.rel("")?.is_root());
        assert!(scope.rel(".")?.is_root());
        assert_eq!(scope.rel("a/./b/../c")?.display(), "a/c");
        assert!(matches!(scope.rel(".."), Err(ScopeError::Escapes(_))));
        assert!(matches!(
            scope.rel("a/../../x"),
            Err(ScopeError::Escapes(_))
        ));
        assert!(matches!(
            scope.rel("/etc/passwd"),
            Err(ScopeError::Escapes(_))
        ));
        assert!(matches!(scope.rel("a\0b"), Err(ScopeError::Invalid(_))));
        let long = "a/".repeat(MAX_PATH_BYTES);
        assert!(matches!(scope.rel(&long), Err(ScopeError::Invalid(_))));
        let absolute = format!("{}/sub/file", fx.ws.display());
        assert_eq!(scope.rel(&absolute)?.display(), "sub/file");
        Ok(())
    }

    #[test]
    fn open_read_never_follows_symlinks_out_of_the_workspace() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        let scope = fx.scope()?;
        for path in [
            "link_file",
            "link_dir/secret.txt",
            "loop/link_file",
            "nested/up/link_file",
        ] {
            let rel = scope.rel(path)?;
            match scope.open_read(&rel) {
                Ok(mut file) => {
                    let mut content = String::new();
                    file.read_to_string(&mut content)?;
                    return Err(TestError::Unexpected(format!(
                        "{path} was readable: {content}"
                    )));
                }
                Err(ScopeError::Io(_)) => {}
                Err(other) => return Err(TestError::Unexpected(format!("{path}: {other}"))),
            }
        }
        Ok(())
    }

    #[test]
    fn lstat_does_not_traverse_intermediate_symlinks() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        let scope = fx.scope()?;
        assert!(scope.lstat(&scope.rel("link_dir/secret.txt")?).is_err());
        let own = scope.lstat(&scope.rel("link_dir")?)?;
        assert_eq!(FileType::from_raw_mode(own.st_mode), FileType::Symlink);
        Ok(())
    }

    #[test]
    fn follow_resolves_inside_and_rejects_outside() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("real/data.txt", b"x")?;
        std::os::unix::fs::symlink("real", fx.ws.join("alias"))?;
        std::os::unix::fs::symlink("../real/data.txt", fx.ws.join("real/rel_link"))?;
        let outer = fx.outside.join("secret.txt");
        std::os::unix::fs::symlink(&outer, fx.ws.join("abs_out"))?;
        std::os::unix::fs::symlink("../outside/secret.txt", fx.ws.join("rel_out"))?;
        std::os::unix::fs::symlink(fx.ws.join("real"), fx.ws.join("abs_in"))?;
        let scope = fx.scope()?;
        assert_eq!(
            scope
                .resolve_follow(&scope.rel("alias/data.txt")?)?
                .display(),
            "real/data.txt"
        );
        assert_eq!(
            scope
                .resolve_follow(&scope.rel("real/rel_link")?)?
                .display(),
            "real/data.txt"
        );
        assert_eq!(
            scope
                .resolve_follow(&scope.rel("abs_in/data.txt")?)?
                .display(),
            "real/data.txt"
        );
        assert!(matches!(
            scope.resolve_follow(&scope.rel("abs_out")?),
            Err(ScopeError::Escapes(_))
        ));
        assert!(matches!(
            scope.resolve_follow(&scope.rel("rel_out")?),
            Err(ScopeError::Escapes(_))
        ));
        Ok(())
    }

    #[test]
    fn follow_chain_longer_than_the_hop_limit_is_rejected() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("target.txt", b"x")?;
        // Kette l0 -> l1 -> ... -> lN -> target.txt
        let build = |count: usize, prefix: &str| -> TestResult<String> {
            for index in 0..count {
                let next = if index + 1 == count {
                    "target.txt".to_owned()
                } else {
                    format!("{prefix}{}", index + 1)
                };
                std::os::unix::fs::symlink(next, fx.ws.join(format!("{prefix}{index}")))?;
            }
            Ok(format!("{prefix}0"))
        };
        let short_head = build(MAX_SYMLINK_HOPS - 1, "s")?;
        let long_head = build(MAX_SYMLINK_HOPS + 2, "l")?;
        let scope = fx.scope()?;
        assert_eq!(
            scope.resolve_follow(&scope.rel(&short_head)?)?.display(),
            "target.txt"
        );
        assert!(matches!(
            scope.resolve_follow(&scope.rel(&long_head)?),
            Err(ScopeError::SymlinkLoop(_))
        ));
        Ok(())
    }

    #[test]
    fn follow_detects_loops() -> TestResult {
        let fx = Fixture::new()?;
        std::os::unix::fs::symlink("b", fx.ws.join("a"))?;
        std::os::unix::fs::symlink("a", fx.ws.join("b"))?;
        let scope = fx.scope()?;
        assert!(matches!(
            scope.resolve_follow(&scope.rel("a")?),
            Err(ScopeError::SymlinkLoop(_))
        ));
        Ok(())
    }

    #[test]
    fn secret_paths_are_denied_for_reading() -> TestResult {
        let fx = Fixture::new()?;
        fx.write(".ssh/id_ed25519", b"KEY")?;
        fx.write("config/server.pem", b"KEY")?;
        fx.write(".env", b"A=1")?;
        fx.write(".env.example", b"A=")?;
        fx.write("src/lib.rs", b"fn main() {}")?;
        let scope = fx.scope()?;
        for path in [
            ".ssh/id_ed25519",
            ".ssh",
            "config/server.pem",
            ".env",
            "deep/auth.toml",
            "x/secrets/a",
        ] {
            assert!(
                matches!(scope.rel_readable(path), Err(ScopeError::Denied(_))),
                "{path} must be denied"
            );
        }
        for path in [".env.example", "src/lib.rs", "id_rsa.pub"] {
            assert!(scope.rel_readable(path).is_ok(), "{path} must be readable");
        }
        let denied = scope.rel(".ssh/id_ed25519")?;
        assert!(matches!(
            scope.open_read(&denied),
            Err(ScopeError::Denied(_))
        ));
        Ok(())
    }

    #[test]
    fn open_read_reads_regular_file_and_rejects_directories() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("dir/file.txt", SECRET.as_bytes())?;
        let scope = fx.scope()?;
        let mut file = scope.open_read(&scope.rel("dir/file.txt")?)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        assert_eq!(content, SECRET);
        assert!(scope.open_read(&scope.rel("dir")?).is_err());
        assert!(scope.open_read(&scope.rel("")?).is_err());
        Ok(())
    }
}
