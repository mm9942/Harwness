//! `container.images` and `container.run`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use harw_authority::Permission;
use harw_tool_container::{
    ContainerPlan, ContainerPolicyError, Enforcement, ImageCatalog, Profile, RunConfig, RunRequest,
};
use harw_tools::sandbox_guard::require_permission;
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{ContainerEngine, EngineError};

/// Name of the image-listing tool.
pub const CONTAINER_IMAGES_TOOL: &str = "container.images";
/// Name of the run tool.
pub const CONTAINER_RUN_TOOL: &str = "container.run";

/// Trusted configuration of the tools (from `[tools.container]`, never from a
/// tool call).
#[derive(Clone)]
pub struct ContainerToolConfig {
    /// Absolute engine path; the file name must be `podman`.
    pub executable: String,
    /// Optional named remote connection.
    pub connection: Option<String>,
    /// Allowed images by alias.
    pub catalog: ImageCatalog,
    /// How plans are run.
    pub engine: Arc<dyn ContainerEngine>,
}

impl std::fmt::Debug for ContainerToolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerToolConfig")
            .field("executable", &self.executable)
            .field("connection", &self.connection)
            .field("images", &self.catalog.len())
            .finish_non_exhaustive()
    }
}

impl ContainerToolConfig {
    /// Builds the config from `alias=name@sha256:…` entries.
    ///
    /// # Errors
    /// An entry without `=`, an invalid alias or reference, or a duplicate.
    pub fn from_entries(
        executable: &str,
        connection: Option<&str>,
        entries: &[String],
        engine: Arc<dyn ContainerEngine>,
    ) -> Result<Self, String> {
        let mut pairs = Vec::new();
        for entry in entries {
            let (alias, reference) = entry
                .split_once('=')
                .ok_or_else(|| format!("image entry `{entry}` must be alias=reference"))?;
            pairs.push((alias.trim(), reference.trim()));
        }
        let catalog = ImageCatalog::new(pairs).map_err(|error| error.to_string())?;
        // Validate the engine path and connection once, at startup.
        let mut probe =
            RunConfig::new(executable, "/", "probe", "probe").map_err(|error| error.to_string())?;
        if let Some(name) = connection {
            probe = probe.with_connection(name).map_err(|e| e.to_string())?;
        }
        drop(probe);
        Ok(Self {
            executable: executable.to_owned(),
            connection: connection.map(str::to_owned),
            catalog,
            engine,
        })
    }
}

struct Shared {
    config: ContainerToolConfig,
    counter: AtomicU64,
}

/// Provider of `container.images` and `container.run`.
#[derive(Clone)]
pub struct ContainerToolProvider {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ContainerToolProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerToolProvider")
            .field("config", &self.shared.config)
            .finish()
    }
}

impl ContainerToolProvider {
    /// Provider over trusted `config`.
    #[must_use]
    pub fn new(config: ContainerToolConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                config,
                counter: AtomicU64::new(0),
            }),
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Images,
    Run,
}

struct ContainerToolExecutor {
    kind: Kind,
    shared: Arc<Shared>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    image: String,
    command: Vec<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    timeout_s: Option<u32>,
}

fn invalid(tool: &str, reason: impl Into<String>) -> ToolsError {
    ToolsError::InvalidArguments {
        name: tool.to_owned(),
        reason: reason.into(),
    }
}

fn policy(tool: &str, error: &ContainerPolicyError) -> ToolOutput {
    ToolOutput::error(format!("{tool}: {error}"))
}

/// A label-safe owner from a session id.
fn owner_label(session: &str) -> String {
    let mut label: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(48)
        .collect();
    if !label
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        label.insert(0, 's');
    }
    label
}

impl ContainerToolExecutor {
    fn images(&self) -> ToolOutput {
        let images: Vec<Value> = self
            .shared
            .config
            .catalog
            .aliases()
            .filter_map(|alias| {
                self.shared
                    .config
                    .catalog
                    .resolve(alias)
                    .ok()
                    .map(|image| json!({"alias": alias, "reference": image.to_arg()}))
            })
            .collect();
        ToolOutput::json(json!({
            "images": images,
            "profiles": Profile::ALL.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        }))
    }

    async fn run(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        let args: RunArgs = serde_json::from_value(call.arguments.clone())
            .map_err(|error| invalid(CONTAINER_RUN_TOOL, error.to_string()))?;
        let config = &self.shared.config;
        let image = match config.catalog.resolve(&args.image) {
            Ok(image) => image.clone(),
            Err(error) => return Ok(policy(CONTAINER_RUN_TOOL, &error)),
        };
        let profile = match args.profile.as_deref() {
            None => Profile::Hermetic,
            Some(name) => match Profile::parse(name) {
                Some(profile) => profile,
                None => {
                    return Err(invalid(
                        CONTAINER_RUN_TOOL,
                        format!("unknown profile `{name}`"),
                    ));
                }
            },
        };
        let plan = match self.plan(context, image, profile, args) {
            Ok(plan) => plan,
            Err(error) => return Ok(policy(CONTAINER_RUN_TOOL, &error)),
        };
        tracing::info!(
            container = plan.container_name(),
            profile = %plan.profile(),
            "container.run requested"
        );
        match config.engine.run(&plan, context.cancel()).await {
            Ok(run) => {
                let readback: Value = match &run.readback {
                    Some(readback) => Value::Array(
                        readback
                            .entries()
                            .iter()
                            .map(|(dimension, state)| {
                                json!({
                                    "dimension": format!("{dimension:?}"),
                                    "state": match state {
                                        Enforcement::Enforced => "enforced",
                                        Enforcement::NotEnforced => "not_enforced",
                                        Enforcement::Unverifiable => "unverifiable",
                                    },
                                })
                            })
                            .collect(),
                    ),
                    None => Value::Null,
                };
                Ok(ToolOutput::json(json!({
                    "container": plan.container_name(),
                    "exit_code": run.output.exit_code,
                    "stdout": run.output.stdout,
                    "stderr": run.output.stderr,
                    "truncated": run.output.truncated,
                    "timed_out": run.output.timed_out,
                    "cancelled": run.output.cancelled,
                    "readback": readback,
                    "readback_note": if run.readback.is_none() {
                        "the container ended before it could be inspected; isolation was requested but not verified"
                    } else {
                        "isolation read back from the engine while the container ran"
                    },
                })))
            }
            Err(error @ EngineError::NotEnforced(_)) => {
                Ok(ToolOutput::error(format!("{CONTAINER_RUN_TOOL}: {error}")))
            }
            Err(error) => Ok(ToolOutput::error(format!("{CONTAINER_RUN_TOOL}: {error}"))),
        }
    }

    fn plan(
        &self,
        context: &ToolExecutionContext,
        image: harw_tool_container::ImageRef,
        profile: Profile,
        args: RunArgs,
    ) -> Result<ContainerPlan, ContainerPolicyError> {
        let config = &self.shared.config;
        let workspace = context
            .sandbox()
            .workspace()
            .canonical_root()
            .to_string_lossy()
            .into_owned();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let run_id = format!(
            "r{stamp:x}{:x}",
            self.shared.counter.fetch_add(1, Ordering::Relaxed)
        );
        let mut run_config = RunConfig::new(
            &config.executable,
            &workspace,
            &run_id,
            &owner_label(context.session_id().as_str()),
        )?;
        if let Some(connection) = &config.connection {
            run_config = run_config.with_connection(connection)?;
        }
        let mut request = RunRequest::new(image, profile, args.command)?;
        if let Some(workdir) = &args.workdir {
            request = request.with_workdir(workdir)?;
        }
        for entry in &args.env {
            let (key, value) = entry
                .split_once('=')
                .ok_or(ContainerPolicyError::InvalidEnv("expected KEY=VALUE"))?;
            request = request.with_env(key, value)?;
        }
        if let Some(timeout) = args.timeout_s {
            request = request.with_timeout_s(timeout);
        }
        ContainerPlan::build(&run_config, &request)
    }
}

impl ToolExecutor for ContainerToolExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let name = match self.kind {
                Kind::Images => CONTAINER_IMAGES_TOOL,
                Kind::Run => CONTAINER_RUN_TOOL,
            };
            if let Some(denied) = require_permission(context, Permission::ManageContainers, name) {
                return Ok(denied);
            }
            match self.kind {
                Kind::Images => Ok(self.images()),
                Kind::Run => self.run(context, call).await,
            }
        })
    }
}

fn prop(schema_type: JsonSchemaType, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(schema_type),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn string_array(description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Array),
        description: Some(description.to_owned()),
        items: Some(Box::new(JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            ..Default::default()
        })),
        ..Default::default()
    }
}

fn object(properties: Vec<(&str, JsonSchema)>, required: &[&str]) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(
            properties
                .into_iter()
                .map(|(name, schema)| (name.to_owned(), schema))
                .collect::<BTreeMap<_, _>>(),
        ),
        required: Some(required.iter().map(|name| (*name).to_owned()).collect()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

fn images_spec() -> ToolSpec {
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(CONTAINER_IMAGES_TOOL),
        description: "List the container images container.run may use (alias and digest-pinned \
                      reference) and the run profiles. Read-only; no engine call."
            .to_owned(),
        parameters: object(Vec::new(), &[]),
        strict: true,
    })
}

fn run_spec() -> ToolSpec {
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(CONTAINER_RUN_TOOL),
        description: "Run a command in a hardened rootless Podman container: digest-pinned image \
                      chosen by alias (see container.images), no network, read-only root \
                      filesystem, all capabilities dropped, memory/process/time limits fixed by \
                      the profile ('hermetic': workspace read-only; 'build': workspace writable), \
                      workspace mounted at /workspace. Isolation is read back from the engine \
                      while the container runs; a violation kills it. Returns JSON {exit_code, \
                      stdout, stderr, truncated, timed_out, readback}. Requires user approval."
            .to_owned(),
        parameters: object(
            vec![
                (
                    "image",
                    prop(JsonSchemaType::String, "Image alias from container.images."),
                ),
                (
                    "command",
                    string_array("Command and arguments (argv words, no shell)."),
                ),
                (
                    "profile",
                    prop(JsonSchemaType::String, "'hermetic' (default) or 'build'."),
                ),
                (
                    "workdir",
                    prop(
                        JsonSchemaType::String,
                        "Working directory relative to /workspace.",
                    ),
                ),
                (
                    "env",
                    string_array("Environment entries KEY=VALUE; only allowlisted keys."),
                ),
                (
                    "timeout_s",
                    prop(
                        JsonSchemaType::Integer,
                        "Shorter wall-time limit in seconds (capped by the profile).",
                    ),
                ),
            ],
            &["image", "command"],
        ),
        strict: false,
    })
}

harw_tools::tool_provider! {
    impl for ContainerToolProvider as provider, parallel_safe: [CONTAINER_IMAGES_TOOL] {
        CONTAINER_IMAGES_TOOL => {
            spec: images_spec(),
            permission: Permission::ManageContainers,
            executor: ContainerToolExecutor { kind: Kind::Images, shared: Arc::clone(&provider.shared) },
        },
        CONTAINER_RUN_TOOL => {
            spec: run_spec(),
            permission: Permission::ManageContainers,
            executor: ContainerToolExecutor { kind: Kind::Run, shared: Arc::clone(&provider.shared) },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineFuture, EngineRun, RunOutput};
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_extension_api::contributors::ToolProvider as _;
    use harw_tool_container::ContainerPlan;
    use harw_types::cancel::CancelToken;
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Records the plans it was asked to run.
    #[derive(Default)]
    struct FakeEngine {
        plans: Mutex<Vec<Vec<String>>>,
    }

    impl ContainerEngine for FakeEngine {
        fn run<'a>(
            &'a self,
            plan: &'a ContainerPlan,
            _cancel: Option<&'a CancelToken>,
        ) -> EngineFuture<'a, Result<EngineRun, EngineError>> {
            if let Ok(mut plans) = self.plans.lock() {
                plans.push(plan.args().to_vec());
            }
            Box::pin(async {
                Ok(EngineRun {
                    output: RunOutput {
                        exit_code: Some(0),
                        stdout: "ok".to_owned(),
                        ..RunOutput::default()
                    },
                    readback: None,
                })
            })
        }
    }

    fn context(root: &Path, permissions: Vec<Permission>) -> TestResult<ToolExecutionContext> {
        std::fs::create_dir_all(root.join("ws"))?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )?;
        let binding = registry.resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        Ok(ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox,
        ))
    }

    fn provider(engine: Arc<FakeEngine>) -> TestResult<ContainerToolProvider> {
        let config = ContainerToolConfig::from_entries(
            "/usr/bin/podman",
            None,
            &[format!("rust=docker.io/library/rust@sha256:{HEX}")],
            engine,
        )?;
        Ok(ContainerToolProvider::new(config))
    }

    async fn call(
        provider: &ContainerToolProvider,
        context: &ToolExecutionContext,
        name: &str,
        arguments: Value,
    ) -> TestResult<Result<ToolOutput, ToolsError>> {
        let executor = provider
            .executor(&ToolName::new(name))
            .ok_or("executor present")?;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments,
        };
        Ok(executor.execute(context, &call).await)
    }

    #[test]
    fn the_surface_is_two_tools_both_requiring_manage_containers() {
        assert_eq!(
            ContainerToolProvider::TOOL_NAMES,
            &[CONTAINER_IMAGES_TOOL, CONTAINER_RUN_TOOL]
        );
        for permission in ContainerToolProvider::TOOL_PERMISSIONS {
            assert_eq!(*permission, Some(Permission::ManageContainers));
        }
        assert!(ContainerToolProvider::tool_parallel_safe(
            CONTAINER_IMAGES_TOOL
        ));
        assert!(!ContainerToolProvider::tool_parallel_safe(
            CONTAINER_RUN_TOOL
        ));
    }

    #[test]
    fn a_bad_catalog_entry_is_refused_at_startup() {
        let engine: Arc<dyn ContainerEngine> = Arc::new(FakeEngine::default());
        for entries in [
            vec!["no-equals".to_owned()],
            vec!["x=docker.io/library/rust:latest".to_owned()],
        ] {
            assert!(
                ContainerToolConfig::from_entries(
                    "/usr/bin/podman",
                    None,
                    &entries,
                    Arc::clone(&engine)
                )
                .is_err(),
                "{entries:?}"
            );
        }
        assert!(
            ContainerToolConfig::from_entries("/usr/bin/docker", None, &[], engine).is_err(),
            "engine must be podman"
        );
    }

    #[tokio::test]
    async fn without_the_permission_both_tools_are_denied_before_anything_runs() -> TestResult {
        let temp = tempfile::tempdir()?;
        let engine = Arc::new(FakeEngine::default());
        let provider = provider(Arc::clone(&engine))?;
        let context = context(temp.path(), vec![Permission::ReadWorkspace])?;
        for (name, args) in [
            (CONTAINER_IMAGES_TOOL, json!({})),
            (
                CONTAINER_RUN_TOOL,
                json!({"image": "rust", "command": ["true"]}),
            ),
        ] {
            let output = call(&provider, &context, name, args).await??;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{name}: {output:?}"
            );
        }
        assert!(engine.plans.lock().map(|p| p.is_empty()).unwrap_or(false));
        Ok(())
    }

    #[tokio::test]
    async fn images_lists_aliases_and_profiles() -> TestResult {
        let temp = tempfile::tempdir()?;
        let provider = provider(Arc::new(FakeEngine::default()))?;
        let context = context(temp.path(), vec![Permission::ManageContainers])?;
        let ToolOutput::Json { content } =
            call(&provider, &context, CONTAINER_IMAGES_TOOL, json!({})).await??
        else {
            return Err("json expected".into());
        };
        assert_eq!(content["images"][0]["alias"], "rust");
        assert_eq!(content["profiles"], json!(["hermetic", "build"]));
        Ok(())
    }

    #[tokio::test]
    async fn run_builds_a_hardened_plan_from_an_alias_and_returns_the_result() -> TestResult {
        let temp = tempfile::tempdir()?;
        let engine = Arc::new(FakeEngine::default());
        let provider = provider(Arc::clone(&engine))?;
        let context = context(temp.path(), vec![Permission::ManageContainers])?;
        let ToolOutput::Json { content } = call(
            &provider,
            &context,
            CONTAINER_RUN_TOOL,
            json!({"image": "rust", "command": ["cargo", "check"], "env": ["RUST_LOG=debug"]}),
        )
        .await??
        else {
            return Err("json expected".into());
        };
        assert_eq!(content["exit_code"], 0);
        assert_eq!(content["stdout"], "ok");
        assert!(
            content["readback_note"]
                .as_str()
                .is_some_and(|n| n.contains("not verified"))
        );
        let plans = engine.plans.lock().map_err(|_| "poisoned")?;
        let args = plans.first().ok_or("one plan")?;
        for expected in [
            "--network=none",
            "--read-only",
            "--cap-drop=all",
            "--env=RUST_LOG=debug",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected} in {args:?}");
        }
        assert!(
            args.iter().any(|a| a.starts_with("--label=harw.owner=")),
            "ownership label present"
        );
        Ok(())
    }

    #[tokio::test]
    async fn run_refuses_what_the_policy_core_refuses() -> TestResult {
        let temp = tempfile::tempdir()?;
        let engine = Arc::new(FakeEngine::default());
        let provider = provider(Arc::clone(&engine))?;
        let context = context(temp.path(), vec![Permission::ManageContainers])?;
        for args in [
            json!({"image": "ubuntu", "command": ["true"]}),
            json!({"image": "rust", "command": ["true"], "env": ["SECRET=1"]}),
            json!({"image": "rust", "command": []}),
            json!({"image": "rust", "command": ["true"], "workdir": "../x"}),
        ] {
            let output = call(&provider, &context, CONTAINER_RUN_TOOL, args.clone()).await?;
            assert!(
                !matches!(output, Ok(ToolOutput::Json { .. })),
                "{args} must not run: {output:?}"
            );
        }
        let bad_profile = call(
            &provider,
            &context,
            CONTAINER_RUN_TOOL,
            json!({"image": "rust", "command": ["true"], "profile": "privileged"}),
        )
        .await?;
        assert!(bad_profile.is_err());
        let unknown_field = call(
            &provider,
            &context,
            CONTAINER_RUN_TOOL,
            json!({"image": "rust", "command": ["true"], "mount": "/"}),
        )
        .await?;
        assert!(unknown_field.is_err(), "no way to ask for a mount");
        assert!(engine.plans.lock().map(|p| p.is_empty()).unwrap_or(false));
        Ok(())
    }

    #[test]
    fn owner_labels_are_label_safe() {
        assert_eq!(owner_label("abc-123"), "abc-123");
        assert_eq!(owner_label("a b=c"), "abc");
        assert!(owner_label("///").starts_with('s'));
        assert!(owner_label(&"x".repeat(200)).len() <= 49);
    }
}
