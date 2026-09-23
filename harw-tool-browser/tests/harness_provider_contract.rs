//! Provider surface and schema contract: the advertised JSON schemas are the
//! serde shapes of the request types (W5 B-TOOL). `schemars` is not a
//! workspace dependency, so equality is proven by validating complete serde
//! samples of every variant against the advertised schema (and the reverse:
//! every schema property/variant appears in a sample), plus negative samples
//! that both serde and the schema must reject.

mod common;

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use common::{TestError, TestResult, ctx};
use harw_browser::action::{ActionRequest, BrowserAction};
use harw_browser::error::Error as BrowserError;
use harw_browser::host::BrowserHost;
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId,
};
use harw_browser::observation::ObservationMode;
use harw_browser::policy::{BiDiRequirement, OpenBrowserRequest, Viewport};
use harw_browser::selector::{Selector, Target};
use harw_browser::session::BrowserSessionHandle;
use harw_browser::wait::{WaitCondition, WaitTimeout};
use harw_extension_api::contributors::ToolProvider;
use harw_tool_browser::{
    ActRequest, BrowserToolSet, CloseRequest, EventsRequest, FindRequest,
    HarwnessBrowserToolProvider, ObserveRequest, OpenRequest, WaitRequest,
};
use harw_tools::{ToolName, ToolSpec};
use serde_json::Value;

const EXPECTED_TOOL_NAMES: [&str; 7] = [
    "browser.open",
    "browser.observe",
    "browser.find",
    "browser.act",
    "browser.wait",
    "browser.events",
    "browser.close",
];

fn provider() -> HarwnessBrowserToolProvider {
    HarwnessBrowserToolProvider::new(BrowserToolSet::new(Arc::new(NeverUsedHost)))
}

// Advertised provider schema of one tool, serialized.
fn provider_schema(name: &str) -> TestResult<Value> {
    let function = provider()
        .tools()
        .into_iter()
        .find_map(|spec| {
            let ToolSpec::Function(function) = spec;
            (function.name.as_str() == name).then_some(function)
        })
        .ok_or_else(|| TestError::Unexpected(format!("{name} is advertised")))?;
    serde_json::to_value(&function.parameters).map_err(ctx("schema serializes"))
}

#[test]
fn test_tools_advertise_only_the_closed_browser_surface() {
    let names: Vec<String> = provider()
        .tools()
        .into_iter()
        .map(|spec| {
            let ToolSpec::Function(function) = spec;
            function.name.as_str().to_owned()
        })
        .collect();
    assert_eq!(names, EXPECTED_TOOL_NAMES);
}

#[test]
fn test_executor_absent_for_unknown_tools() {
    assert!(
        provider()
            .executor(&ToolName::new("browser.delete_everything"))
            .is_none()
    );
    assert!(
        provider()
            .executor(&ToolName::new("browser.open"))
            .is_some()
    );
}

#[test]
fn test_descriptor_schema_equals_provider_schema() -> TestResult {
    let tools = BrowserToolSet::new(Arc::new(NeverUsedHost));
    for descriptor in tools.descriptors() {
        assert_eq!(
            descriptor.input_schema,
            provider_schema(&descriptor.name)?,
            "{} descriptor and provider schema diverge",
            descriptor.name
        );
    }
    Ok(())
}

#[test]
fn test_schema_accepts_every_serde_sample_exactly() -> TestResult {
    for (name, sample) in samples()? {
        let schema = provider_schema(name)?;
        if let Err(reason) = check(&schema, &sample, name, true) {
            return Err(TestError::Unexpected(format!(
                "{name}: serde sample {sample} violates advertised schema: {reason}"
            )));
        }
    }
    Ok(())
}

#[test]
fn test_schema_variants_equal_serde_variants() -> TestResult {
    let act = provider_schema("browser.act")?;
    let action_schema = &act["properties"]["request"]["properties"]["action"];
    assert_eq!(
        variant_names(action_schema)?,
        action_samples()?
            .iter()
            .map(action_name)
            .map(str::to_owned)
            .collect::<BTreeSet<String>>()
    );

    let wait = provider_schema("browser.wait")?;
    assert_eq!(
        variant_names(&wait["properties"]["condition"])?,
        condition_samples()
            .iter()
            .map(condition_name)
            .map(str::to_owned)
            .collect::<BTreeSet<String>>()
    );

    let observe = provider_schema("browser.observe")?;
    assert_eq!(
        variant_names(&observe["properties"]["mode"])?,
        mode_samples()
            .iter()
            .map(mode_name)
            .map(str::to_owned)
            .collect::<BTreeSet<String>>()
    );

    let find = provider_schema("browser.find")?;
    assert_eq!(
        variant_names(&find["properties"]["target"]["properties"]["primary"])?,
        selector_samples()
            .iter()
            .map(selector_name)
            .map(str::to_owned)
            .collect::<BTreeSet<String>>()
    );
    Ok(())
}

#[test]
fn test_schema_and_serde_both_reject_authority_upload_and_script() -> TestResult {
    let mut open = serde_json::to_value(OpenRequest {
        start_url: url::Url::parse("https://erp.example.com").map_err(ctx("fixture URL"))?,
        headless: true,
        bidi: BiDiRequirement::Preferred,
        viewport: None,
    })
    .map_err(ctx("serializes"))?;
    open["allowed_origins"] = serde_json::json!(["https://evil.example"]);

    let upload = serde_json::json!({
        "session_id": session_id()?,
        "request": {"context_id": context_id()?, "expected_revision": 0,
            "action": {"Upload": {"target": {"primary": {"Css": "input"}, "fallbacks": []}, "file_path": "/etc/passwd"}}}
    });
    let script = serde_json::json!({
        "session_id": session_id()?, "context_id": context_id()?,
        "condition": {"CustomScript": {"predicate": "return true"}}, "timeout": 1000
    });

    assert!(check(&provider_schema("browser.open")?, &open, "open", false).is_err());
    assert!(serde_json::from_value::<OpenRequest>(open).is_err());
    assert!(check(&provider_schema("browser.act")?, &upload, "act", false).is_err());
    assert!(serde_json::from_value::<ActRequest>(upload).is_err());
    assert!(check(&provider_schema("browser.wait")?, &script, "wait", false).is_err());
    assert!(serde_json::from_value::<WaitRequest>(script).is_err());
    Ok(())
}

// ── Samples (complete serde output; `Option` fields serialize as null) ──────

fn session_id() -> TestResult<BrowserSessionId> {
    BrowserSessionId::from_str("00000000-0000-4000-8000-000000000001").map_err(ctx("fixture UUID"))
}

fn context_id() -> TestResult<BrowserContextId> {
    BrowserContextId::from_str("00000000-0000-4000-8000-000000000002").map_err(ctx("fixture UUID"))
}

fn target() -> Target {
    Target::new(Selector::Css("main".to_owned())).with_fallback(Selector::Role {
        role: "button".to_owned(),
        name: None,
    })
}

fn selector_samples() -> Vec<Selector> {
    vec![
        Selector::TestId("t".to_owned()),
        Selector::Css("c".to_owned()),
        Selector::Id("i".to_owned()),
        Selector::Name("n".to_owned()),
        Selector::TagClass {
            tag: "a".to_owned(),
            class: "b".to_owned(),
        },
        Selector::LinkText("l".to_owned()),
        Selector::XPath("//a".to_owned()),
        Selector::TextAnchor("x".to_owned()),
        Selector::Role {
            role: "button".to_owned(),
            name: Some("Save".to_owned()),
        },
        Selector::Role {
            role: "link".to_owned(),
            name: None,
        },
    ]
}

fn action_samples() -> TestResult<Vec<BrowserAction>> {
    Ok(vec![
        BrowserAction::Click { target: target() },
        BrowserAction::Type {
            target: target(),
            text: "hi".to_owned(),
        },
        BrowserAction::Clear { target: target() },
        BrowserAction::Select {
            target: target(),
            value: "v".to_owned(),
        },
        BrowserAction::Focus { target: target() },
        BrowserAction::Hover { target: target() },
        BrowserAction::Scroll {
            target: None,
            x: 0,
            y: -40,
        },
        BrowserAction::Scroll {
            target: Some(target()),
            x: 1,
            y: 2,
        },
        BrowserAction::KeyPress {
            target: None,
            key: "Enter".to_owned(),
        },
        BrowserAction::KeyPress {
            target: Some(target()),
            key: "Tab".to_owned(),
        },
        BrowserAction::Drag {
            source: target(),
            destination: target(),
        },
        BrowserAction::Submit { target: target() },
        BrowserAction::Navigate {
            url: url::Url::parse("https://erp.example.com/x").map_err(ctx("fixture URL"))?,
        },
        BrowserAction::Back,
        BrowserAction::Forward,
        BrowserAction::Reload,
    ])
}

fn condition_samples() -> Vec<WaitCondition> {
    vec![
        WaitCondition::ElementPresent(target()),
        WaitCondition::ElementVisible(target()),
        WaitCondition::ElementClickable(target()),
        WaitCondition::ElementGone(target()),
        WaitCondition::UrlMatches("done".to_owned()),
        WaitCondition::TitleMatches("Inbox".to_owned()),
        WaitCondition::NavigationComplete,
        WaitCondition::NetworkQuiescence { idle_ms: 250 },
        WaitCondition::RequestObserved {
            path_contains: "/api".to_owned(),
        },
        WaitCondition::LogMatches("ready".to_owned()),
        WaitCondition::ScriptMessage {
            channel: "c".to_owned(),
        },
        WaitCondition::DownloadComplete,
    ]
}

fn mode_samples() -> Vec<ObservationMode> {
    let mut modes = vec![
        ObservationMode::PageSummary,
        ObservationMode::InteractiveElements,
        ObservationMode::TextExtraction,
        ObservationMode::FormsAndLinks,
        ObservationMode::NetworkActivitySummary,
        ObservationMode::ConsoleLogSummary,
        ObservationMode::Screenshot,
        ObservationMode::CombinedDiagnostic,
    ];
    modes.extend(
        selector_samples()
            .into_iter()
            .map(|selector| ObservationMode::DomSelection { selector }),
    );
    modes
}

fn to_value<T: serde::Serialize>(value: &T) -> TestResult<Value> {
    serde_json::to_value(value).map_err(ctx("sample serializes"))
}

fn samples() -> TestResult<Vec<(&'static str, Value)>> {
    let mut samples = Vec::new();
    for viewport in [
        None,
        Some(Viewport {
            width: 800,
            height: 600,
        }),
    ] {
        for bidi in [
            BiDiRequirement::Required,
            BiDiRequirement::Preferred,
            BiDiRequirement::NotRequired,
        ] {
            samples.push((
                "browser.open",
                to_value(&OpenRequest {
                    start_url: url::Url::parse("https://erp.example.com/inbox")
                        .map_err(ctx("fixture URL"))?,
                    headless: false,
                    bidi,
                    viewport,
                })?,
            ));
        }
    }
    for mode in mode_samples() {
        samples.push((
            "browser.observe",
            to_value(&ObserveRequest {
                session_id: session_id()?,
                context_id: context_id()?,
                mode,
            })?,
        ));
    }
    for selector in selector_samples() {
        samples.push((
            "browser.find",
            to_value(&FindRequest {
                session_id: session_id()?,
                context_id: context_id()?,
                target: Target::new(selector.clone()).with_fallback(selector),
                revision: BrowserObservationRevision::initial().next(),
            })?,
        ));
    }
    for action in action_samples()? {
        for revision in [None, Some(BrowserObservationRevision::initial())] {
            let mut request = ActionRequest::new(context_id()?, action.clone());
            request.expected_revision = revision;
            samples.push((
                "browser.act",
                to_value(&ActRequest {
                    session_id: session_id()?,
                    request,
                })?,
            ));
        }
    }
    for condition in condition_samples() {
        samples.push((
            "browser.wait",
            to_value(&WaitRequest {
                session_id: session_id()?,
                context_id: context_id()?,
                condition,
                timeout: WaitTimeout::from_millis(500),
            })?,
        ));
    }
    samples.push((
        "browser.events",
        to_value(&EventsRequest {
            session_id: session_id()?,
            since: BrowserEventCursor::zero().next(),
        })?,
    ));
    samples.push((
        "browser.close",
        to_value(&CloseRequest {
            session_id: session_id()?,
        })?,
    ));
    Ok(samples)
}

// Exhaustive name functions: adding a serde variant fails to compile here,
// forcing schema and samples to be updated together.
fn action_name(action: &BrowserAction) -> &'static str {
    match action {
        BrowserAction::Click { .. } => "Click",
        BrowserAction::Type { .. } => "Type",
        BrowserAction::Clear { .. } => "Clear",
        BrowserAction::Select { .. } => "Select",
        BrowserAction::Focus { .. } => "Focus",
        BrowserAction::Hover { .. } => "Hover",
        BrowserAction::Scroll { .. } => "Scroll",
        BrowserAction::KeyPress { .. } => "KeyPress",
        BrowserAction::Drag { .. } => "Drag",
        BrowserAction::Submit { .. } => "Submit",
        BrowserAction::Navigate { .. } => "Navigate",
        BrowserAction::Back => "Back",
        BrowserAction::Forward => "Forward",
        BrowserAction::Reload => "Reload",
    }
}

fn condition_name(condition: &WaitCondition) -> &'static str {
    match condition {
        WaitCondition::ElementPresent(_) => "ElementPresent",
        WaitCondition::ElementVisible(_) => "ElementVisible",
        WaitCondition::ElementClickable(_) => "ElementClickable",
        WaitCondition::ElementGone(_) => "ElementGone",
        WaitCondition::UrlMatches(_) => "UrlMatches",
        WaitCondition::TitleMatches(_) => "TitleMatches",
        WaitCondition::NavigationComplete => "NavigationComplete",
        WaitCondition::NetworkQuiescence { .. } => "NetworkQuiescence",
        WaitCondition::RequestObserved { .. } => "RequestObserved",
        WaitCondition::LogMatches(_) => "LogMatches",
        WaitCondition::ScriptMessage { .. } => "ScriptMessage",
        WaitCondition::DownloadComplete => "DownloadComplete",
    }
}

fn mode_name(mode: &ObservationMode) -> &'static str {
    match mode {
        ObservationMode::PageSummary => "PageSummary",
        ObservationMode::InteractiveElements => "InteractiveElements",
        ObservationMode::DomSelection { .. } => "DomSelection",
        ObservationMode::TextExtraction => "TextExtraction",
        ObservationMode::FormsAndLinks => "FormsAndLinks",
        ObservationMode::NetworkActivitySummary => "NetworkActivitySummary",
        ObservationMode::ConsoleLogSummary => "ConsoleLogSummary",
        ObservationMode::Screenshot => "Screenshot",
        ObservationMode::CombinedDiagnostic => "CombinedDiagnostic",
    }
}

fn selector_name(selector: &Selector) -> &'static str {
    match selector {
        Selector::TestId(_) => "TestId",
        Selector::Css(_) => "Css",
        Selector::Id(_) => "Id",
        Selector::Name(_) => "Name",
        Selector::TagClass { .. } => "TagClass",
        Selector::LinkText(_) => "LinkText",
        Selector::XPath(_) => "XPath",
        Selector::TextAnchor(_) => "TextAnchor",
        Selector::Role { .. } => "Role",
    }
}

// Variant names of an externally tagged enum schema: string enum values plus
// the single property key of each object branch.
fn variant_names(schema: &Value) -> TestResult<BTreeSet<String>> {
    let branches = schema["anyOf"]
        .as_array()
        .ok_or_else(|| TestError::Unexpected("tagged enum schema uses anyOf".to_owned()))?;
    let mut names = BTreeSet::new();
    for branch in branches {
        if let Some(values) = branch["enum"].as_array() {
            for value in values {
                let value = value
                    .as_str()
                    .ok_or_else(|| TestError::Unexpected("string variant".to_owned()))?;
                names.insert(value.to_owned());
            }
        } else {
            let properties = branch["properties"]
                .as_object()
                .ok_or_else(|| TestError::Unexpected("object variant".to_owned()))?;
            assert_eq!(
                properties.len(),
                1,
                "externally tagged variant has exactly one key"
            );
            names.extend(properties.keys().cloned());
        }
    }
    Ok(names)
}

// Minimal validator for the JSON-schema subset `harw_tools::JsonSchema` emits.
// `exact` additionally requires every declared property to be present, which
// holds for complete serde output (None serializes as null).
fn check(schema: &Value, value: &Value, path: &str, exact: bool) -> Result<(), String> {
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        if !branches
            .iter()
            .any(|branch| check(branch, value, path, exact).is_ok())
        {
            return Err(format!("{path}: no anyOf branch accepts {value}"));
        }
    }
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            other => return Err(format!("{path}: unsupported schema type {other}")),
        };
        if !matches {
            return Err(format!("{path}: expected {expected}, got {value}"));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            return Err(format!("{path}: {value} not in enum"));
        }
    }
    if let (Some(properties), Some(object)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) {
        let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
        for (key, field) in object {
            match properties.get(key) {
                Some(field_schema) => check(field_schema, field, &format!("{path}.{key}"), exact)?,
                None if closed => return Err(format!("{path}: unknown property {key}")),
                None => {}
            }
        }
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let required = required
                .as_str()
                .ok_or_else(|| format!("{path}: non-string required"))?;
            if !object.contains_key(required) {
                return Err(format!("{path}: missing required {required}"));
            }
        }
        if exact {
            if let Some(missing) = properties.keys().find(|key| !object.contains_key(*key)) {
                return Err(format!(
                    "{path}: schema property {missing} absent from serde output"
                ));
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (index, item) in array.iter().enumerate() {
            check(items, item, &format!("{path}[{index}]"), exact)?;
        }
    }
    Ok(())
}

struct NeverUsedHost;

#[async_trait]
impl BrowserHost for NeverUsedHost {
    async fn open(
        &self,
        _request: OpenBrowserRequest,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        Err(BrowserError::InvalidArgument {
            detail: "provider surface tests must not invoke the browser host".to_owned(),
        })
    }

    async fn session(&self, _id: &BrowserSessionId) -> harw_browser::Result<BrowserSessionHandle> {
        Err(BrowserError::InvalidArgument {
            detail: "provider surface tests must not invoke the browser host".to_owned(),
        })
    }

    async fn close(&self, _id: &BrowserSessionId) -> harw_browser::Result<()> {
        Err(BrowserError::InvalidArgument {
            detail: "provider surface tests must not invoke the browser host".to_owned(),
        })
    }
}
