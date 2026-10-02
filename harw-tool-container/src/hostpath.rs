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
//! to read metadata. A path can still change between this check and the
//! engine's mount (time of check to time of use). The mount is therefore
//! non-recursive (nested mounts are not carried along), the container runs
//! without network and capabilities, and the caller should canonicalize
//! immediately before `create`.

use std::fs;
use std::os::unix::fs::FileTypeExt;
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

/// A host directory that was resolved and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPath(String);

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
        Ok(Self(resolved_str))
    }

    /// The canonical path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Applies only the lexical rules, for unit tests of code that takes a
    /// [`HostPath`] without touching the filesystem.
    #[cfg(test)]
    pub(crate) fn lexical(path: &str) -> Result<Self, ContainerPolicyError> {
        check_lexical(path)?;
        Ok(Self(path.to_owned()))
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
