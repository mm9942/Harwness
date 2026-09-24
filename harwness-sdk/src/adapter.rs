//! Interner Adapter zwischen SDK-Fläche und Runtime.
//!
//! # Beschreibung
//! Einziger Ort, an dem SDK-Typen in interne Typen übersetzt werden:
//! - [`SdkToolProvider`] / [`SdkToolExecutor`]: [`Tool`] → interner
//!   Tool-Provider,
//! - [`SdkContextProvider`]: [`ContextSource`] → interner Kontext-Provider,
//! - [`SdkContributor`]: hängt beides über den öffentlichen
//!   Montage-Erweiterungspunkt der Runtime in die Wurzel-Registry,
//! - [`open_state_store`]: der Verlaufsspeicher einer [`crate::Harwness`].
//!
//! Nichts hiervon ist öffentlich; ändert sich eine interne Schnittstelle,
//! ändert sich nur diese Datei.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_core::{InMemoryStateStore, StateStore, TranscriptStateStore};
use harw_extension_api::{
    ContextFragment, ContextProvider, ExtFuture, ToolCall, ToolExecutionContext, ToolExecutor,
    ToolExecutorFuture, ToolName, ToolProvider, ToolSpec, TurnInputContext,
};
use harw_runtime::{
    AssemblyContributor, AssemblyInputs, AssemblyParts, RuntimeError, RuntimeResult,
};
use harw_tools::{FunctionToolSpec, JsonSchema, JsonSchemaType, ToolsError};

use crate::error::SdkError;
use crate::ids::SessionId;
use crate::tool::{ContextSource, Tool, ToolContext};

/// Größte zulässige Länge eines Werkzeugnamens.
const MAX_TOOL_NAME_LEN: usize = 64;

/// Präfix der Thread-Referenz, unter der SDK-Sitzungen gespeichert werden.
const SDK_THREAD_PREFIX: &str = "sdk-session:";

/// Prüft einen Werkzeugnamen gegen `[A-Za-z0-9_.-]{1,64}`.
pub(crate) fn validate_tool_name(name: &str) -> Result<(), SdkError> {
    if name.is_empty() || name.len() > MAX_TOOL_NAME_LEN {
        return Err(SdkError::invalid(
            "tool.name",
            format!("'{name}' must be 1..={MAX_TOOL_NAME_LEN} characters long"),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(SdkError::invalid(
            "tool.name",
            format!("'{name}' may only contain ASCII letters, digits, '_', '.' and '-'"),
        ));
    }
    Ok(())
}

/// Übersetzt das JSON-Schema eines Werkzeugs; verlangt `"type": "object"`.
pub(crate) fn tool_schema(
    name: &str,
    parameters: serde_json::Value,
) -> Result<JsonSchema, SdkError> {
    let schema: JsonSchema = serde_json::from_value(parameters).map_err(|error| {
        SdkError::invalid(
            "tool.parameters",
            format!("schema of '{name}' is not a supported JSON schema: {error}"),
        )
    })?;
    if schema.schema_type != Some(JsonSchemaType::Object) {
        return Err(SdkError::invalid(
            "tool.parameters",
            format!("schema of '{name}' must have \"type\": \"object\""),
        ));
    }
    Ok(schema)
}

/// Ein geprüftes SDK-Werkzeug samt fertiger Spezifikation.
struct ToolEntry {
    spec: ToolSpec,
    tool: Arc<dyn Tool>,
}

/// Alle SDK-Werkzeuge einer [`crate::Harwness`] als ein interner Provider.
pub(crate) struct SdkToolProvider {
    entries: Vec<ToolEntry>,
}

impl std::fmt::Debug for SdkToolProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkToolProvider")
            .field("tools", &self.names())
            .finish()
    }
}

impl SdkToolProvider {
    /// Prüft Namen, Eindeutigkeit und Schemata und baut die Spezifikationen.
    ///
    /// # Fehler
    /// [`SdkError::InvalidInput`] bei ungültigem Namen, doppeltem Namen oder
    /// ungültigem Schema — vor jeder Montage, damit ein Fehler nie erst im
    /// ersten Turn auffällt.
    pub(crate) fn new(tools: Vec<Arc<dyn Tool>>) -> Result<Self, SdkError> {
        let mut seen = HashSet::new();
        let mut entries = Vec::with_capacity(tools.len());
        for tool in tools {
            let name = tool.name().to_owned();
            validate_tool_name(&name)?;
            if !seen.insert(name.clone()) {
                return Err(SdkError::invalid(
                    "tool.name",
                    format!("'{name}' is registered twice"),
                ));
            }
            let parameters = tool_schema(&name, tool.parameters())?;
            let spec = ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new(name),
                description: tool.description().to_owned(),
                parameters,
                strict: false,
            });
            entries.push(ToolEntry { spec, tool });
        }
        Ok(Self { entries })
    }

    /// Die Werkzeugnamen in Registrierungsreihenfolge.
    pub(crate) fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.spec.name().to_owned())
            .collect()
    }

    /// `true` ohne Werkzeuge.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn find(&self, name: &str) -> Option<&ToolEntry> {
        self.entries.iter().find(|entry| entry.spec.name() == name)
    }
}

impl ToolProvider for SdkToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.entries
            .iter()
            .map(|entry| entry.spec.clone())
            .collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.find(name.as_str()).map(|entry| {
            Arc::new(SdkToolExecutor {
                tool: Arc::clone(&entry.tool),
            }) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        self.find(name.as_str())
            .is_some_and(|entry| entry.tool.parallel_safe())
    }
}

/// Führt ein SDK-Werkzeug als internen Executor aus.
struct SdkToolExecutor {
    tool: Arc<dyn Tool>,
}

impl ToolExecutor for SdkToolExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let ctx = ToolContext::new(
            SessionId::from_core(context.session_id()),
            call.id.as_str().to_owned(),
            context.cancel().cloned(),
        );
        let arguments = call.arguments.clone();
        Box::pin(async move {
            let result = self.tool.call(&ctx, arguments).await;
            Ok::<_, ToolsError>(core_tool_output(result))
        })
    }
}

/// Übersetzt ein SDK-Werkzeugergebnis: `String` → Text, sonst JSON; ein
/// [`crate::ToolError`] wird zum nutzersicheren Fehlerergebnis.
fn core_tool_output(
    result: Result<serde_json::Value, crate::tool::ToolError>,
) -> harw_tools::ToolOutput {
    match result {
        Ok(serde_json::Value::String(text)) => harw_tools::ToolOutput::text(text),
        Ok(value) => harw_tools::ToolOutput::json(value),
        Err(error) => harw_tools::ToolOutput::error(error.message()),
    }
}

/// Eine [`ContextSource`] als interner Kontext-Provider.
pub(crate) struct SdkContextProvider {
    source: Arc<dyn ContextSource>,
}

impl SdkContextProvider {
    /// Umhüllt eine Quelle.
    pub(crate) fn new(source: Arc<dyn ContextSource>) -> Self {
        Self { source }
    }
}

impl ContextProvider for SdkContextProvider {
    fn contribute<'a>(&'a self, ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
        Box::pin(async move {
            let session = SessionId::from_core(&ctx.session_id);
            self.source
                .items(&session)
                .await
                .into_iter()
                .map(|item| ContextFragment {
                    label: item.label,
                    content: item.content,
                })
                .collect()
        })
    }

    fn namespace(&self) -> &'static str {
        self.source.namespace()
    }
}

/// Prüft die Namensräume der Kontextquellen (nicht leer, eindeutig).
pub(crate) fn validate_context_sources(sources: &[Arc<dyn ContextSource>]) -> Result<(), SdkError> {
    let mut seen = HashSet::new();
    for source in sources {
        let namespace = source.namespace();
        if namespace.trim().is_empty() {
            return Err(SdkError::invalid("context.namespace", "must not be empty"));
        }
        if !seen.insert(namespace) {
            return Err(SdkError::invalid(
                "context.namespace",
                format!("'{namespace}' is registered twice"),
            ));
        }
    }
    Ok(())
}

/// Hängt SDK-Werkzeuge und -Kontextquellen in die Wurzel-Registry.
///
/// # Beschreibung
/// Nutzt den öffentlichen Erweiterungspunkt
/// [`harw_runtime::AssemblyContributor`]: additiv, ohne Rechte zu vergeben.
pub(crate) struct SdkContributor {
    pub(crate) tools: Option<Arc<SdkToolProvider>>,
    pub(crate) raw_tools: Vec<Arc<dyn ToolProvider>>,
    pub(crate) contexts: Vec<Arc<dyn ContextProvider>>,
}

impl AssemblyContributor for SdkContributor {
    fn contribute(
        &self,
        _inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        let mut registry = std::mem::take(&mut parts.registry);
        if let Some(tools) = self.tools.as_ref().filter(|tools| !tools.is_empty()) {
            registry = registry.tool_provider(Arc::clone(tools) as Arc<dyn ToolProvider>);
        }
        for provider in &self.raw_tools {
            registry = registry.tool_provider(Arc::clone(provider));
        }
        for provider in &self.contexts {
            registry = registry
                .context_provider(Arc::clone(provider))
                .map_err(|error| RuntimeError::Registry {
                    detail: format!("could not register an SDK context source: {error}"),
                })?;
        }
        parts.registry = registry;
        Ok(())
    }
}

/// Leitet die Thread-Referenz einer SDK-Sitzung ab (stabil über Neustarts).
fn sdk_thread_for_session(session_id: &harw_types::SessionId) -> harw_types::ThreadRef {
    harw_types::ThreadRef::from_str(format!("{SDK_THREAD_PREFIX}{}", session_id.as_str()))
}

/// Das Transkriptverzeichnis des aktiven Profils.
///
/// # Beschreibung
/// Wie der CLI-Einstieg: `[session].store_dir` aus den vertrauten Home- und
/// Profil-Layern, relativ zum Profilordner aufgelöst; Vorgabe `sessions`.
fn sessions_root(home: &Path) -> Result<PathBuf, SdkError> {
    let profile = harw_home::active_profile_name(home);
    let profile_dir = harw_home::profile_dir(home, &profile).map_err(|error| SdkError::Home {
        detail: format!("could not resolve profile '{profile}': {error}"),
    })?;
    let layers = [home.to_path_buf(), profile_dir.clone()];
    let store_dir = match harw_config::discover_config(&layers) {
        Ok(config) => config.harness.session.store_dir,
        Err(error) => {
            tracing::warn!(%error, "sdk.session_store.config_unreadable_using_default");
            String::new()
        }
    };
    let trimmed = store_dir.trim();
    let root = if trimmed.is_empty() {
        profile_dir.join("sessions")
    } else if Path::new(trimmed).is_absolute() {
        PathBuf::from(trimmed)
    } else {
        profile_dir.join(trimmed)
    };
    std::fs::create_dir_all(&root).map_err(|error| SdkError::Session {
        detail: format!(
            "could not create the sessions directory at '{}': {error}",
            root.display()
        ),
    })?;
    Ok(root)
}

/// Öffnet den Verlaufsspeicher einer [`crate::Harwness`].
///
/// # Argumente
/// - `home`: aufgelöster Root-Space.
/// - `ephemeral`: `true` für einen flüchtigen In-Memory-Speicher (kein
///   Fortsetzen über Prozessgrenzen).
pub(crate) fn open_state_store(
    home: &Path,
    ephemeral: bool,
) -> Result<Arc<dyn StateStore>, SdkError> {
    if ephemeral {
        return Ok(Arc::new(InMemoryStateStore::new()));
    }
    let root = sessions_root(home)?;
    Ok(Arc::new(TranscriptStateStore::new(
        harw_session_store::TranscriptStore::new(&root),
        sdk_thread_for_session,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BoxFuture;
    use crate::tool::{ContextItem, FnTool, ToolError};
    use serde_json::json;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn echo_tool(name: &str, parameters: serde_json::Value) -> Arc<dyn Tool> {
        Arc::new(FnTool::new(
            name,
            "echo",
            parameters,
            |args: serde_json::Value| async move { Ok::<_, ToolError>(args) },
        ))
    }

    #[test]
    fn tool_names_are_validated() {
        assert!(validate_tool_name("host.clock").is_ok());
        assert!(validate_tool_name("a_b-c.1").is_ok());
        assert!(validate_tool_name("").is_err());
        assert!(validate_tool_name("has space").is_err());
        assert!(validate_tool_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn duplicate_tools_and_bad_schemas_are_rejected() {
        let object = json!({"type": "object"});
        let duplicate = SdkToolProvider::new(vec![
            echo_tool("a", object.clone()),
            echo_tool("a", object.clone()),
        ]);
        assert!(matches!(
            duplicate,
            Err(SdkError::InvalidInput {
                field: "tool.name",
                ..
            })
        ));
        let not_object = SdkToolProvider::new(vec![echo_tool("b", json!({"type": "string"}))]);
        assert!(matches!(
            not_object,
            Err(SdkError::InvalidInput {
                field: "tool.parameters",
                ..
            })
        ));
        let not_schema = SdkToolProvider::new(vec![echo_tool("c", json!("object"))]);
        assert!(matches!(
            not_schema,
            Err(SdkError::InvalidInput {
                field: "tool.parameters",
                ..
            })
        ));
    }

    #[test]
    fn provider_exposes_specs_and_executors() -> TestResult {
        let provider = SdkToolProvider::new(vec![echo_tool(
            "host.echo",
            json!({"type": "object", "properties": {"v": {"type": "string"}}}),
        )])?;
        assert_eq!(provider.names(), vec!["host.echo".to_owned()]);
        let specs = provider.tools();
        assert_eq!(specs.len(), 1);
        assert!(!provider.parallel_safe(&ToolName::new("host.echo")));
        assert!(provider.executor(&ToolName::new("missing")).is_none());

        assert!(provider.executor(&ToolName::new("host.echo")).is_some());
        Ok(())
    }

    #[test]
    fn tool_results_map_to_text_json_or_error() {
        assert!(matches!(
            core_tool_output(Ok(json!("plain"))),
            harw_tools::ToolOutput::Text { ref content } if content == "plain"
        ));
        assert!(matches!(
            core_tool_output(Ok(json!({"v": 1}))),
            harw_tools::ToolOutput::Json { ref content } if content == &json!({"v": 1})
        ));
        assert!(matches!(
            core_tool_output(Err(ToolError::new("kaputt"))),
            harw_tools::ToolOutput::Error { ref message } if message == "kaputt"
        ));
    }

    struct Fixed(&'static str);

    impl ContextSource for Fixed {
        fn namespace(&self) -> &'static str {
            self.0
        }

        fn items<'a>(&'a self, session_id: &'a SessionId) -> BoxFuture<'a, Vec<ContextItem>> {
            let content = format!("session={session_id}");
            Box::pin(async move { vec![ContextItem::new("host.session", content)] })
        }
    }

    #[tokio::test]
    async fn context_sources_become_fragments() -> TestResult {
        let provider = SdkContextProvider::new(Arc::new(Fixed("host.fixed")));
        assert_eq!(provider.namespace(), "host.fixed");
        let ctx = TurnInputContext {
            session_id: harw_types::SessionId::from_str("s-1"),
            turn_id: harw_types::TurnId::from_str("t-1"),
            metadata: serde_json::Value::Null,
        };
        let fragments = provider.contribute(&ctx).await;
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].label, "host.session");
        assert_eq!(fragments[0].content, "session=s-1");
        Ok(())
    }

    #[test]
    fn context_namespaces_must_be_unique_and_non_empty() {
        let unique: Vec<Arc<dyn ContextSource>> = vec![Arc::new(Fixed("a")), Arc::new(Fixed("b"))];
        assert!(validate_context_sources(&unique).is_ok());
        let twice: Vec<Arc<dyn ContextSource>> = vec![Arc::new(Fixed("a")), Arc::new(Fixed("a"))];
        assert!(validate_context_sources(&twice).is_err());
        let blank: Vec<Arc<dyn ContextSource>> = vec![Arc::new(Fixed(" "))];
        assert!(validate_context_sources(&blank).is_err());
    }

    #[test]
    fn thread_refs_are_stable_and_prefixed() {
        let id = harw_types::SessionId::from_str("abc");
        assert_eq!(sdk_thread_for_session(&id).as_str(), "sdk-session:abc");
        assert_eq!(sdk_thread_for_session(&id), sdk_thread_for_session(&id));
    }
}
