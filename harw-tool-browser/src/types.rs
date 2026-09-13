use harw_browser::action::{ActionOutcome, ActionRequest};
use harw_browser::event::EventEnvelope;
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId,
};
use harw_browser::observation::{BrowserObservation, ObservationMode, ObservedElement};
use harw_browser::policy::{OpenBrowserRequest, OriginPolicy, ProfilePolicy};
use harw_browser::selector::Target;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};

/// One stable model-facing browser tool and its versioned Harwness capability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BrowserToolDescriptor {
    pub name: String,
    pub capability: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpenRequest {
    /// This compatibility envelope is deserialized from model input, but its
    /// origin and profile fields are never trusted. `BrowserToolSet::prepare`
    /// replaces those fields with the host-owned grant before dispatch.
    pub request: OpenBrowserRequest,
}

/// One host-owned authority grant for opening a browser session.
///
/// The model never receives this type as part of the browser tool schema. In
/// particular, it cannot select a persistent profile binding or expand either
/// origin list by supplying fields on [`OpenBrowserRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserOpenGrant {
    allowed_origins: OriginPolicy,
    authentication_origins: OriginPolicy,
    profile: ProfilePolicy,
}

impl BrowserOpenGrant {
    pub fn ephemeral(allowed_origins: OriginPolicy, authentication_origins: OriginPolicy) -> Self {
        Self {
            allowed_origins,
            authentication_origins,
            profile: ProfilePolicy::Ephemeral,
        }
    }

    pub fn persistent(
        allowed_origins: OriginPolicy,
        authentication_origins: OriginPolicy,
        binding: impl Into<String>,
    ) -> Self {
        Self {
            allowed_origins,
            authentication_origins,
            profile: ProfilePolicy::Persistent {
                binding: binding.into(),
            },
        }
    }

    pub fn allowed_origins(&self) -> &OriginPolicy {
        &self.allowed_origins
    }

    pub fn authentication_origins(&self) -> &OriginPolicy {
        &self.authentication_origins
    }

    pub fn profile(&self) -> &ProfilePolicy {
        &self.profile
    }
}

/// Complete host-owned authority configuration for `browser.open`.
///
/// An empty policy is intentional and fail-closed. Callers must explicitly
/// install at least one grant on [`crate::BrowserToolSet`] before an open call
/// can reach a browser host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserOpenPolicy {
    grant: Option<BrowserOpenGrant>,
}

impl BrowserOpenPolicy {
    pub fn grant(grant: BrowserOpenGrant) -> Self {
        Self { grant: Some(grant) }
    }

    pub fn configured_grant(&self) -> Option<&BrowserOpenGrant> {
        self.grant.as_ref()
    }

    pub(crate) fn authorize(
        &self,
        model_request: &OpenBrowserRequest,
    ) -> harw_browser::Result<OpenBrowserRequest> {
        let Some(grant) = self
            .grant
            .as_ref()
            .filter(|grant| grant.allowed_origins.is_allowed(&model_request.start_url))
        else {
            return Err(harw_browser::error::Error::OriginNotAllowed {
                origin: model_request.start_url.origin().ascii_serialization(),
            });
        };

        let mut allow = grant.allowed_origins.allow.clone();
        for origin in &grant.authentication_origins.allow {
            if !allow.contains(origin) {
                allow.push(origin.clone());
            }
        }

        Ok(OpenBrowserRequest {
            start_url: model_request.start_url.clone(),
            headless: model_request.headless,
            bidi: model_request.bidi,
            viewport: model_request.viewport,
            allowed_origins: OriginPolicy::new(
                allow,
                grant.allowed_origins.deny_private_networks
                    || grant.authentication_origins.deny_private_networks,
            ),
            profile: grant.profile.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ObserveRequest {
    pub session_id: BrowserSessionId,
    pub context_id: BrowserContextId,
    pub mode: ObservationMode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FindRequest {
    pub session_id: BrowserSessionId,
    pub context_id: BrowserContextId,
    pub target: Target,
    pub revision: BrowserObservationRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActRequest {
    pub session_id: BrowserSessionId,
    pub request: ActionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WaitRequest {
    pub session_id: BrowserSessionId,
    pub context_id: BrowserContextId,
    pub condition: WaitCondition,
    pub timeout: WaitTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventsRequest {
    pub session_id: BrowserSessionId,
    pub since: BrowserEventCursor,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CloseRequest {
    pub session_id: BrowserSessionId,
}

/// Closed request envelope for the compact seven-tool model surface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BrowserToolRequest {
    Open(OpenRequest),
    Observe(ObserveRequest),
    Find(FindRequest),
    Act(ActRequest),
    Wait(WaitRequest),
    Events(EventsRequest),
    Close(CloseRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpenResponse {
    pub session_id: BrowserSessionId,
    /// The session's primary browsing context, allocated by the authoritative
    /// browser host while opening the session.
    pub primary_context_id: BrowserContextId,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObserveResponse {
    pub observation: BrowserObservation,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FindResponse {
    pub element: ObservedElement,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActResponse {
    pub outcome: ActionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WaitResponse {
    pub outcome: WaitOutcome,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EventsResponse {
    pub events: Vec<EventEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CloseResponse {
    pub session_id: BrowserSessionId,
}

/// Closed response envelope matching [`BrowserToolRequest`] one-for-one.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum BrowserToolResponse {
    Open(OpenResponse),
    Observe(ObserveResponse),
    Find(FindResponse),
    Act(ActResponse),
    Wait(WaitResponse),
    Events(EventsResponse),
    Close(CloseResponse),
}

/// Semantic operation admitted by a prepared browser call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BrowserScopeAction {
    Open,
    Observe,
    Find,
    Act,
    Wait,
    Events,
    Close,
}

/// Authority and optimistic-concurrency boundary derived before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BrowserResourceScope {
    pub session_id: Option<BrowserSessionId>,
    pub context_id: Option<BrowserContextId>,
    pub origins: Vec<String>,
    pub action: BrowserScopeAction,
    pub expected_revision: Option<BrowserObservationRevision>,
}

/// Immutable normalized call passed from preparation to dispatch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreparedBrowserCall {
    pub scope: BrowserResourceScope,
    pub(crate) request: BrowserToolRequest,
}

impl PreparedBrowserCall {
    pub(crate) fn new(request: BrowserToolRequest, scope: BrowserResourceScope) -> Self {
        Self { scope, request }
    }

    pub fn request(&self) -> &BrowserToolRequest {
        &self.request
    }

    pub(crate) fn into_request(self) -> BrowserToolRequest {
        self.request
    }
}
