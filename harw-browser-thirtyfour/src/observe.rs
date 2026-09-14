use harw_browser::error::{Error, Result};
use harw_browser::ids::{BrowserContextId, BrowserObservationRevision};
use harw_browser::observation::{
    BoundingBox, BrowserObservation, DocumentIdentity, ObservationMode, ObservedElement,
};
use harw_browser::selector::Target;
use thirtyfour::error::{WebDriverError, WebDriverErrorInner};
use thirtyfour::{By, WebElement, WindowHandle};

use crate::runtime::FirefoxRuntime;

const MAX_OBSERVED_ELEMENTS: usize = 128;

pub(crate) async fn observe(
    runtime: &FirefoxRuntime,
    context_id: &BrowserContextId,
    mode: ObservationMode,
) -> Result<BrowserObservation> {
    let window = resolve_window(runtime, context_id).await?;
    let revision = current_revision(runtime, context_id).await?;
    let driver = runtime.driver().await;
    let webdriver = driver.webdriver();
    webdriver
        .switch_to_window(window)
        .await
        .map_err(|error| map_driver_error("switch browsing context", error))?;

    let url = webdriver
        .current_url()
        .await
        .map_err(|error| map_driver_error("read current URL", error))?;
    let title = webdriver
        .title()
        .await
        .map_err(|error| map_driver_error("read document title", error))?;
    let elements = collect_elements(webdriver, mode).await?;
    let event_cursor = runtime.last_event_cursor().await;

    Ok(BrowserObservation {
        session_id: runtime.session_id(),
        context_id: *context_id,
        revision,
        document_identity: DocumentIdentity::new(format!(
            "{}#harw-revision-{}",
            url.as_str(),
            revision.value()
        )),
        url,
        title,
        elements,
        // This adapter has no artifact store yet. Claiming screenshot or text
        // references without durable bytes would be dishonest.
        artifacts: Vec::new(),
        event_cursor,
    })
}

pub(crate) async fn find(
    runtime: &FirefoxRuntime,
    context_id: &BrowserContextId,
    target: &Target,
    expected_revision: BrowserObservationRevision,
) -> Result<ObservedElement> {
    let window = resolve_window(runtime, context_id).await?;
    enforce_revision(runtime, context_id, expected_revision).await?;
    let candidates = crate::selector::target_candidates(target)?;
    let driver = runtime.driver().await;
    let webdriver = driver.webdriver();
    webdriver
        .switch_to_window(window)
        .await
        .map_err(|error| map_driver_error("switch browsing context", error))?;

    for candidate in candidates {
        match webdriver.find(candidate).await {
            Ok(element) => return observed_element(element).await,
            Err(error) if matches!(&*error, WebDriverErrorInner::NoSuchElement(_)) => continue,
            Err(error) => return Err(map_driver_error("resolve element selector", error)),
        }
    }

    Err(Error::SelectorNotFound {
        detail: "no target candidate matched in the requested browsing context".to_owned(),
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

async fn current_revision(
    runtime: &FirefoxRuntime,
    context_id: &BrowserContextId,
) -> Result<BrowserObservationRevision> {
    runtime
        .revisions()
        .read()
        .await
        .get(context_id)
        .copied()
        .ok_or_else(|| Error::InvalidArgument {
            detail: format!("browser context '{context_id}' has no observation revision"),
        })
}

async fn enforce_revision(
    runtime: &FirefoxRuntime,
    context_id: &BrowserContextId,
    expected: BrowserObservationRevision,
) -> Result<()> {
    let current = current_revision(runtime, context_id).await?;
    if current != expected {
        return Err(Error::StaleRevision { expected, current });
    }
    Ok(())
}

async fn collect_elements(
    webdriver: &thirtyfour::WebDriver,
    mode: ObservationMode,
) -> Result<Vec<ObservedElement>> {
    let selectors = match mode {
        ObservationMode::PageSummary
        | ObservationMode::NetworkActivitySummary
        | ObservationMode::ConsoleLogSummary
        | ObservationMode::Screenshot => Vec::new(),
        ObservationMode::InteractiveElements | ObservationMode::CombinedDiagnostic => {
            vec![By::Css(
                "a,button,input,select,textarea,summary,[role=button],[role=link],[tabindex]",
            )]
        }
        ObservationMode::DomSelection { selector } => {
            crate::selector::target_candidates(&Target::new(selector))?
        }
        ObservationMode::TextExtraction => vec![By::Tag("body")],
        ObservationMode::FormsAndLinks => vec![By::Css("form,a")],
    };

    let mut observed = Vec::new();
    for selector in selectors {
        let elements = webdriver
            .find_all(selector)
            .await
            .map_err(|error| map_driver_error("collect observed elements", error))?;
        for element in elements {
            if observed.len() == MAX_OBSERVED_ELEMENTS {
                return Ok(observed);
            }
            observed.push(observed_element(element).await?);
        }
    }
    Ok(observed)
}

async fn observed_element(element: WebElement) -> Result<ObservedElement> {
    let element_ref = element.element_id().to_string();
    let tag = element
        .tag_name()
        .await
        .map_err(|error| map_driver_error("read observed element tag", error))?;
    let mut observed = ObservedElement::new(element_ref, tag);
    observed.role = element
        .attr("role")
        .await
        .map_err(|error| map_driver_error("read observed element role", error))?;
    observed.accessible_name = element
        .attr("aria-label")
        .await
        .map_err(|error| map_driver_error("read observed element accessible name", error))?;
    let text = element
        .text()
        .await
        .map_err(|error| map_driver_error("read observed element text", error))?;
    if !text.is_empty() {
        observed.text = Some(text);
    }
    for name in ["id", "name", "type", "href", "data-testid"] {
        if let Some(value) = element
            .attr(name)
            .await
            .map_err(|error| map_driver_error("read observed element attribute", error))?
        {
            observed.attributes.insert(name.to_owned(), value);
        }
    }
    let rect = element
        .rect()
        .await
        .map_err(|error| map_driver_error("read observed element bounds", error))?;
    observed.bounding_box = Some(BoundingBox {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    });
    Ok(observed)
}

fn map_driver_error(operation: &str, error: WebDriverError) -> Error {
    match &*error {
        WebDriverErrorInner::NoSuchElement(_) => Error::SelectorNotFound {
            detail: format!("{operation}: {error}"),
        },
        WebDriverErrorInner::InvalidSelector(_) => Error::InvalidArgument {
            detail: format!("{operation}: {error}"),
        },
        WebDriverErrorInner::Timeout(_)
        | WebDriverErrorInner::WebDriverTimeout(_)
        | WebDriverErrorInner::ScriptTimeout(_) => Error::Timeout {
            detail: format!("{operation}: {error}"),
        },
        WebDriverErrorInner::InvalidSessionId(_) | WebDriverErrorInner::NoSuchWindow(_) => {
            Error::CapabilityUnavailable {
                detail: format!(
                    "Firefox WebDriver lost the active session while attempting to {operation}: {error}"
                ),
            }
        }
        WebDriverErrorInner::StaleElementReference(_) => Error::SelectorNotFound {
            detail: format!("{operation}: element became stale: {error}"),
        },
        _ => Error::CapabilityUnavailable {
            detail: format!("Firefox WebDriver could not {operation}: {error}"),
        },
    }
}
