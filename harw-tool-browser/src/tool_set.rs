use std::sync::Arc;

use harw_browser::host::BrowserHost;

use crate::prepare::{browser_tool_descriptors, prepare_browser_call_with_policy};
use crate::types::{
    BrowserOpenPolicy, BrowserToolDescriptor, BrowserToolRequest, PreparedBrowserCall,
};

/// Compact model-facing browser tool collection backed by an authoritative host.
///
/// Preparation remains pure and host-free. The retained host is available only
/// inside this crate so dispatch can resolve session handles without exposing a
/// concrete browser driver or backend through the public API.
pub struct BrowserToolSet {
    host: Arc<dyn BrowserHost>,
    open_policy: BrowserOpenPolicy,
}

impl BrowserToolSet {
    /// Creates a browser tool set with no `browser.open` authority.
    ///
    /// This deliberately fail-closed constructor preserves existing call sites
    /// without making them silently authorize model-provided origin or profile
    /// policy. Use [`Self::with_open_policy`] for an explicit host grant.
    pub fn new(host: Arc<dyn BrowserHost>) -> Self {
        Self {
            host,
            open_policy: BrowserOpenPolicy::default(),
        }
    }

    pub fn with_open_policy(host: Arc<dyn BrowserHost>, open_policy: BrowserOpenPolicy) -> Self {
        Self { host, open_policy }
    }

    pub fn descriptors(&self) -> &'static [BrowserToolDescriptor] {
        browser_tool_descriptors()
    }

    pub fn prepare(
        &self,
        request: BrowserToolRequest,
    ) -> harw_browser::Result<PreparedBrowserCall> {
        prepare_browser_call_with_policy(request, Some(&self.open_policy))
    }

    pub(crate) fn host(&self) -> &dyn BrowserHost {
        self.host.as_ref()
    }
}
