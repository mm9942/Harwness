//! Pure, host-free preparation of browser tool calls.
//!
//! # Responsibility
//! Converts a model [`BrowserToolRequest`] into a sealed [`PreparedBrowserCall`]:
//! `browser.open` is authorized against the host-owned [`BrowserOpenPolicy`]
//! (the model's [`crate::OpenRequest`] never reaches dispatch), every other
//! request is validated against [`BrowserLimits`], and the authority scope is
//! derived. [`validate_request`] is shared with dispatch, which repeats it with
//! the limits of the targeted session.
//!
//! # Concurrency
//! Pure functions; safe from any thread.
//!
//! # Errors
//! [`harw_browser::error::Error::InvalidArgument`] for limit violations and
//! missing revisions, `OriginNotAllowed` for unauthorized opens.

use std::sync::OnceLock;

use harw_browser::action::{BrowserAction, validate_selector, validate_target};
use harw_browser::error::{Error, Result};
use harw_browser::observation::ObservationMode;
use harw_browser::policy::BrowserLimits;

use crate::harness_provider::browser_tool_parameters;
use crate::types::{
    BrowserOpenPolicy, BrowserResourceScope, BrowserScopeAction, BrowserToolDescriptor,
    BrowserToolRequest, PreparedBrowserCall, PreparedBrowserRequest,
};

/// Stable tool names and capabilities in presentation order.
pub(crate) const BROWSER_TOOLS: [(&str, &str); 7] = [
    ("browser.open", "harwness.browser.open@1"),
    ("browser.observe", "harwness.browser.observe@1"),
    ("browser.find", "harwness.browser.find@1"),
    ("browser.act", "harwness.browser.act@1"),
    ("browser.wait", "harwness.browser.wait@1"),
    ("browser.events", "harwness.browser.events@1"),
    ("browser.close", "harwness.browser.close@1"),
];

/// Returns the stable, compact model-facing browser surface in presentation order.
///
/// # Description
/// `input_schema` is the serialized form of the exact schema the harness
/// provider advertises (one source, no divergence; F-115 schema follow-up).
///
/// # Examples
/// ```rust,no_run
/// let names: Vec<&str> = harw_tool_browser::browser_tool_descriptors()
///     .iter()
///     .map(|descriptor| descriptor.name.as_str())
///     .collect();
/// assert_eq!(names[0], "browser.open");
/// ```
pub fn browser_tool_descriptors() -> &'static [BrowserToolDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<BrowserToolDescriptor>> = OnceLock::new();
    DESCRIPTORS.get_or_init(|| {
        BROWSER_TOOLS
            .into_iter()
            .map(|(name, capability)| BrowserToolDescriptor {
                name: name.to_owned(),
                capability: capability.to_owned(),
                input_schema: schema_value(name),
            })
            .collect()
    })
}

// Serializes the provider schema. `JsonSchema` holds only strings, maps and
// JSON values, so serialization cannot fail in practice; should it, the
// fallback accepts no properties at all (fail-closed) and is logged.
fn schema_value(name: &str) -> serde_json::Value {
    match serde_json::to_value(browser_tool_parameters(name)) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(tool = name, %error, "browser tool schema failed to serialize");
            serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
        }
    }
}

/// Validates and freezes a non-open typed request with default limits.
///
/// # Description
/// Performs no host lookup and no browser I/O. `browser.open` is rejected
/// here: it requires a host-owned [`BrowserOpenPolicy`] and must be prepared
/// through [`crate::BrowserToolSet::prepare`]. Dispatch re-validates with the
/// limits of the targeted session, so default limits here never widen a grant.
///
/// # Errors
/// - `Error::InvalidArgument`: open without policy, limit violation, missing
///   expected revision on an element-targeted action.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::ids::BrowserSessionId;
/// use harw_tool_browser::{BrowserToolRequest, CloseRequest, prepare_browser_call};
///
/// let prepared = prepare_browser_call(BrowserToolRequest::Close(CloseRequest {
///     session_id: BrowserSessionId::new(),
/// }));
/// assert!(prepared.is_ok());
/// ```
pub fn prepare_browser_call(request: BrowserToolRequest) -> Result<PreparedBrowserCall> {
    prepare_browser_call_with_policy(request, None)
}

/// Prepares a call; `open_policy` supplies open authority and the limits.
pub(crate) fn prepare_browser_call_with_policy(
    request: BrowserToolRequest,
    open_policy: Option<&BrowserOpenPolicy>,
) -> Result<PreparedBrowserCall> {
    let limits = open_policy
        .and_then(BrowserOpenPolicy::configured_grant)
        .map_or_else(BrowserLimits::default, |grant| *grant.limits());

    let prepared = match request {
        BrowserToolRequest::Open(open) => {
            let Some(open_policy) = open_policy else {
                return Err(Error::InvalidArgument {
                    detail: "browser.open requires a host-owned BrowserOpenPolicy".to_owned(),
                });
            };
            PreparedBrowserRequest::Open(open_policy.authorize(&open)?)
        }
        BrowserToolRequest::Observe(observe) => PreparedBrowserRequest::Observe(observe),
        BrowserToolRequest::Find(find) => PreparedBrowserRequest::Find(find),
        BrowserToolRequest::Act(act) => PreparedBrowserRequest::Act(act),
        BrowserToolRequest::Wait(wait) => PreparedBrowserRequest::Wait(wait),
        BrowserToolRequest::Events(events) => PreparedBrowserRequest::Events(events),
        BrowserToolRequest::Close(close) => PreparedBrowserRequest::Close(close),
    };

    validate_request(&prepared, &limits)?;
    let scope = scope_for(&prepared);
    Ok(PreparedBrowserCall::new(prepared, scope))
}

/// Validates a normalized request against `limits` (no I/O, no state).
///
/// # Errors
/// - `Error::InvalidArgument`: selector/target/text/key/scroll/URL/wait
///   limits violated, or an element-targeted action lacks an expected revision.
/// - `Error::OriginNotAllowed`: an authorized open request no longer validates.
pub(crate) fn validate_request(
    request: &PreparedBrowserRequest,
    limits: &BrowserLimits,
) -> Result<()> {
    match request {
        PreparedBrowserRequest::Open(open) => open.validate(),
        PreparedBrowserRequest::Observe(observe) => match &observe.mode {
            ObservationMode::DomSelection { selector } => validate_selector(selector, limits),
            ObservationMode::PageSummary
            | ObservationMode::InteractiveElements
            | ObservationMode::TextExtraction
            | ObservationMode::FormsAndLinks
            | ObservationMode::NetworkActivitySummary
            | ObservationMode::ConsoleLogSummary
            | ObservationMode::Screenshot
            | ObservationMode::CombinedDiagnostic => Ok(()),
        },
        PreparedBrowserRequest::Find(find) => validate_target(&find.target, limits),
        PreparedBrowserRequest::Act(act) => {
            if action_targets_element(&act.request.action)
                && act.request.expected_revision.is_none()
            {
                return Err(Error::InvalidArgument {
                    detail:
                        "element-targeted browser actions require an expected observation revision"
                            .to_owned(),
                });
            }
            act.request.validate(limits)
        }
        PreparedBrowserRequest::Wait(wait) => {
            wait.condition.validate(limits)?;
            wait.timeout.validate(limits)
        }
        PreparedBrowserRequest::Events(_) | PreparedBrowserRequest::Close(_) => Ok(()),
    }
}

// Derives the audit/approval scope from the normalized request.
fn scope_for(request: &PreparedBrowserRequest) -> BrowserResourceScope {
    match request {
        PreparedBrowserRequest::Open(open) => BrowserResourceScope {
            session_id: None,
            context_id: None,
            origins: open.allowed_origins.origins(),
            action: BrowserScopeAction::Open,
            expected_revision: None,
        },
        PreparedBrowserRequest::Observe(observe) => BrowserResourceScope {
            session_id: Some(observe.session_id),
            context_id: Some(observe.context_id),
            origins: Vec::new(),
            action: BrowserScopeAction::Observe,
            expected_revision: None,
        },
        PreparedBrowserRequest::Find(find) => BrowserResourceScope {
            session_id: Some(find.session_id),
            context_id: Some(find.context_id),
            origins: Vec::new(),
            action: BrowserScopeAction::Find,
            expected_revision: Some(find.revision),
        },
        PreparedBrowserRequest::Act(act) => BrowserResourceScope {
            session_id: Some(act.session_id),
            context_id: Some(act.request.context_id),
            origins: action_origins(&act.request.action),
            action: BrowserScopeAction::Act,
            expected_revision: act.request.expected_revision,
        },
        PreparedBrowserRequest::Wait(wait) => BrowserResourceScope {
            session_id: Some(wait.session_id),
            context_id: Some(wait.context_id),
            origins: Vec::new(),
            action: BrowserScopeAction::Wait,
            expected_revision: None,
        },
        PreparedBrowserRequest::Events(events) => BrowserResourceScope {
            session_id: Some(events.session_id),
            context_id: None,
            origins: Vec::new(),
            action: BrowserScopeAction::Events,
            expected_revision: None,
        },
        PreparedBrowserRequest::Close(close) => BrowserResourceScope {
            session_id: Some(close.session_id),
            context_id: None,
            origins: Vec::new(),
            action: BrowserScopeAction::Close,
            expected_revision: None,
        },
    }
}

// Explicit navigation origin of an action (implicit navigations are caught by
// the post-action location check in dispatch).
fn action_origins(action: &BrowserAction) -> Vec<String> {
    action
        .navigation_target()
        .map(|url| vec![url.origin().ascii_serialization()])
        .unwrap_or_default()
}

// Whether the action addresses a concrete element (needs a revision).
fn action_targets_element(action: &BrowserAction) -> bool {
    match action {
        BrowserAction::Click { .. }
        | BrowserAction::Type { .. }
        | BrowserAction::Clear { .. }
        | BrowserAction::Select { .. }
        | BrowserAction::Focus { .. }
        | BrowserAction::Hover { .. }
        | BrowserAction::Drag { .. }
        | BrowserAction::Submit { .. } => true,
        BrowserAction::Scroll { target, .. } | BrowserAction::KeyPress { target, .. } => {
            target.is_some()
        }
        BrowserAction::Navigate { .. }
        | BrowserAction::Back
        | BrowserAction::Forward
        | BrowserAction::Reload => false,
    }
}
