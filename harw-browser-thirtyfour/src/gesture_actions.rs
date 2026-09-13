use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::error::{Error, Result};
use harw_browser::ids::{BrowserContextId, BrowserObservationRevision, EffectId};
use harw_browser::selector::Target;
use thirtyfour::error::{WebDriverError, WebDriverErrorInner};
use thirtyfour::{WebDriver, WebElement, WindowHandle};

use crate::runtime::FirefoxRuntime;

/// Executes the pointer, scrolling, keyboard, and drag subset of browser actions.
///
/// Other action families deliberately remain outside this module. The caller
/// receives `InvalidArgument` if it routes a non-gesture action here.
pub(crate) async fn execute_gesture_action(
    runtime: &FirefoxRuntime,
    request: ActionRequest,
) -> Result<ActionOutcome> {
    let context_id = request.context_id;
    let current_revision = enforce_revision(
        runtime,
        &context_id,
        request.expected_revision,
        action_requires_revision(&request.action),
    )
    .await?;
    let window = resolve_window(runtime, &context_id).await?;
    let driver = runtime.driver().await;
    let webdriver = driver.webdriver();
    webdriver
        .switch_to_window(window)
        .await
        .map_err(|error| map_driver_error("switch browsing context", error))?;

    let state_changed = match request.action {
        BrowserAction::Hover { target } => {
            let element = resolve_target(webdriver, &target).await?;
            webdriver
                .action_chain()
                .move_to_element_center(&element)
                .perform()
                .await
                .map_err(|error| map_driver_error("hover over element", error))?;
            true
        }
        BrowserAction::Scroll { target, x, y } => {
            if x == 0 && y == 0 {
                false
            } else {
                scroll(webdriver, target.as_ref(), x, y).await?;
                true
            }
        }
        BrowserAction::KeyPress { target, key } => {
            if key.is_empty() {
                return Err(Error::InvalidArgument {
                    detail: "keypress value must not be empty".to_owned(),
                });
            }
            if let Some(target) = target.as_ref() {
                resolve_target(webdriver, target)
                    .await?
                    .send_keys(key)
                    .await
                    .map_err(|error| map_driver_error("send keypress to element", error))?;
            } else {
                webdriver
                    .action_chain()
                    .send_keys(key)
                    .perform()
                    .await
                    .map_err(|error| map_driver_error("send keypress to active document", error))?;
            }
            true
        }
        BrowserAction::Drag {
            source,
            destination,
        } => {
            let source = resolve_target(webdriver, &source).await?;
            let destination = resolve_target(webdriver, &destination).await?;
            webdriver
                .action_chain()
                .drag_and_drop_element(&source, &destination)
                .perform()
                .await
                .map_err(|error| map_driver_error("drag element", error))?;
            true
        }
        _ => {
            return Err(Error::InvalidArgument {
                detail: "action is not a hover, scroll, keypress, or drag operation".to_owned(),
            });
        }
    };

    let resulting_revision = if state_changed {
        let next = current_revision.next();
        runtime.revisions().write().await.insert(context_id, next);
        next
    } else {
        current_revision
    };
    let mut outcome = ActionOutcome::new(EffectId::new());
    outcome.confirmed = true;
    outcome.new_revision = Some(resulting_revision);
    Ok(outcome)
}

async fn scroll(webdriver: &WebDriver, target: Option<&Target>, x: i32, y: i32) -> Result<()> {
    let x = serde_json::json!(x);
    let y = serde_json::json!(y);
    match target {
        Some(target) => {
            let element = resolve_target(webdriver, target).await?;
            let element = element
                .to_json()
                .map_err(|error| map_driver_error("serialize scroll target", error))?;
            webdriver
                .execute(
                    "arguments[0].scrollBy(arguments[1], arguments[2]);",
                    vec![element, x, y],
                )
                .await
                .map_err(|error| map_driver_error("scroll element", error))?;
        }
        None => {
            webdriver
                .execute("window.scrollBy(arguments[0], arguments[1]);", vec![x, y])
                .await
                .map_err(|error| map_driver_error("scroll document", error))?;
        }
    }
    Ok(())
}

async fn resolve_target(webdriver: &WebDriver, target: &Target) -> Result<WebElement> {
    for candidate in crate::selector::target_candidates(target)? {
        match webdriver.find(candidate).await {
            Ok(element) => return Ok(element),
            Err(error) if matches!(&*error, WebDriverErrorInner::NoSuchElement(_)) => continue,
            Err(error) => return Err(map_driver_error("resolve gesture target", error)),
        }
    }
    Err(Error::SelectorNotFound {
        detail: "no gesture target candidate matched in the browsing context".to_owned(),
    })
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

async fn enforce_revision(
    runtime: &FirefoxRuntime,
    context_id: &BrowserContextId,
    expected: Option<BrowserObservationRevision>,
    required: bool,
) -> Result<BrowserObservationRevision> {
    let current = runtime
        .revisions()
        .read()
        .await
        .get(context_id)
        .copied()
        .ok_or_else(|| Error::InvalidArgument {
            detail: format!("browser context '{context_id}' has no observation revision"),
        })?;
    let Some(expected) = expected else {
        if required {
            return Err(Error::InvalidArgument {
                detail: "element-targeted gesture requires an expected observation revision"
                    .to_owned(),
            });
        }
        return Ok(current);
    };
    if expected != current {
        return Err(Error::StaleRevision { expected, current });
    }
    Ok(current)
}

fn action_requires_revision(action: &BrowserAction) -> bool {
    match action {
        BrowserAction::Hover { .. } | BrowserAction::Drag { .. } => true,
        BrowserAction::Scroll { target, .. } | BrowserAction::KeyPress { target, .. } => {
            target.is_some()
        }
        _ => false,
    }
}

fn map_driver_error(operation: &str, error: WebDriverError) -> Error {
    match &*error {
        WebDriverErrorInner::NoSuchElement(_) | WebDriverErrorInner::StaleElementReference(_) => {
            Error::SelectorNotFound {
                detail: format!("{operation}: {error}"),
            }
        }
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
        _ => Error::CapabilityUnavailable {
            detail: format!("Firefox WebDriver could not {operation}: {error}"),
        },
    }
}
