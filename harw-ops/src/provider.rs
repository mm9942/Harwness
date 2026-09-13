//! `/provider` operation — shows, lists, tests and switches configured providers.
//!
//! # Verantwortungsbereich
//! Implements the `/provider` command for the harw-ops crate. Reads **live runtime
//! state** from the [`SharedSessionController`] first, falling back to config-layer
//! defaults only when no explicit runtime state exists.
//!
//! # Security rule — no ModelTool
//! Provider switches are exclusively permitted as operator commands. The model MUST NOT
//! change its own provider — neither directly nor indirectly. The `permission = "operator"`
//! declaration enforces this at harness level. No `model_tool` attribute is set.
//!
//! # Sub-Commands
//! - `show` (default): reports the *runtime-active* provider from the controller snapshot;
//!   falls back to `harness.default_provider` from config if no switch has occurred.
//!   Also reports credential status (variant type, not value) for the active provider.
//! - `list`: enumerates every provider in the live resolved configuration and marks each as `[active]`,
//!   `[auth-ok]`, or `[auth-missing]`.
//! - `switch <provider-id>`: validates configured existence, enabled/auth state,
//!   and model compatibility before atomically updating the controller.
//! - `test`: shows the auth-ref type for the config-default provider (no secret value).
//!
//! # Exported Types
//! - [`ProviderArgs`] — argument struct for the `/provider` command.
//!
//! # Error Types
//! - [`harw_operations::OpError::Execution`]: controller not available in context.
//! - [`harw_operations::OpError::InvalidArguments`]: unknown subcommand, `switch` without
//!   ID, unknown provider ID, missing credentials, or incompatible active model.
//!
//! # Concurrency
//! The function is `async` but performs only synchronous reads except for the `switch`
//! mutation path. Thread-safe — the controller uses interior mutability.
//!
//! # Spec Reference
//! harwness Plan v2 — `/provider` meta-definition + Wave 5 runtime-truthful ops.

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput, SharedSessionController};
use std::sync::Arc;

/// Argument struct for the `/provider` command.
///
/// # Fields
/// - `cmd` (`Option<String>`): optional sub-command. Valid values:
///   - `"show"` (default) — shows the runtime-active provider.
///   - `"list"` — enumerates all configured providers (no secret values).
///   - `"test"` — shows auth-ref type of the default provider (no secret value).
///   - `"switch <provider-id>"` — switches the active provider via [`SessionController`].
///
/// # Note
/// Empty or absent `cmd` falls back to `"show"`. Multiple tokens (e.g. `["switch",
/// "anthropic"]`) are joined into a single space-separated string so that
/// `strip_prefix("switch ")` works correctly.
///
/// # Spec Reference
/// harwness Plan v2 — `/provider` sub-command table.
#[derive(Default, serde::Deserialize)]
pub struct ProviderArgs {
    /// Sub-command: `"show"` (default), `"list"`, `"test"`, `"switch <id>"`.
    #[serde(default)]
    pub cmd: Option<String>,
}

impl harw_operations::FromRawArgs for ProviderArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        // Join all tokens so that `/provider switch anthropic` (two tokens) is
        // normalised to `"switch anthropic"`, which `strip_prefix("switch ")` handles.
        let cmd = if tokens.is_empty() {
            None
        } else {
            Some(tokens.join(" "))
        };
        Ok(Self { cmd })
    }
}

/// Resolves a configured provider key or configured name to its canonical name.
///
/// The resolved config is the operation layer's provider authority.  Its map key
/// is normally the same as `ProviderToml::name`; accepting either keeps a
/// deliberately configured alias from leaking into session state.
fn configured_provider<'a>(
    config: &'a harw_config::ResolvedConfig,
    requested: &str,
) -> Option<(&'a str, &'a harw_config::ProviderToml)> {
    config.providers.iter().find_map(|(key, provider)| {
        (key == requested || provider.name == requested)
            .then_some((provider.name.as_str(), provider))
    })
}

fn configured_auth_is_present(provider: &harw_config::ProviderToml) -> bool {
    provider.auth.is_some() || provider.has_plaintext_secret()
}

fn configured_auth_status_label(provider: &harw_config::ProviderToml) -> &'static str {
    if provider.auth.is_some() {
        "auth-ref  [configured]"
    } else if provider.has_plaintext_secret() {
        "api-key  [configured plaintext]"
    } else {
        "none     [no auth configured — requests will be unauthenticated]"
    }
}

/// Resolves the operation's provider catalog from the execution context.
///
/// The context-scoped configuration is authoritative when present, which makes
/// provider operations composable with callers that already resolved their
/// configuration. Standalone operation execution retains the discovery fallback.
fn resolved_config(ctx: &OpContext) -> Result<Arc<harw_config::ResolvedConfig>, OpError> {
    if let Some(config) = ctx.service::<Arc<harw_config::ResolvedConfig>>() {
        return Ok(Arc::clone(config));
    }

    crate::config_util::load_default_config("Config-Discovery fehlgeschlagen").map(Arc::new)
}

/// Shows, lists, tests and switches configured providers, using live runtime state.
///
/// # Description
/// Reads live session state from [`SharedSessionController`] and dispatches on
/// the sub-command:
///
/// - **`show`** (default): reports the runtime-active provider (from controller
///   snapshot). Falls back to the config default and labels it clearly. Also reports
///   the credential variant type for the active provider (no secret value).
/// - **`list`**: enumerates every provider in the resolved configuration; marks each as active,
///   `auth-ok`, or `auth-missing`.
/// - **`switch <id>`**: validates provider existence, credentials, and active-model
///   compatibility before atomically switching via the controller.
/// - **`test`**: shows the auth-ref type for the config-default provider.
/// - **anything else**: returns [`OpError::InvalidArguments`].
///
/// # Arguments
/// - `ctx` (`&OpContext`): execution context — required for `switch` and `show`
///   to access the [`SharedSessionController`] from the [`ServiceMap`].
/// - `args` (`ProviderArgs`): contains the optional sub-command.
///
/// # Returns
/// [`OpOutput`] with compact, multi-line text.
///
/// # Errors
/// - [`OpError::Execution`]: when the [`SessionController`] is not registered in context.
/// - [`OpError::InvalidArguments`]: unknown sub-command; `switch` without ID;
///   unknown provider ID; missing credentials; incompatible active model.
///
/// # Panics
/// None.
///
/// # Concurrency
/// Stateless on read paths; the `switch` path calls `controller.set_active_provider`
/// which uses interior mutability. Thread-safe.
///
/// # Examples
/// ```rust,no_run
/// // Invoked via the harw dispatcher — no direct calls.
/// // /provider            → shows runtime-active provider
/// // /provider list       → lists all configured providers
/// // /provider test       → shows auth-ref type of config-default provider
/// // /provider switch foo → switches active provider to "foo"
/// ```
#[operation(
    name = "provider",
    summary = "Zeigt aktiven Provider; listet/testet/wechselt konfigurierte Provider.",
    domain = "catalog_config",
    permission = "operator",
    aliases = ["p"],
    category = "model",
    command(path = "/provider", visibility = "tui_only"),
)]
async fn provider(ctx: &OpContext, args: ProviderArgs) -> Result<OpOutput, OpError> {
    let sub = args.cmd.as_deref().unwrap_or("show");

    // ── `switch <id>` — atomic validation + mutation via SessionController ────
    if let Some(target) = sub.strip_prefix("switch ") {
        let target = target.trim().to_string();
        if target.is_empty() {
            return Err(OpError::InvalidArguments(
                "switch requires a provider ID: /provider switch <id>".into(),
            ));
        }
        return handle_switch(ctx, target);
    }

    match sub {
        "show" => handle_show(ctx),
        "list" => handle_list(ctx),
        "test" => handle_test(ctx),
        other => Err(OpError::InvalidArguments(format!(
            "Unknown /provider sub-command: '{other}'. \
             Supported: show, list, switch <id>, test."
        ))),
    }
}

/// Implements `/provider show` — runtime-truthful active-provider display.
///
/// # Description
/// Checks `controller.snapshot().active_provider` first. If `Some`, that is the
/// live provider. Looks it up in the resolved configuration to display its name
/// and credential status. If `None`, falls back to config-default and labels it as
/// such. Emits a warning line if the active provider ID is unknown to the configuration.
///
/// # Arguments
/// - `ctx` (`&OpContext`): used to obtain the [`SharedSessionController`].
///
/// # Returns
/// [`OpOutput`] with provider ID, name, and credential status.
///
/// # Errors
/// - [`OpError::Execution`]: controller not in context.
///
/// # Spec Reference
/// harwness Plan v2 — Task A: `/provider show` becomes runtime-truthful.
fn handle_show(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let controller = ctx
        .service::<SharedSessionController>()
        .ok_or_else(|| OpError::Execution("SessionController not available".into()))?;

    let config = resolved_config(ctx)?;
    if config.providers.is_empty() {
        return Err(OpError::Execution(
            "configured provider catalog is unavailable; refusing to select a provider".into(),
        ));
    }

    let snap = controller.snapshot();

    if let Some(active_id) = snap.active_provider {
        match configured_provider(&config, &active_id) {
            Some((canonical_id, provider)) => {
                let auth = configured_auth_status_label(provider);
                let text = format!(
                    "Active provider : {canonical_id}  (runtime, explicitly switched)\n\
                     Name            : {display_name}\n\
                     Credentials     : {auth}",
                    display_name = provider.name,
                );
                Ok(OpOutput { text })
            }
            None => {
                let text = format!(
                    "Active provider : {active_id}  (runtime, explicitly switched)\n\
                     WARNING: provider '{active_id}' is not present in the configured provider catalog. \
                     State may be stale — use `/provider list` to see configured providers."
                );
                Ok(OpOutput { text })
            }
        }
    } else {
        let text = match &config.harness.default_provider {
            Some(name) if configured_provider(&config, name).is_some() => {
                let (canonical_id, _) = configured_provider(&config, name)
                    .expect("configured provider was checked in the match guard");
                format!(
                    "Active provider : {canonical_id}  (default from config, not yet switched)\n\
                 Use `/provider switch <id>` to change the active provider."
                )
            }
            Some(name) => format!(
                "Default provider '{name}' is not present in the configured provider catalog. \
                 Use `harw onboard` to repair the configuration."
            ),
            None => "No default provider configured. Use `harw onboard` to set one up.".to_owned(),
        };
        Ok(OpOutput { text })
    }
}

/// Implements `/provider list` — configuration-driven provider enumeration.
///
/// # Description
/// Enumerates all providers in the resolved configuration. For each provider,
/// marks it as `[active]`, `[auth-ok]`, or `[auth-missing]`.
///
/// # Arguments
/// - `ctx` (`&OpContext`): used to obtain the controller snapshot for the active ID.
///
/// # Returns
/// [`OpOutput`] with the provider list.
///
/// # Errors
/// - [`OpError::Execution`]: controller not in context.
///
/// # Spec Reference
/// harwness Plan v2 — Task B: `/provider list` becomes configuration-driven.
fn handle_list(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let controller = ctx
        .service::<SharedSessionController>()
        .ok_or_else(|| OpError::Execution("SessionController not available".into()))?;

    let config = resolved_config(ctx)?;
    let active_id = controller.snapshot().active_provider;
    let active_canonical = active_id
        .as_deref()
        .and_then(|id| configured_provider(&config, id).map(|(canonical, _)| canonical));

    let mut lines = vec!["Registered providers:".to_owned()];
    let mut providers: Vec<_> = config.providers.values().collect();
    providers.sort_by(|left, right| left.name.cmp(&right.name));
    for provider in providers {
        let is_active = active_canonical == Some(provider.name.as_str());
        let status = if is_active {
            "[active]"
        } else if configured_auth_is_present(provider) {
            "[auth-ok]"
        } else {
            "[auth-missing]"
        };
        lines.push(format!(
            "  {name}  id={id}  {status}",
            name = provider.name,
            id = provider.name,
        ));
    }

    if let Some(ref aid) = active_id {
        lines.push(format!("\nActive: {aid}  (runtime-switched)"));
    } else {
        lines.push("\nNo provider explicitly switched — using config default.".to_owned());
    }

    Ok(OpOutput {
        text: lines.join("\n"),
    })
}

/// Implements `/provider switch <id>` — atomic, validated provider switch.
///
/// # Description
/// Performs three checks before mutating the controller:
/// 1. Validates the provider exists in the resolved configuration. Returns [`OpError::InvalidArguments`] with
///    `"unknown provider: <id>"` if not found.
/// 2. Validates enabled/auth configuration and returns [`OpError::InvalidArguments`]
///    with a credential hint when it cannot be used.
/// 3. Validates active-model compatibility: if `controller.snapshot().active_model`
///    is `Some(m)`, resolves `m` in the configured model catalog and checks whether
///    its configured provider matches the target provider. If incompatible,
///    returns [`OpError::InvalidArguments`] with a message asking the operator to
///    use `/model switch` first. **Never silently falls back to another model.**
/// 4. Only when all checks pass, calls `controller.set_active_provider(id)`.
///
/// # Arguments
/// - `ctx` (`&OpContext`): used to obtain the controller.
/// - `target` (`String`): the provider ID to switch to (already trimmed).
///
/// # Returns
/// [`OpOutput`] with `"provider switched to <id>; next turn will use it"`.
///
/// # Errors
/// - [`OpError::Execution`]: controller not in context, or controller mutation failed.
/// - [`OpError::InvalidArguments`]: unknown provider, missing credentials, or model
///   incompatibility.
///
/// # Spec Reference
/// harwness Plan v2 — Task C: `/provider switch` becomes atomic + compatibility-checked.
fn handle_switch(ctx: &OpContext, target: String) -> Result<OpOutput, OpError> {
    let config = resolved_config(ctx)?;
    if config.providers.is_empty() {
        return Err(OpError::Execution(
            "configured provider catalog is unavailable; refusing to switch providers".into(),
        ));
    }

    // ── Step 1: validate against the configured provider catalog ─────────────
    let (canonical_target, provider) = configured_provider(&config, &target)
        .ok_or_else(|| OpError::InvalidArguments(format!("unknown provider: {target}")))?;
    let canonical_target = canonical_target.to_owned();

    // ── Step 2: validate credentials ─────────────────────────────────────────
    if !provider.enabled {
        return Err(OpError::InvalidArguments(format!(
            "provider {canonical_target}: disabled in configuration"
        )));
    }
    if !configured_auth_is_present(provider) {
        return Err(OpError::InvalidArguments(format!(
            "provider {canonical_target}: credentials not available \
             (provider has no configured auth — configure auth before switching)"
        )));
    }

    // ── Step 3: validate active-model compatibility ───────────────────────────
    let controller = ctx
        .service::<SharedSessionController>()
        .ok_or_else(|| OpError::Execution("SessionController not available".into()))?;

    let snap = controller.snapshot();
    if let Some(ref active_model) = snap.active_model {
        if let Some(model) = config.models.values().find(|model| {
            model.id == *active_model || model.aliases.iter().any(|alias| alias == active_model)
        }) {
            let model_provider = configured_provider(&config, &model.provider)
                .map(|(canonical, _)| canonical)
                .unwrap_or(model.provider.as_str());
            if model_provider != canonical_target {
                return Err(OpError::InvalidArguments(format!(
                    "active model '{active_model}' is not available on provider '{canonical_target}' \
                     (model belongs to provider '{model_provider}'); \
                     use `/model switch` first or accept a provider-appropriate model."
                )));
            }
        }
        // An unknown active model is outside this operation's authority. Its runtime
        // executor remains responsible for rejecting it; provider selection stays
        // constrained to the configured provider catalog above.
    }

    // ── Step 4: mutate the controller ─────────────────────────────────────────
    controller
        .set_active_provider(canonical_target.clone())
        .map_err(|e| OpError::Execution(e.to_string()))?;

    Ok(OpOutput {
        text: format!("provider switched to {canonical_target}; next turn will use it"),
    })
}

/// Implements `/provider test` — shows the auth-ref type for the config-default provider.
///
/// # Description
/// Reads `harness.default_provider` from the config layer and reports what
/// auth method is configured. Shows only the variant type — never the secret
/// value. Also reports whether the provider is enabled.
///
/// # Returns
/// [`OpOutput`] with auth-type label and provider status.
///
/// # Errors
/// - [`OpError::Execution`]: config discovery failed.
///
/// # Concurrency
/// Stateless; thread-safe.
///
/// # Spec Reference
/// harwness Plan v2 — `/provider test`.
fn handle_test(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let config = resolved_config(ctx)?;

    let Some(default_name) = &config.harness.default_provider else {
        return Ok(OpOutput {
            text: "No default provider configured — no test possible. \
                   Use `harw onboard` to set one up."
                .to_owned(),
        });
    };

    let Some(provider_toml) = config.providers.get(default_name) else {
        return Ok(OpOutput {
            text: format!(
                "Default provider '{default_name}' is listed in harness config but \
                 no matching provider entry was found.\n\
                 Run `harw onboard` again or check your config layers."
            ),
        });
    };

    let auth_info = match &provider_toml.auth {
        Some(secret_ref) => {
            // Show only the ref type (prefix before ':'), never the value.
            let ref_string = secret_ref.as_ref_string();
            let ref_type = ref_string
                .split_once(':')
                .map(|(prefix, _)| prefix)
                .unwrap_or("unknown");
            format!("Auth method: {ref_type}:  [ref present — value not shown]")
        }
        None if provider_toml.has_plaintext_secret() => {
            "Auth method: api_key (plaintext) — WARNING: insecure; \
             use an `auth` SecretRef instead."
                .to_owned()
        }
        None => "Auth method: (none) — provider has no `auth` field configured.".to_owned(),
    };

    let status = if provider_toml.enabled {
        "enabled"
    } else {
        "disabled"
    };

    Ok(OpOutput {
        text: format!(
            "Default provider : {default_name}  [{status}]\n\
             API type         : {api}\n\
             {auth_info}\n\
             Note: live connection test (HTTP ping) not yet wired — \
             re-run after harw-provider-http integration.",
            api = provider_toml.api,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{ProviderArgs, configured_provider};
    use harw_operations::{
        FromRawArgs, NullSessionController, OpContext, OpError, SharedSessionController,
        context::ServiceMap,
    };
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;

    use crate::testutil::toks;

    // ── Context builder ───────────────────────────────────────────────────────

    /// Builds a minimal `OpContext` with optional controller and config services.
    fn make_test_ctx(
        ctrl: Option<SharedSessionController>,
        config: Option<Arc<harw_config::ResolvedConfig>>,
    ) -> (OpContext, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("harw-provider-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(tmp.join("ws")).unwrap();
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("WorkspaceRegistry::build");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("resolve binding");
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let mut services = ServiceMap::new();
        if let Some(c) = ctrl {
            services.insert(c);
        }
        if let Some(config) = config {
            services.insert(config);
        }
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        (ctx, tmp)
    }

    // ── Existing arg-parsing tests (preserved) ────────────────────────────────

    #[test]
    fn test_provider_args_from_raw_args_sets_cmd() {
        let args = ProviderArgs::from_raw_args(&toks(&["list"]));
        match args {
            Ok(a) => assert_eq!(a.cmd.as_deref(), Some("list")),
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn test_provider_args_from_raw_args_empty_tokens_sets_cmd_none() {
        let args = ProviderArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.cmd.is_none()),
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    /// `switch` without a provider ID must return `OpError::InvalidArguments`.
    #[tokio::test]
    async fn test_provider_switch_requires_target_id() {
        let args = ProviderArgs {
            cmd: Some("switch ".to_string()),
        };
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None);

        let result = super::provider(&ctx, args).await;

        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(
                    msg.contains("switch"),
                    "Error message must contain 'switch': {msg}"
                );
            }
            other => panic!("Expected OpError::InvalidArguments, got: {other:?}"),
        }
    }

    /// `FromRawArgs` joins multiple tokens so that `["switch", "anthropic"]`
    /// becomes `"switch anthropic"`, which `strip_prefix("switch ")` recognises.
    #[test]
    fn test_provider_args_recognizes_switch_prefix() {
        let args = ProviderArgs::from_raw_args(&toks(&["switch", "anthropic"]))
            .expect("from_raw_args must not fail");
        let cmd = args.cmd.expect("cmd must be set");
        assert!(
            cmd.strip_prefix("switch ").is_some(),
            "cmd must start with 'switch ', got: {cmd:?}"
        );
        assert_eq!(
            cmd.strip_prefix("switch ").unwrap().trim(),
            "anthropic",
            "Provider ID after 'switch ' must be 'anthropic'"
        );
    }

    #[test]
    fn configured_provider_canonicalizes_a_configured_alias() {
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "operator-alias".to_owned(),
            harw_config::ProviderToml {
                name: "canonical-provider".to_owned(),
                api: "openai-chat".to_owned(),
                base_url: "https://api.example.test/v1".to_owned(),
                auth: None,
                auth_header: None,
            api_key: None,
                headers: Default::default(),
                models: Vec::new(),
                enabled: true,
                origin_allowlist: Default::default(),
            },
        );

        let (canonical, _) =
            configured_provider(&config, "operator-alias").expect("configured alias must resolve");
        assert_eq!(canonical, "canonical-provider");
    }

    #[test]
    fn resolved_config_prefers_context_service() {
        let config = Arc::new(harw_config::ResolvedConfig::default());
        let (ctx, _tmp) = make_test_ctx(None, Some(Arc::clone(&config)));

        let resolved = super::resolved_config(&ctx).expect("context config must resolve");

        assert!(
            Arc::ptr_eq(&resolved, &config),
            "the context-scoped resolved config must be authoritative"
        );
    }

    // ── Task D: provider_unknown_subcommand_rejected ──────────────────────────

    /// An unrecognised sub-command must return `OpError::InvalidArguments`.
    #[tokio::test]
    async fn provider_unknown_subcommand_rejected() {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None);

        let args = ProviderArgs {
            cmd: Some("frobnicator".to_owned()),
        };
        let result = super::provider(&ctx, args).await;

        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(
                    msg.contains("frobnicator"),
                    "Error must contain the unrecognised command: {msg}"
                );
                assert!(
                    msg.contains("show") && msg.contains("list") && msg.contains("switch"),
                    "Error must list the supported sub-commands: {msg}"
                );
            }
            other => {
                panic!("Expected OpError::InvalidArguments for unknown sub-command, got: {other:?}")
            }
        }
    }
}
