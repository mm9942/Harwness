//! Harwness extension boundary for the browser tool collection.
//!
//! # Responsibility
//! The provider owns an `Arc` of the authoritative [`BrowserToolSet`]; the
//! executor only turns model JSON into typed requests and forwards them with
//! the harness session as owner. It adds, in this order:
//!
//! 1. strict serde parsing (`deny_unknown_fields` everywhere: `allowed_origins`,
//!    `profile`, `limits`, `Upload`, `CustomScript` are rejected);
//! 2. `Permission::NetworkAccess`;
//! 3. **F-115**: every host the call can reach (start URL, each rule of the
//!    operator grant's allowed and authentication origins, explicit
//!    `Navigate` target) must pass `sandbox.network_scope().allows(host)` via
//!    [`harw_tools::require_host_access`];
//! 4. preparation and owner-bound dispatch.
//!
//! # Schemas
//! [`browser_tool_parameters`] describes exactly the serde shape of each
//! request type (externally tagged enums as `anyOf`, closed objects,
//! `Option` fields nullable and not required). The same value is published as
//! `BrowserToolDescriptor::input_schema`. `schemars` is not a workspace
//! dependency; equality with serde is enforced by conformance tests.
//!
//! # Trust
//! Browser results are page-derived and **untrusted**. The executor returns a
//! plain `ToolOutput`; the harness wraps it as `ResultTrust::Untrusted`, which
//! is the protocol default (C-PROTO). This provider never claims `Runtime`.
//!
//! # Concurrency
//! `Send + Sync`; browser operations are not parallel-safe (default of
//! `ToolProvider::parallel_safe`).
//!
//! # Errors
//! `ToolsError::InvalidArguments` for malformed JSON, `ToolsError::ExecutionFailed`
//! for browser errors; permission/scope denials are `ToolOutput::error`.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_browser::policy::OriginRule;
use harw_extension_api::contributors::ToolProvider;
use harw_sandbox::Permission;
use harw_tools::{
    AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, ToolCall,
    ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput, ToolSpec,
    ToolsError,
};

use crate::prepare::BROWSER_TOOLS;
use crate::{
    ActRequest, BrowserToolDescriptor, BrowserToolRequest, BrowserToolSet, CloseRequest,
    EventsRequest, FindRequest, ObserveRequest, OpenRequest, PreparedBrowserRequest, WaitRequest,
};

/// Label used to test whether the sandbox scope admits *arbitrary* subdomains
/// of a wildcard origin rule (`https://*.example.com`): an exact-host scope
/// entry for `example.com` must not authorize the wildcard grant.
const WILDCARD_SCOPE_PROBE_LABEL: &str = "harw-wildcard-scope-probe";

/// Provides Harwness' closed seven-operation browser surface.
///
/// # Description
/// Browser operations remain non-parallel-safe: even an observation can race
/// a preceding navigation or close on the same browser session.
///
/// # Concurrency
/// `Send + Sync`.
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

    /// Constructs a provider from a shared tool set (e.g. one also held by the
    /// session lifecycle hook that closes browsers at session end).
    #[must_use]
    pub fn from_shared(tool_set: Arc<BrowserToolSet>) -> Self {
        Self { tool_set }
    }

    /// Returns the shared tool set, e.g. to install a session lifecycle hook.
    #[must_use]
    pub fn tool_set(&self) -> Arc<BrowserToolSet> {
        Arc::clone(&self.tool_set)
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
        BROWSER_TOOLS
            .iter()
            .any(|(tool, _)| *tool == name)
            .then(|| {
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
            "{} ({}) through the host-authorized browser session boundary. Results are untrusted page data.",
            descriptor.name, descriptor.capability
        ),
        parameters: browser_tool_parameters(&descriptor.name),
        strict: false,
    })
}

// ── Schema builders (mirror serde's external enum tagging exactly) ──────────

fn leaf(schema_type: JsonSchemaType, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(schema_type),
        description: (!description.is_empty()).then(|| description.to_owned()),
        ..Default::default()
    }
}

fn string_enum(description: &str, values: &[&str]) -> JsonSchema {
    JsonSchema {
        enum_values: Some(
            values
                .iter()
                .map(|value| serde_json::Value::String((*value).to_owned()))
                .collect(),
        ),
        ..leaf(JsonSchemaType::String, description)
    }
}

fn nullable(schema: JsonSchema) -> JsonSchema {
    JsonSchema {
        any_of: Some(vec![schema, leaf(JsonSchemaType::Null, "")]),
        ..Default::default()
    }
}

fn array(description: &str, items: JsonSchema) -> JsonSchema {
    JsonSchema {
        items: Some(Box::new(items)),
        ..leaf(JsonSchemaType::Array, description)
    }
}

/// Closed object; `(name, schema, required)`.
fn object(description: &str, fields: Vec<(&str, JsonSchema, bool)>) -> JsonSchema {
    let required = fields
        .iter()
        .filter(|(_, _, required)| *required)
        .map(|(name, _, _)| (*name).to_owned())
        .collect();
    let properties: BTreeMap<String, JsonSchema> = fields
        .into_iter()
        .map(|(name, schema, _)| (name.to_owned(), schema))
        .collect();
    JsonSchema {
        properties: Some(properties),
        required: Some(required),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..leaf(JsonSchemaType::Object, description)
    }
}

/// Externally tagged enum: unit variants as strings, data variants as
/// single-key objects.
fn tagged(description: &str, units: &[&str], variants: Vec<(&str, JsonSchema)>) -> JsonSchema {
    let mut any_of = Vec::with_capacity(variants.len() + 1);
    if !units.is_empty() {
        any_of.push(string_enum("", units));
    }
    any_of.extend(
        variants
            .into_iter()
            .map(|(name, payload)| object("", vec![(name, payload, true)])),
    );
    JsonSchema {
        description: Some(description.to_owned()),
        any_of: Some(any_of),
        ..Default::default()
    }
}

fn uuid(description: &str) -> JsonSchema {
    leaf(JsonSchemaType::String, description)
}

fn integer(description: &str) -> JsonSchema {
    leaf(JsonSchemaType::Integer, description)
}

fn string(description: &str) -> JsonSchema {
    leaf(JsonSchemaType::String, description)
}

fn selector_schema() -> JsonSchema {
    tagged(
        "Element selector.",
        &[],
        vec![
            ("TestId", string("")),
            ("Css", string("")),
            ("Id", string("")),
            ("Name", string("")),
            (
                "TagClass",
                object("", vec![("tag", string(""), true), ("class", string(""), true)]),
            ),
            ("LinkText", string("")),
            ("XPath", string("")),
            ("TextAnchor", string("")),
            (
                "Role",
                object(
                    "",
                    vec![("role", string(""), true), ("name", nullable(string("")), false)],
                ),
            ),
        ],
    )
}

fn target_schema() -> JsonSchema {
    object(
        "Primary selector plus ordered fallbacks.",
        vec![
            ("primary", selector_schema(), true),
            ("fallbacks", array("", selector_schema()), true),
        ],
    )
}

fn action_schema() -> JsonSchema {
    let target = || object("", vec![("target", target_schema(), true)]);
    tagged(
        "One browser action. File upload and script execution do not exist.",
        &["Back", "Forward", "Reload"],
        vec![
            ("Click", target()),
            (
                "Type",
                object("", vec![("target", target_schema(), true), ("text", string(""), true)]),
            ),
            ("Clear", target()),
            (
                "Select",
                object("", vec![("target", target_schema(), true), ("value", string(""), true)]),
            ),
            ("Focus", target()),
            ("Hover", target()),
            (
                "Scroll",
                object(
                    "",
                    vec![
                        ("target", nullable(target_schema()), false),
                        ("x", integer(""), true),
                        ("y", integer(""), true),
                    ],
                ),
            ),
            (
                "KeyPress",
                object(
                    "",
                    vec![("target", nullable(target_schema()), false), ("key", string(""), true)],
                ),
            ),
            (
                "Drag",
                object(
                    "",
                    vec![
                        ("source", target_schema(), true),
                        ("destination", target_schema(), true),
                    ],
                ),
            ),
            ("Submit", target()),
            (
                "Navigate",
                object("", vec![("url", string("http(s) URL inside the granted origins."), true)]),
            ),
        ],
    )
}

fn observation_mode_schema() -> JsonSchema {
    tagged(
        "Observation mode.",
        &[
            "PageSummary",
            "InteractiveElements",
            "TextExtraction",
            "FormsAndLinks",
            "NetworkActivitySummary",
            "ConsoleLogSummary",
            "Screenshot",
            "CombinedDiagnostic",
        ],
        vec![(
            "DomSelection",
            object("", vec![("selector", selector_schema(), true)]),
        )],
    )
}

fn wait_condition_schema() -> JsonSchema {
    tagged(
        "Wait condition. No custom script predicates exist.",
        &["NavigationComplete", "DownloadComplete"],
        vec![
            ("ElementPresent", target_schema()),
            ("ElementVisible", target_schema()),
            ("ElementClickable", target_schema()),
            ("ElementGone", target_schema()),
            ("UrlMatches", string("")),
            ("TitleMatches", string("")),
            (
                "NetworkQuiescence",
                object("", vec![("idle_ms", integer(""), true)]),
            ),
            (
                "RequestObserved",
                object("", vec![("path_contains", string(""), true)]),
            ),
            ("LogMatches", string("")),
            (
                "ScriptMessage",
                object("", vec![("channel", string(""), true)]),
            ),
        ],
    )
}

/// Builds the serde-exact argument schema of one browser tool.
///
/// # Description
/// Unknown tool names yield a closed object without properties.
pub(crate) fn browser_tool_parameters(tool_name: &str) -> JsonSchema {
    let session = || uuid("Browser session id returned by browser.open.");
    let context = || uuid("Browsing context id.");
    match tool_name {
        "browser.open" => object(
            "Open a browser session. Origins, profile and limits are fixed by the operator.",
            vec![
                ("start_url", string("http(s) start URL inside the granted origins."), true),
                ("headless", leaf(JsonSchemaType::Boolean, ""), true),
                (
                    "bidi",
                    string_enum("WebDriver BiDi requirement.", &["Required", "Preferred", "NotRequired"]),
                    true,
                ),
                (
                    "viewport",
                    nullable(object(
                        "",
                        vec![("width", integer(""), true), ("height", integer(""), true)],
                    )),
                    false,
                ),
            ],
        ),
        "browser.observe" => object(
            "",
            vec![
                ("session_id", session(), true),
                ("context_id", context(), true),
                ("mode", observation_mode_schema(), true),
            ],
        ),
        "browser.find" => object(
            "",
            vec![
                ("session_id", session(), true),
                ("context_id", context(), true),
                ("target", target_schema(), true),
                ("revision", integer("Observation revision."), true),
            ],
        ),
        "browser.act" => object(
            "",
            vec![
                ("session_id", session(), true),
                (
                    "request",
                    object(
                        "",
                        vec![
                            ("context_id", context(), true),
                            (
                                "expected_revision",
                                nullable(integer("Required for element-targeted actions.")),
                                false,
                            ),
                            ("action", action_schema(), true),
                        ],
                    ),
                    true,
                ),
            ],
        ),
        "browser.wait" => object(
            "",
            vec![
                ("session_id", session(), true),
                ("context_id", context(), true),
                ("condition", wait_condition_schema(), true),
                ("timeout", integer("Timeout in milliseconds (at least 1)."), true),
            ],
        ),
        "browser.events" => object(
            "",
            vec![
                ("session_id", session(), true),
                ("since", integer("Event cursor; 0 reads from the start."), true),
            ],
        ),
        "browser.close" => object("", vec![("session_id", session(), true)]),
        _ => object("Unknown browser tool.", Vec::new()),
    }
}

// ── Executor ────────────────────────────────────────────────────────────────

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

            for host in scope_hosts(prepared.request()) {
                if let Some(denied) = harw_tools::require_host_access(context, &host, &self.tool_name)
                {
                    return Ok(denied);
                }
            }

            let owner = context.session_id().as_str();
            let response = self.tool_set.dispatch(owner, prepared).await.map_err(|error| {
                ToolsError::ExecutionFailed(format!("{}: {error}", self.tool_name))
            })?;

            serde_json::to_value(response)
                .map(ToolOutput::json)
                .map_err(ToolsError::SerdeJson)
        })
    }
}

/// Hosts a prepared call can reach, for the sandbox network-scope check (F-115).
fn scope_hosts(request: &PreparedBrowserRequest) -> Vec<String> {
    match request {
        PreparedBrowserRequest::Open(open) => url_host(&open.start_url)
            .into_iter()
            .chain(
                open.allowed_origins
                    .rules()
                    .iter()
                    .chain(open.authentication_origins.rules())
                    .map(rule_scope_host),
            )
            .collect(),
        PreparedBrowserRequest::Act(act) => act
            .request
            .action
            .navigation_target()
            .and_then(url_host)
            .into_iter()
            .collect(),
        PreparedBrowserRequest::Observe(_)
        | PreparedBrowserRequest::Find(_)
        | PreparedBrowserRequest::Wait(_)
        | PreparedBrowserRequest::Events(_)
        | PreparedBrowserRequest::Close(_) => Vec::new(),
    }
}

// Bare host of a URL (IPv6 without brackets); `None` for host-less URLs,
// which the origin policy already rejects.
fn url_host(url: &url::Url) -> Option<String> {
    url.host().map(|host| match host {
        url::Host::Domain(domain) => domain.to_owned(),
        url::Host::Ipv4(address) => address.to_string(),
        url::Host::Ipv6(address) => address.to_string(),
    })
}

// Host to check for one canonical origin rule (`scheme://[*.]host[:port]`).
// Wildcard rules are checked with a probe subdomain so that only a scope that
// admits arbitrary subdomains authorizes them.
fn rule_scope_host(rule: &OriginRule) -> String {
    let text = rule.to_string();
    let after_scheme = text
        .split_once("://")
        .map_or(text.as_str(), |(_, rest)| rest);
    let authority = after_scheme.strip_prefix("*.").unwrap_or(after_scheme);
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or(bracketed),
        None => authority.split(':').next().unwrap_or(authority),
    };
    if rule.include_subdomains() {
        format!("{WILDCARD_SCOPE_PROBE_LABEL}.{host}")
    } else {
        host.to_owned()
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
        policy::{BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy, OriginRule, ProfilePolicy},
        selector::{Selector, Target},
        wait::{WaitCondition, WaitTimeout},
    };

    use super::{
        ActRequest, BrowserToolRequest, CloseRequest, EventsRequest, FindRequest, ObserveRequest,
        OpenRequest, PreparedBrowserRequest, WaitRequest, browser_tool_parameters, parse_request,
        rule_scope_host, scope_hosts,
    };

    // Test fixtures use literal ids/URLs; a parse failure is a test bug.
    fn session_id() -> BrowserSessionId {
        BrowserSessionId::from_str("00000000-0000-4000-8000-000000000001").expect("fixed UUID")
    }

    fn context_id() -> BrowserContextId {
        BrowserContextId::from_str("00000000-0000-4000-8000-000000000002").expect("fixed UUID")
    }

    fn url(text: &str) -> url::Url {
        url::Url::parse(text).expect("fixed URL")
    }

    fn open_request() -> OpenRequest {
        OpenRequest {
            start_url: url("https://example.com/start"),
            headless: true,
            bidi: BiDiRequirement::Preferred,
            viewport: None,
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
    fn test_parse_request_accepts_each_browser_operation_shape() {
        let target = Target::new(Selector::Css("main".to_owned()));
        let requests = [
            ("browser.open", BrowserToolRequest::Open(open_request())),
            (
                "browser.observe",
                BrowserToolRequest::Observe(ObserveRequest {
                    session_id: session_id(),
                    context_id: context_id(),
                    mode: ObservationMode::PageSummary,
                }),
            ),
            (
                "browser.find",
                BrowserToolRequest::Find(FindRequest {
                    session_id: session_id(),
                    context_id: context_id(),
                    target: target.clone(),
                    revision: BrowserObservationRevision::initial(),
                }),
            ),
            (
                "browser.act",
                BrowserToolRequest::Act(ActRequest {
                    session_id: session_id(),
                    request: ActionRequest::new(context_id(), BrowserAction::Click { target }),
                }),
            ),
            (
                "browser.wait",
                BrowserToolRequest::Wait(WaitRequest {
                    session_id: session_id(),
                    context_id: context_id(),
                    condition: WaitCondition::NavigationComplete,
                    timeout: WaitTimeout::from_millis(1),
                }),
            ),
            (
                "browser.events",
                BrowserToolRequest::Events(EventsRequest {
                    session_id: session_id(),
                    since: BrowserEventCursor::zero(),
                }),
            ),
            ("browser.close", BrowserToolRequest::Close(CloseRequest { session_id: session_id() })),
        ];

        for (name, expected) in requests {
            let parsed = parse_request(name, request_arguments(&expected))
                .expect("serialized request shape is accepted by its browser operation");
            assert_eq!(parsed, expected, "{name} must preserve its request");
        }
    }

    #[test]
    fn test_parse_request_rejects_model_supplied_open_authority() {
        for (field, value) in [
            ("allowed_origins", serde_json::json!(["https://evil.example"])),
            ("authentication_origins", serde_json::json!(["https://evil.example"])),
            ("profile", serde_json::json!({"Persistent": {"binding": "admin"}})),
            ("limits", serde_json::json!({"max_actions_per_session": 5000})),
        ] {
            let mut arguments = serde_json::to_value(open_request()).expect("serializes");
            arguments[field] = value;
            let error = parse_request("browser.open", arguments)
                .expect_err("authority fields must not be accepted from the model");
            assert!(
                matches!(&error, harw_tools::ToolsError::InvalidArguments { reason, .. } if reason.contains(field)),
                "{field}: {error:?}"
            );
        }
    }

    #[test]
    fn test_parse_request_rejects_upload_and_script() {
        let upload = serde_json::json!({
            "session_id": session_id(),
            "request": {
                "context_id": context_id(),
                "expected_revision": 1,
                "action": {"Upload": {"target": {"primary": {"Css": "input"}, "fallbacks": []}, "file_path": "/etc/passwd"}}
            }
        });
        assert!(parse_request("browser.act", upload).is_err());

        let script = serde_json::json!({
            "session_id": session_id(),
            "context_id": context_id(),
            "condition": {"CustomScript": {"predicate": "return true"}},
            "timeout": 1000
        });
        assert!(parse_request("browser.wait", script).is_err());
    }

    #[test]
    fn test_parse_request_rejects_unknown_browser_operation() {
        let error = parse_request("browser.unknown", serde_json::json!({}))
            .expect_err("unknown browser operation must be rejected");
        assert!(matches!(
            error,
            harw_tools::ToolsError::InvalidArguments { name, .. } if name == "browser.unknown"
        ));
    }

    #[test]
    fn test_browser_tool_parameters_open_is_flat_and_closed() {
        let schema = serde_json::to_value(browser_tool_parameters("browser.open")).expect("serializes");
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        assert_eq!(
            schema["required"],
            serde_json::json!(["start_url", "headless", "bidi"])
        );
        let properties = schema["properties"].as_object().expect("object properties");
        let mut keys: Vec<&str> = properties.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["bidi", "headless", "start_url", "viewport"]);
    }

    #[test]
    fn test_rule_scope_host_strips_scheme_port_and_probes_wildcards() {
        let exact = OriginRule::parse("https://erp.example.com:8443").expect("valid rule");
        assert_eq!(rule_scope_host(&exact), "erp.example.com");
        let wildcard = OriginRule::parse("https://*.example.com").expect("valid rule");
        assert_eq!(rule_scope_host(&wildcard), "harw-wildcard-scope-probe.example.com");
        let ipv6 = OriginRule::parse("https://[2001:db8::1]:444").expect("valid rule");
        assert_eq!(rule_scope_host(&ipv6), "2001:db8::1");
    }

    #[test]
    fn test_scope_hosts_open_covers_start_and_both_policies() {
        let open = OpenBrowserRequest {
            start_url: url("https://erp.example.com/inbox"),
            headless: true,
            profile: ProfilePolicy::Ephemeral,
            bidi: BiDiRequirement::Preferred,
            allowed_origins: OriginPolicy::from_origins(["https://erp.example.com"], true)
                .expect("valid policy"),
            authentication_origins: OriginPolicy::from_origins(["https://sso.example.org"], true)
                .expect("valid policy"),
            viewport: None,
            limits: BrowserLimits::default(),
        };
        assert_eq!(
            scope_hosts(&PreparedBrowserRequest::Open(open)),
            ["erp.example.com", "erp.example.com", "sso.example.org"]
        );

        let act = ActRequest {
            session_id: session_id(),
            request: ActionRequest::new(
                context_id(),
                BrowserAction::Navigate { url: url("https://other.example.net/x") },
            ),
        };
        assert_eq!(scope_hosts(&PreparedBrowserRequest::Act(act)), ["other.example.net"]);
    }
}
