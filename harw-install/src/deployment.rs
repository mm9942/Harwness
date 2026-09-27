//! The embedded deployment assets: every file under `deploy/`, compiled in.
//!
//! # One source of truth (Crypto Masterplan v2 §22, §40, H10)
//! `deploy/` at the repository root is the **only** place a systemd unit,
//! the `sysusers.d` accounts and the `tmpfiles.d` directories of Harwness
//! are written down. This module embeds each of those files byte-identically
//! with `include_str!` in production code, so
//!
//! - `harw install --print-systemd [UNIT]` prints exactly what is shipped,
//! - the unit checks in [`crate::dod_units`] inspect the same text that
//!   production uses, and
//! - the DoD installer (`dod/scripts/install.sh`), which installs the same
//!   files straight from `deploy/`, cannot drift from what this crate knows.
//!
//! The parity test below walks `deploy/` at test time and fails when a file
//! exists there without being embedded here, when an embedded file is gone,
//! or when the bytes differ. There is no second copy under `dod/packaging/`
//! any more (H10 deleted it).
//!
//! # Placeholders
//! Paths that the DoD installer lets an operator choose are written as
//! `@NAME@` placeholders ([`PLACEHOLDERS`]); `install.sh` substitutes them with
//! `sed`, and [`render`] does the same with [`RenderPaths`]. The defaults of
//! [`RenderPaths::default`] are exactly the defaults of `install.sh`
//! (`PREFIX=/usr/local`). A rendered asset never contains an unresolved
//! placeholder ([`unresolved_placeholders`]). Runtime paths are fixed:
//! `/run/harw` (DoD sockets) and `/run/harw/infra` (infrastructure sockets).
//!
//! # Concurrency
//! `'static` data and pure functions only; `Send + Sync`.

use std::fmt;

/// What kind of deployment file an [`EmbeddedDeploymentAsset`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    /// A systemd `.service` unit.
    SystemdService,
    /// A systemd `.socket` unit.
    SystemdSocket,
    /// A systemd `.target` unit.
    SystemdTarget,
    /// A `sysusers.d` snippet (system accounts and groups).
    Sysusers,
    /// A `tmpfiles.d` snippet (runtime/state/log directories).
    Tmpfiles,
}

impl AssetKind {
    /// Whether this kind is a systemd unit (service, socket or target).
    #[must_use]
    pub const fn is_systemd_unit(self) -> bool {
        matches!(
            self,
            Self::SystemdService | Self::SystemdSocket | Self::SystemdTarget
        )
    }
}

/// One file under `deploy/`, embedded at compile time.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedDeploymentAsset {
    /// Path relative to the repository root, e.g.
    /// `"deploy/systemd/harw-warden.socket"`.
    pub path: &'static str,
    /// The file's exact bytes (UTF-8), placeholders unrendered.
    pub contents: &'static str,
    /// The kind of file.
    pub kind: AssetKind,
}

impl EmbeddedDeploymentAsset {
    /// The file name without directories, e.g. `"harw-warden.socket"`.
    #[must_use]
    pub fn file_name(&self) -> &'static str {
        self.path.rsplit('/').next().unwrap_or(self.path)
    }
}

/// Embeds one `deploy/` file; the path is relative to the repository root.
macro_rules! deployment_asset {
    ($path:literal, $kind:expr) => {
        EmbeddedDeploymentAsset {
            path: $path,
            contents: include_str!(concat!("../../", $path)),
            kind: $kind,
        }
    };
}

/// Every file under `deploy/`, sorted by path.
///
/// # Description
/// Adding a file under `deploy/` without listing it here fails the parity
/// test (`test_every_deploy_file_is_embedded_byte_identically`).
pub const DEPLOYMENT_ASSETS: &[EmbeddedDeploymentAsset] = &[
    deployment_asset!(
        "deploy/systemd/harw-auth-hub.service",
        AssetKind::SystemdService
    ),
    deployment_asset!(
        "deploy/systemd/harw-auth-hub.socket",
        AssetKind::SystemdSocket
    ),
    deployment_asset!(
        "deploy/systemd/harw-control.service",
        AssetKind::SystemdService
    ),
    deployment_asset!(
        "deploy/systemd/harw-control.socket",
        AssetKind::SystemdSocket
    ),
    deployment_asset!("deploy/systemd/harw-dod.target", AssetKind::SystemdTarget),
    deployment_asset!("deploy/systemd/harw-infra.target", AssetKind::SystemdTarget),
    deployment_asset!(
        "deploy/systemd/harw-netsec.service",
        AssetKind::SystemdService
    ),
    deployment_asset!(
        "deploy/systemd/harw-netsec.socket",
        AssetKind::SystemdSocket
    ),
    deployment_asset!(
        "deploy/systemd/harw-probe-bpf.service",
        AssetKind::SystemdService
    ),
    deployment_asset!(
        "deploy/systemd/harw-probe-fs.service",
        AssetKind::SystemdService
    ),
    deployment_asset!(
        "deploy/systemd/harw-security-hub.service",
        AssetKind::SystemdService
    ),
    deployment_asset!(
        "deploy/systemd/harw-security-hub.socket",
        AssetKind::SystemdSocket
    ),
    deployment_asset!(
        "deploy/systemd/harw-sentinel.service",
        AssetKind::SystemdService
    ),
    deployment_asset!(
        "deploy/systemd/harw-warden.service",
        AssetKind::SystemdService
    ),
    deployment_asset!(
        "deploy/systemd/harw-warden.socket",
        AssetKind::SystemdSocket
    ),
    deployment_asset!("deploy/sysusers.d/harw.conf", AssetKind::Sysusers),
    deployment_asset!("deploy/tmpfiles.d/harw.conf", AssetKind::Tmpfiles),
];

/// The assets the DoD installer (`dod/scripts/install.sh`) installs, as paths
/// relative to the repository root.
///
/// # Description
/// Mirrors the `systemd/`, `sysusers/` and `tmpfiles/` entries of
/// `dod/packaging/manifest`; the parity tests check both directions. The
/// infrastructure units (`harw-infra.target`, `harw-auth-hub.*`, …) are
/// embedded but not part of the DoD package.
pub const DOD_PACKAGE_ASSETS: &[&str] = &[
    "deploy/systemd/harw-dod.target",
    "deploy/systemd/harw-sentinel.service",
    "deploy/systemd/harw-probe-bpf.service",
    "deploy/systemd/harw-probe-fs.service",
    "deploy/systemd/harw-warden.service",
    "deploy/systemd/harw-warden.socket",
    "deploy/sysusers.d/harw.conf",
    "deploy/tmpfiles.d/harw.conf",
];

/// Every placeholder an asset may contain; `install.sh` substitutes exactly
/// this set.
pub const PLACEHOLDERS: &[&str] = &[
    "@LIBEXECDIR@",
    "@BPFDIR@",
    "@SYSCONFDIR@",
    "@STATEDIR@",
    "@LOGDIR@",
];

/// Installation paths substituted for the [`PLACEHOLDERS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderPaths {
    /// `@LIBEXECDIR@`: directory of the DoD binaries.
    pub libexecdir: String,
    /// `@BPFDIR@`: directory of the eBPF objects and their manifest.
    pub bpfdir: String,
    /// `@SYSCONFDIR@`: DoD configuration directory.
    pub sysconfdir: String,
    /// `@STATEDIR@`: sentinel state directory.
    pub statedir: String,
    /// `@LOGDIR@`: sentinel telemetry directory.
    pub logdir: String,
}

impl Default for RenderPaths {
    /// The defaults of `dod/scripts/install.sh` (`PREFIX=/usr/local`).
    fn default() -> Self {
        Self {
            libexecdir: "/usr/local/libexec/harw-dod".to_owned(),
            bpfdir: "/usr/local/lib/harw-dod/bpf".to_owned(),
            sysconfdir: "/etc/harw-dod".to_owned(),
            statedir: "/var/lib/harw-dod".to_owned(),
            logdir: "/var/log/harw-dod".to_owned(),
        }
    }
}

/// Substitutes every placeholder in `contents`.
///
/// # Arguments
/// - `contents` (`&str`): an asset's text, e.g.
///   [`EmbeddedDeploymentAsset::contents`].
/// - `paths` (`&RenderPaths`): the values to substitute.
///
/// # Returns
/// The rendered text; the same substitution as `install.sh`'s `render_unit`.
#[must_use]
pub fn render(contents: &str, paths: &RenderPaths) -> String {
    contents
        .replace("@LIBEXECDIR@", &paths.libexecdir)
        .replace("@BPFDIR@", &paths.bpfdir)
        .replace("@SYSCONFDIR@", &paths.sysconfdir)
        .replace("@STATEDIR@", &paths.statedir)
        .replace("@LOGDIR@", &paths.logdir)
}

/// Finds every `@NAME@` token (upper-case ASCII letters and `_`) in `text`.
///
/// # Returns
/// The tokens in order of appearance, including the `@` delimiters; empty
/// when the text is fully rendered.
#[must_use]
pub fn unresolved_placeholders(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('@') {
        let after = &rest[start + 1..];
        let name_len = after
            .bytes()
            .take_while(|b| b.is_ascii_uppercase() || *b == b'_')
            .count();
        if name_len > 0 && after.as_bytes().get(name_len) == Some(&b'@') {
            found.push(format!("@{}@", &after[..name_len]));
            rest = &after[name_len + 1..];
        } else {
            rest = after;
        }
    }
    found
}

/// Looks an asset up by its repository-relative path.
#[must_use]
pub fn asset(path: &str) -> Option<&'static EmbeddedDeploymentAsset> {
    DEPLOYMENT_ASSETS.iter().find(|asset| asset.path == path)
}

/// Looks a systemd unit up by file name (`"harw-warden.socket"`).
///
/// # Description
/// A name without a unit suffix is tried as `<name>.service`, so
/// `harw-sentinel` finds `harw-sentinel.service`.
#[must_use]
pub fn systemd_unit(name: &str) -> Option<&'static EmbeddedDeploymentAsset> {
    let exact = DEPLOYMENT_ASSETS
        .iter()
        .find(|asset| asset.kind.is_systemd_unit() && asset.file_name() == name);
    exact.or_else(|| {
        let with_suffix = format!("{name}.service");
        DEPLOYMENT_ASSETS
            .iter()
            .find(|asset| asset.kind.is_systemd_unit() && asset.file_name() == with_suffix)
    })
}

/// All embedded systemd units, in [`DEPLOYMENT_ASSETS`] order.
pub fn systemd_units() -> impl Iterator<Item = &'static EmbeddedDeploymentAsset> {
    DEPLOYMENT_ASSETS
        .iter()
        .filter(|asset| asset.kind.is_systemd_unit())
}

/// A unit name that [`print_systemd`] does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownUnitError {
    /// The name that was asked for.
    pub requested: String,
}

impl fmt::Display for UnknownUnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown systemd unit '{}'; embedded units:",
            self.requested
        )?;
        for unit in systemd_units() {
            write!(f, " {}", unit.file_name())?;
        }
        Ok(())
    }
}

impl std::error::Error for UnknownUnitError {}

/// Renders one embedded unit, or all of them, for `harw install
/// --print-systemd [UNIT]`.
///
/// # Arguments
/// - `unit` (`Option<&str>`): a unit file name (see [`systemd_unit`]); `None`
///   prints every unit.
/// - `paths` (`&RenderPaths`): placeholder values.
///
/// # Returns
/// With a unit: exactly that unit's rendered text. Without: every unit,
/// each preceded by a `# ---- deploy/systemd/<name> ----` line.
///
/// # Errors
/// [`UnknownUnitError`] when `unit` names no embedded systemd unit.
pub fn print_systemd(unit: Option<&str>, paths: &RenderPaths) -> Result<String, UnknownUnitError> {
    match unit {
        Some(name) => systemd_unit(name)
            .map(|asset| render(asset.contents, paths))
            .ok_or_else(|| UnknownUnitError {
                requested: name.to_owned(),
            }),
        None => {
            let mut out = String::new();
            for (index, asset) in systemd_units().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str(&format!("# ---- {} ----\n", asset.path));
                out.push_str(&render(asset.contents, paths));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests;
