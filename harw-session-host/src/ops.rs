//! Operations backend of the minor-4 `manage`/`stepup` methods (contract K-W3c §8.1).
use harw_protocol::ClientCaps;
use harw_protocol::session_wire::{
    CatalogGetParams, CatalogResult, CommandRunParams, CommandRunResult, CommandsListParams,
    CommandsListResult, ConfigGetParams, ConfigGetResult, ConfigResetParams, ConfigSetParams,
    SecretPutParams, SecretPutResult, StepUpCancelParams, StepUpConfirmParams,
};
use harw_types::{PermissionTier, SessionId, TenantId};

use crate::driver::DriverFuture;
use crate::identity::ClientIdentity;

/// Who calls, as admitted by the host. Built only by `HostConnection`, never from the wire.
#[derive(Clone, Debug)]
pub struct OpsCaller {
    /// Identity of the admitted connection.
    pub identity: ClientIdentity,
    /// Caps granted at hello (already `requested ∩ ceiling`).
    pub granted: ClientCaps,
    /// Effective tier = min(`tier_for_caps(granted)`, listener tier of the profile share).
    pub tier: PermissionTier,
    /// Tenant of the shared profile the ops act on (`ShareSettings::tenant`); `None` for a local unscoped host.
    pub profile_tenant: Option<TenantId>,
}

/// The session a call refers to, after host admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpsSession {
    /// Id of the referenced session.
    pub session_id: SessionId,
    /// `true`: the host has a record/slot (typed session ways possible). `false`: profile-only row.
    pub hosted: bool,
    /// `TurnDriver::model_name`, when known.
    pub model: Option<String>,
}

/// A typed session way the HOST executes after its own admission (plan 4.1 slice 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionAction {
    /// `Setting::Model` via `HostConnection::set` (needs `control`).
    SetModel(String),
    /// `Setting::Mode` (needs `control`).
    SetMode(String),
    /// `Setting::Effort` (needs `control`).
    SetEffort(String),
    /// `turn.interrupt` without `turn_id` (needs `steer`). Used for `/stop` and `/cancel`. [OFFEN-4]
    Interrupt,
}

/// Outcome of `command_run`.
#[derive(Clone, Debug, PartialEq)]
pub enum OpsRun {
    /// Finished (read, refusal, step-up challenge).
    Done(CommandRunResult),
    /// The host runs `action` on `OpsSession::session_id`; on success it answers `result`,
    /// on a host error it answers that error (no partial success).
    Session {
        /// Typed session way the host executes.
        action: SessionAction,
        /// Answer on success of `action`.
        result: CommandRunResult,
    },
}

/// Implemented in `harw-cli/src/session_ops.rs` (`HostedOps`). All methods are fail closed.
pub trait OpsPort: Send + Sync + 'static {
    /// Whether feature `stepup` may be granted (B5: `REMOTE_ENABLED_SLICE >= Global`).
    fn stepup_enabled(&self) -> bool;
    /// Lists the commands the caller may see.
    fn commands_list(
        &self,
        caller: OpsCaller,
        session: Option<OpsSession>,
        params: CommandsListParams,
    ) -> DriverFuture<'_, CommandsListResult>;
    /// Runs one allow-listed command line.
    fn command_run(
        &self,
        caller: OpsCaller,
        session: Option<OpsSession>,
        params: CommandRunParams,
    ) -> DriverFuture<'_, OpsRun>;
    /// Returns the redacted catalog view.
    fn catalog_get(
        &self,
        caller: OpsCaller,
        session: Option<OpsSession>,
        params: CatalogGetParams,
    ) -> DriverFuture<'_, CatalogResult>;
    /// Reads redacted config values.
    fn config_get(
        &self,
        caller: OpsCaller,
        params: ConfigGetParams,
    ) -> DriverFuture<'_, ConfigGetResult>;
    /// Sets one config value.
    fn config_set(
        &self,
        caller: OpsCaller,
        params: ConfigSetParams,
    ) -> DriverFuture<'_, CommandRunResult>;
    /// Resets one config value.
    fn config_reset(
        &self,
        caller: OpsCaller,
        params: ConfigResetParams,
    ) -> DriverFuture<'_, CommandRunResult>;
    /// Stages a write-only secret.
    fn secret_put(
        &self,
        caller: OpsCaller,
        params: SecretPutParams,
    ) -> DriverFuture<'_, SecretPutResult>;
    /// Confirms a pending step-up.
    fn stepup_confirm(
        &self,
        caller: OpsCaller,
        params: StepUpConfirmParams,
    ) -> DriverFuture<'_, CommandRunResult>;
    /// Cancels a pending step-up.
    fn stepup_cancel(&self, caller: OpsCaller, params: StepUpCancelParams) -> DriverFuture<'_, ()>;
    /// Connection closed: drop its step-ups and staged secrets. Never panics, never blocks long.
    fn connection_closed(&self, identity: &ClientIdentity);
}

/// Inverse of `caps_for_tier` for a granted cap set: `gateway_admin` -> Owner, `control` -> Maintainer,
/// `steer` or `approve` -> Operator, else Observer.
#[must_use]
pub const fn tier_for_caps(caps: ClientCaps) -> PermissionTier {
    if caps.gateway_admin {
        PermissionTier::Owner
    } else if caps.control {
        PermissionTier::Maintainer
    } else if caps.steer || caps.approve {
        PermissionTier::Operator
    } else {
        PermissionTier::Observer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_for_caps_maps_each_cap_set() {
        assert_eq!(tier_for_caps(ClientCaps::NONE), PermissionTier::Observer);
        assert_eq!(tier_for_caps(ClientCaps::OBSERVE), PermissionTier::Observer);
        assert_eq!(tier_for_caps(ClientCaps::OPERATE), PermissionTier::Operator);
        assert_eq!(tier_for_caps(ClientCaps::ALL), PermissionTier::Maintainer);
        assert_eq!(
            tier_for_caps(ClientCaps::ALL.with(ClientCaps::GATEWAY_ADMIN)),
            PermissionTier::Owner
        );
        let approve_only = ClientCaps {
            approve: true,
            ..ClientCaps::NONE
        };
        assert_eq!(tier_for_caps(approve_only), PermissionTier::Operator);
    }
}
