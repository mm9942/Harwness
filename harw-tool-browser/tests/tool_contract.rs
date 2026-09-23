//! Host-free contract of the browser tool surface: serde envelopes, open
//! authorization from the operator grant only, scope derivation and
//! preparation-time validation (W3 C-BROWSER, W5 B-TOOL).

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::error::Error;
use harw_browser::host::BrowserHost;
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId, EffectId,
};
use harw_browser::observation::{
    BrowserObservation, DocumentIdentity, ObservationMode, ObservedElement,
};
use harw_browser::policy::{
    BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy, ProfilePolicy, Viewport,
};
use harw_browser::selector::{Selector, Target};
use harw_browser::session::BrowserSessionHandle;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use harw_tool_browser::{
    ActRequest, ActResponse, BrowserOpenGrant, BrowserOpenPolicy, BrowserScopeAction,
    BrowserToolRequest, BrowserToolResponse, BrowserToolSet, CloseRequest, CloseResponse,
    EventsRequest, EventsResponse, FindRequest, FindResponse, ObserveRequest, ObserveResponse,
    OpenRequest, OpenResponse, PreparedBrowserRequest, WaitRequest, WaitResponse,
    prepare_browser_call,
};

mod common;
use common::{TestError, TestResult, ctx};

// Fixtures: literal inputs, a parse failure is a fixture bug (reported as
// `Err` instead of a panic, per Bible R087/R165).
fn session_id() -> TestResult<BrowserSessionId> {
    BrowserSessionId::from_str("00000000-0000-4000-8000-000000000001").map_err(ctx("fixture UUID"))
}

fn context_id() -> TestResult<BrowserContextId> {
    BrowserContextId::from_str("00000000-0000-4000-8000-000000000002").map_err(ctx("fixture UUID"))
}

fn url(text: &str) -> TestResult<url::Url> {
    url::Url::parse(text).map_err(ctx("fixture URL"))
}

fn policy(origins: &[&str]) -> TestResult<OriginPolicy> {
    OriginPolicy::from_origins(origins, true).map_err(ctx("fixture policy"))
}

fn open_request() -> TestResult<OpenRequest> {
    Ok(OpenRequest {
        start_url: url("https://erp.example.com/inbox")?,
        headless: true,
        bidi: BiDiRequirement::Required,
        viewport: Some(Viewport {
            width: 1440,
            height: 900,
        }),
    })
}

fn grant() -> TestResult<BrowserOpenGrant> {
    Ok(BrowserOpenGrant::ephemeral(
        policy(&["https://erp.example.com", "https://assets.example.com"])?,
        policy(&["https://sso.example.org"])?,
    ))
}

fn configured_tools() -> TestResult<BrowserToolSet> {
    Ok(BrowserToolSet::with_open_policy(
        Arc::new(NeverUsedHost),
        BrowserOpenPolicy::grant(grant()?),
    ))
}

fn requests() -> TestResult<Vec<BrowserToolRequest>> {
    let revision = BrowserObservationRevision::initial().next();
    Ok(vec![
        BrowserToolRequest::Open(open_request()?),
        BrowserToolRequest::Observe(ObserveRequest {
            session_id: session_id()?,
            context_id: context_id()?,
            mode: ObservationMode::InteractiveElements,
        }),
        BrowserToolRequest::Find(FindRequest {
            session_id: session_id()?,
            context_id: context_id()?,
            target: Target::new(Selector::TestId("reply".to_owned())),
            revision,
        }),
        BrowserToolRequest::Act(ActRequest {
            session_id: session_id()?,
            request: ActionRequest::new(
                context_id()?,
                BrowserAction::Navigate {
                    url: url("https://erp.example.com/ticket/42")?,
                },
            )
            .with_expected_revision(revision),
        }),
        BrowserToolRequest::Wait(WaitRequest {
            session_id: session_id()?,
            context_id: context_id()?,
            condition: WaitCondition::NavigationComplete,
            timeout: WaitTimeout::from_millis(2_000),
        }),
        BrowserToolRequest::Events(EventsRequest {
            session_id: session_id()?,
            since: BrowserEventCursor::zero(),
        }),
        BrowserToolRequest::Close(CloseRequest {
            session_id: session_id()?,
        }),
    ])
}

#[test]
fn test_descriptors_compact_surface_has_exactly_the_seven_plan_tools() {
    let tools = BrowserToolSet::new(Arc::new(NeverUsedHost));
    let descriptors = tools.descriptors();
    let names: Vec<&str> = descriptors.iter().map(|d| d.name.as_str()).collect();
    let capabilities: Vec<&str> = descriptors.iter().map(|d| d.capability.as_str()).collect();

    assert_eq!(
        names,
        [
            "browser.open",
            "browser.observe",
            "browser.find",
            "browser.act",
            "browser.wait",
            "browser.events",
            "browser.close",
        ]
    );
    assert_eq!(
        capabilities,
        [
            "harwness.browser.open@1",
            "harwness.browser.observe@1",
            "harwness.browser.find@1",
            "harwness.browser.act@1",
            "harwness.browser.wait@1",
            "harwness.browser.events@1",
            "harwness.browser.close@1",
        ]
    );
    assert!(
        descriptors
            .iter()
            .all(|d| d.input_schema["additionalProperties"] == serde_json::json!(false))
    );
}

#[test]
fn test_request_envelopes_round_trip_through_json() -> TestResult {
    for request in requests()? {
        let json = serde_json::to_value(&request).map_err(ctx("typed request serializes"))?;
        let decoded: BrowserToolRequest =
            serde_json::from_value(json).map_err(ctx("typed request deserializes"))?;
        assert_eq!(decoded, request);
    }
    Ok(())
}

#[test]
fn test_response_envelopes_round_trip_through_json() -> TestResult {
    let responses = vec![
        BrowserToolResponse::Open(OpenResponse {
            session_id: session_id()?,
            primary_context_id: context_id()?,
        }),
        BrowserToolResponse::Observe(ObserveResponse {
            observation: BrowserObservation {
                session_id: session_id()?,
                context_id: context_id()?,
                revision: BrowserObservationRevision::initial(),
                url: url("https://erp.example.com/inbox")?,
                title: "Inbox".to_owned(),
                document_identity: DocumentIdentity::new("doc"),
                elements: Vec::new(),
                artifacts: Vec::new(),
                event_cursor: BrowserEventCursor::zero(),
            },
        }),
        BrowserToolResponse::Find(FindResponse {
            element: ObservedElement::new("reply", "button"),
        }),
        BrowserToolResponse::Act(ActResponse {
            outcome: ActionOutcome::new(EffectId::new()),
        }),
        BrowserToolResponse::Wait(WaitResponse {
            outcome: WaitOutcome::new(true, 0, WaitCondition::NavigationComplete),
        }),
        BrowserToolResponse::Events(EventsResponse { events: Vec::new() }),
        BrowserToolResponse::Close(CloseResponse {
            session_id: session_id()?,
        }),
    ];

    for response in responses {
        let json = serde_json::to_value(&response).map_err(ctx("typed response serializes"))?;
        let decoded: BrowserToolResponse =
            serde_json::from_value(json).map_err(ctx("typed response deserializes"))?;
        assert_eq!(decoded, response);
    }
    Ok(())
}

#[test]
fn test_prepare_open_takes_origins_profile_and_limits_from_grant_only() -> TestResult {
    let limits = BrowserLimits::default().with_max_actions_per_session(7);
    let tools = BrowserToolSet::with_open_policy(
        Arc::new(NeverUsedHost),
        BrowserOpenPolicy::grant(grant()?.with_limits(limits)),
    );
    let prepared = tools
        .prepare(BrowserToolRequest::Open(open_request()?))
        .map_err(ctx("valid open request prepares"))?;

    let scope = prepared.scope();
    assert_eq!(scope.session_id, None);
    assert_eq!(scope.action, BrowserScopeAction::Open);
    assert_eq!(
        scope.origins,
        [
            "https://erp.example.com".to_owned(),
            "https://assets.example.com".to_owned()
        ]
    );
    let PreparedBrowserRequest::Open(open) = prepared.request() else {
        return Err(TestError::Unexpected(
            "prepared request remains browser.open".to_owned(),
        ));
    };
    assert_eq!(open.profile, ProfilePolicy::Ephemeral);
    assert_eq!(
        open.allowed_origins,
        policy(&["https://erp.example.com", "https://assets.example.com"])?
    );
    assert_eq!(
        open.authentication_origins,
        policy(&["https://sso.example.org"])?
    );
    assert_eq!(open.limits, limits);
    assert_eq!(open.start_url, url("https://erp.example.com/inbox")?);
    Ok(())
}

#[test]
fn test_prepare_open_rejects_model_json_with_authority_fields() -> TestResult {
    let mut json = serde_json::to_value(BrowserToolRequest::Open(open_request()?))
        .map_err(ctx("serializes"))?;
    json["Open"]["allowed_origins"] = serde_json::json!(["https://evil.example"]);
    assert!(serde_json::from_value::<BrowserToolRequest>(json).is_err());

    let mut json = serde_json::to_value(open_request()?).map_err(ctx("serializes"))?;
    json["profile"] = serde_json::json!({"Persistent": {"binding": "model"}});
    assert!(serde_json::from_value::<OpenRequest>(json).is_err());
    Ok(())
}

#[test]
fn test_prepare_open_rejects_authentication_origin_as_start() -> TestResult {
    let mut request = open_request()?;
    request.start_url = url("https://sso.example.org/login")?;

    let Err(error) = configured_tools()?.prepare(BrowserToolRequest::Open(request)) else {
        return Err(TestError::Unexpected(
            "authentication origins are redirect-only".to_owned(),
        ));
    };

    assert!(matches!(error, Error::OriginNotAllowed { .. }), "{error}");
    Ok(())
}

#[test]
fn test_prepare_unconfigured_tool_set_rejects_browser_open_fail_closed() -> TestResult {
    let Err(error) = BrowserToolSet::new(Arc::new(NeverUsedHost))
        .prepare(BrowserToolRequest::Open(open_request()?))
    else {
        return Err(TestError::Unexpected(
            "default tool set must not grant browser.open authority".to_owned(),
        ));
    };
    assert!(matches!(error, Error::OriginNotAllowed { .. }));

    let Err(error) = prepare_browser_call(BrowserToolRequest::Open(open_request()?)) else {
        return Err(TestError::Unexpected(
            "policy-free preparation never opens".to_owned(),
        ));
    };
    assert!(matches!(error, Error::InvalidArgument { .. }));
    Ok(())
}

#[test]
fn test_prepare_act_preserves_session_context_origin_and_revision() -> TestResult {
    let revision = BrowserObservationRevision::initial().next();
    let prepared = configured_tools()?
        .prepare(BrowserToolRequest::Act(ActRequest {
            session_id: session_id()?,
            request: ActionRequest::new(
                context_id()?,
                BrowserAction::Navigate {
                    url: url("https://erp.example.com/ticket/42")?,
                },
            )
            .with_expected_revision(revision),
        }))
        .map_err(ctx("valid action prepares"))?;

    let scope = prepared.scope();
    assert_eq!(scope.session_id, Some(session_id()?));
    assert_eq!(scope.context_id, Some(context_id()?));
    assert_eq!(scope.action, BrowserScopeAction::Act);
    assert_eq!(scope.origins, ["https://erp.example.com".to_owned()]);
    assert_eq!(scope.expected_revision, Some(revision));
    Ok(())
}

#[test]
fn test_prepare_rejects_element_action_without_revision() -> TestResult {
    let Err(error) = configured_tools()?.prepare(BrowserToolRequest::Act(ActRequest {
        session_id: session_id()?,
        request: ActionRequest::new(
            context_id()?,
            BrowserAction::Click {
                target: Target::new(Selector::Css("button".to_owned())),
            },
        ),
    })) else {
        return Err(TestError::Unexpected(
            "element actions need a revision".to_owned(),
        ));
    };
    assert!(matches!(error, Error::InvalidArgument { .. }));
    Ok(())
}

#[test]
fn test_prepare_applies_grant_limits_to_actions_selectors_and_waits() -> TestResult {
    let limits = BrowserLimits::default()
        .with_max_text_bytes(4)
        .with_max_selector_bytes(8)
        .with_max_wait_ms(1_000);
    let tools = BrowserToolSet::with_open_policy(
        Arc::new(NeverUsedHost),
        BrowserOpenPolicy::grant(grant()?.with_limits(limits)),
    );

    let too_long_text = BrowserToolRequest::Act(ActRequest {
        session_id: session_id()?,
        request: ActionRequest::new(
            context_id()?,
            BrowserAction::Type {
                target: Target::new(Selector::Id("q".to_owned())),
                text: "hello".to_owned(),
            },
        )
        .with_expected_revision(BrowserObservationRevision::initial()),
    });
    let too_long_selector = BrowserToolRequest::Observe(ObserveRequest {
        session_id: session_id()?,
        context_id: context_id()?,
        mode: ObservationMode::DomSelection {
            selector: Selector::Css("main > article".to_owned()),
        },
    });
    let too_long_find = BrowserToolRequest::Find(FindRequest {
        session_id: session_id()?,
        context_id: context_id()?,
        target: Target::new(Selector::Css("main > article".to_owned())),
        revision: BrowserObservationRevision::initial(),
    });
    let too_long_wait = BrowserToolRequest::Wait(WaitRequest {
        session_id: session_id()?,
        context_id: context_id()?,
        condition: WaitCondition::NavigationComplete,
        timeout: WaitTimeout::from_millis(1_001),
    });

    for request in [
        too_long_text,
        too_long_selector,
        too_long_find,
        too_long_wait,
    ] {
        let Err(error) = tools.prepare(request.clone()) else {
            return Err(TestError::Unexpected("grant limits apply".to_owned()));
        };
        assert!(
            matches!(error, Error::InvalidArgument { .. }),
            "{request:?}: {error}"
        );
    }
    Ok(())
}

#[test]
fn test_prepare_rejects_upload_and_custom_script_json() -> TestResult {
    let act = serde_json::json!({"Act": {
        "session_id": session_id()?,
        "request": {"context_id": context_id()?, "expected_revision": 0,
            "action": {"Upload": {"target": {"primary": {"Css": "input"}, "fallbacks": []}, "file_path": "/etc/passwd"}}}
    }});
    assert!(serde_json::from_value::<BrowserToolRequest>(act).is_err());

    let wait = serde_json::json!({"Wait": {
        "session_id": session_id()?, "context_id": context_id()?,
        "condition": {"CustomScript": {"predicate": "return true"}}, "timeout": 1000
    }});
    assert!(serde_json::from_value::<BrowserToolRequest>(wait).is_err());
    Ok(())
}

// Host that must never be reached by host-free contract tests. Any call
// returns an `Err` (instead of panicking) so a bug that does reach this
// host is reported as a failed assertion in the caller, not a process abort.
struct NeverUsedHost;

#[async_trait]
impl BrowserHost for NeverUsedHost {
    async fn open(
        &self,
        _request: OpenBrowserRequest,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        Err(Error::CapabilityUnavailable {
            detail: "host-free contract tests must not open sessions".to_owned(),
        })
    }

    async fn session(&self, _id: &BrowserSessionId) -> harw_browser::Result<BrowserSessionHandle> {
        Err(Error::CapabilityUnavailable {
            detail: "host-free contract tests must not look up sessions".to_owned(),
        })
    }

    async fn close(&self, _id: &BrowserSessionId) -> harw_browser::Result<()> {
        Err(Error::CapabilityUnavailable {
            detail: "host-free contract tests must not close sessions".to_owned(),
        })
    }
}
