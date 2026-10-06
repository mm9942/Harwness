//! Das Repository: Zugriff auf `<workspace>/.git` und das Arbeitsverzeichnis.
//!
//! # Verantwortung
//! [`Repo::open`] sucht `<Workspace-Wurzel>/.git`. Ist es ein Verzeichnis, ist
//! es das Git-Verzeichnis. Ist es eine Datei (`gitdir: …`, verknüpfte
//! Worktrees und Submodule), wird das Ziel nur akzeptiert, wenn es **innerhalb**
//! der Workspace-Wurzel liegt; sonst gibt es eine klare Ablehnung (die
//! Objektdatenbank läge außerhalb des erlaubten Bereichs).
//!
//! Alle Dateizugriffe laufen über [`harw_tool_fsread::scope::Scope`]
//! (symlinkfrei, an die Wurzel gebunden).
//!
//! # Grenzen
//! Gelesene Git-Dateien sind einzeln auf [`MAX_GIT_FILE_BYTES`] begrenzt.
//! SHA-256-Repositories werden nicht unterstützt.

use harw_tool_fsread::scope::{RelPath, Scope, io_message};
use harw_tool_fsread::walk::smallest_names;
use rustix::fs::{AtFlags, FileType};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

/// Obergrenze beim Lesen einer einzelnen Git-Datei (Index, Refs, Packs-Index, …).
pub const MAX_GIT_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Obergrenze beim Auflisten eines Verzeichnisses.
pub const MAX_DIR_NAMES: usize = 200_000;

/// Frist für Verzeichnislistungen.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// Ein geöffnetes Repository.
#[derive(Debug)]
pub struct Repo {
    /// Zugriff auf die Workspace-Wurzel.
    pub scope: Scope,
    /// Das Git-Verzeichnis relativ zur Wurzel (`.git` oder ein Ziel innerhalb).
    pub git_dir: RelPath,
}

/// Eintrag einer Verzeichnisliste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirName {
    /// Name.
    pub name: String,
    /// `true` für Verzeichnisse.
    pub is_dir: bool,
}

impl Repo {
    /// Öffnet das Repository der Workspace-Wurzel `root`.
    ///
    /// # Errors
    /// Meldung, wenn kein `.git` vorhanden ist oder es außerhalb zeigt.
    pub fn open(root: &Path) -> Result<Self, String> {
        let scope = Scope::new(root)
            .map_err(|e| format!("workspace root is not accessible: {}", io_message(&e)))?;
        let dot_git = scope.rel(".git").map_err(|e| e.to_string())?;
        let stat = scope
            .lstat(&dot_git)
            .map_err(|_| "not a git repository: the workspace root has no .git".to_owned())?;
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => Ok(Self {
                scope,
                git_dir: dot_git,
            }),
            FileType::RegularFile => {
                let mut file = scope.open_read(&dot_git).map_err(|e| e.to_string())?;
                let mut text = String::new();
                file.by_ref()
                    .take(4096)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())?;
                let target = text
                    .trim()
                    .strip_prefix("gitdir:")
                    .map(str::trim)
                    .ok_or("unsupported .git file (no gitdir line)")?;
                let git_dir = scope.rel(target).map_err(|_| {
                    "the .git file points to a git directory outside the workspace (linked worktree or submodule); only repositories whose .git directory lies inside the workspace are supported".to_owned()
                })?;
                let is_dir = scope
                    .lstat(&git_dir)
                    .map(|s| FileType::from_raw_mode(s.st_mode) == FileType::Directory)
                    .unwrap_or(false);
                if !is_dir {
                    return Err(
                        "the gitdir of the .git file is not a directory inside the workspace"
                            .to_owned(),
                    );
                }
                Ok(Self { scope, git_dir })
            }
            _ => Err("unsupported .git entry (not a directory or file)".to_owned()),
        }
    }

    /// `<git_dir>/<rest>` als [`RelPath`].
    ///
    /// # Errors
    /// Wenn `rest` aus dem Git-Verzeichnis ausbräche.
    pub fn git_path(&self, rest: &str) -> Result<RelPath, String> {
        let joined = if self.git_dir.is_root() {
            rest.to_owned()
        } else {
            format!("{}/{rest}", self.git_dir.display())
        };
        self.scope.rel(&joined).map_err(|e| e.to_string())
    }

    /// Öffnet eine Datei im Git-Verzeichnis (symlinkfrei).
    ///
    /// # Errors
    /// I/O-Fehler (`NotFound` wenn es sie nicht gibt).
    pub fn open_git_file(&self, rest: &str) -> std::io::Result<File> {
        let rel = self
            .git_path(rest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        match self.scope.open_read(&rel) {
            Ok(file) => Ok(file),
            Err(harw_tool_fsread::scope::ScopeError::Io(error)) => Err(error),
            Err(other) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                other.to_string(),
            )),
        }
    }

    /// Liest eine Datei im Git-Verzeichnis, höchstens `cap` Bytes.
    ///
    /// # Errors
    /// I/O-Fehler oder Überschreitung von `cap` (`InvalidData`).
    pub fn read_git_file(&self, rest: &str, cap: u64) -> std::io::Result<Vec<u8>> {
        let mut file = self.open_git_file(rest)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(cap.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{rest} is larger than {cap} bytes"),
            ));
        }
        Ok(bytes)
    }

    /// Listet ein Verzeichnis im Git-Verzeichnis (sortiert; `[]` wenn es fehlt).
    #[must_use]
    pub fn list_git_dir(&self, rest: &str) -> Vec<DirName> {
        let Ok(rel) = self.git_path(rest) else {
            return Vec::new();
        };
        self.list_dir(&rel)
    }

    /// Listet ein Verzeichnis relativ zur Wurzel (sortiert; `[]` wenn es fehlt).
    #[must_use]
    pub fn list_dir(&self, rel: &RelPath) -> Vec<DirName> {
        let Ok(fd) = self.scope.open_dir(rel) else {
            return Vec::new();
        };
        let Ok(Some(names)) = smallest_names(&fd, MAX_DIR_NAMES, Instant::now() + LIST_TIMEOUT)
        else {
            return Vec::new();
        };
        names
            .into_iter()
            .map(|name| {
                let is_dir = rustix::fs::statat(&fd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
                    .map(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
                    .unwrap_or(false);
                DirName {
                    name: name.to_string_lossy().into_owned(),
                    is_dir,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestRepo, TestResult};

    #[test]
    fn opens_a_dot_git_directory_and_reads_files_inside() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write_git("HEAD", "ref: refs/heads/main\n")?;
        let opened = Repo::open(&repo.ws)?;
        assert_eq!(opened.git_dir.display(), ".git");
        assert_eq!(
            opened.read_git_file("HEAD", 100)?,
            b"ref: refs/heads/main\n"
        );
        assert!(opened.read_git_file("HEAD", 3).is_err());
        assert!(opened.read_git_file("missing", 10).is_err());
        Ok(())
    }

    #[test]
    fn missing_dot_git_and_foreign_gitdir_are_rejected() -> TestResult {
        let repo = TestRepo::new_empty()?;
        let error = Repo::open(&repo.ws)
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(error.contains("not a git repository"), "{error}");
        // verknüpfter Worktree: gitdir zeigt nach außen
        std::fs::write(
            repo.ws.join(".git"),
            format!("gitdir: {}\n", repo.outside.display()),
        )?;
        let error = Repo::open(&repo.ws)
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(error.contains("outside the workspace"), "{error}");
        // relatives Ziel mit Ausbruch
        std::fs::write(repo.ws.join(".git"), "gitdir: ../outside\n")?;
        assert!(Repo::open(&repo.ws).is_err());
        // gitdir innerhalb ist erlaubt
        std::fs::create_dir_all(repo.ws.join("meta/gitdir"))?;
        std::fs::write(repo.ws.join(".git"), "gitdir: meta/gitdir\n")?;
        let opened = Repo::open(&repo.ws)?;
        assert_eq!(opened.git_dir.display(), "meta/gitdir");
        std::fs::write(repo.ws.join(".git"), "garbage")?;
        assert!(Repo::open(&repo.ws).is_err());
        Ok(())
    }

    #[test]
    fn dot_git_symlink_is_not_followed() -> TestResult {
        let repo = TestRepo::new_empty()?;
        std::os::unix::fs::symlink(&repo.outside, repo.ws.join(".git"))?;
        assert!(Repo::open(&repo.ws).is_err());
        Ok(())
    }

    #[test]
    fn lists_directories_sorted_with_types() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write_git("refs/heads/b", "x")?;
        repo.write_git("refs/heads/a", "x")?;
        repo.write_git("refs/heads/dir/c", "x")?;
        let opened = Repo::open(&repo.ws)?;
        let names = opened.list_git_dir("refs/heads");
        assert_eq!(
            names,
            vec![
                DirName {
                    name: "a".into(),
                    is_dir: false
                },
                DirName {
                    name: "b".into(),
                    is_dir: false
                },
                DirName {
                    name: "dir".into(),
                    is_dir: true
                },
            ]
        );
        assert!(opened.list_git_dir("nope").is_empty());
        Ok(())
    }
}
