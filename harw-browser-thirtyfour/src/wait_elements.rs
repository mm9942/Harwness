use std::time::Duration;

use harw_browser::error::{Error, Result};
use harw_browser::ids::BrowserContextId;
use harw_browser::selector::Target;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use thirtyfour::error::{WebDriverError, WebDriverErrorInner};
use thirtyfour::{WebDriver, WebElement, WindowHandle};
use tokio::time::{Instant, sleep};

use crate::runtime::FirefoxRuntime;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Waits for one of the four element-state conditions supported by this module.
///
/// The driver mutex is taken fresh for every poll (switch window, evaluate the
/// predicate, drop) and released before each `sleep`, matching
/// [`crate::wait_navigation`]'s `wait_navigation_condition`. A wait can run
/// for up to the maximum wait timeout, so holding the mutex across the whole
/// loop would block `close_runtime` and every other session operation for
/// that long; each iteration also re-checks `ensure_open` so a session that
/// closes mid-wait ends the wait immediately instead of polling a dead
/// driver.
pub(crate) async fn wait_for_element(
    runtime: &FirefoxRuntime,
    context_id: &BrowserContextId,
    condition: WaitCondition,
    timeout: WaitTimeout,
) -> Result<WaitOutcome> {
    let predicate = ElementPredicate::from_condition(&condition)?;
    let window = resolve_window(runtime, context_id).await?;

    let started = Instant::now();
    let deadline = started + timeout.duration();
    loop {
        runtime.ensure_open()?;

        let driver = runtime.driver().await;
        driver
            .webdriver()
            .switch_to_window(window.clone())
            .await
            .map_err(|error| map_driver_error("switch browsing context", error))?;
        let satisfied = predicate.is_satisfied(driver.webdriver()).await?;
        drop(driver);

        if satisfied {
            return Ok(WaitOutcome::new(true, elapsed_millis(started), condition));
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(WaitOutcome::new(false, elapsed_millis(started), condition));
        }
        sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now))).await;
    }
}

enum ElementPredicate<'a> {
    Present(&'a Target),
    Visible(&'a Target),
    Clickable(&'a Target),
    Gone(&'a Target),
}

impl<'a> ElementPredicate<'a> {
    fn from_condition(condition: &'a WaitCondition) -> Result<Self> {
        match condition {
            WaitCondition::ElementPresent(target) => Ok(Self::Present(target)),
            WaitCondition::ElementVisible(target) => Ok(Self::Visible(target)),
            WaitCondition::ElementClickable(target) => Ok(Self::Clickable(target)),
            WaitCondition::ElementGone(target) => Ok(Self::Gone(target)),
            _ => Err(Error::InvalidArgument {
                detail: "wait_elements only accepts element present, visible, clickable, or gone"
                    .to_owned(),
            }),
        }
    }

    async fn is_satisfied(&self, webdriver: &WebDriver) -> Result<bool> {
        match self {
            Self::Present(target) => Ok(find_first(webdriver, target).await?.is_some()),
            Self::Visible(target) => match find_first(webdriver, target).await? {
                Some(element) => element
                    .is_displayed()
                    .await
                    .map_err(|error| map_driver_error("read element visibility", error)),
                None => Ok(false),
            },
            Self::Clickable(target) => match find_first(webdriver, target).await? {
                Some(element) => {
                    let displayed = element
                        .is_displayed()
                        .await
                        .map_err(|error| map_driver_error("read element visibility", error))?;
                    if !displayed {
                        return Ok(false);
                    }
                    element
                        .is_enabled()
                        .await
                        .map_err(|error| map_driver_error("read element enabled state", error))
                }
                None => Ok(false),
            },
            Self::Gone(target) => Ok(find_first(webdriver, target).await?.is_none()),
        }
    }
}

async fn find_first(webdriver: &WebDriver, target: &Target) -> Result<Option<WebElement>> {
    for candidate in crate::selector::target_candidates(target)? {
        match webdriver.find(candidate).await {
            Ok(element) => return Ok(Some(element)),
            Err(error) if matches!(&*error, WebDriverErrorInner::NoSuchElement(_)) => continue,
            Err(error) => return Err(map_driver_error("query element wait target", error)),
        }
    }
    Ok(None)
}

async fn resolve_window(
    runtime: &FirefoxRuntime,
    context_id: &BrowserContextId,
) -> Result<WindowHandle> {
    runtime
        .windows()
        .read()
        .await
        .get(context_id)
        .cloned()
        .ok_or_else(|| Error::InvalidArgument {
            detail: format!("browser context '{context_id}' is not registered in this session"),
        })
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn map_driver_error(operation: &str, error: WebDriverError) -> Error {
    match &*error {
        WebDriverErrorInner::InvalidArgument(_) | WebDriverErrorInner::InvalidSelector(_) => {
            Error::InvalidArgument {
                detail: format!("{operation}: {error}"),
            }
        }
        WebDriverErrorInner::Timeout(_)
        | WebDriverErrorInner::WebDriverTimeout(_)
        | WebDriverErrorInner::ScriptTimeout(_) => Error::Timeout {
            detail: format!("{operation}: {error}"),
        },
        WebDriverErrorInner::StaleElementReference(_) => Error::SelectorNotFound {
            detail: format!("{operation}: element became stale: {error}"),
        },
        _ => Error::CapabilityUnavailable {
            detail: format!("Firefox WebDriver could not {operation}: {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_browser::selector::Selector;
    use thirtyfour::error::WebDriverErrorInfo;

    // `wait_for_element` itself needs a live driver session (a real
    // `FirefoxRuntime` cannot be constructed without one) and is exercised by
    // the crate's integration tests instead; the cases below cover the pure
    // logic that runs without a driver: condition routing and error mapping.

    fn target() -> Target {
        Target::new(Selector::Css("#login".to_owned()))
    }

    #[test]
    fn test_from_condition_accepts_all_four_element_conditions() -> TestResult {
        let present = WaitCondition::ElementPresent(target());
        assert!(matches!(
            ElementPredicate::from_condition(&present),
            Ok(ElementPredicate::Present(_))
        ));

        let visible = WaitCondition::ElementVisible(target());
        assert!(matches!(
            ElementPredicate::from_condition(&visible),
            Ok(ElementPredicate::Visible(_))
        ));

        let clickable = WaitCondition::ElementClickable(target());
        assert!(matches!(
            ElementPredicate::from_condition(&clickable),
            Ok(ElementPredicate::Clickable(_))
        ));

        let gone = WaitCondition::ElementGone(target());
        assert!(matches!(
            ElementPredicate::from_condition(&gone),
            Ok(ElementPredicate::Gone(_))
        ));
        Ok(())
    }

    #[test]
    fn test_from_condition_rejects_non_element_conditions() -> TestResult {
        let condition = WaitCondition::NavigationComplete;
        match ElementPredicate::from_condition(&condition) {
            Err(Error::InvalidArgument { .. }) => Ok(()),
            Ok(_) => Err(TestError::Unexpected(
                "expected InvalidArgument, got Ok".to_owned(),
            )),
            Err(other) => Err(TestError::Unexpected(format!(
                "expected InvalidArgument, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_map_driver_error_invalid_argument_and_selector_stay_invalid_argument() -> TestResult {
        for error in [
            WebDriverError::InvalidArgument(WebDriverErrorInfo::new("bad arg".to_owned())),
            WebDriverError::InvalidSelector(WebDriverErrorInfo::new("bad selector".to_owned())),
        ] {
            match map_driver_error("query element wait target", error) {
                Error::InvalidArgument { .. } => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected InvalidArgument, got {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_map_driver_error_timeouts_become_timeout() -> TestResult {
        for error in [
            WebDriverError::Timeout("slow".to_owned()),
            WebDriverError::WebDriverTimeout(WebDriverErrorInfo::new("slow".to_owned())),
            WebDriverError::ScriptTimeout(WebDriverErrorInfo::new("slow".to_owned())),
        ] {
            match map_driver_error("evaluate predicate", error) {
                Error::Timeout { .. } => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected Timeout, got {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_map_driver_error_stale_element_becomes_selector_not_found() -> TestResult {
        let error = WebDriverError::StaleElementReference(WebDriverErrorInfo::new(
            "element vanished".to_owned(),
        ));
        match map_driver_error("read element visibility", error) {
            Error::SelectorNotFound { .. } => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected SelectorNotFound, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_map_driver_error_falls_back_to_capability_unavailable() -> TestResult {
        let error = WebDriverError::NoSuchWindow(WebDriverErrorInfo::new("gone".to_owned()));
        match map_driver_error("switch browsing context", error) {
            Error::CapabilityUnavailable { .. } => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected CapabilityUnavailable, got {other:?}"
            ))),
        }
    }
}
