//! `harw-security-hub` — the local SecurityHub (crypto masterplan v2 H8,
//! WP-14): **WHO + WHERE → SHOULD**.
//!
//! # What it does
//! - **Policy engine** ([`policy`]): maps the kernel-attested `SO_PEERCRED`
//!   uid of a local caller to a principal, tenant, workspace scope, trust
//!   zone, authentication strength and TTL cap, and mints a short-lived
//!   [`harw_types::SecurityContext`] with the hub's single
//!   [`harw_types::SecurityContextIssuer`]. Requests may only narrow.
//! - **Context delivery** ([`server`], [`table`]): `POST /v1/contexts`
//!   returns a context reference plus a non-authoritative summary; other
//!   daemons resolve the reference with `GET /v1/contexts/{id}` and revoke it
//!   with `DELETE`. The trusted context never leaves this process (§25).
//! - **DoD correlation** ([`findings`]): read-only. `GET /v1/posture`
//!   attaches recent high-severity findings for this host from a JSON Lines
//!   export. Nothing is ever written to DoD, and no DoD crate is linked
//!   (§20: no DoD TCB widening).
//!
//! # Where it listens
//! Its own socket, `/run/harw/infra/security.sock` (drift report D1/D2 —
//! a deliberate deviation from masterplan §39, which pointed the security
//! profile at `control.sock`). Mode `0660`; HTTP/1 via Hyper, like
//! `harw-web` and `harw-auth-hub`. With `--systemd-socket` the socket comes
//! from systemd activation instead (`harw-security-hub.socket`, [`systemd`]).
//!
//! # What it is not
//! Not a KMS (that is `harw-auth-hub`), not a network policy daemon (that is
//! `harw-netsec`), and not an enforcement path (that stays with the DoD
//! Warden). `PermissionTier` carried in a context is operation-class
//! admission only, never cryptographic authorization (§32).

pub mod config;
pub mod error;
pub mod findings;
pub mod policy;
pub mod server;
pub mod systemd;
pub mod table;

#[cfg(test)]
mod test_support;

pub use config::HubConfig;
pub use error::HubError;
pub use findings::{DodFinding, DodFindingSource, FindingBatch, FindingFilter, JsonlFindingSource};
pub use policy::{ContextRequest, PeerIdentity, PolicyDenied, PolicyEngine, SecurityPolicy};
pub use server::{HubState, bind_listener, run, run_systemd, serve, systemd_listener};
pub use table::{ContextTable, Lookup, RevokeOutcome};
