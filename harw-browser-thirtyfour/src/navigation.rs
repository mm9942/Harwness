//! Navigate/Back/Forward/Reload execution with the pre-navigation origin check.

use crate::error::{AdapterError, DriverOperation};
use crate::runtime::FirefoxRuntime;
use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::error::Error as BrowserError;
use harw_browser::ids::{BrowserContextId, BrowserObservationRevision, EffectId};

impl FirefoxRuntime {
    pub(crate) async fn execute_navigation(
        &self,
        request: ActionRequest,
    ) -> harw_browser::Result<ActionOutcome> {
        self.require_revision(request.context_id, request.expected_revision)
            .await?;

        // Pre-navigation check; the observed location after the effect is
        // enforced by the runtime (F-009).
        if let BrowserAction::Navigate { url } = &request.action {
            self.request().check_navigation_target(url)?;
        }

        let window = {
            let windows = self.windows().read().await;
            windows.get(&request.context_id).cloned().ok_or_else(|| {
                BrowserError::InvalidArgument {
                    detail: format!("browser context '{}' is not registered", request.context_id),
                }
            })?
        };

        let driver = self.driver().await;
        driver
            .webdriver()
            .switch_to_window(window)
            .await
            .map_err(|error| driver_error(DriverOperation::ExecuteCommand, error))?;
        match request.action {
            BrowserAction::Navigate { url } => driver
                .webdriver()
                .goto(url.as_str())
                .await
                .map_err(|error| driver_error(DriverOperation::ExecuteCommand, error))?,
            BrowserAction::Back => driver
                .webdriver()
                .back()
                .await
                .map_err(|error| driver_error(DriverOperation::ExecuteCommand, error))?,
            BrowserAction::Forward => driver
                .webdriver()
                .forward()
                .await
                .map_err(|error| driver_error(DriverOperation::ExecuteCommand, error))?,
            BrowserAction::Reload => driver
                .webdriver()
                .refresh()
                .await
                .map_err(|error| driver_error(DriverOperation::ExecuteCommand, error))?,
            _ => {
                return Err(BrowserError::InvalidArgument {
                    detail: "navigation helper accepts only Navigate, Back, Forward, or Reload"
                        .to_owned(),
                });
            }
        }
        drop(driver);

        let revision = self.advance_revision(request.context_id).await?;
        let mut outcome = ActionOutcome::new(EffectId::new());
        outcome.new_revision = Some(revision);
        outcome.confirmed = true;
        Ok(outcome)
    }

    async fn require_revision(
        &self,
        context_id: BrowserContextId,
        expected: Option<BrowserObservationRevision>,
    ) -> harw_browser::Result<()> {
        let revisions = self.revisions().read().await;
        let current =
            revisions
                .get(&context_id)
                .copied()
                .ok_or_else(|| BrowserError::InvalidArgument {
                    detail: format!("browser context '{context_id}' has no revision state"),
                })?;
        if let Some(expected) = expected {
            if expected != current {
                return Err(BrowserError::StaleRevision { expected, current });
            }
        }
        Ok(())
    }

    async fn advance_revision(
        &self,
        context_id: BrowserContextId,
    ) -> harw_browser::Result<BrowserObservationRevision> {
        let mut revisions = self.revisions().write().await;
        let current =
            revisions
                .get_mut(&context_id)
                .ok_or_else(|| BrowserError::InvalidArgument {
                    detail: format!("browser context '{context_id}' has no revision state"),
                })?;
        *current = current.next();
        Ok(*current)
    }
}

fn driver_error(
    operation: DriverOperation,
    error: thirtyfour::error::WebDriverError,
) -> BrowserError {
    AdapterError::Driver {
        operation,
        detail: error.to_string(),
    }
    .into()
}
