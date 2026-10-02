//! Mounts. Sources come from trusted config, never from the model.
//!
//! Rules (fail closed):
//! - a bind source is a [`HostPath`]: resolved on the host (a symlink to
//!   `/etc` is refused), not a denied tree, not a credential directory, and
//!   free of sockets anywhere below it;
//! - a bind is non-recursive, so nested mounts are not carried along;
//! - the destination is below an allowlisted root, so a mount can never
//!   shadow `/proc`, `/usr`, `/etc` or the engine's own files.

use crate::error::ContainerPolicyError;
use crate::hostpath::HostPath;
use crate::validate::{check_abs_path, check_name};

/// Roots below which a container destination is allowed.
const DST_ROOTS: [&str; 5] = ["/workspace", "/cache", "/mnt", "/data", "/opt"];

/// What kind of source a mount has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    /// A host directory.
    Bind,
    /// A named volume.
    Volume,
}

/// What the engine must report for one mount: the plan's expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedMount {
    /// Bind or volume.
    pub kind: MountKind,
    /// The canonical host path of a bind, or the name of a volume.
    pub source: String,
    /// Destination inside the container.
    pub destination: String,
    /// The mount must be read-only.
    pub read_only: bool,
}

/// Kind of a mount.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Bind(HostPath),
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

impl Mount {
    /// A bind mount of a canonical host path. The source type cannot be
    /// built from a bare string: see [`HostPath::canonicalize`].
    ///
    /// # Errors
    /// [`ContainerPolicyError::InvalidMount`] with the reason.
    pub fn bind(src: &HostPath, dst: &str, read_only: bool) -> Result<Self, ContainerPolicyError> {
        check_dst(dst)?;
        Ok(Self {
            kind: Kind::Bind(src.clone()),
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

    /// Checks that a bind source still names the directory that was checked
    /// (see [`HostPath::revalidate`]); a volume has nothing to revalidate.
    ///
    /// # Errors
    /// [`ContainerPolicyError::InvalidMount`] when the source changed.
    pub fn revalidate(&self) -> Result<(), ContainerPolicyError> {
        match &self.kind {
            Kind::Bind(path) => path.revalidate(),
            Kind::Volume(_) => Ok(()),
        }
    }

    /// What the engine must report for this mount after `create`.
    #[must_use]
    pub fn expectation(&self) -> ExpectedMount {
        let (kind, source) = match &self.kind {
            Kind::Bind(path) => (MountKind::Bind, path.as_str()),
            Kind::Volume(name) => (MountKind::Volume, name.as_str()),
        };
        ExpectedMount {
            kind,
            source: source.to_owned(),
            destination: self.dst.clone(),
            read_only: self.read_only,
        }
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
            Kind::Bind(s) => ("bind", s.as_str()),
            Kind::Volume(n) => ("volume", n.as_str()),
        };
        let ro = if self.read_only { ",ro" } else { "" };
        // A bind is non-recursive: a mount nested below the source (a
        // docker socket directory mounted into the workspace, say) is not
        // carried into the container.
        let flat = if matches!(self.kind, Kind::Bind(_)) {
            ",bind-nonrecursive"
        } else {
            ""
        };
        format!("--mount=type={kind},src={src},dst={}{ro}{flat}", self.dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    fn host(path: &str) -> Result<HostPath, ContainerPolicyError> {
        HostPath::lexical(path)
    }

    #[test]
    fn renders_one_argument_per_mount() -> TestResult {
        let m = Mount::bind(&host("/srv/ws")?, "/workspace", true)?;
        ensure(
            m.to_arg() == "--mount=type=bind,src=/srv/ws,dst=/workspace,ro,bind-nonrecursive",
            "bind ro",
        )?;
        let m = Mount::bind(&host("/srv/ws")?, "/workspace", false)?;
        ensure(
            m.to_arg() == "--mount=type=bind,src=/srv/ws,dst=/workspace,bind-nonrecursive",
            "bind rw",
        )?;
        let v = Mount::volume("harw-cache", "/cache", false)?;
        ensure(
            v.to_arg() == "--mount=type=volume,src=harw-cache,dst=/cache",
            "volume",
        )
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
            ensure(Mount::bind(&host("/srv/ws")?, dst, true).is_err(), dst)?;
        }
        ensure(
            Mount::bind(&host("/srv/ws")?, "/workspace/sub", true).is_ok(),
            "below root",
        )?;
        ensure(
            Mount::bind(&host("/srv/ws")?, "/cache", true).is_ok(),
            "exact root",
        )
    }

    #[test]
    fn volume_names_cannot_add_options() -> TestResult {
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
