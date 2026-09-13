use std::sync::OnceLock;

use harw_browser::action::BrowserAction;
use harw_browser::error::{Error, Result};

use crate::types::{
    BrowserOpenPolicy, BrowserResourceScope, BrowserScopeAction, BrowserToolDescriptor,
    BrowserToolRequest, PreparedBrowserCall,
};

/// Returns the stable, compact model-facing browser surface in presentation order.
pub fn browser_tool_descriptors() -> &'static [BrowserToolDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<BrowserToolDescriptor>> = OnceLock::new();
    DESCRIPTORS.get_or_init(|| {
        [
            ("browser.open", "harwness.browser.open@1"),
            ("browser.observe", "harwness.browser.observe@1"),
            ("browser.find", "harwness.browser.find@1"),
            ("browser.act", "harwness.browser.act@1"),
            ("browser.wait", "harwness.browser.wait@1"),
            ("browser.events", "harwness.browser.events@1"),
            ("browser.close", "harwness.browser.close@1"),
        ]
        .into_iter()
        .map(|(name, capability)| BrowserToolDescriptor {
            name: name.to_owned(),
            capability: capability.to_owned(),
            input_schema: object_schema(name),
        })
        .collect()
    })
}

/// Validates and freezes a non-open typed request together with its requested authority scope.
///
/// This function performs no host lookup and no browser I/O. Runtime-dependent policy
/// checks remain dispatch concerns; preparation only admits facts present in the request.
/// `browser.open` is deliberately rejected here: it requires a host-owned
/// [`BrowserOpenPolicy`] and must be prepared through [`crate::BrowserToolSet`].
pub fn prepare_browser_call(request: BrowserToolRequest) -> Result<PreparedBrowserCall> {
    prepare_browser_call_with_policy(request, None)
}

pub(crate) fn prepare_browser_call_with_policy(
    mut request: BrowserToolRequest,
    open_policy: Option<&BrowserOpenPolicy>,
) -> Result<PreparedBrowserCall> {
    let scope = match &mut request {
        BrowserToolRequest::Open(open) => {
            let Some(open_policy) = open_policy else {
                return Err(Error::InvalidArgument {
                    detail: "browser.open requires a host-owned BrowserOpenPolicy".to_owned(),
                });
            };
            let authorized = open_policy.authorize(&open.request)?;

            // This mutation is intentionally before the immutable prepared
            // boundary. Dispatch can therefore only receive the trusted
            // authority material derived above.
            open.request = authorized;

            BrowserResourceScope {
                session_id: None,
                context_id: None,
                origins: open.request.allowed_origins.allow.to_vec(),
                action: BrowserScopeAction::Open,
                expected_revision: None,
            }
        }
        BrowserToolRequest::Observe(observe) => BrowserResourceScope {
            session_id: Some(observe.session_id),
            context_id: Some(observe.context_id),
            origins: Vec::new(),
            action: BrowserScopeAction::Observe,
            expected_revision: None,
        },
        BrowserToolRequest::Find(find) => BrowserResourceScope {
            session_id: Some(find.session_id),
            context_id: Some(find.context_id),
            origins: Vec::new(),
            action: BrowserScopeAction::Find,
            expected_revision: Some(find.revision),
        },
        BrowserToolRequest::Act(act) => {
            if action_targets_element(&act.request.action)
                && act.request.expected_revision.is_none()
            {
                return Err(Error::InvalidArgument {
                    detail:
                        "element-targeted browser actions require an expected observation revision"
                            .to_owned(),
                });
            }

            BrowserResourceScope {
                session_id: Some(act.session_id),
                context_id: Some(act.request.context_id),
                origins: action_origins(&act.request.action),
                action: BrowserScopeAction::Act,
                expected_revision: act.request.expected_revision,
            }
        }
        BrowserToolRequest::Wait(wait) => {
            if wait.timeout.duration().is_zero() {
                return Err(Error::InvalidArgument {
                    detail: "browser.wait timeout must be greater than zero".to_owned(),
                });
            }
            BrowserResourceScope {
                session_id: Some(wait.session_id),
                context_id: Some(wait.context_id),
                origins: Vec::new(),
                action: BrowserScopeAction::Wait,
                expected_revision: None,
            }
        }
        BrowserToolRequest::Events(events) => BrowserResourceScope {
            session_id: Some(events.session_id),
            context_id: None,
            origins: Vec::new(),
            action: BrowserScopeAction::Events,
            expected_revision: None,
        },
        BrowserToolRequest::Close(close) => BrowserResourceScope {
            session_id: Some(close.session_id),
            context_id: None,
            origins: Vec::new(),
            action: BrowserScopeAction::Close,
            expected_revision: None,
        },
    };

    Ok(PreparedBrowserCall::new(request, scope))
}

fn object_schema(title: &str) -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "type": "object"
    })
}

fn action_origins(action: &BrowserAction) -> Vec<String> {
    match action {
        BrowserAction::Navigate { url } => vec![url.origin().ascii_serialization()],
        _ => Vec::new(),
    }
}

fn action_targets_element(action: &BrowserAction) -> bool {
    match action {
        BrowserAction::Click { .. }
        | BrowserAction::Type { .. }
        | BrowserAction::Clear { .. }
        | BrowserAction::Select { .. }
        | BrowserAction::Focus { .. }
        | BrowserAction::Hover { .. }
        | BrowserAction::Drag { .. }
        | BrowserAction::Upload { .. }
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
