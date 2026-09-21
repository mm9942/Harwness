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
//! - `test`: shows the auth-ref type for the config-default provider (no secret value).
//!
//! `switch` is **no longer** a `/provider` sub-command (Welle 2, 2d). An atomic
//! provider(+model) switch is exclusively driven by `/model switch <id>` (and,
//! on the UIA axis, `/uia-model switch <id>`), which resolve the target
//! model's configured provider and delegate to [`handle_switch_core`]
//! (respectively [`handle_uia_switch_core`]) here — both are `pub(crate)` for
//! exactly this. `/provider switch ...` now falls into the unknown-sub-command
//! catchall, whose message points the operator to `/model`.
//!
//! # Exported Types
//! - [`ProviderArgs`] — argument struct for the `/provider` command.
//!
//! # Error Types
//! - [`harw_operations::OpError::Execution`]: controller not available in context.
//! - [`harw_operations::OpError::InvalidArguments`]: unknown subcommand, unknown
//!   provider ID, missing credentials, or incompatible active model.
//!
//! # Concurrency
//! The function is `async` but performs only synchronous reads except for the
//! [`handle_switch_core`]/[`handle_uia_switch_core`] mutation path invoked from
//! `harw-ops::model`. Thread-safe — the controller uses interior mutability.
//!
//! # Spec Reference
//! harwness Plan v2 — `/provider` meta-definition + Wave 5 runtime-truthful ops;
//! Welle 2 (2d) — `/model switch` becomes the sole atomic provider+model switch.

use harw_macros::operation;
use harw_operations::{
    OpContext, OpError, OpOutput, SessionController, SharedSessionController,
};
use harw_operations::session_control::UiaSelection;
use std::sync::Arc;

/// Argument struct for the `/provider` command.
///
/// # Fields
/// - `cmd` (`Option<String>`): optional sub-command. Valid values:
///   - `"show"` (default) — shows the runtime-active provider.
///   - `"list"` — enumerates all configured providers (no secret values).
///   - `"test"` — shows auth-ref type of the default provider (no secret value).
///
///   Any other value (including a bare `"switch ..."`, no longer supported
///   here — see `/model switch`) is rejected with [`harw_operations::OpError::InvalidArguments`].
///
/// # Note
/// Empty or absent `cmd` falls back to `"show"`. Multiple tokens are joined
/// into a single space-separated string.
///
/// # Spec Reference
/// harwness Plan v2 — `/provider` sub-command table; Welle 2 (2d) — `switch` retired.
#[derive(Default, serde::Deserialize)]
pub struct ProviderArgs {
    /// Sub-command: `"show"` (default), `"list"`, `"test"`.
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
///
/// `pub(crate)` so [`crate::model::handle_switch_core`],
/// [`crate::model::handle_uia_model_switch`] and
/// [`crate::model::handle_uia_worker_model_switch`] resolve the target
/// model's configured provider through the same context-scoped-first
/// authority that this function's own `handle_switch_core`/
/// `handle_uia_switch_core` already use — the runtime (`harw-tui`'s
/// `command_exec::build_services`) injects `Arc<harw_config::ResolvedConfig>`
/// into the `ServiceMap` specifically for `/model`- and `/provider`-ops.
pub(crate) fn resolved_config(ctx: &OpContext) -> Result<Arc<harw_config::ResolvedConfig>, OpError> {
    if let Some(config) = ctx.service::<Arc<harw_config::ResolvedConfig>>() {
        return Ok(Arc::clone(config));
    }

    crate::config_util::load_default_config("Config-Discovery fehlgeschlagen").map(Arc::new)
}

/// Shows, lists and tests configured providers, using live runtime state.
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
/// - **`test`**: shows the auth-ref type for the config-default provider.
/// - **anything else** (including `switch ...`, retired here — see `/model
///   switch`): returns [`OpError::InvalidArguments`] pointing to `/model`.
///
/// # Arguments
/// - `ctx` (`&OpContext`): execution context — required for `show` and `list`
///   to access the [`SharedSessionController`] from the [`ServiceMap`].
/// - `args` (`ProviderArgs`): contains the optional sub-command.
///
/// # Returns
/// [`OpOutput`] with compact, multi-line text.
///
/// # Errors
/// - [`OpError::Execution`]: when the [`SessionController`] is not registered in context.
/// - [`OpError::InvalidArguments`]: unknown sub-command; unknown provider ID;
///   missing credentials; incompatible active model.
///
/// # Panics
/// None.
///
/// # Concurrency
/// Stateless — read paths only. The atomic mutation path lives in
/// [`handle_switch_core`], reached exclusively via `/model switch` in
/// `harw-ops::model`, not through this function's own dispatch.
///
/// # Examples
/// ```rust,no_run
/// // Invoked via the harw dispatcher — no direct calls.
/// // /provider            → shows runtime-active provider
/// // /provider list       → lists all configured providers
/// // /provider test       → shows auth-ref type of config-default provider
/// // /model switch <id>   → atomically switches provider+model (see harw-ops::model)
/// ```
#[operation(
    name = "provider",
    summary = "Zeigt aktiven Provider; listet/testet konfigurierte Provider.",
    domain = "catalog_config",
    permission = "operator",
    aliases = ["p"],
    category = "model",
    command(path = "/provider", visibility = "tui_only", busy = "immediate"),
)]
async fn provider(ctx: &OpContext, args: ProviderArgs) -> Result<OpOutput, OpError> {
    let sub = args.cmd.as_deref().unwrap_or("show");

    // `switch` is no longer a `/provider` sub-command: an atomic
    // provider+model switch is now exclusively driven by `/model switch`
    // (which delegates to `handle_switch_core` below), so that a target
    // model belonging to a different provider is always accepted, not just
    // rejected with a hint. `switch ...` therefore falls into the catchall
    // arm below like any other unknown sub-command.
    match sub {
        "show" => handle_show(ctx),
        "list" => handle_list(ctx),
        "test" => handle_test(ctx),
        other => Err(OpError::InvalidArguments(format!(
            "Unknown /provider sub-command: '{other}'. \
             Supported: show, list, test. Use `/model` to change the active provider and model together."
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
                Ok(OpOutput::from(text))
            }
            None => {
                let text = format!(
                    "Active provider : {active_id}  (runtime, explicitly switched)\n\
                     WARNING: provider '{active_id}' is not present in the configured provider catalog. \
                     State may be stale — use `/provider list` to see configured providers."
                );
                Ok(OpOutput::from(text))
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
        Ok(OpOutput::from(text))
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

    Ok(OpOutput::from(lines.join("\n")))
}

/// Shared core of the atomic, validated provider (and optional model) switch.
///
/// # Description
/// Performs every validation step before mutating the controller, so a
/// failed check never leaves the session in a half-switched state:
/// 1. Validates the provider exists in the resolved configuration. Returns [`OpError::InvalidArguments`] with
///    `"unknown provider: <id>"` if not found.
/// 2. Validates enabled/auth configuration and returns [`OpError::InvalidArguments`]
///    with a credential hint when it cannot be used.
/// 3. Validates model compatibility:
///    - If `model` is `Some`, resolves it in the configured model catalog
///      (id or alias, via [`crate::model::configured_model`]) and rejects it
///      with [`OpError::InvalidArguments`] if it does not exist or belongs to
///      a different (canonicalized) provider than the target.
///    - If `model` is `None`, falls back to the previous behavior: if
///      `controller.snapshot().active_model` is `Some(m)`, resolves `m` in the
///      configured model catalog and checks whether its configured provider
///      matches the target provider. If incompatible, returns
///      [`OpError::InvalidArguments`] with a message asking the operator to
///      use `/model switch` first. **Never silently falls back to another
///      model.**
/// 4. Only when all checks pass, calls `controller.set_active_provider(id)`
///    and, if a model argument was given, `controller.set_active_model(id)`.
/// 5. Best-effort persists the resulting active provider/model as the
///    profile's on-disk default via
///    [`crate::config_util::persist_default_selection`], so future sessions
///    start with the same selection. A persistence failure never fails the
///    operation — it is appended to the success text as a clear note instead.
///
/// `pub(crate)` because this is now the sole atomic provider(+model) switch
/// path: `/provider switch`/`/uia-provider switch` were retired as
/// sub-commands (an operator now always goes through `/model switch` or
/// `/uia-model switch`, which resolve the target model's configured provider
/// and delegate here so a provider+model pair never passes through a moment
/// of incompatibility). [`crate::model::handle_switch_core`] calls this
/// directly with `persist = `[`crate::config_util::persist_default_selection`].
///
/// # Arguments
/// - `ctx` (`&OpContext`): used to obtain the controller.
/// - `target` (`String`): the provider ID to switch to (already trimmed).
/// - `model` (`Option<String>`): an optional model ID/alias to switch to in
///   the same call.
/// - `persist` (`impl FnOnce(Option<&str>, Option<&str>) -> Option<String>`):
///   called once, after the controller mutation succeeds, with
///   `(Some(canonical_provider), resolved_model.as_deref())`. Returns `None`
///   on successful persistence or `Some(note)` with a human-readable failure
///   note to append to the output.
///
/// # Returns
/// [`OpOutput`] confirming the switch, with a trailing persistence note.
///
/// # Errors
/// - [`OpError::Execution`]: controller not in context, or controller mutation failed.
/// - [`OpError::InvalidArguments`]: unknown provider, missing credentials, unknown
///   model, or provider/model incompatibility.
///
/// # Spec Reference
/// harwness Plan v2 — Task C; `/uia-provider` follow-up (UIA-specific pinned selection);
/// Welle 2 (2d) — `/model switch`/`/uia-model switch` become the sole atomic
/// provider+model switch entry points.
pub(crate) fn handle_switch_core(
    ctx: &OpContext,
    target: String,
    model: Option<String>,
    persist: impl FnOnce(Option<&str>, Option<&str>) -> Option<String>,
) -> Result<OpOutput, OpError> {
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

    let controller = ctx
        .service::<SharedSessionController>()
        .ok_or_else(|| OpError::Execution("SessionController not available".into()))?;

    // ── Step 3: validate model compatibility ──────────────────────────────────
    // An explicit model argument is validated against the catalog directly;
    // otherwise the *current* active model (if any) must already be
    // compatible with the target provider.
    let resolved_model: Option<String> = match &model {
        Some(requested_model) => {
            let configured = crate::model::configured_model(&config, requested_model)
                .ok_or_else(|| OpError::InvalidArguments(format!("unknown model: {requested_model}")))?;
            let model_provider = configured_provider(&config, &configured.provider)
                .map(|(canonical, _)| canonical)
                .unwrap_or(configured.provider.as_str());
            if model_provider != canonical_target {
                return Err(OpError::InvalidArguments(format!(
                    "model '{requested_model}' is not available on provider '{canonical_target}' \
                     (model belongs to provider '{model_provider}')"
                )));
            }
            Some(configured.id.clone())
        }
        None => {
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
            None
        }
    };

    // ── Step 4: mutate the controller ─────────────────────────────────────────
    controller
        .set_active_provider(canonical_target.clone())
        .map_err(|e| OpError::Execution(e.to_string()))?;
    if let Some(ref model_id) = resolved_model {
        controller
            .set_active_model(model_id.clone())
            .map_err(|e| OpError::Execution(e.to_string()))?;
    }

    // ── Step 5: persist as the profile's default for future sessions ─────────
    // Read back the post-mutation snapshot rather than re-deriving it, so a
    // provider-only switch with no active model never writes a stale
    // `default_model`.
    let snap_after = controller.snapshot();
    let mut text = match &resolved_model {
        Some(model_id) => format!(
            "provider switched to {canonical_target}; model switched to {model_id}; \
             next turn will use it"
        ),
        None => format!("provider switched to {canonical_target}; next turn will use it"),
    };
    match persist(
        Some(canonical_target.as_str()),
        snap_after.active_model.as_deref(),
    ) {
        Some(note) => {
            text.push('\n');
            text.push_str(&note);
        }
        None => text.push_str("\n(als Standard für künftige Sitzungen gespeichert)"),
    }

    Ok(OpOutput::from(text))
}

/// Wechselt ausschließlich die UIA-Auswahl.
///
/// Das aktuelle UIA-Modell stammt aus der Live-Auswahl oder aus
/// `harness.uia_model`; generische `active_*`-Werte werden nie übernommen.
///
/// `pub(crate)`, weil dies inzwischen der einzige atomare UIA-Provider(+Modell)-
/// Wechselpfad ist: `/uia-provider switch` wurde als Unterbefehl entfernt —
/// [`crate::model::handle_uia_model_switch`] löst stattdessen den konfigurierten
/// Provider des Ziel-Modells auf und delegiert direkt hierher.
///
/// `persist` ist — wie bei [`handle_switch_core`] — ein injizierter Abschluss
/// statt eines hartcodierten Aufrufs von
/// [`crate::config_util::persist_uia_selection`]. Der einzige Produktions-
/// Aufrufer ([`crate::model::handle_uia_model_switch`]) übergibt weiterhin
/// genau diese Funktion, sodass sich am Laufzeitverhalten nichts ändert;
/// die Injektion existiert, damit Tests einen No-op-Abschluss einsetzen
/// können und **niemals** die echte, `HARW_HOME`-auflösende Persistenz
/// berühren — diese Crate deklariert `#![forbid(unsafe_code)]`, sodass eine
/// testweise `HARW_HOME`-Env-Isolation (die `unsafe fn
/// std::env::set_var`/`remove_var` bräuchte) hier nicht zur Verfügung steht.
pub(crate) fn handle_uia_switch_core(
    ctx: &OpContext,
    target: String,
    model: Option<String>,
    persist: impl FnOnce(Option<&str>, Option<&str>) -> Option<String>,
) -> Result<OpOutput, OpError> {
    let config = resolved_config(ctx)?;
    if config.providers.is_empty() {
        return Err(OpError::Execution(
            "configured provider catalog is unavailable; refusing to switch UIA providers".into(),
        ));
    }

    let (canonical_target, provider) = configured_provider(&config, &target)
        .ok_or_else(|| OpError::InvalidArguments(format!("unknown provider: {target}")))?;
    let canonical_target = canonical_target.to_owned();
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

    let controller = ctx
        .service::<SharedSessionController>()
        .ok_or_else(|| OpError::Execution("SessionController not available".into()))?;
    let current = crate::model::effective_uia_selection(Some(controller), &config);

    let mut cleared_incompatible_model = None;
    let resolved_model = match model {
        Some(requested_model) => {
            let configured = crate::model::configured_model(&config, &requested_model)
                .ok_or_else(|| OpError::InvalidArguments(format!("unknown model: {requested_model}")))?;
            let model_provider = configured_provider(&config, &configured.provider)
                .map(|(canonical, _)| canonical)
                .unwrap_or(configured.provider.as_str());
            if model_provider != canonical_target {
                return Err(OpError::InvalidArguments(format!(
                    "model '{requested_model}' is not available on UIA provider '{canonical_target}' \
                     (model belongs to provider '{model_provider}'); \
                     use `/uia-model switch <id>` for a compatible UIA model"
                )));
            }
            Some(configured.id.clone())
        }
        None => match current.model() {
            None => None,
            Some(current_model) => match crate::model::configured_model(&config, current_model) {
                Some(configured) => {
                    let model_provider = configured_provider(&config, &configured.provider)
                        .map(|(canonical, _)| canonical)
                        .unwrap_or(configured.provider.as_str());
                    if model_provider == canonical_target {
                        Some(configured.id.clone())
                    } else {
                        cleared_incompatible_model = Some(current_model.to_owned());
                        None
                    }
                }
                None => {
                    cleared_incompatible_model = Some(current_model.to_owned());
                    None
                }
            }
        },
    };

    let selection = UiaSelection::new(Some(canonical_target.clone()), resolved_model.clone());
    controller
        .set_uia_selection(selection.clone())
        .map_err(|e| OpError::Execution(e.to_string()))?;

    let mut text = match &resolved_model {
        Some(model_id) => format!(
            "UIA provider switched to {canonical_target}; UIA model is {model_id}; \
             next UIA turn will use it"
        ),
        None => match cleared_incompatible_model {
            Some(previous_model) => format!(
                "UIA provider switched to {canonical_target}; incompatible UIA model \
                 {previous_model} was cleared; use `/uia-model list` to choose a compatible model"
            ),
            None => format!(
                "UIA provider switched to {canonical_target}; no UIA model selected; \
                 use `/uia-model list` to choose one"
            ),
        },
    };
    match persist(selection.provider.as_deref(), selection.model.as_deref()) {
        Some(note) => {
            text.push('\n');
            text.push_str(&note);
        }
        None => text.push_str("\n(UIA-Auswahl für künftige Sitzungen gespeichert)"),
    }

    Ok(OpOutput::from(text))
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
        return Ok(OpOutput::from(
            "No default provider configured — no test possible. \
             Use `harw onboard` to set one up."
                .to_owned(),
        ));
    };

    let Some(provider_toml) = config.providers.get(default_name) else {
        return Ok(OpOutput::from(format!(
            "Default provider '{default_name}' is listed in harness config but \
             no matching provider entry was found.\n\
             Run `harw onboard` again or check your config layers."
        )));
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

    Ok(OpOutput::from(format!(
        "Default provider : {default_name}  [{status}]\n\
         API type         : {api}\n\
         {auth_info}\n\
         Note: live connection test (HTTP ping) not yet wired — \
         re-run after harw-provider-http integration.",
        api = provider_toml.api,
    )))
}

/// Implements `/uia-provider` — shows, lists and tests the UIA's
/// own pinned provider selection (`harness.uia_provider`), independent of
/// `default_provider`.
///
/// # Description
/// Reuses [`ProviderArgs`], since the sub-command grammar is otherwise
/// unchanged:
///
/// - **`show`** (default): reports the effective UIA provider/model from the
///   live UIA selection, otherwise `uia_provider`/`uia_model` config.
/// - **`list`**: lists the provider catalog and marks the effective UIA provider.
/// - **`test`**: tests the effective UIA provider's configured auth variant.
/// - **anything else** (including `switch ...`, retired here — see
///   `/uia-model switch`, which resolves the target model's configured
///   provider and delegates to [`handle_uia_switch_core`]): returns
///   [`OpError::InvalidArguments`] pointing to `/uia-model`.
///
/// # Arguments
/// - `ctx` (`&OpContext`): execution context.
/// - `args` (`ProviderArgs`): contains the optional sub-command.
///
/// # Returns
/// [`OpOutput`] with compact, multi-line text.
///
/// # Errors
/// - [`OpError::Execution`]: controller not in context, or config discovery failed.
/// - [`OpError::InvalidArguments`]: unknown sub-command; unknown provider ID;
///   missing credentials; incompatible active model.
///
/// # Panics
/// None.
///
/// # Concurrency
/// Stateless — read paths only. The atomic mutation path lives in
/// [`handle_uia_switch_core`], reached exclusively via `/uia-model switch` in
/// `harw-ops::model`.
///
/// # Spec Reference
/// harwness Plan v2 — UIA-specific pinned provider/model selection.
#[operation(
    name = "uia-provider",
    summary = "Zeigt/wechselt den für die UIA gepinnten Provider (uia_provider), unabhängig vom Default.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(path = "/uia-provider", visibility = "tui_only"),
)]
async fn uia_provider(ctx: &OpContext, args: ProviderArgs) -> Result<OpOutput, OpError> {
    let sub = args.cmd.as_deref().unwrap_or("show");

    // `switch` is no longer a `/uia-provider` sub-command — see the
    // analogous comment on `provider()` above. The atomic UIA provider+model
    // switch now lives exclusively behind `/uia-model switch`, which
    // delegates to `handle_uia_switch_core` below.
    match sub {
        "show" => handle_uia_show(ctx),
        "list" => handle_uia_list(ctx),
        "test" => handle_uia_test(ctx),
        other => Err(OpError::InvalidArguments(format!(
            "Unknown /uia-provider sub-command: '{other}'. \
             Supported: show, list, test. Use `/uia-model` to change the active provider and model together."
        ))),
    }
}

/// Implements `/uia-provider show` — reports the UIA's pinned provider.
///
/// # Description
/// Reads `config.harness.uia_provider` directly (a persisted config value,
/// not runtime-switched state — the UIA pin is not mutated by
/// `/provider switch`). If unset, reports the fallback to `default_provider`
/// (or the absence of any configured default).
///
/// # Arguments
/// - `ctx` (`&OpContext`): used to resolve the configuration.
///
/// # Returns
/// [`OpOutput`] with the pinned provider ID, name, and credential status, or
/// a fallback note.
///
/// # Errors
/// - [`OpError::Execution`]: config discovery failed, or the configured
///   provider catalog is unavailable.
///
/// # Spec Reference
/// harwness Plan v2 — UIA-specific pinned provider/model selection.
fn handle_uia_show(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let config = resolved_config(ctx)?;
    if config.providers.is_empty() {
        return Err(OpError::Execution(
            "configured provider catalog is unavailable; refusing to select a provider".into(),
        ));
    }

    let controller = ctx.service::<SharedSessionController>();
    let selection = crate::model::effective_uia_selection(controller, &config);
    let provider_line = match selection.provider() {
        Some(provider) => match configured_provider(&config, provider) {
            Some((canonical_id, resolved_provider)) => format!(
                "UIA provider : {canonical_id}\nName         : {}\nCredentials  : {}",
                resolved_provider.name,
                configured_auth_status_label(resolved_provider),
            ),
            None => format!(
                "UIA provider : {provider}\nWARNING: provider '{provider}' is not present in the configured provider catalog. \
                 Use `/uia-provider list` to see configured providers."
            ),
        },
        None => "UIA provider : (nicht gesetzt)".to_owned(),
    };
    let text = format!(
        "{provider_line}\nUIA model    : {}",
        selection.model().unwrap_or("(nicht gesetzt)")
    );
    Ok(OpOutput::from(text))
}

/// Implements `/uia-provider list` — marks the effective UIA provider.
fn handle_uia_list(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let config = resolved_config(ctx)?;
    let controller = ctx.service::<SharedSessionController>();
    let selection = crate::model::effective_uia_selection(controller, &config);
    let active = selection.provider();

    let mut lines = vec!["Registered providers (UIA):".to_owned()];
    let mut providers: Vec<_> = config.providers.values().collect();
    providers.sort_by(|left, right| left.name.cmp(&right.name));
    for provider in providers {
        let status = if active == Some(provider.name.as_str()) {
            "[uia-active]"
        } else if configured_auth_is_present(provider) {
            "[auth-ok]"
        } else {
            "[auth-missing]"
        };
        lines.push(format!("  {}  id={}  {status}", provider.name, provider.name));
    }
    lines.push(format!(
        "\nEffective UIA provider: {}",
        active.unwrap_or("(none)")
    ));
    lines.push(format!(
        "Effective UIA model: {}",
        selection.model().unwrap_or("(none)")
    ));
    Ok(OpOutput::from(lines.join("\n")))
}

/// Implements `/uia-provider test` — tests the effective UIA provider.
fn handle_uia_test(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let config = resolved_config(ctx)?;
    let controller = ctx.service::<SharedSessionController>();
    let selection = crate::model::effective_uia_selection(controller, &config);

    let Some(provider_name) = selection.provider() else {
        return Ok(OpOutput::from(
            "No effective UIA provider configured — no test possible. \
             Use `/uia-provider switch <id>` to set one."
                .to_owned(),
        ));
    };
    let Some((canonical, provider)) = configured_provider(&config, provider_name) else {
        return Ok(OpOutput::from(format!(
            "Effective UIA provider '{provider_name}' is not present in the configured provider catalog."
        )));
    };

    let auth_info = match &provider.auth {
        Some(secret_ref) => {
            let ref_string = secret_ref.as_ref_string();
            let ref_type = ref_string
                .split_once(':')
                .map(|(prefix, _)| prefix)
                .unwrap_or("unknown");
            format!("Auth method: {ref_type}:  [ref present — value not shown]")
        }
        None if provider.has_plaintext_secret() => {
            "Auth method: api_key (plaintext) — WARNING: insecure; use an `auth` SecretRef instead."
                .to_owned()
        }
        None => "Auth method: (none) — provider has no `auth` field configured.".to_owned(),
    };
    let status = if provider.enabled { "enabled" } else { "disabled" };
    Ok(OpOutput::from(format!(
        "Effective UIA provider : {canonical}  [{status}]\nAPI type                : {}\n{}\nNote: live connection test (HTTP ping) not yet wired.",
        provider.api, auth_info
    )))
}

#[cfg(test)]
mod tests {
    use super::{ProviderArgs, configured_provider};
    use harw_operations::{
        FromRawArgs, NullSessionController, OpContext, OpError, SharedSessionController,
        context::ServiceMap,
    };
    use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
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
                rate_limit: None,
                max_concurrency: None,
                originator: None,
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
                    msg.contains("show") && msg.contains("list") && msg.contains("test"),
                    "Error must list the supported sub-commands: {msg}"
                );
            }
            other => {
                panic!("Expected OpError::InvalidArguments for unknown sub-command, got: {other:?}")
            }
        }
    }

    // ── Welle 2 (2d), Teil 1: `switch` retired as a `/provider` sub-command ───

    /// `/provider switch ...` must no longer be recognised as its own
    /// sub-command: it falls into the unknown-sub-command catchall, whose
    /// message must redirect the operator to `/model` (the sole atomic
    /// provider+model switch entry point since this node).
    #[tokio::test]
    async fn provider_switch_subcommand_no_longer_supported() {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None);

        let args = ProviderArgs {
            cmd: Some("switch anthropic".to_owned()),
        };
        let result = super::provider(&ctx, args).await;

        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(
                    msg.contains("switch"),
                    "message must echo the rejected 'switch' sub-command: {msg}"
                );
                assert!(
                    msg.contains("/model"),
                    "message must point the operator to /model: {msg}"
                );
            }
            other => panic!("Expected OpError::InvalidArguments, got: {other:?}"),
        }
    }

    /// Same as [`provider_switch_subcommand_no_longer_supported`] but for
    /// `/uia-provider`, whose message must point to `/uia-model` instead.
    #[tokio::test]
    async fn uia_provider_switch_subcommand_no_longer_supported() {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None);

        let args = ProviderArgs {
            cmd: Some("switch anthropic".to_owned()),
        };
        let result = super::uia_provider(&ctx, args).await;

        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(
                    msg.contains("switch"),
                    "message must echo the rejected 'switch' sub-command: {msg}"
                );
                assert!(
                    msg.contains("/uia-model"),
                    "message must point the operator to /uia-model: {msg}"
                );
            }
            other => panic!("Expected OpError::InvalidArguments, got: {other:?}"),
        }
    }
}
