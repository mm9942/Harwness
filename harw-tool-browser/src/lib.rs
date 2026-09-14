//! Model-facing browser tools backed by the pure `harw-browser` contracts.
//!
//! # Responsibility
//! The crate keeps typed request preparation separate from effectful dispatch:
//! callers can inspect the complete requested browser authority scope before a
//! prepared call is admitted to the host. Remediation W5 B-TOOL adds:
//!
//! - **Authority only from the operator**: origins, profile and limits come from
//!   the [`BrowserOpenGrant`] installed on [`BrowserToolSet::with_open_policy`];
//!   model JSON carrying them (or `Upload`/`CustomScript`) fails to deserialize.
//! - **Validation before every call**: [`BrowserToolSet::prepare`] and again
//!   [`BrowserToolSet::dispatch`] (with the session's own limits) apply
//!   `BrowserAction::validate`, `validate_target`, `validate_selector`,
//!   `WaitCondition::validate` and `WaitTimeout::validate`; `browser.act`
//!   consumes one unit of the session's `ActionBudget`.
//! - **Ownership**: every browser session belongs to exactly one harness
//!   session (`owner`); any other owner sees `SessionNotFound`.
//! - **Location post-check**: after open, find, act, wait, events and observe
//!   the observed URL is checked with `OpenBrowserRequest::check_observed_location`;
//!   a breach closes the browser session.
//! - **Schemas equal serde**: the advertised JSON schemas describe the exact
//!   serde shapes of the request types (see [`browser_tool_descriptors`]).
//!
//! # Key types
//! [`BrowserToolSet`], [`HarwnessBrowserToolProvider`], [`PreparedBrowserCall`],
//! [`PreparedBrowserRequest`], request/response envelopes.
//!
//! # Concurrency
//! [`BrowserToolSet`] is `Send + Sync`; its session registry is a short-lived
//! `std::sync::Mutex` never held across an `await`.
//!
//! # Errors
//! All fallible APIs return [`harw_browser::Result`].
//!
//! # Examples
//! ```rust,no_run
//! use harw_tool_browser::browser_tool_descriptors;
//!
//! assert_eq!(browser_tool_descriptors().len(), 7);
//! ```

mod dispatch;
mod harness_provider;
mod prepare;
mod tool_set;
mod types;

pub use harness_provider::HarwnessBrowserToolProvider;
pub use prepare::{browser_tool_descriptors, prepare_browser_call};
pub use tool_set::BrowserToolSet;
pub use types::{
    ActRequest, ActResponse, BrowserOpenGrant, BrowserOpenPolicy, BrowserResourceScope,
    BrowserScopeAction, BrowserToolDescriptor, BrowserToolRequest, BrowserToolResponse,
    CloseRequest, CloseResponse, EventsRequest, EventsResponse, FindRequest, FindResponse,
    ObserveRequest, ObserveResponse, OpenRequest, OpenResponse, PreparedBrowserCall,
    PreparedBrowserRequest, WaitRequest, WaitResponse,
};
