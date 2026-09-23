//! Typed request/response envelopes for the seven model-facing browser tools,
//! plus the host-owned open authority ([`BrowserOpenGrant`],
//! [`BrowserOpenPolicy`]) and the sealed prepared call ([`PreparedBrowserCall`]).
//!
//! Remediation C-BROWSER (F-010, F-114):
//! - Every model-facing request type rejects unknown fields; the serde shape
//!   *is* the schema. There are no untagged enums, so removed actions
//!   (`Upload`) and conditions (`CustomScript`) cannot re-enter through a
//!   fallback variant.
//! - [`OpenRequest`] contains **no authority fields**: a model sending
//!   `allowed_origins`, `authentication_origins`, `profile` or `limits` is
//!   rejected at deserialization instead of being silently overwritten.
//! - [`PreparedBrowserCall`] has no `Deserialize`, no public constructor and
//!   no public fields, so in-process callers cannot forge authority.
//! - Authentication origins stay a separate policy on the authorized
//!   [`OpenBrowserRequest`] and never become navigation targets.
//!
//! Concurrency: plain data, `Send + Sync`.

use harw_browser::action::{ActionOutcome, ActionRequest};
use harw_browser::event::EventEnvelope;
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId,
};
use harw_browser::observation::{BrowserObservation, ObservationMode, ObservedElement};
use harw_browser::policy::{
    BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy, ProfilePolicy, Viewport,
};
use harw_browser::selector::Target;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};

/// One stable model-facing browser tool and its versioned Harwness capability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserToolDescriptor {
    pub name: String,
    pub capability: String,
    pub input_schema: serde_json::Value,
}

/// Model input for `browser.open`.
///
/// Carries only what the model may choose. Origins, profile and limits come
/// exclusively from the host-owned [`BrowserOpenGrant`] via the crate-private
/// `BrowserOpenPolicy::authorize`; supplying them is a deserialization
/// error.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    pub start_url: url::Url,
    pub headless: bool,
    pub bidi: BiDiRequirement,
    pub viewport: Option<Viewport>,
}

/// One host-owned authority grant for opening a browser session.
///
/// The model never receives this type as part of the browser tool schema. It
/// cannot select a persistent profile binding, expand either origin list, or
/// raise limits. `allowed_origins` bounds start URL and navigation;
/// `authentication_origins` is accepted only as an observed (redirect)
/// location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserOpenGrant {
    allowed_origins: OriginPolicy,
    authentication_origins: OriginPolicy,
    profile: ProfilePolicy,
    limits: BrowserLimits,
}

impl BrowserOpenGrant {
    /// Builds a grant with an ephemeral profile and default limits.
    pub fn ephemeral(allowed_origins: OriginPolicy, authentication_origins: OriginPolicy) -> Self {
        Self {
            allowed_origins,
            authentication_origins,
            profile: ProfilePolicy::Ephemeral,
            limits: BrowserLimits::default(),
        }
    }

    /// Builds a grant seeded from a host-configured persistent profile binding
    /// with default limits. The binding is validated during authorization.
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
            limits: BrowserLimits::default(),
        }
    }

    /// Replaces the grant's limits (already clamped by [`BrowserLimits`]).
    #[must_use]
    pub fn with_limits(mut self, limits: BrowserLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Returns the navigation allowlist.
    pub fn allowed_origins(&self) -> &OriginPolicy {
        &self.allowed_origins
    }

    /// Returns the redirect-only authentication allowlist.
    pub fn authentication_origins(&self) -> &OriginPolicy {
        &self.authentication_origins
    }

    /// Returns the profile policy.
    pub fn profile(&self) -> &ProfilePolicy {
        &self.profile
    }

    /// Returns the session limits.
    pub fn limits(&self) -> &BrowserLimits {
        &self.limits
    }
}

/// Complete host-owned authority configuration for `browser.open`.
///
/// An empty policy is intentional and fail-closed. Callers must explicitly
/// install a grant on [`crate::BrowserToolSet`] before an open call can reach
/// a browser host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserOpenPolicy {
    grant: Option<BrowserOpenGrant>,
}

impl BrowserOpenPolicy {
    /// Builds a policy holding exactly one grant.
    pub fn grant(grant: BrowserOpenGrant) -> Self {
        Self { grant: Some(grant) }
    }

    /// Returns the configured grant, if any.
    pub fn configured_grant(&self) -> Option<&BrowserOpenGrant> {
        self.grant.as_ref()
    }

    /// Combines the model's open input with the host grant.
    ///
    /// The private-network veto is applied to both origin policies when either
    /// enables it (it can only tighten). The result is fully validated via
    /// [`OpenBrowserRequest::validate`]; in particular the start URL must be in
    /// `allowed_origins` — an authentication origin is not a valid start.
    ///
    /// # Errors
    /// - `Error::OriginNotAllowed`: no grant, or start URL outside
    ///   `allowed_origins`.
    /// - `Error::InvalidArgument`: start URL too long, invalid viewport or
    ///   profile binding.
    pub(crate) fn authorize(
        &self,
        model_request: &OpenRequest,
    ) -> harw_browser::Result<OpenBrowserRequest> {
        let Some(grant) = self.grant.as_ref() else {
            return Err(harw_browser::error::Error::OriginNotAllowed {
                origin: model_request.start_url.origin().ascii_serialization(),
            });
        };

        let veto = grant.allowed_origins.deny_private_networks()
            || grant.authentication_origins.deny_private_networks();
        let tighten = |policy: &OriginPolicy| {
            if veto {
                policy.clone().with_private_network_veto()
            } else {
                policy.clone()
            }
        };

        let request = OpenBrowserRequest {
            start_url: model_request.start_url.clone(),
            headless: model_request.headless,
            profile: grant.profile.clone(),
            bidi: model_request.bidi,
            allowed_origins: tighten(&grant.allowed_origins),
            authentication_origins: tighten(&grant.authentication_origins),
            viewport: model_request.viewport,
            limits: grant.limits,
        };
        request.validate()?;
        Ok(request)
    }
}

/// Model input for `browser.observe`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveRequest {
    pub session_id: BrowserSessionId,
    pub context_id: BrowserContextId,
    pub mode: ObservationMode,
}

/// Model input for `browser.find`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindRequest {
    pub session_id: BrowserSessionId,
    pub context_id: BrowserContextId,
    pub target: Target,
    pub revision: BrowserObservationRevision,
}

/// Model input for `browser.act`. Validate with
/// [`harw_browser::action::ActionRequest::validate`] before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActRequest {
    pub session_id: BrowserSessionId,
    pub request: ActionRequest,
}

/// Model input for `browser.wait`. `timeout` is integer milliseconds within
/// `1..=HARD_MAX_WAIT_MS`; validate condition and timeout against the
/// session limits before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitRequest {
    pub session_id: BrowserSessionId,
    pub context_id: BrowserContextId,
    pub condition: WaitCondition,
    pub timeout: WaitTimeout,
}

/// Model input for `browser.events`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsRequest {
    pub session_id: BrowserSessionId,
    pub since: BrowserEventCursor,
}

/// Model input for `browser.close`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseRequest {
    pub session_id: BrowserSessionId,
}

/// Closed request envelope for the compact seven-tool model surface.
///
/// Externally tagged, no untagged or `other` fallback variants.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum BrowserToolRequest {
    Open(OpenRequest),
    Observe(ObserveRequest),
    Find(FindRequest),
    Act(ActRequest),
    Wait(WaitRequest),
    Events(EventsRequest),
    Close(CloseRequest),
}

/// Response of `browser.open`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenResponse {
    pub session_id: BrowserSessionId,
    /// The session's primary browsing context, allocated by the authoritative
    /// browser host while opening the session.
    pub primary_context_id: BrowserContextId,
}

/// Response of `browser.observe`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveResponse {
    pub observation: BrowserObservation,
}

/// Response of `browser.find`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindResponse {
    pub element: ObservedElement,
}

/// Response of `browser.act`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActResponse {
    pub outcome: ActionOutcome,
}

/// Response of `browser.wait`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitResponse {
    pub outcome: WaitOutcome,
}

/// Response of `browser.events`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsResponse {
    pub events: Vec<EventEnvelope>,
}

/// Response of `browser.close`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseResponse {
    pub session_id: BrowserSessionId,
}

/// Closed response envelope matching [`BrowserToolRequest`] one-for-one.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
///
/// Serialize-only (audit/logging); it is never accepted as input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BrowserResourceScope {
    pub session_id: Option<BrowserSessionId>,
    pub context_id: Option<BrowserContextId>,
    pub origins: Vec<String>,
    pub action: BrowserScopeAction,
    pub expected_revision: Option<BrowserObservationRevision>,
}

/// Normalized request inside a [`PreparedBrowserCall`].
///
/// `Open` carries the host-authorized [`OpenBrowserRequest`] (never the model's
/// [`OpenRequest`]). Not deserializable; constructing a value grants nothing
/// because only this crate can wrap it into a [`PreparedBrowserCall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedBrowserRequest {
    Open(OpenBrowserRequest),
    Observe(ObserveRequest),
    Find(FindRequest),
    Act(ActRequest),
    Wait(WaitRequest),
    Events(EventsRequest),
    Close(CloseRequest),
}

/// Immutable normalized call passed from preparation to dispatch.
///
/// Sealed: no serde, private fields, crate-private constructor. The only way
/// to obtain one is through this crate's preparation path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBrowserCall {
    scope: BrowserResourceScope,
    request: PreparedBrowserRequest,
}

impl PreparedBrowserCall {
    pub(crate) fn new(request: PreparedBrowserRequest, scope: BrowserResourceScope) -> Self {
        Self { scope, request }
    }

    /// Returns the derived authority scope.
    pub fn scope(&self) -> &BrowserResourceScope {
        &self.scope
    }

    /// Returns the normalized request.
    pub fn request(&self) -> &PreparedBrowserRequest {
        &self.request
    }

    pub(crate) fn into_request(self) -> PreparedBrowserRequest {
        self.request
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_browser::action::BrowserAction;
    use harw_browser::error::Error;
    use harw_browser::selector::Selector;

    // Test helper: literals only, a parse failure is a test bug.
    fn url(s: &str) -> TestResult<url::Url> {
        url::Url::parse(s).map_err(ctx("valid test url"))
    }

    fn origins(list: &[&str], deny: bool) -> TestResult<OriginPolicy> {
        OriginPolicy::from_origins(list, deny).map_err(ctx("valid test policy"))
    }

    fn open_request(start: &str) -> TestResult<OpenRequest> {
        Ok(OpenRequest {
            start_url: url(start)?,
            headless: true,
            bidi: BiDiRequirement::Preferred,
            viewport: None,
        })
    }

    fn grant_policy() -> TestResult<BrowserOpenPolicy> {
        Ok(BrowserOpenPolicy::grant(BrowserOpenGrant::ephemeral(
            origins(&["https://erp.example.com"], false)?,
            origins(&["https://sso.example.net"], true)?,
        )))
    }

    fn act_json(action: serde_json::Value) -> TestResult<serde_json::Value> {
        let request = ActRequest {
            session_id: BrowserSessionId::new(),
            request: ActionRequest::new(BrowserContextId::new(), BrowserAction::Reload),
        };
        let mut json = serde_json::to_value(request).map_err(ctx("act serializes"))?;
        json["request"]["action"] = action;
        Ok(json)
    }

    #[test]
    fn test_authorize_without_grant_fails_closed() -> TestResult {
        let Err(error) =
            BrowserOpenPolicy::default().authorize(&open_request("https://erp.example.com/")?)
        else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, Error::OriginNotAllowed { .. }));
        Ok(())
    }

    #[test]
    fn test_authorize_keeps_authentication_origins_separate() -> TestResult {
        let authorized = grant_policy()?
            .authorize(&open_request("https://erp.example.com/login")?)
            .map_err(ctx("allowed start authorizes"))?;
        assert_eq!(
            authorized.allowed_origins.origins(),
            vec!["https://erp.example.com".to_owned()]
        );
        assert_eq!(
            authorized.authentication_origins.origins(),
            vec!["https://sso.example.net".to_owned()]
        );
        let sso = url("https://sso.example.net/authorize")?;
        assert!(authorized.check_navigation_target(&sso).is_err());
        assert!(authorized.check_observed_location(&sso).is_ok());
        Ok(())
    }

    #[test]
    fn test_authorize_rejects_authentication_origin_as_start() -> TestResult {
        let Err(error) = grant_policy()?.authorize(&open_request("https://sso.example.net/login")?)
        else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, Error::OriginNotAllowed { .. }));
        Ok(())
    }

    #[test]
    fn test_authorize_rejects_origin_change_in_start_url() -> TestResult {
        for start in [
            "http://erp.example.com/",
            "https://erp.example.com:8443/",
            "https://evil.example.com/",
            "https://sub.erp.example.com/",
        ] {
            assert!(
                grant_policy()?.authorize(&open_request(start)?).is_err(),
                "{start} must be rejected"
            );
        }
        Ok(())
    }

    #[test]
    fn test_authorize_tightens_private_network_veto_and_applies_grant_profile_limits() -> TestResult
    {
        let limits = BrowserLimits::default().with_max_actions_per_session(7);
        let policy = BrowserOpenPolicy::grant(
            BrowserOpenGrant::persistent(
                origins(&["https://erp.example.com"], false)?,
                origins(&["https://sso.example.net"], true)?,
                "sales-team",
            )
            .with_limits(limits),
        );
        let authorized = policy
            .authorize(&open_request("https://erp.example.com/")?)
            .map_err(ctx("authorizes"))?;
        assert!(authorized.allowed_origins.deny_private_networks());
        assert!(authorized.authentication_origins.deny_private_networks());
        assert_eq!(
            authorized.profile,
            ProfilePolicy::Persistent {
                binding: "sales-team".to_owned()
            }
        );
        assert_eq!(authorized.limits.max_actions_per_session(), 7);
        Ok(())
    }

    #[test]
    fn test_authorize_rejects_invalid_persistent_binding() -> TestResult {
        let policy = BrowserOpenPolicy::grant(BrowserOpenGrant::persistent(
            origins(&["https://erp.example.com"], true)?,
            OriginPolicy::default(),
            "../../.ssh",
        ));
        assert!(
            policy
                .authorize(&open_request("https://erp.example.com/")?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn test_open_request_rejects_model_supplied_authority_fields() -> TestResult {
        let base = serde_json::to_value(open_request("https://erp.example.com/")?)
            .map_err(ctx("open serializes"))?;
        let decoded: OpenRequest =
            serde_json::from_value(base.clone()).map_err(ctx("plain open parses"))?;
        assert_eq!(decoded, open_request("https://erp.example.com/")?);

        for (field, value) in [
            (
                "allowed_origins",
                serde_json::json!({"allow": ["https://evil.example"], "deny_private_networks": false}),
            ),
            (
                "authentication_origins",
                serde_json::json!({"allow": [], "deny_private_networks": false}),
            ),
            (
                "profile",
                serde_json::json!({"Persistent": {"binding": "x"}}),
            ),
            ("limits", serde_json::json!({})),
        ] {
            let mut json = base.clone();
            json[field] = value;
            assert!(
                serde_json::from_value::<OpenRequest>(json).is_err(),
                "field {field} must be rejected"
            );
        }
        Ok(())
    }

    #[test]
    fn test_act_request_rejects_upload_and_script_actions() -> TestResult {
        let target = serde_json::to_value(Target::new(Selector::Id("f".to_owned())))
            .map_err(ctx("target serializes"))?;
        for action in [
            serde_json::json!({"Upload": {"target": target.clone(), "file_path": "/etc/shadow"}}),
            serde_json::json!({"upload": {"target": target.clone(), "file_path": "/etc/shadow"}}),
            serde_json::json!({"Script": {"source": "fetch('https://evil')"}}),
            serde_json::json!({"script": "fetch('https://evil')"}),
        ] {
            assert!(serde_json::from_value::<ActRequest>(act_json(action)?).is_err());
        }
        let click = serde_json::json!({"Click": {"target": target}});
        assert!(serde_json::from_value::<ActRequest>(act_json(click)?).is_ok());
        Ok(())
    }

    #[test]
    fn test_wait_request_rejects_custom_script_and_bad_timeout() -> TestResult {
        let request = WaitRequest {
            session_id: BrowserSessionId::new(),
            context_id: BrowserContextId::new(),
            condition: WaitCondition::NavigationComplete,
            timeout: WaitTimeout::from_millis(1_000),
        };
        let base = serde_json::to_value(&request).map_err(ctx("wait serializes"))?;
        let decoded: WaitRequest =
            serde_json::from_value(base.clone()).map_err(ctx("wait parses"))?;
        assert_eq!(decoded, request);

        let mut script = base.clone();
        script["condition"] = serde_json::json!({"CustomScript": {"predicate": "true"}});
        assert!(serde_json::from_value::<WaitRequest>(script).is_err());

        let mut zero = base.clone();
        zero["timeout"] = serde_json::json!(0);
        assert!(serde_json::from_value::<WaitRequest>(zero).is_err());
        Ok(())
    }

    #[test]
    fn test_request_types_reject_unknown_fields() {
        let close = serde_json::json!({
            "session_id": BrowserSessionId::new(),
            "force": true
        });
        assert!(serde_json::from_value::<CloseRequest>(close).is_err());

        let envelope = serde_json::json!({
            "Close": { "session_id": BrowserSessionId::new(), "grant": "all" }
        });
        assert!(serde_json::from_value::<BrowserToolRequest>(envelope).is_err());

        let unknown_tool = serde_json::json!({ "Upload": {} });
        assert!(serde_json::from_value::<BrowserToolRequest>(unknown_tool).is_err());
    }

    #[test]
    fn test_browser_tool_request_round_trip_open() -> TestResult {
        let request = BrowserToolRequest::Open(open_request("https://erp.example.com/")?);
        let json = serde_json::to_value(&request).map_err(ctx("serializes"))?;
        let decoded: BrowserToolRequest =
            serde_json::from_value(json).map_err(ctx("deserializes"))?;
        assert_eq!(decoded, request);
        Ok(())
    }

    #[test]
    fn test_prepared_browser_call_accessors() {
        let close = CloseRequest {
            session_id: BrowserSessionId::new(),
        };
        let scope = BrowserResourceScope {
            session_id: Some(close.session_id),
            context_id: None,
            origins: Vec::new(),
            action: BrowserScopeAction::Close,
            expected_revision: None,
        };
        let prepared =
            PreparedBrowserCall::new(PreparedBrowserRequest::Close(close.clone()), scope.clone());
        assert_eq!(prepared.scope(), &scope);
        assert_eq!(
            prepared.request(),
            &PreparedBrowserRequest::Close(close.clone())
        );
        assert_eq!(
            prepared.into_request(),
            PreparedBrowserRequest::Close(close)
        );
    }

    #[test]
    fn test_browser_open_grant_accessors() -> TestResult {
        let grant = BrowserOpenGrant::ephemeral(
            origins(&["https://erp.example.com"], true)?,
            OriginPolicy::default(),
        );
        assert_eq!(grant.profile(), &ProfilePolicy::Ephemeral);
        assert_eq!(grant.limits(), &BrowserLimits::default());
        assert!(grant.authentication_origins().is_empty());
        assert_eq!(grant.allowed_origins().rules().len(), 1);
        assert!(
            BrowserOpenPolicy::grant(grant.clone())
                .configured_grant()
                .is_some_and(|g| g == &grant)
        );
        Ok(())
    }
}
