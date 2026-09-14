//! Wait routing. Model-supplied script predicates are not supported (F-007).

use harw_browser::error::Error;
use harw_browser::ids::BrowserContextId;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};

use crate::runtime::FirefoxRuntime;

impl FirefoxRuntime {
    /// Routes a validated wait request to its single family-specific helper.
    pub(crate) async fn wait_condition(
        &self,
        context_id: &BrowserContextId,
        condition: WaitCondition,
        timeout: WaitTimeout,
    ) -> harw_browser::Result<WaitOutcome> {
        self.ensure_open()?;

        match condition {
            condition @ (WaitCondition::ElementPresent(_)
            | WaitCondition::ElementVisible(_)
            | WaitCondition::ElementClickable(_)
            | WaitCondition::ElementGone(_)) => {
                crate::wait_elements::wait_for_element(self, context_id, condition, timeout).await
            }
            condition @ (WaitCondition::UrlMatches(_)
            | WaitCondition::TitleMatches(_)
            | WaitCondition::NavigationComplete) => {
                self.wait_navigation_condition(context_id, condition, timeout)
                    .await
            }
            WaitCondition::NetworkQuiescence { .. } => {
                Err(bidi_wait_unavailable("network quiescence"))
            }
            WaitCondition::RequestObserved { .. } => {
                Err(bidi_wait_unavailable("observed network request"))
            }
            WaitCondition::LogMatches(_) => Err(bidi_wait_unavailable("matching log entry")),
            WaitCondition::ScriptMessage { .. } => {
                Err(bidi_wait_unavailable("script-channel message"))
            }
            WaitCondition::DownloadComplete => {
                Err(bidi_wait_unavailable("download completion event"))
            }
        }
    }
}

fn bidi_wait_unavailable(condition: &str) -> Error {
    Error::CapabilityUnavailable {
        detail: format!("waiting for {condition} requires the normalized BiDi event wait pipeline"),
    }
}
