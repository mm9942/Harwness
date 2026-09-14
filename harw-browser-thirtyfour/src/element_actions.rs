//! Click/Type/Clear/Focus/Select execution. File upload is intentionally not supported.

use crate::error::{AdapterError, DriverOperation};
use crate::runtime::FirefoxRuntime;
use crate::selector::target_candidates;
use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::error::Error;
use harw_browser::ids::EffectId;
use harw_browser::selector::Target;
use thirtyfour::components::SelectElement;

impl FirefoxRuntime {
    pub(crate) async fn execute_element_action(
        &self,
        request: ActionRequest,
    ) -> harw_browser::Result<ActionOutcome> {
        let current = self
            .revisions()
            .read()
            .await
            .get(&request.context_id)
            .copied()
            .ok_or_else(|| Error::InvalidArgument {
                detail: "unknown browser context".to_owned(),
            })?;
        if let Some(expected) = request.expected_revision {
            if expected != current {
                return Err(Error::StaleRevision { expected, current });
            }
        }
        let target = action_target(&request.action)?;
        let candidates = target_candidates(target)?;
        let window = self
            .windows()
            .read()
            .await
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| Error::InvalidArgument {
                detail: "unknown browser context".to_owned(),
            })?;
        let driver = self.driver().await;
        driver
            .webdriver()
            .switch_to_window(window)
            .await
            .map_err(driver_error)?;
        let mut found = None;
        for by in candidates {
            if let Ok(element) = driver.webdriver().find(by).await {
                found = Some(element);
                break;
            }
        }
        let element = found.ok_or_else(|| Error::SelectorNotFound {
            detail: "no selector candidate matched".to_owned(),
        })?;
        match request.action {
            BrowserAction::Click { .. } => element.click().await,
            BrowserAction::Type { text, .. } => element.send_keys(text).await,
            BrowserAction::Clear { .. } => element.clear().await,
            BrowserAction::Focus { .. } => element.focus().await,
            BrowserAction::Select { value, .. } => {
                SelectElement::new(&element)
                    .await
                    .map_err(driver_error)?
                    .select_by_value(&value)
                    .await
            }
            _ => {
                return Err(Error::InvalidArgument {
                    detail: "unsupported element action".to_owned(),
                });
            }
        }
        .map_err(driver_error)?;
        drop(driver);
        let mut revisions = self.revisions().write().await;
        let revision =
            revisions
                .get_mut(&request.context_id)
                .ok_or_else(|| Error::InvalidArgument {
                    detail: "unknown browser context".to_owned(),
                })?;
        *revision = revision.next();
        let mut outcome = ActionOutcome::new(EffectId::new());
        outcome.new_revision = Some(*revision);
        outcome.confirmed = true;
        Ok(outcome)
    }
}

fn action_target(action: &BrowserAction) -> harw_browser::Result<&Target> {
    match action {
        BrowserAction::Click { target }
        | BrowserAction::Type { target, .. }
        | BrowserAction::Clear { target }
        | BrowserAction::Focus { target }
        | BrowserAction::Select { target, .. } => Ok(target),
        _ => Err(Error::InvalidArgument {
            detail: "unsupported element action".to_owned(),
        }),
    }
}

fn driver_error(error: thirtyfour::error::WebDriverError) -> Error {
    AdapterError::Driver {
        operation: DriverOperation::ExecuteCommand,
        detail: error.to_string(),
    }
    .into()
}
