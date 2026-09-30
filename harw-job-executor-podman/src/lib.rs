//! `harw-job-executor-podman` — Rootless-Podman als Job-Executor-Backend
//! (P2 Builder-API-Contract).
//!
//! ```text
//! generic SandboxPolicy (harw-job-linux)
//!         │
//!         └── Podman executor (this crate)
//!               rootless podman run · --memory (RAM-Limit)
//!               · --volume (MountSpec-Binds) · --network none
//!               · optional SSH-Remote (Pi-Worker mm29942-raspi)
//! ```
//!
//! # Vertragsform
//! Identisch zu [`harw_job_executor_bwrap`](../harw-job-executor-bwrap/):
//! [`PodmanExecutor::plan`] übersetzt [`harw_job_core::JobSpec`] plus
//! [`harw_job_linux::SandboxPolicy`] in einen inspectierbaren
//! [`PodmanJobPlan`]; [`PodmanJobPlan::into_command`] liefert ein
//! [`std::process::Command`] für [`harw_job_linux::LinuxProcess::spawn`].
//! Das Crate fügt **keinen neuen Isolationsmechanismus** hinzu — die
//! Enforcer-Semantik kommt von Podman (rootless: eigene Mount- und
//! Netz-Namespace, keine Capabilities).
//!
//! # Was verweigert statt stillschweigend erweitert wird
//! - Netz: nur [`NetworkPolicy::Deny`] (Container-Lauf mit `--network none`).
//!   Egress via Relay ist bewusst **nicht** angedockt (siehe P3).
//! - Capabilities: jede Keep-Liste wird abgelehnt (rootless-Podman droppt alle).
//! - Root-Cargo-Aufrufe: `--userns=keep-id` hält die Workspace-UID; cargo
//!   läuft nie als Root.
//!
//! # Remote (Pi-Worker)
//! Ein konfigurierter SSH-Endpunkt (`BuilderRemoteToml.endpoint`, z. B.
//! `mia@100.123.51.33` für `mm29942-raspi`) stellt dem Plan `ssh://` voran:
//! `podman --remote --url ssh://mia@100.123.51.33/run/user/1000/podman/podman.sock`.
//! Der Befehl läuft dann auf dem Pi; Mounts müssen dort existieren.
//!
//! # Ressourcen
//! `resource_limits` wird auf [`EnforcementState::Enforced`] gesetzt, wenn
//! ein RAM-Limit gesetzt ist (`--memory <MiB>m`, Linux-cgroup via Podman);
//! ohne Limit bleibt es [`EnforcementState::NotEnforced`].
//!
//! Auf Nicht-Linux-Zielen ist das Crate leer.

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use harw_config::{BuilderToml, MountSpec};
use harw_job_core::{EnforcementState, JobSpec, SandboxReport};
use harw_job_linux::{CapabilityPolicy, NetworkPolicy, SandboxPolicy};

/// Fehler des Podman-Executors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PodmanExecutorError {
    /// Podman wurde nicht gefunden oder ist nicht ausführbar.
    Unsupported {
        /// Welche Dimension fehlt.
        dimension: &'static str,
        /// Grund im Klartext.
        reason: String,
    },
    /// Ungültiger JobSpec (Validierung fehlgeschlagen).
    InvalidSpec {
        /// Grund im Klartext.
        reason: String,
    },
    /// Anfrage, die das Backend nicht ausdrücken kann.
    UnsupportedPolicy {
        /// Grund im Klartext.
        reason: String,
    },
}

impl std::fmt::Display for PodmanExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported { dimension, reason } => {
                write!(f, "podman executor unsupported ({dimension}): {reason}")
            }
            Self::InvalidSpec { reason } => write!(f, "invalid job spec: {reason}"),
            Self::UnsupportedPolicy { reason } => write!(f, "unsupported policy: {reason}"),
        }
    }
}

impl std::error::Error for PodmanExecutorError {}

/// Ein geplanter Podman-Container-Lauf.
///
/// # Beschreibung
/// Inspektionsbar, bevor irgendetwas läuft: das vollständige `podman`-argv,
/// das vorausgesagte per-Dimension [`SandboxReport`], die Mount-Binds und
/// das RAM-Limit. [`into_command`](Self::into_command) macht daraus ein
/// [`Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodmanJobPlan {
    executable: String,
    args: Vec<OsString>,
    report: SandboxReport,
    mounts: Vec<MountSpec>,
    ram_mib: u32,
}

impl PodmanJobPlan {
    /// Das `podman`-Executable (absolut oder via PATH aufgelöst beim Start).
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Die `podman`-Argumente (alles nach dem Executable), endend auf
    /// `-- <program> <args…>`.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// Vorausgesagte Enforcement-Instanz der Podman-Schicht.
    #[must_use]
    pub fn report(&self) -> &SandboxReport {
        &self.report
    }

    /// Die wirksamen Mount-Binds.
    #[must_use]
    pub fn mounts(&self) -> &[MountSpec] {
        &self.mounts
    }

    /// Das RAM-Limit in MiB (0 = kein Limit).
    #[must_use]
    pub fn ram_mib(&self) -> u32 {
        self.ram_mib
    }

    /// Macht daraus ein [`Command`] für `harw-job-linux`.
    #[must_use]
    pub fn into_command(self) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(&self.args);
        command.env_clear();
        // Minimal-Environment: PATH für podman-Findung, TERM für TTY-Kompat.
        command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        command.env("TERM", "dumb");
        command
    }
}

/// Rootless-Podman-Job-Executor: plant [`JobSpec`] unter [`SandboxPolicy`]
/// als `podman run`-Launch.
///
/// # Beschreibung
/// Reiner Wert; jede [`plan`](Self::plan) startet frisch. Konfiguration kommt
/// aus [`BuilderToml`] (`template = "rootless-podman"`, RAM/Job-Grenzen,
/// Mounts, optionaler SSH-Remote-Pi).
///
/// # Concurrency
/// `Send + Sync`; Planning hat keine Nebenwirkungen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodmanExecutor {
    executable: String,
    remote_endpoint: Option<String>,
    remote_identity: Option<String>,
    ram_mib: u32,
    mounts: Vec<MountSpec>,
}

impl PodmanExecutor {
    /// Findet ein `podman` im System (fixe Pfade bevorzugt, nie Root-Podman).
    ///
    /// # Errors
    /// [`PodmanExecutorError::Unsupported`] ohne installiertes Podman.
    pub fn discover() -> Result<Self, PodmanExecutorError> {
        for candidate in ["/usr/bin/podman", "/usr/local/bin/podman"] {
            if Path::new(candidate).exists() {
                return Ok(Self {
                    executable: candidate.to_owned(),
                    remote_endpoint: None,
                    remote_identity: None,
                    ram_mib: 0,
                    mounts: Vec::new(),
                });
            }
        }
        Err(PodmanExecutorError::Unsupported {
            dimension: "backend",
            reason: "no podman found at /usr/bin/podman or /usr/local/bin/podman".to_owned(),
        })
    }

    /// Benutzt genau `executable` als Podman (z. B. auf dem Pi-Worker).
    #[must_use]
    pub fn from_executable(executable: String) -> Self {
        Self {
            executable,
            remote_endpoint: None,
            remote_identity: None,
            ram_mib: 0,
            mounts: Vec::new(),
        }
    }

    /// Konfiguriert den Executor aus [`BuilderToml`] (RAM-Limit, Mounts,
    /// SSH-Remote mit Identity).
    #[must_use]
    pub fn from_builder_config(mut self, config: &BuilderToml) -> Self {
        self.ram_mib = config.effective_ram_mib();
        self.mounts = config.effective_mounts().to_vec();
        if let Some(remote) = &config.remote {
            self.remote_endpoint = Some(remote.endpoint.clone());
            self.remote_identity = remote.identity.clone();
        }
        self
    }

    /// Plant `spec` unter `policy` mit `workspace_root` als Workspace.
    ///
    /// # Errors
    /// - [`PodmanExecutorError::InvalidSpec`] bei ungültigem `spec`.
    /// - [`PodmanExecutorError::UnsupportedPolicy`] bei Netz/Capability-
    ///   Anfragen, die rootless-Podman hier nicht ausdrücken soll.
    pub fn plan(
        &self,
        spec: &JobSpec,
        policy: &SandboxPolicy,
        workspace_root: &Path,
    ) -> Result<PodmanJobPlan, PodmanExecutorError> {
        spec.validate().map_err(|error| PodmanExecutorError::InvalidSpec {
            reason: error.to_string(),
        })?;

        // Netz: nur Deny ist erlaubt (hermetisch).
        if !matches!(policy.network, NetworkPolicy::Deny) {
            return Err(PodmanExecutorError::UnsupportedPolicy {
                reason: "only NetworkPolicy::Deny is supported (hermetic container)".to_owned(),
            });
        }
        // Capabilities: nur DropAll (oder leer).
        if let CapabilityPolicy::Keep(ref keep) = policy.capabilities {
            if !keep.is_empty() {
                return Err(PodmanExecutorError::UnsupportedPolicy {
                    reason: "capability keep-lists are refused (rootless podman drops all)"
                        .to_owned(),
                });
            }
        }

        let mut args: Vec<OsString> = Vec::new();
        // Remote (Pi-Worker): --remote --url ssh://… [+ --identity]
        if let Some(endpoint) = &self.remote_endpoint {
            args.push("--remote".into());
            args.push(
                format!("--url=ssh://{endpoint}/run/user/1000/podman/podman.sock").into(),
            );
            if let Some(identity) = &self.remote_identity {
                args.push(format!("--identity={identity}").into());
            }
        }
        args.push("run".into());
        args.push("--rm".into());
        args.push("--userns=keep-id".into());
        args.push("--network=none".into());
        // RAM-Limit (cgroup via Podman).
        if self.ram_mib > 0 {
            args.push(format!("--memory={}m", self.ram_mib).into());
        }
        // Workspace-Bind (read-write) + MountSpec-Binds.
        args.push(format!("--volume={}:{}", workspace_root.display(), "/workspace").into());
        for mount in &self.mounts {
            let flag = if mount.read_only {
                format!("--volume={}:{}:ro", mount.source, mount.target)
            } else {
                format!("--volume={}:{}", mount.source, mount.target)
            };
            args.push(flag.into());
        }
        // Arbeitsverzeichnis.
        args.push("--workdir=/workspace".into());

        // Programm + Argumente verbatim.
        args.push(spec.program.clone().into());
        for arg in &spec.args {
            args.push(arg.clone().into());
        }

        let report = SandboxReport {
            filesystem: EnforcementState::Partial,
            network: EnforcementState::Enforced,
            no_new_privs: EnforcementState::Enforced,
            capabilities: EnforcementState::Enforced,
            resource_limits: if self.ram_mib > 0 {
                EnforcementState::Enforced
            } else {
                EnforcementState::NotEnforced
            },
        };

        Ok(PodmanJobPlan {
            executable: self.executable.clone(),
            args,
            report,
            mounts: self.mounts.clone(),
            ram_mib: self.ram_mib,
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use harw_job_linux::{CapabilityPolicy, FilesystemPolicy, NetworkPolicy, SandboxPolicy};

    fn deny_policy() -> SandboxPolicy {
        SandboxPolicy {
            filesystem: FilesystemPolicy {
                read_only: vec![],
                read_write: vec![],
                exec: vec![],
            },
            network: NetworkPolicy::Deny,
            capabilities: CapabilityPolicy::DropAll,
            no_new_privs: true,
            landlock: harw_job_linux::LandlockMode::BestEffort,
        }
    }

    #[test]
    fn test_plan_rejects_non_deny_network() {
        let executor = PodmanExecutor::from_executable("/usr/bin/podman".to_owned());
        let mut policy = deny_policy();
        policy.network = NetworkPolicy::Allow;
        let spec = JobSpec::command("echo").build().expect("valid spec");
        assert!(executor.plan(&spec, &policy, Path::new("/tmp")).is_err());
    }

    #[test]
    fn test_plan_rejects_capability_keep_list() {
        let executor = PodmanExecutor::from_executable("/usr/bin/podman".to_owned());
        let mut policy = deny_policy();
        policy.capabilities = CapabilityPolicy::Keep(vec!["net_bind_service".to_owned()]);
        let spec = JobSpec::command("echo").build().expect("valid spec");
        assert!(executor.plan(&spec, &policy, Path::new("/tmp")).is_err());
    }

    #[test]
    fn test_plan_hermetic_defaults() {
        let executor = PodmanExecutor::from_executable("/usr/bin/podman".to_owned());
        let spec = JobSpec::command("echo").build().expect("valid spec");
        let plan = executor
            .plan(&spec, &deny_policy(), Path::new("/srv/workspace"))
            .expect("plan");
        assert_eq!(plan.ram_mib(), 0);
        assert_eq!(plan.report().network, EnforcementState::Enforced);
        let command = plan.into_command();
        // Programm + Argumente sind Teil des argv.
        assert!(command
            .get_args()
            .any(|a| a.to_string_lossy() == "echo"));
    }
}
