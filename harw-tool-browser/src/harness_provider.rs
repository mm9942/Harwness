//! Harwness extension boundary for the browser tool collection.
//!
//! The provider deliberately owns the concrete [`BrowserToolSet`] while the
//! executor only receives an `Arc` to it.  This keeps the authoritative
//! browser host and its open policy out of model-controlled tool arguments.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_extension_api::contributors::ToolProvider;
use harw_sandbox::Permission;
use harw_tools::{
    AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, ToolCall,
    ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput, ToolSpec,
    ToolsError,
};

use crate::{
    ActRequest, BrowserToolDescriptor, BrowserToolRequest, BrowserToolSet, CloseRequest,
    EventsRequest, FindRequest, ObserveRequest, OpenRequest, WaitRequest,
};

const BROWSER_TOOL_NAMES: [&str; 7] = [
    "browser.open",
    "browser.observe",
    "browser.find",
    "browser.act",
    "browser.wait",
    "browser.events",
    "browser.close",
];

/// Provides Harwness' closed seven-operation browser surface.
///
/// Browser operations remain non-parallel-safe: even an observation can race
/// a preceding navigation or close on the same browser session.
pub struct HarwnessBrowserToolProvider {
    tool_set: Arc<BrowserToolSet>,
}

impl HarwnessBrowserToolProvider {
    /// Constructs a provider from its authoritative browser tool set.
    #[must_use]
    pub fn new(tool_set: BrowserToolSet) -> Self {
        Self {
            tool_set: Arc::new(tool_set),
        }
    }

    /// Constructs a provider from a shared, authoritative browser tool set.
    #[must_use]
    pub fn from_shared(tool_set: Arc<BrowserToolSet>) -> Self {
        Self { tool_set }
    }
}

impl ToolProvider for HarwnessBrowserToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.tool_set
            .descriptors()
            .iter()
            .map(browser_tool_spec)
            .collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        let name = name.as_str();
        BROWSER_TOOL_NAMES.contains(&name).then(|| {
            Arc::new(BrowserToolExecutor {
                tool_set: Arc::clone(&self.tool_set),
                tool_name: name.to_owned(),
            }) as Arc<dyn ToolExecutor>
        })
    }
}

fn browser_tool_spec(descriptor: &BrowserToolDescriptor) -> ToolSpec {
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(&descriptor.name),
        description: format!(
            "{} ({}) through the host-authorized browser session boundary.",
            descriptor.name, descriptor.capability
        ),
        parameters: browser_tool_parameters(&descriptor.name),
        strict: false,
    })
}

/// String-typed leaf schema used for opaque identifier fields.
fn string_field(description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

/// Nested-object leaf schema for fields whose full shape is validated by the
/// executor's `serde_json` deserialization rather than by the advertised
/// schema (e.g. tagged enums such as `Action`/`WaitCondition`/`Target`).
fn object_field(description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        description: Some(description.to_owned()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(true))),
        ..Default::default()
    }
}

/// Builds the real, per-operation argument shape for each of the seven
/// browser tools, expressed as top-level property names and coarse types.
/// Deeply nested tagged-union fields (`request`, `target`, `condition`,
/// `mode`, `since`, `revision`) are advertised as permissive objects because
/// their concrete shape is validated by the executor's `serde_json`
/// deserialization (see `parse_request`) rather than by JSON Schema.
fn browser_tool_parameters(tool_name: &str) -> JsonSchema {
    let mut properties = BTreeMap::new();

    let required: Vec<String> = match tool_name {
        "browser.open" => {
            properties.insert(
                "request".to_owned(),
                object_field(
                    "Requested navigation/session policy (start_url, headless, profile,                      bidi, allowed_origins, viewport). Origin and profile fields are never                      trusted from model input; the host-owned grant replaces them before dispatch.",
                ),
            );
            vec!["request".to_owned()]
        }
        "browser.observe" => {
            properties.insert("session_id".to_owned(), string_field("Target browser session UUID."));
            properties.insert("context_id".to_owned(), string_field("Target browsing context UUID."));
            properties.insert(
                "mode".to_owned(),
                object_field("Observation mode (e.g. page summary or full accessibility tree)."),
            );
            vec![
                "session_id".to_owned(),
                "context_id".to_owned(),
                "mode".to_owned(),
            ]
        }
        "browser.find" => {
            properties.insert("session_id".to_owned(), string_field("Target browser session UUID."));
            properties.insert("context_id".to_owned(), string_field("Target browsing context UUID."));
            properties.insert(
                "target".to_owned(),
                object_field("Element selector (e.g. CSS selector or role-based query)."),
            );
            properties.insert(
                "revision".to_owned(),
                object_field("Observation revision this find call is scoped against."),
            );
            vec![
                "session_id".to_owned(),
                "context_id".to_owned(),
                "target".to_owned(),
                "revision".to_owned(),
            ]
        }
        "browser.act" => {
            properties.insert("session_id".to_owned(), string_field("Target browser session UUID."));
            properties.insert(
                "request".to_owned(),
                object_field(
                    "Action request: a context ID plus one tagged browser action                      (click, type, scroll, etc.).",
                ),
            );
            vec!["session_id".to_owned(), "request".to_owned()]
        }
        "browser.wait" => {
            properties.insert("session_id".to_owned(), string_field("Target browser session UUID."));
            properties.insert("context_id".to_owned(), string_field("Target browsing context UUID."));
            properties.insert(
                "condition".to_owned(),
                object_field("Tagged wait condition (e.g. navigation complete, element visible)."),
            );
            properties.insert(
                "timeout".to_owned(),
                object_field("Maximum wait duration before the call fails."),
            );
            vec![
                "session_id".to_owned(),
                "context_id".to_owned(),
                "condition".to_owned(),
                "timeout".to_owned(),
            ]
        }
        "browser.events" => {
            properties.insert("session_id".to_owned(), string_field("Target browser session UUID."));
            properties.insert(
                "since".to_owned(),
                object_field("Event cursor to resume from (zero cursor reads from the start)."),
            );
            vec!["session_id".to_owned(), "since".to_owned()]
        }
        // `_` already matches `"browser.close"`; listing it explicitly next to
        // `_` was redundant (clippy::wildcard_in_or_patterns) and not a
        // distinct case — `browser.close` deliberately shares this
        // single-`session_id`-required fallback shape with any unrecognized
        // tool name.
        _ => {
            properties.insert("session_id".to_owned(), string_field("Browser session UUID to close."));
            vec!["session_id".to_owned()]
        }
    };

    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(properties),
        required: Some(required),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

#[derive(Clone)]
struct BrowserToolExecutor {
    tool_set: Arc<BrowserToolSet>,
    tool_name: String,
}

impl ToolExecutor for BrowserToolExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if call.name.as_str() != self.tool_name {
                return Err(ToolsError::InvalidArguments {
                    name: self.tool_name.clone(),
                    reason: "tool call name does not match its selected executor".to_owned(),
                });
            }

            let request = parse_request(&self.tool_name, call.arguments.clone())?;

            if let Some(denied) =
                harw_tools::require_permission(context, Permission::NetworkAccess, &self.tool_name)
            {
                return Ok(denied);
            }

            let prepared = self.tool_set.prepare(request).map_err(|error| {
                ToolsError::ExecutionFailed(format!("{}: {error}", self.tool_name))
            })?;
            let response = self.tool_set.dispatch(prepared).await.map_err(|error| {
                ToolsError::ExecutionFailed(format!("{}: {error}", self.tool_name))
            })?;

            serde_json::to_value(response)
                .map(ToolOutput::json)
                .map_err(ToolsError::SerdeJson)
        })
    }
}

fn parse_request(
    name: &str,
    arguments: serde_json::Value,
) -> Result<BrowserToolRequest, ToolsError> {
    let invalid = |error: serde_json::Error| ToolsError::InvalidArguments {
        name: name.to_owned(),
        reason: error.to_string(),
    };

    match name {
        "browser.open" => serde_json::from_value::<OpenRequest>(arguments)
            .map(BrowserToolRequest::Open)
            .map_err(invalid),
        "browser.observe" => serde_json::from_value::<ObserveRequest>(arguments)
            .map(BrowserToolRequest::Observe)
            .map_err(invalid),
        "browser.find" => serde_json::from_value::<FindRequest>(arguments)
            .map(BrowserToolRequest::Find)
            .map_err(invalid),
        "browser.act" => serde_json::from_value::<ActRequest>(arguments)
            .map(BrowserToolRequest::Act)
            .map_err(invalid),
        "browser.wait" => serde_json::from_value::<WaitRequest>(arguments)
            .map(BrowserToolRequest::Wait)
            .map_err(invalid),
        "browser.events" => serde_json::from_value::<EventsRequest>(arguments)
            .map(BrowserToolRequest::Events)
            .map_err(invalid),
        "browser.close" => serde_json::from_value::<CloseRequest>(arguments)
            .map(BrowserToolRequest::Close)
            .map_err(invalid),
        _ => Err(ToolsError::InvalidArguments {
            name: name.to_owned(),
            reason: "unknown browser tool".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use harw_browser::{
        action::{ActionRequest, BrowserAction},
        ids::{BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId},
        observation::ObservationMode,
        policy::{BiDiRequirement, OpenBrowserRequest, OriginPolicy, ProfilePolicy},
        selector::{Selector, Target},
        wait::{WaitCondition, WaitTimeout},
    };

    use super::{
        ActRequest, BrowserToolDescriptor, BrowserToolRequest, CloseRequest, EventsRequest,
        FindRequest, ObserveRequest, OpenRequest, WaitRequest, browser_tool_spec, parse_request,
    };

    fn session_id() -> BrowserSessionId {
        BrowserSessionId::from_str("00000000-0000-0000-0000-000000000001")
            .expect("fixed UUID is valid")
    }

    fn context_id() -> BrowserContextId {
        BrowserContextId::from_str("00000000-0000-0000-0000-000000000002")
            .expect("fixed UUID is valid")
    }

    fn open_request() -> OpenRequest {
        OpenRequest {
            request: OpenBrowserRequest {
                start_url: url::Url::parse("https://example.com/start")
                    .expect("fixed test URL is valid"),
                headless: true,
                profile: ProfilePolicy::Ephemeral,
                bidi: BiDiRequirement::Preferred,
                allowed_origins: OriginPolicy::new(vec!["example.com".to_owned()], true),
                viewport: None,
            },
        }
    }

    fn request_arguments(request: &BrowserToolRequest) -> serde_json::Value {
        let payload = match request {
            BrowserToolRequest::Open(request) => serde_json::to_value(request),
            BrowserToolRequest::Observe(request) => serde_json::to_value(request),
            BrowserToolRequest::Find(request) => serde_json::to_value(request),
            BrowserToolRequest::Act(request) => serde_json::to_value(request),
            BrowserToolRequest::Wait(request) => serde_json::to_value(request),
            BrowserToolRequest::Events(request) => serde_json::to_value(request),
            BrowserToolRequest::Close(request) => serde_json::to_value(request),
        };

        payload.expect("all public browser operation payloads serialize")
    }

    #[test]
    fn parse_request_accepts_each_browser_operation_shape() {
        let session_id = session_id();
        let context_id = context_id();
        let target = Target::new(Selector::Css("main".to_owned()));
        let requests = [
            ("browser.open", BrowserToolRequest::Open(open_request())),
            (
                "browser.observe",
                BrowserToolRequest::Observe(ObserveRequest {
                    session_id,
                    context_id,
                    mode: ObservationMode::PageSummary,
                }),
            ),
            (
                "browser.find",
                BrowserToolRequest::Find(FindRequest {
                    session_id,
                    context_id,
                    target: target.clone(),
                    revision: BrowserObservationRevision::initial(),
                }),
            ),
            (
                "browser.act",
                BrowserToolRequest::Act(ActRequest {
                    session_id,
                    request: ActionRequest::new(context_id, BrowserAction::Click { target }),
                }),
            ),
            (
                "browser.wait",
                BrowserToolRequest::Wait(WaitRequest {
                    session_id,
                    context_id,
                    condition: WaitCondition::NavigationComplete,
                    timeout: WaitTimeout::from_millis(1),
                }),
            ),
            (
                "browser.events",
                BrowserToolRequest::Events(EventsRequest {
                    session_id,
                    since: BrowserEventCursor::zero(),
                }),
            ),
            (
                "browser.close",
                BrowserToolRequest::Close(CloseRequest { session_id }),
            ),
        ];

        for (name, expected) in requests {
            let arguments = request_arguments(&expected);
            let parsed = parse_request(name, arguments)
                .expect("serialized request shape is accepted by its browser operation");
            assert_eq!(parsed, expected, "{name} must preserve its request");
        }
    }

    #[test]
    fn advertised_schema_declares_the_real_open_argument_shape() {
        let descriptor = BrowserToolDescriptor {
            name: "browser.open".to_owned(),
            capability: "harwness.browser.open@1".to_owned(),
            input_schema: serde_json::json!({}),
        };

        let spec = browser_tool_spec(&descriptor);
        let harw_tools::ToolSpec::Function(function) = spec;

        assert!(
            !function.strict,
            "the nested request/target/condition fields remain permissive objects"
        );
        let parameters = serde_json::to_value(&function.parameters).expect("schema serializes");
        assert_eq!(parameters["type"], serde_json::json!("object"));
        assert_eq!(parameters["additionalProperties"], serde_json::json!(false));
        assert_eq!(
            parameters["required"],
            serde_json::json!(["request"]),
            "browser.open must require its request field"
        );
        assert!(
            parameters["properties"]["request"].is_object(),
            "browser.open must declare a request property"
        );
    }

    #[test]
    fn every_browser_tool_declares_its_top_level_argument_names() {
        let expected: &[(&str, &[&str])] = &[
            ("browser.open", &["request"]),
            ("browser.observe", &["session_id", "context_id", "mode"]),
            (
                "browser.find",
                &["session_id", "context_id", "target", "revision"],
            ),
            ("browser.act", &["session_id", "request"]),
            (
                "browser.wait",
                &["session_id", "context_id", "condition", "timeout"],
            ),
            ("browser.events", &["session_id", "since"]),
            ("browser.close", &["session_id"]),
        ];

        for (name, fields) in expected {
            let descriptor = BrowserToolDescriptor {
                name: (*name).to_owned(),
                capability: format!("harwness.{name}@1"),
                input_schema: serde_json::json!({}),
            };
            let spec = browser_tool_spec(&descriptor);
            let harw_tools::ToolSpec::Function(function) = spec;
            let parameters =
                serde_json::to_value(&function.parameters).expect("schema serializes");
            let properties = parameters["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{name} must declare object properties"));
            for field in *fields {
                assert!(
                    properties.contains_key(*field),
                    "{name} must declare property '{field}'"
                );
            }
            let required: Vec<String> = parameters["required"]
                .as_array()
                .unwrap_or_else(|| panic!("{name} must declare required fields"))
                .iter()
                .map(|value| value.as_str().expect("required entries are strings").to_owned())
                .collect();
            assert_eq!(&required, fields, "{name} required fields must match its argument shape");
        }
    }

    #[test]
    fn parse_request_rejects_unknown_browser_operation() {
        let error = parse_request("browser.unknown", serde_json::json!({}))
            .expect_err("unknown browser operation must be rejected");

        assert!(matches!(
            error,
            harw_tools::ToolsError::InvalidArguments { name, .. } if name == "browser.unknown"
        ));
    }

    #[test]
    fn parse_request_rejects_malformed_open_arguments() {
        let error = parse_request(
            "browser.open",
            serde_json::json!({"request": "not-an-open-request"}),
        )
        .expect_err("malformed open request must be rejected");

        assert!(matches!(
            error,
            harw_tools::ToolsError::InvalidArguments { name, .. } if name == "browser.open"
        ));
    }
}
