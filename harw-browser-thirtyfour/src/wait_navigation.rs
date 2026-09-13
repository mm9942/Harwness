use crate::error::{AdapterError, DriverOperation};
use crate::runtime::FirefoxRuntime;
use harw_browser::error::Error;
use harw_browser::ids::BrowserContextId;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use std::time::Instant;
use tokio::time::{Duration, sleep};

impl FirefoxRuntime {
    pub(crate) async fn wait_navigation_condition(
        &self,
        context_id: &BrowserContextId,
        condition: WaitCondition,
        timeout: WaitTimeout,
    ) -> harw_browser::Result<WaitOutcome> {
        if matches!(
            condition,
            WaitCondition::NetworkQuiescence { .. }
                | WaitCondition::RequestObserved { .. }
                | WaitCondition::LogMatches(_)
                | WaitCondition::ScriptMessage { .. }
                | WaitCondition::DownloadComplete
        ) {
            return Err(Error::CapabilityUnavailable {
                detail: "the requested wait requires an active BiDi event stream".to_owned(),
            });
        }
        if !matches!(
            condition,
            WaitCondition::UrlMatches(_)
                | WaitCondition::TitleMatches(_)
                | WaitCondition::NavigationComplete
        ) {
            return Err(Error::InvalidArgument {
                detail: "navigation wait helper received a non-navigation condition".to_owned(),
            });
        }

        let window = self
            .windows()
            .read()
            .await
            .get(context_id)
            .cloned()
            .ok_or_else(|| Error::InvalidArgument {
                detail: format!("browser context '{context_id}' is not registered"),
            })?;
        let started = Instant::now();
        let deadline = started + timeout.duration();
        loop {
            let driver = self.driver().await;
            driver
                .webdriver()
                .switch_to_window(window.clone())
                .await
                .map_err(driver_error)?;
            let satisfied = match &condition {
                WaitCondition::UrlMatches(pattern) => driver
                    .webdriver()
                    .current_url()
                    .await
                    .map_err(driver_error)?
                    .as_str()
                    .contains(pattern),
                WaitCondition::TitleMatches(pattern) => driver
                    .webdriver()
                    .title()
                    .await
                    .map_err(driver_error)?
                    .contains(pattern),
                WaitCondition::NavigationComplete => driver
                    .webdriver()
                    .execute("return document.readyState === 'complete'", Vec::new())
                    .await
                    .map_err(driver_error)?
                    .json()
                    .as_bool()
                    .unwrap_or(false),
                _ => false,
            };
            drop(driver);
            let elapsed = started.elapsed();
            if satisfied || Instant::now() >= deadline {
                return Ok(WaitOutcome::new(
                    satisfied,
                    elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
                    condition,
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            sleep(remaining.min(Duration::from_millis(50))).await;
        }
    }
}

fn driver_error(error: thirtyfour::error::WebDriverError) -> Error {
    AdapterError::Driver {
        operation: DriverOperation::ExecuteCommand,
        detail: error.to_string(),
    }
    .into()
}
