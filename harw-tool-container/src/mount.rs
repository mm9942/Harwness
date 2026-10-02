//! Mounts. Sources come from trusted config, never from the model.
//!
//! Rules (fail closed):
//! - the source is a clean absolute path without any engine-flag separator;
//! - the source is not the host root, a pseudo filesystem, `/run` (where the
//!   container and systemd sockets live), `/etc`, a socket file, or a
//!   credential directory (`.ssh`, `.gnupg`, `.aws`, `.kube`, `.docker`,
//!   `.config/containers`);
//! - the destination is below an allowlisted root, so a mount can never
//!   shadow `/proc`, `/usr`, `/etc` or the engine's own files.
//!
//! A symlink is invisible to a pure function: the caller must canonicalize a
//! source before constructing the mount.

use crate::error::ContainerPolicyError;
use crate::validate::{check_abs_path, check_name};

/// Roots below which a container destination is allowed.
const DST_ROOTS: [&str; 5] = ["/workspace", "/cache", "/mnt", "/data", "/opt"];

/// Host trees that are never a bind source (exact match or prefix).
const DENIED_SRC_TREES: [&str; 8] = [
    "/", "/proc", "/sys", "/dev", "/run", "/var/run", "/boot", "/etc",
];

/// Directory names that hold credentials; any path component matching one is
/// refused as a source.
const CREDENTIAL_DIRS: [&str; 5] = [".ssh", ".gnupg", ".aws", ".kube", ".docker"];

/// Kind of a mount.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Bind(String),
    Volume(String),
}

/// A validated container mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    kind: Kind,
    dst: String,
    read_only: bool,
}

fn check_dst(dst: &str) -> Result<(), ContainerPolicyError> {
    check_abs_path(dst)
        .map_err(|_| ContainerPolicyError::InvalidMount("invalid destination path"))?;
    let under_root = DST_ROOTS
        .iter()
        .any(|root| dst == *root || dst.strip_prefix(root).is_some_and(|r| r.starts_with('/')));
    if under_root {
        Ok(())
    } else {
        Err(ContainerPolicyError::InvalidMount(
            "destination must be below /workspace, /cache, /mnt, /data or /opt",
        ))
    }
}

fn check_src(src: &str) -> Result<(), ContainerPolicyError> {
    let err = ContainerPolicyError::InvalidMount;
    check_abs_path(src).map_err(|_| err("invalid source path"))?;
    for tree in DENIED_SRC_TREES {
        let denied = if tree == "/" {
            src == "/"
        } else {
            src == tree || src.strip_prefix(tree).is_some_and(|r| r.starts_with('/'))
        };
        if denied {
            return Err(err("source is a denied host tree"));
        }
    }
    let components: Vec<&str> = src.split('/').filter(|c| !c.is_empty()).collect();
    if components.iter().any(|c| CREDENTIAL_DIRS.contains(c)) {
        return Err(err("source is inside a credential directory"));
    }
    if components
        .windows(2)
        .any(|w| w == [".config", "containers"])
    {
        return Err(err("source is inside the container engine configuration"));
    }
    if components.last().is_some_and(|c| c.ends_with(".sock")) {
        return Err(err("sockets are never mounted"));
    }
    Ok(())
}

impl Mount {
    /// A bind mount of a host path.
    ///
    /// # Errors
    /// [`ContainerPolicyError::InvalidMount`] with the reason.
    pub fn bind(src: &str, dst: &str, read_only: bool) -> Result<Self, ContainerPolicyError> {
        check_src(src)?;
        check_dst(dst)?;
        Ok(Self {
            kind: Kind::Bind(src.to_owned()),
            dst: dst.to_owned(),
            read_only,
        })
    }

    /// A named volume, for example a project-scoped cache.
    ///
    /// # Errors
    /// [`ContainerPolicyError::InvalidName`] or `InvalidMount`.
    pub fn volume(name: &str, dst: &str, read_only: bool) -> Result<Self, ContainerPolicyError> {
        check_name(name, 64, "volume name")?;
        check_dst(dst)?;
        Ok(Self {
            kind: Kind::Volume(name.to_owned()),
            dst: dst.to_owned(),
            read_only,
        })
    }

    /// Destination inside the container.
    #[must_use]
    pub fn dst(&self) -> &str {
        &self.dst
    }

    /// `true` when mounted read-only.
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// The single engine argument: `--mount=type=...,src=...,dst=...[,ro]`.
    #[must_use]
    pub fn to_arg(&self) -> String {
        let (kind, src) = match &self.kind {
            Kind::Bind(s) => ("bind", s),
            Kind::Volume(n) => ("volume", n),
        };
        let ro = if self.read_only { ",ro" } else { "" };
        format!("--mount=type={kind},src={src},dst={}{ro}", self.dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn renders_one_argument_per_mount() -> TestResult {
        let m = Mount::bind("/srv/ws", "/workspace", true)?;
        ensure(
            m.to_arg() == "--mount=type=bind,src=/srv/ws,dst=/workspace,ro",
            "bind ro",
        )?;
        let m = Mount::bind("/srv/ws", "/workspace", false)?;
        ensure(
            m.to_arg() == "--mount=type=bind,src=/srv/ws,dst=/workspace",
            "bind rw",
        )?;
        let v = Mount::volume("harw-cache", "/cache", false)?;
        ensure(
            v.to_arg() == "--mount=type=volume,src=harw-cache,dst=/cache",
            "volume",
        )
    }

    #[test]
    fn refuses_denied_sources() -> TestResult {
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
            ensure(Mount::bind(src, "/workspace", true).is_err(), src)?;
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
            ensure(Mount::bind(src, "/workspace", true).is_ok(), src)?;
        }
        Ok(())
    }

    #[test]
    fn refuses_destinations_outside_the_allowlist() -> TestResult {
        for dst in [
            "/",
            "/proc",
            "/usr",
            "/usr/bin",
            "/etc",
            "/run",
            "/workspaces",
            "workspace",
            "/workspace/",
            "/workspace/..",
        ] {
            ensure(Mount::bind("/srv/ws", dst, true).is_err(), dst)?;
        }
        ensure(
            Mount::bind("/srv/ws", "/workspace/sub", true).is_ok(),
            "below root",
        )?;
        ensure(Mount::bind("/srv/ws", "/cache", true).is_ok(), "exact root")
    }

    #[test]
    fn separators_cannot_add_options() -> TestResult {
        for src in ["/srv/a,b", "/srv/a:b", "/srv/a=b", "/srv/a\nb", "/srv/a\"b"] {
            ensure(Mount::bind(src, "/workspace", true).is_err(), src)?;
        }
        ensure(
            Mount::volume("Bad Name", "/cache", false).is_err(),
            "volume name",
        )?;
        ensure(
            Mount::volume("a,b", "/cache", false).is_err(),
            "volume comma",
        )
    }
}
