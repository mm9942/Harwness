//! Einmalige und sitzungsgebundene Freigaben für Prozessausführung.
//!
//! Ein [`ProcessPermit`] ist bewusst kein Tool-Argument und kein konfigurierbarer
//! Sandbox-Schalter. Er wird von der Runtime vor der Ausführung ausgestellt und
//! vom syscall-nahen Executor atomar verbraucht beziehungsweise geprüft. Damit
//! kann ein Modell weder einen Permit aus JSON erzeugen noch einen Permit für
//! einen anderen Befehl, Worker oder Workspace wiederverwenden.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Umgebung, die ein genehmigter Prozess benutzen darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessEnvironment {
    /// Normale isolierte Bubblewrap-Ausführung.
    StrictSandbox,
    /// Isolierte Ausführung mit dem vorher konfigurierten Cargo-Modul.
    CargoSandbox,
    /// Isolierte Ausführung mit einem einzelnen validierten tmux-Socket.
    TmuxInspection,
    /// Lokale Host-Ausführung; nur nach UI-Approval möglich.
    LocalHost,
}

/// Lebensdauer einer durch die lokale UI bestätigten Host-Freigabe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostApprovalScope {
    /// Ein Permit darf nur genau einen Auftrag ausführen.
    SingleExecution,
    /// Mehrere Aufträge derselben lokalen Sitzung dürfen bis Ablauf des Leases
    /// Host-Ausführung beantragen. Jeder Auftrag wird trotzdem gegen Worker,
    /// Workspace und Umgebung geprüft.
    SessionLease,
}

/// Nicht erratbare Kennung eines Runtime-ausgestellten Permits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessPermitId(u64);

/// Der unveränderliche Bindungsgegenstand einer Prozessfreigabe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPermitRequest {
    /// Lokale Parent-Sitzung, für die die UI zugestimmt hat.
    pub session: String,
    /// Exakte eingebettete Worker-Definition, nicht nur deren Organisationsrolle.
    pub worker_definition: String,
    /// Kanonischer Prozessauftrag. Für Shell-Aufträge ist dies der exakt an
    /// `/bin/sh -c` zu übergebende Text.
    pub command: String,
    /// Bereits kanonische Workspace-Wurzel.
    pub workspace: PathBuf,
    /// Gewünschte, enge Prozessumgebung.
    pub environment: ProcessEnvironment,
}

impl ProcessPermitRequest {
    /// Prüft ausschließlich die strukturellen Invarianten eines Antrags.
    pub fn validate(&self) -> Result<(), ProcessPermitError> {
        if self.session.trim().is_empty()
            || self.worker_definition.trim().is_empty()
            || self.command.trim().is_empty()
        {
            return Err(ProcessPermitError::InvalidRequest);
        }
        if !self.workspace.is_absolute() {
            return Err(ProcessPermitError::WorkspaceNotAbsolute);
        }
        Ok(())
    }
}

/// Ergebnis einer erfolgreichen Permit-Prüfung; nur der Ledger kann es erzeugen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedProcessPermit {
    id: ProcessPermitId,
    request: ProcessPermitRequest,
    scope: HostApprovalScope,
}

impl GrantedProcessPermit {
    #[must_use]
    pub fn id(&self) -> ProcessPermitId {
        self.id
    }

    #[must_use]
    pub fn request(&self) -> &ProcessPermitRequest {
        &self.request
    }

    #[must_use]
    pub fn scope(&self) -> HostApprovalScope {
        self.scope
    }
}

#[derive(Debug, Clone)]
struct StoredPermit {
    request: ProcessPermitRequest,
    scope: HostApprovalScope,
    expires_at: Instant,
    consumed: bool,
}

/// Runtime-eigene, nebenläufig sichere Freigabe-Registry.
///
/// Die Registry ist der Durchsetzungspunkt für Einmaligkeit und Ablauf. Ihre
/// Ausstellungsfunktionen gehören an die lokale UI-Approval-Grenze; ein Modell
/// erhält nur eine Kennung, niemals eine Möglichkeit, einen Eintrag zu erzeugen.
#[derive(Debug)]
pub struct ProcessPermitLedger {
    state: Mutex<PermitState>,
}

#[derive(Debug)]
struct PermitState {
    next_id: u64,
    permits: BTreeMap<ProcessPermitId, StoredPermit>,
}

impl Default for ProcessPermitLedger {
    fn default() -> Self {
        Self {
            state: Mutex::new(PermitState {
                next_id: 1,
                permits: BTreeMap::new(),
            }),
        }
    }
}

impl ProcessPermitLedger {
    /// Stellt nach einer lokalen UI-Entscheidung eine Freigabe aus.
    ///
    /// `SessionLease` ist nur für `LocalHost` gültig. Damit kann eine normale
    /// Cargo- oder tmux-Modulfreigabe nicht versehentlich zur längeren
    /// Host-Arbeitsphase werden.
    pub fn issue_after_local_approval(
        &self,
        request: ProcessPermitRequest,
        scope: HostApprovalScope,
        ttl: Duration,
    ) -> Result<ProcessPermitId, ProcessPermitError> {
        request.validate()?;
        if ttl.is_zero() {
            return Err(ProcessPermitError::ZeroTtl);
        }
        if scope == HostApprovalScope::SessionLease
            && request.environment != ProcessEnvironment::LocalHost
        {
            return Err(ProcessPermitError::SessionLeaseRequiresHost);
        }
        let expires_at = Instant::now()
            .checked_add(ttl)
            .ok_or(ProcessPermitError::TtlOverflow)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ProcessPermitError::Poisoned)?;
        let id = ProcessPermitId(state.next_id);
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or(ProcessPermitError::IdExhausted)?;
        state.permits.insert(
            id,
            StoredPermit {
                request,
                scope,
                expires_at,
                consumed: false,
            },
        );
        Ok(id)
    }

    /// Prüft die vollständige Bindung und verbraucht Einmal-Permits atomar.
    ///
    /// Bei `SessionLease` bleibt der Eintrag bis Ablauf erhalten, aber nur ein
    /// exakt gleicher Auftrag darf ihn verwenden. Das verhindert, dass eine
    /// Zustimmung zu `tmux capture-pane` als allgemeines Host-Shell-Recht dient.
    pub fn authorize(
        &self,
        id: ProcessPermitId,
        actual: &ProcessPermitRequest,
    ) -> Result<GrantedProcessPermit, ProcessPermitError> {
        actual.validate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ProcessPermitError::Poisoned)?;
        let expired = state
            .permits
            .get(&id)
            .ok_or(ProcessPermitError::UnknownPermit)?
            .expires_at
            <= Instant::now();
        if expired {
            state.permits.remove(&id);
            return Err(ProcessPermitError::Expired);
        }
        let permit = state
            .permits
            .get_mut(&id)
            .expect("permit was checked while the ledger lock is held");
        if permit.request != *actual {
            return Err(ProcessPermitError::BindingMismatch);
        }
        if permit.scope == HostApprovalScope::SingleExecution {
            if permit.consumed {
                return Err(ProcessPermitError::AlreadyConsumed);
            }
            permit.consumed = true;
        }
        Ok(GrantedProcessPermit {
            id,
            request: permit.request.clone(),
            scope: permit.scope,
        })
    }

    /// Widerruft einen Permit oder Lease sofort, etwa bei UI-„Isolation wieder
    /// aktivieren“, Sitzungsende oder Disconnect. Unbekannte Kennungen sind
    /// absichtlich idempotent.
    pub fn revoke(&self, id: ProcessPermitId) -> Result<(), ProcessPermitError> {
        self.state
            .lock()
            .map_err(|_| ProcessPermitError::Poisoned)?
            .permits
            .remove(&id);
        Ok(())
    }

    /// Widerruft sämtliche Permits einer Sitzung. Der Stringvergleich ist
    /// absichtlich exakt; keine Präfix- oder Teiltreffer dürfen fremde Sessions
    /// beeinflussen.
    pub fn revoke_session(&self, session: &str) -> Result<usize, ProcessPermitError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ProcessPermitError::Poisoned)?;
        let before = state.permits.len();
        state
            .permits
            .retain(|_, permit| permit.request.session != session);
        Ok(before - state.permits.len())
    }
}

/// Fail-closed Fehler der Permit-Grenze. Details über andere Permits werden nie
/// offengelegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPermitError {
    InvalidRequest,
    WorkspaceNotAbsolute,
    ZeroTtl,
    TtlOverflow,
    SessionLeaseRequiresHost,
    UnknownPermit,
    Expired,
    BindingMismatch,
    AlreadyConsumed,
    IdExhausted,
    Poisoned,
}

impl std::fmt::Display for ProcessPermitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidRequest | Self::WorkspaceNotAbsolute => "invalid process permit request",
            Self::ZeroTtl | Self::TtlOverflow => "invalid process permit lifetime",
            Self::SessionLeaseRequiresHost => "a session lease requires local host execution",
            Self::UnknownPermit | Self::Expired | Self::BindingMismatch | Self::AlreadyConsumed => {
                "process execution is not authorized"
            }
            Self::IdExhausted | Self::Poisoned => "process permit service is unavailable",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ProcessPermitError {}

/// Helper for callers that already hold a canonical workspace root.
#[must_use]
pub fn request_for_workspace(
    session: impl Into<String>,
    worker_definition: impl Into<String>,
    command: impl Into<String>,
    workspace: &Path,
    environment: ProcessEnvironment,
) -> ProcessPermitRequest {
    ProcessPermitRequest {
        session: session.into(),
        worker_definition: worker_definition.into(),
        command: command.into(),
        workspace: workspace.to_path_buf(),
        environment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(environment: ProcessEnvironment) -> ProcessPermitRequest {
        request_for_workspace(
            "local-session",
            "host-process-worker@1",
            "tmux ls",
            Path::new("/workspace"),
            environment,
        )
    }

    #[test]
    fn single_execution_is_bound_and_consumed() {
        let ledger = ProcessPermitLedger::default();
        let request = request(ProcessEnvironment::LocalHost);
        let id = ledger
            .issue_after_local_approval(
                request.clone(),
                HostApprovalScope::SingleExecution,
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(ledger.authorize(id, &request).is_ok());
        assert_eq!(
            ledger.authorize(id, &request),
            Err(ProcessPermitError::AlreadyConsumed)
        );

        let mut changed = request;
        changed.command = "tmux kill-server".to_owned();
        assert_eq!(
            ledger.authorize(id, &changed),
            Err(ProcessPermitError::BindingMismatch)
        );
    }

    #[test]
    fn session_lease_is_host_only_and_revocable() {
        let ledger = ProcessPermitLedger::default();
        let strict = request(ProcessEnvironment::StrictSandbox);
        assert_eq!(
            ledger.issue_after_local_approval(
                strict,
                HostApprovalScope::SessionLease,
                Duration::from_secs(30)
            ),
            Err(ProcessPermitError::SessionLeaseRequiresHost)
        );
        let host = request(ProcessEnvironment::LocalHost);
        let id = ledger
            .issue_after_local_approval(
                host.clone(),
                HostApprovalScope::SessionLease,
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(ledger.authorize(id, &host).is_ok());
        assert!(ledger.authorize(id, &host).is_ok());
        assert_eq!(ledger.revoke_session("local-session"), Ok(1));
        assert_eq!(
            ledger.authorize(id, &host),
            Err(ProcessPermitError::UnknownPermit)
        );
    }
}
