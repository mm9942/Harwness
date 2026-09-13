use std::sync::Arc;

use async_trait::async_trait;
use harw_browser::host::BrowserHost;
use harw_browser::ids::BrowserSessionId;
use harw_browser::policy::OpenBrowserRequest;
use harw_browser::session::BrowserSessionHandle;
use harw_extension_api::contributors::ToolProvider;
use harw_tool_browser::{BrowserToolSet, HarwnessBrowserToolProvider};
use harw_tools::{ToolName, ToolSpec};

const EXPECTED_TOOL_NAMES: [&str; 7] = [
    "browser.open",
    "browser.observe",
    "browser.find",
    "browser.act",
    "browser.wait",
    "browser.events",
    "browser.close",
];

#[test]
fn provider_advertises_only_the_closed_browser_surface() {
    let provider = HarwnessBrowserToolProvider::new(BrowserToolSet::new(Arc::new(NeverUsedHost)));

    let tools = provider.tools();
    let names: Vec<&str> = tools.iter().map(function_tool_name).collect();

    assert_eq!(names, EXPECTED_TOOL_NAMES);
}

#[test]
fn provider_does_not_create_an_executor_for_unknown_tools() {
    let provider = HarwnessBrowserToolProvider::new(BrowserToolSet::new(Arc::new(NeverUsedHost)));

    assert!(
        provider
            .executor(&ToolName::new("browser.delete_everything"))
            .is_none()
    );
}

fn function_tool_name(ToolSpec::Function(function): &ToolSpec) -> &str {
    function.name.as_str()
}

struct NeverUsedHost;

#[async_trait]
impl BrowserHost for NeverUsedHost {
    async fn open(
        &self,
        _request: OpenBrowserRequest,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        panic!("provider surface tests must not invoke the browser host")
    }

    async fn session(
        &self,
        _requested_session: &BrowserSessionId,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        panic!("provider surface tests must not invoke the browser host")
    }

    async fn close(&self, _id: &BrowserSessionId) -> harw_browser::Result<()> {
        panic!("provider surface tests must not invoke the browser host")
    }
}
