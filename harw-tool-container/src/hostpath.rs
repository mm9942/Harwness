//! A canonical host path: the only kind of path a bind mount accepts.
//!
//! Lexical checks cannot see a symlink: `/srv/ws-link -> /etc` passes every
//! string rule while the engine mounts `/etc`. [`HostPath`] is therefore
//! produced by [`HostPath::canonicalize`], which resolves the path on the
//! host first and applies the denied-tree rules to the *resolved* path. It
//! also walks the directory (bounded, symlinks not followed) and refuses a
//! tree that holds a socket: a read-only bind still allows `connect(2)`, so a
//! `docker.sock` inside an otherwise harmless workspace would hand the
//! container the daemon.
//!
//! This is the one place in the crate that touches the filesystem, and only
//! to read metadata. A path can still change between the check and the
//! engine's mount (time of check to time of use). Two things limit that:
//! [`HostPath::revalidate`] re-checks the identity (device and inode, not a
//! symlink, still no socket) immediately before `create` and again before
//! `start`, and the mount is non-recursive. What remains is the instant
//! between the last re-check and the engine's own `mount(2)`: the engine CLI
//! takes a path, not a held directory handle, so that window cannot be closed
//! from here. The container also runs without network and capabilities.

use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use crate::error::ContainerPolicyError;
use crate::validate::check_abs_path;

/// Host trees that are never a bind source (exact match or prefix).
const DENIED_SRC_TREES: [&str; 8] = [
    "/", "/proc", "/sys", "/dev", "/run", "/var/run", "/boot", "/etc",
];

/// Directory names that hold credentials; any path component matching one is
/// refused as a source.
const CREDENTIAL_DIRS: [&str; 5] = [".ssh", ".gnupg", ".aws", ".kube", ".docker"];

/// Most directory entries examined before the tree counts as too large to
/// verify (fail closed).
const MAX_SCANNED_ENTRIES: usize = 100_000;
/// Deepest directory level examined.
const MAX_SCAN_DEPTH: usize = 32;

/// A host directory that was resolved and checked. It remembers the device
/// and inode it resolved to, so [`HostPath::revalidate`] can tell whether the
/// path still names the same directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPath {
    path: String,
    dev: u64,
    ino: u64,
}

fn refuse(reason: &'static str) -> ContainerPolicyError {
    ContainerPolicyError::InvalidMount(reason)
}

/// The denied-tree, credential and socket-name rules on an already
/// canonical string.
fn check_lexical(src: &str) -> Result<(), ContainerPolicyError> {
    check_abs_path(src).map_err(|_| refuse("invalid source path"))?;
    for tree in DENIED_SRC_TREES {
        let denied = if tree == "/" {
            src == "/"
        } else {
            src == tree || src.strip_prefix(tree).is_some_and(|r| r.starts_with('/'))
        };
        if denied {
            return Err(refuse("source is a denied host tree"));
        }
    }
    let components: Vec<&str> = src.split('/').filter(|c| !c.is_empty()).collect();
    if components.iter().any(|c| CREDENTIAL_DIRS.contains(c)) {
        return Err(refuse("source is inside a credential directory"));
    }
    if components
        .windows(2)
        .any(|w| w == [".config", "containers"])
    {
        return Err(refuse(
            "source is inside the container engine configuration",
        ));
    }
    if components.last().is_some_and(|c| c.ends_with(".sock")) {
        return Err(refuse("sockets are never mounted"));
    }
    Ok(())
}

/// Refuses a tree that contains a socket, or is too large to verify.
fn scan_for_sockets(root: &Path) -> Result<(), ContainerPolicyError> {
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut seen = 0_usize;
    while let Some((dir, depth)) = pending.pop() {
        let entries = fs::read_dir(&dir).map_err(|_| refuse("source directory is not readable"))?;
        for entry in entries {
            let entry = entry.map_err(|_| refuse("source directory is not readable"))?;
            seen += 1;
            if seen > MAX_SCANNED_ENTRIES {
                return Err(refuse("source tree is too large to verify"));
            }
            // `file_type` does not follow symlinks.
            let kind = entry
                .file_type()
                .map_err(|_| refuse("source directory is not readable"))?;
            if kind.is_socket() {
                return Err(refuse("source tree contains a socket"));
            }
            if kind.is_dir() {
                if depth + 1 > MAX_SCAN_DEPTH {
                    return Err(refuse("source tree is too deep to verify"));
                }
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    Ok(())
}

impl HostPath {
    /// Resolves `path` on the host and checks the result.
    ///
    /// # Errors
    /// [`ContainerPolicyError::InvalidMount`] when the path does not exist,
    /// is not a directory, resolves into a denied tree or a credential
    /// directory, or its tree holds a socket.
    pub fn canonicalize(path: &str) -> Result<Self, ContainerPolicyError> {
        check_abs_path(path).map_err(|_| refuse("invalid source path"))?;
        let resolved = fs::canonicalize(path)
            .map_err(|_| refuse("source does not exist or is not readable"))?;
        if !resolved.is_dir() {
            return Err(refuse("source is not a directory"));
        }
        let resolved_str = resolved
            .to_str()
            .ok_or_else(|| refuse("source path is not valid UTF-8"))?
            .to_owned();
        check_lexical(&resolved_str)?;
        scan_for_sockets(&resolved)?;
        let meta = fs::metadata(&resolved)
            .map_err(|_| refuse("source does not exist or is not readable"))?;
        Ok(Self {
            path: resolved_str,
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    /// Checks again that the path still names the directory that was
    /// checked: it is not a symlink now, it is the same device and inode, it
    /// still resolves to itself, and its tree holds no socket.
    ///
    /// A path can change between [`Self::canonicalize`] and the moment the
    /// engine mounts it. Call this immediately before `create` and again
    /// immediately before `start`: a replacement (a symlink to `/run`, another
    /// directory, a new socket) that happened earlier is refused. A change in
    /// the instant between this check and the engine's own `mount` cannot be
    /// excluded from here, because the engine CLI takes a path, not a held
    /// directory handle.
    ///
    /// # Errors
    /// [`ContainerPolicyError::InvalidMount`] when anything differs.
    pub fn revalidate(&self) -> Result<(), ContainerPolicyError> {
        if self.ino == 0 {
            return Err(refuse("source was not resolved on the host"));
        }
        let own = fs::symlink_metadata(&self.path).map_err(|_| refuse("source disappeared"))?;
        if own.file_type().is_symlink() {
            return Err(refuse("source is a symlink now"));
        }
        if !own.is_dir() || own.dev() != self.dev || own.ino() != self.ino {
            return Err(refuse("source is not the directory that was checked"));
        }
        let resolved = fs::canonicalize(&self.path).map_err(|_| refuse("source disappeared"))?;
        if resolved.to_str() != Some(self.path.as_str()) {
            return Err(refuse("source resolves elsewhere now"));
        }
        scan_for_sockets(&resolved)
    }

    /// The canonical path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// Applies only the lexical rules, for unit tests of code that takes a
    /// [`HostPath`] without touching the filesystem.
    #[cfg(test)]
    pub(crate) fn lexical(path: &str) -> Result<Self, ContainerPolicyError> {
        check_lexical(path)?;
        Ok(Self {
            path: path.to_owned(),
            dev: 0,
            ino: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    /// A scratch directory below the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Result<Self, std::io::Error> {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir =
                std::env::temp_dir().join(format!("harw-hp-{tag}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&dir)?;
            Ok(Self(dir))
        }

        fn path(&self, rel: &str) -> String {
            self.0.join(rel).to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_plain_directory_resolves_to_its_canonical_path() -> TestResult {
        let scratch = Scratch::new("plain")?;
        fs::create_dir_all(scratch.0.join("ws"))?;
        let host = HostPath::canonicalize(&scratch.path("ws"))?;
        let canonical = fs::canonicalize(scratch.0.join("ws"))?;
        ensure(Path::new(host.as_str()) == canonical, "canonical path")
    }

    #[test]
    fn a_symlink_into_a_denied_tree_is_refused() -> TestResult {
        let scratch = Scratch::new("link")?;
        symlink("/etc", scratch.0.join("ws-link"))?;
        ensure(
            HostPath::canonicalize(&scratch.path("ws-link")).is_err(),
            "the link resolves to /etc",
        )
    }

    #[test]
    fn a_symlink_to_a_harmless_directory_mounts_the_target() -> TestResult {
        let scratch = Scratch::new("good-link")?;
        fs::create_dir_all(scratch.0.join("real"))?;
        symlink(scratch.0.join("real"), scratch.0.join("alias"))?;
        let host = HostPath::canonicalize(&scratch.path("alias"))?;
        ensure(host.as_str().ends_with("/real"), "the resolved target")
    }

    #[test]
    fn a_socket_anywhere_in_the_tree_is_refused() -> TestResult {
        let scratch = Scratch::new("sock")?;
        fs::create_dir_all(scratch.0.join("ws/deep/er"))?;
        let _listener = UnixListener::bind(scratch.0.join("ws/deep/er/x"))?;
        ensure(
            HostPath::canonicalize(&scratch.path("ws")).is_err(),
            "a nested socket, whatever its name",
        )
    }

    #[test]
    fn an_unchanged_directory_revalidates() -> TestResult {
        let scratch = Scratch::new("same")?;
        fs::create_dir_all(scratch.0.join("ws/sub"))?;
        let host = HostPath::canonicalize(&scratch.path("ws"))?;
        ensure(host.revalidate().is_ok(), "nothing changed")
    }

    #[test]
    fn a_directory_swapped_for_a_symlink_after_the_check_is_refused() -> TestResult {
        let scratch = Scratch::new("swap-link")?;
        fs::create_dir_all(scratch.0.join("ws"))?;
        let host = HostPath::canonicalize(&scratch.path("ws"))?;
        // An attacker who can rename replaces the checked directory.
        fs::rename(scratch.0.join("ws"), scratch.0.join("ws-moved"))?;
        symlink("/run", scratch.0.join("ws"))?;
        ensure(host.revalidate().is_err(), "the symlink to /run is refused")
    }

    #[test]
    fn a_different_directory_at_the_same_path_is_refused() -> TestResult {
        let scratch = Scratch::new("swap-dir")?;
        fs::create_dir_all(scratch.0.join("ws"))?;
        let host = HostPath::canonicalize(&scratch.path("ws"))?;
        fs::rename(scratch.0.join("ws"), scratch.0.join("ws-moved"))?;
        fs::create_dir_all(scratch.0.join("ws"))?;
        ensure(
            host.revalidate().is_err(),
            "another inode under the same name",
        )
    }

    #[test]
    fn a_socket_that_appears_after_the_check_is_refused() -> TestResult {
        let scratch = Scratch::new("late-sock")?;
        fs::create_dir_all(scratch.0.join("ws"))?;
        let host = HostPath::canonicalize(&scratch.path("ws"))?;
        let _listener = UnixListener::bind(scratch.0.join("ws/late"))?;
        ensure(host.revalidate().is_err(), "a socket created later")
    }

    #[test]
    fn a_removed_directory_and_an_unresolved_path_are_refused() -> TestResult {
        let scratch = Scratch::new("gone")?;
        fs::create_dir_all(scratch.0.join("ws"))?;
        let host = HostPath::canonicalize(&scratch.path("ws"))?;
        fs::remove_dir_all(scratch.0.join("ws"))?;
        ensure(host.revalidate().is_err(), "gone")?;
        ensure(
            HostPath::lexical("/srv/ws")?.revalidate().is_err(),
            "a path that was never resolved on the host cannot be revalidated",
        )
    }

    #[test]
    fn a_file_or_a_missing_path_is_refused() -> TestResult {
        let scratch = Scratch::new("kind")?;
        fs::write(scratch.0.join("file"), b"x")?;
        ensure(
            HostPath::canonicalize(&scratch.path("file")).is_err(),
            "a file",
        )?;
        ensure(
            HostPath::canonicalize(&scratch.path("missing")).is_err(),
            "missing",
        )?;
        ensure(HostPath::canonicalize("relative/dir").is_err(), "relative")
    }

    #[test]
    fn denied_sources_are_refused_lexically() -> TestResult {
        for src in [
            "/",
            "/proc",
            "/proc/1",
            "/sys/fs",
            "/dev",
            "/run",
            "/run/user/1000/podman/podman.sock",
            "/var/run/docker.sock",
            "/etc",
            "/etc/ssh",
            "/boot",
            "/home/u/.ssh",
            "/home/u/.ssh/id_ed25519",
            "/home/u/.docker",
            "/home/u/.config/containers/auth.json",
            "/srv/app/engine.sock",
        ] {
            ensure(HostPath::lexical(src).is_err(), src)?;
        }
        Ok(())
    }

    #[test]
    fn near_misses_are_allowed() -> TestResult {
        for src in [
            "/runner/work",
            "/etcetera",
            "/srv/.sshx",
            "/home/u/.config/other",
        ] {
            ensure(HostPath::lexical(src).is_ok(), src)?;
        }
        Ok(())
    }

    #[test]
    fn separators_cannot_add_options() -> TestResult {
        for src in ["/srv/a,b", "/srv/a:b", "/srv/a=b", "/srv/a\nb", "/srv/a\"b"] {
            ensure(HostPath::lexical(src).is_err(), src)?;
        }
        Ok(())
    }
}
