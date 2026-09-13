use crate::error::{AdapterError, DriverOperation};
use crate::runtime::FirefoxRuntime;
use crate::selector::target_candidates;
use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::error::Error as BrowserError;
use harw_browser::ids::EffectId;

const REQUEST_SUBMIT_SCRIPT: &str = r#"
const target = arguments[0];
const form = target instanceof HTMLFormElement ? target : target.closest("form");
if (form === null) {
    throw new Error("submit target is not a form and has no ancestor form");
}
form.requestSubmit();
"#;

impl FirefoxRuntime {
    pub(crate) async fn execute_submit_action(
        &self,
        request: ActionRequest,
    ) -> harw_browser::Result<ActionOutcome> {
        let current = self
            .revisions()
            .read()
            .await
            .get(&request.context_id)
            .copied()
            .ok_or_else(|| BrowserError::InvalidArgument {
                detail: format!(
                    "browser context '{}' has no revision state",
                    request.context_id
                ),
            })?;
        if let Some(expected) = request.expected_revision {
            if expected != current {
                return Err(BrowserError::StaleRevision { expected, current });
            }
        }

        let target = match &request.action {
            BrowserAction::Submit { target } => target,
            _ => {
                return Err(BrowserError::InvalidArgument {
                    detail: "submit helper accepts only BrowserAction::Submit".to_owned(),
                });
            }
        };
        let candidates = target_candidates(target)?;
        let window = self
            .windows()
            .read()
            .await
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| BrowserError::InvalidArgument {
                detail: format!("browser context '{}' is not registered", request.context_id),
            })?;

        let driver = self.driver().await;
        driver
            .webdriver()
            .switch_to_window(window)
            .await
            .map_err(driver_error)?;

        let mut resolved = None;
        for candidate in candidates {
            if let Ok(element) = driver.webdriver().find(candidate).await {
                resolved = Some(element);
                break;
            }
        }
        let element = resolved.ok_or_else(|| BrowserError::SelectorNotFound {
            detail: "no selector candidate matched submit target".to_owned(),
        })?;
        let element_argument = element.to_json().map_err(driver_error)?;
        driver
            .webdriver()
            .execute(REQUEST_SUBMIT_SCRIPT, vec![element_argument])
            .await
            .map_err(driver_error)?;
        drop(driver);

        let revision = {
            let mut revisions = self.revisions().write().await;
            let revision = revisions.get_mut(&request.context_id).ok_or_else(|| {
                BrowserError::InvalidArgument {
                    detail: format!(
                        "browser context '{}' has no revision state",
                        request.context_id
                    ),
                }
            })?;
            *revision = revision.next();
            *revision
        };

        let mut outcome = ActionOutcome::new(EffectId::new());
        outcome.new_revision = Some(revision);
        outcome.confirmed = true;
        Ok(outcome)
    }
}

fn driver_error(error: thirtyfour::error::WebDriverError) -> BrowserError {
    AdapterError::Driver {
        operation: DriverOperation::ExecuteCommand,
        detail: error.to_string(),
    }
    .into()
}
