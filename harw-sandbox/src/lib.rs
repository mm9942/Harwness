//! Linux sandbox backends for HARW.
//!
//! Authorization, workspace containment, network scope evaluation and grants
//! live in [`harw_authority`]. This crate owns only operating-system-adjacent
//! launch profiles and Bubblewrap execution. It intentionally does not
//! re-export authority types: consumers must name `harw_authority` directly.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::PathBuf;

mod cargo;
pub use cargo::{CargoExecutionMode, CargoProfileError, CargoSandboxProfile};

mod tmux;
pub use tmux::{SANDBOX_TMUX_SOCKET_PATH, TmuxOperationMode, TmuxProfileError, TmuxSandboxProfile};

mod profile;
pub use profile::SandboxProfile;

mod bwrap;
pub use bwrap::{
    BwrapCommandPlan, BwrapLauncher, HostPathBinding, SANDBOX_PROXY_SOCKET_PATH,
    SANDBOX_RELAY_PATH, SandboxChild,
};

pub mod egress;
pub use egress::{EgressHost, EgressUrl, EgressUrlError};

mod extra_roots;
pub use extra_roots::{
    ExtraRoot, ExtraRootError, ExtraRootsCell, MAX_EXTRA_ROOTS, validate_extra_root,
};

mod process_permit;
pub use process_permit::{
    GrantedProcessPermit, HostApprovalScope, ProcessEnvironment, ProcessPermitError,
    ProcessPermitId, ProcessPermitLedger, ProcessPermitRequest, request_for_workspace,
};

mod host_permit_session;
pub use host_permit_session::HostPermitSessionRegistry;

#[cfg(test)]
mod test_support;

/// Network mode for a process sandbox. No mode shares the host network
/// namespace; proxy-only mode still needs
/// [`harw_authority::Permission::NetworkAccess`] in the supplied authority.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetworkMode {
    #[default]
    None,
    ProxyOnly(RelaySpec),
}

/// Host-side relay configuration for [`NetworkMode::ProxyOnly`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySpec {
    pub binary: PathBuf,
    pub listen_port: u16,
    pub proxy_socket: PathBuf,
}

/// Errors from sandbox OS backends. Authority and scope validation errors
/// remain `harw_authority::AuthorityError` and are not wrapped here.
#[derive(Debug)]
pub enum SandboxError {
    Io {
        path: PathBuf,
        reason: String,
    },
    ProcessExecutionDenied,
    MissingSandboxCommand,
    InvalidSandboxWorkspaceDestination {
        path: PathBuf,
    },
    SandboxProcessSpawn {
        executable: PathBuf,
        reason: String,
    },
    NetworkModeNotGranted,
    CargoFetchNetworkDenied,
    InvalidRelaySpec {
        field: &'static str,
        reason: String,
    },
    /// Ein Pfad, der laut fester Konfiguration oder Kanonikalisierung immer
    /// einen Elternpfad haben muss, hatte keinen (Bible R087: keine Panik bei
    /// einem eigentlich unerreichbaren Zustand, sondern ein benannter Fehler).
    FixedPathWithoutParent {
        path: PathBuf,
    },
}

pub type SandboxResult<T> = Result<T, SandboxError>;

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, reason } => write!(f, "sandbox I/O at '{}': {reason}", path.display()),
            Self::ProcessExecutionDenied => write!(f, "sandbox does not permit process execution"),
            Self::MissingSandboxCommand => write!(f, "sandbox launch command is empty"),
            Self::InvalidSandboxWorkspaceDestination { path } => write!(
                f,
                "sandbox workspace mount destination must be absolute and normal: '{}'",
                path.display()
            ),
            Self::SandboxProcessSpawn { executable, reason } => write!(
                f,
                "could not launch sandbox executable '{}': {reason}",
                executable.display()
            ),
            Self::NetworkModeNotGranted => write!(
                f,
                "proxy-only network mode requires the network access permission"
            ),
            Self::CargoFetchNetworkDenied => write!(
                f,
                "Cargo fetch requires proxy-only network access and a non-empty network scope"
            ),
            Self::InvalidRelaySpec { field, reason } => {
                write!(f, "invalid egress relay configuration ({field}): {reason}")
            }
            Self::FixedPathWithoutParent { path } => write!(
                f,
                "path '{}' unexpectedly has no parent directory",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SandboxError {}
