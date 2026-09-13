//! Model-facing browser tools backed by the pure `harw-browser` contracts.
//!
//! The crate keeps typed request preparation separate from effectful dispatch:
//! callers can inspect the complete requested browser authority scope before a
//! prepared call is admitted to the host.

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
    ObserveRequest, ObserveResponse, OpenRequest, OpenResponse, PreparedBrowserCall, WaitRequest,
    WaitResponse,
};
