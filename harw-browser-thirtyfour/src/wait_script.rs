use std::sync::Arc;
use std::time::Duration;

use crate::error::{AdapterError, DriverOperation};
use crate::runtime::FirefoxRuntime;
use harw_browser::error::Error as BrowserError;
use harw_browser::ids::BrowserContextId;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use serde_json::Value;
use tokio::time::Instant;

const MAX_PREDICATE_BYTES: usize = 16 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const PREDICATE_SCRIPT: &str = r#"
const predicate = arguments[0];
const evaluatePredicate = new Function('"use strict"; return (' + predicate + ');');
return evaluatePredicate();
"#;

impl FirefoxRuntime {
    pub(crate) async fn wait_for_custom_script(
        &self,
        context_id: &BrowserContextId,
        condition: WaitCondition,
        timeout: WaitTimeout,
    ) -> harw_browser::Result<WaitOutcome> {
        let predicate = match &condition {
            WaitCondition::CustomScript { predicate } => predicate,
            _ => {
                return Err(BrowserError::InvalidArgument {
                    detail: "custom-script wait helper accepts only WaitCondition::CustomScript"
                        .to_owned(),
                });
            }
        };
        validate_predicate(predicate)?;

        let window = self
            .windows()
            .read()
            .await
            .get(context_id)
            .cloned()
            .ok_or_else(|| BrowserError::InvalidArgument {
                detail: format!("browser context '{context_id}' is not registered"),
            })?;
        let arguments: Arc<[Value]> = vec![Value::String(predicate.to_owned())].into();
        let started = Instant::now();
        let deadline = started + timeout.duration();
        let mut satisfied = false;

        while Instant::now() < deadline {
            let attempt = tokio::time::timeout_at(deadline, async {
                let driver = self.driver().await;
                driver
                    .webdriver()
                    .switch_to_window(window.clone())
                    .await
                    .map_err(driver_error)?;
                let returned = driver
                    .webdriver()
                    .execute(PREDICATE_SCRIPT, Arc::clone(&arguments))
                    .await
                    .map_err(driver_error)?;
                Ok::<bool, BrowserError>(returned.json() == &Value::Bool(true))
            })
            .await;

            match attempt {
                Ok(Ok(true)) => {
                    satisfied = true;
                    break;
                }
                Ok(Ok(false)) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => break,
            }

            let next_poll = Instant::now() + POLL_INTERVAL;
            tokio::time::sleep_until(next_poll.min(deadline)).await;
        }

        Ok(WaitOutcome::new(
            satisfied,
            elapsed_millis(started.elapsed()),
            condition,
        ))
    }
}

fn validate_predicate(predicate: &str) -> harw_browser::Result<()> {
    if predicate.trim().is_empty() {
        return Err(BrowserError::InvalidArgument {
            detail: "custom-script predicate must not be empty".to_owned(),
        });
    }
    if predicate.len() > MAX_PREDICATE_BYTES {
        return Err(BrowserError::InvalidArgument {
            detail: format!("custom-script predicate exceeds the {MAX_PREDICATE_BYTES}-byte limit"),
        });
    }
    Ok(())
}

fn elapsed_millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn driver_error(error: thirtyfour::error::WebDriverError) -> BrowserError {
    AdapterError::Driver {
        operation: DriverOperation::ExecuteCommand,
        detail: error.to_string(),
    }
    .into()
}
