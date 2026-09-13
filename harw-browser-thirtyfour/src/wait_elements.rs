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
pub(crate) async fn wait_for_element(
    runtime: &FirefoxRuntime,
    context_id: &BrowserContextId,
    condition: WaitCondition,
    timeout: WaitTimeout,
) -> Result<WaitOutcome> {
    let predicate = ElementPredicate::from_condition(&condition)?;
    let window = resolve_window(runtime, context_id).await?;
    let driver = runtime.driver().await;
    let webdriver = driver.webdriver();
    webdriver
        .switch_to_window(window)
        .await
        .map_err(|error| map_driver_error("switch browsing context", error))?;

    let started = Instant::now();
    let deadline = started + timeout.duration();
    loop {
        if predicate.is_satisfied(webdriver).await? {
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
