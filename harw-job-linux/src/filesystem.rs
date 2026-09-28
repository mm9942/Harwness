//! Capability-oriented filesystem authority (Job-Runtime-Doc §3.4).
//!
//! A [`CapDir`] is a directory handle that grants access to exactly one
//! hierarchy: every path passed to its methods is resolved *beneath* it
//! (`..` escapes and absolute paths are rejected by `cap-std`, symlinks
//! cannot leave it). Code that receives a `CapDir` for a job's log or
//! workspace root cannot touch anything else.
//!
//! The only ambient-authority entry point is [`CapDir::open_ambient`],
//! used once at the boundary where configuration names the root. The
//! `cap-std` type itself stays private.

use std::io;
use std::path::Path;

use cap_std::fs::Dir;

/// A directory capability (job state root, log root, workspace root …).
#[derive(Debug)]
pub struct CapDir {
    dir: Dir,
}

impl CapDir {
    /// Opens `path` with the process' ambient authority — the single,
    /// explicit boundary where a path string becomes a capability.
    ///
    /// # Errors
    /// The `open` error (e.g. `NotFound`, `PermissionDenied`).
    pub fn open_ambient(path: &Path) -> io::Result<Self> {
        Dir::open_ambient_dir(path, cap_std::ambient_authority()).map(|dir| Self { dir })
    }

    /// Opens a subdirectory as a narrower capability.
    ///
    /// # Errors
    /// `open` errors; escapes beneath the root are refused.
    pub fn open_dir(&self, relative: impl AsRef<Path>) -> io::Result<Self> {
        self.dir.open_dir(relative).map(|dir| Self { dir })
    }

    /// Creates `relative` and all missing parents beneath the root.
    ///
    /// # Errors
    /// `mkdir` errors; escapes are refused.
    pub fn create_dir_all(&self, relative: impl AsRef<Path>) -> io::Result<()> {
        self.dir.create_dir_all(relative)
    }

    /// Reads a whole file beneath the root.
    ///
    /// # Errors
    /// `open`/`read` errors; escapes are refused.
    pub fn read(&self, relative: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        self.dir.read(relative)
    }

    /// Reads a whole UTF-8 file beneath the root.
    ///
    /// # Errors
    /// `open`/`read` errors, invalid UTF-8; escapes are refused.
    pub fn read_to_string(&self, relative: impl AsRef<Path>) -> io::Result<String> {
        self.dir.read_to_string(relative)
    }

    /// Creates or replaces a file beneath the root.
    ///
    /// # Errors
    /// `open`/`write` errors; escapes are refused.
    pub fn write(&self, relative: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
        self.dir.write(relative, contents)
    }

    /// Appends to a file beneath the root, creating it if missing.
    ///
    /// # Errors
    /// `open`/`write` errors; escapes are refused.
    pub fn append(&self, relative: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
        use std::io::Write as _;
        let mut options = cap_std::fs::OpenOptions::new();
        options.append(true).create(true);
        let mut file = self.dir.open_with(relative, &options)?;
        file.write_all(contents.as_ref())
    }

    /// Whether `relative` exists beneath the root (escapes report `false`).
    #[must_use]
    pub fn exists(&self, relative: impl AsRef<Path>) -> bool {
        self.dir.exists(relative)
    }

    /// Removes a file beneath the root.
    ///
    /// # Errors
    /// `unlink` errors; escapes are refused.
    pub fn remove_file(&self, relative: impl AsRef<Path>) -> io::Result<()> {
        self.dir.remove_file(relative)
    }

    /// Duplicates the capability.
    ///
    /// # Errors
    /// `dup` errors (descriptor exhaustion).
    pub fn try_clone(&self) -> io::Result<Self> {
        self.dir.try_clone().map(|dir| Self { dir })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_cap_dir_read_write_beneath_root() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let root = CapDir::open_ambient(temp.path()).map_err(ctx("open root"))?;
        root.create_dir_all("jobs/job-1").map_err(ctx("mkdir"))?;
        let job = root.open_dir("jobs/job-1").map_err(ctx("open job dir"))?;
        job.write("meta.json", b"{}").map_err(ctx("write"))?;
        job.append("log", b"a").map_err(ctx("append 1"))?;
        job.append("log", b"b").map_err(ctx("append 2"))?;
        assert_eq!(job.read_to_string("log").map_err(ctx("read log"))?, "ab");
        assert!(root.exists("jobs/job-1/meta.json"));
        job.remove_file("meta.json").map_err(ctx("remove"))?;
        assert!(!job.exists("meta.json"));
        Ok(())
    }

    #[test]
    fn test_cap_dir_refuses_escapes() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(temp.path().join("inner")).map_err(ctx("mkdir inner"))?;
        std::fs::write(temp.path().join("secret"), b"x").map_err(ctx("write secret"))?;
        let inner = CapDir::open_ambient(&temp.path().join("inner")).map_err(ctx("open inner"))?;
        if inner.read("../secret").is_ok() {
            return Err(TestError::Unexpected("parent escape succeeded".into()));
        }
        if inner.read("/etc/hostname").is_ok() {
            return Err(TestError::Unexpected("absolute path succeeded".into()));
        }
        std::os::unix::fs::symlink(temp.path().join("secret"), temp.path().join("inner/link"))
            .map_err(ctx("symlink"))?;
        if inner.read("link").is_ok() {
            return Err(TestError::Unexpected("symlink escape succeeded".into()));
        }
        assert!(!inner.exists("../secret"));
        Ok(())
    }
}
