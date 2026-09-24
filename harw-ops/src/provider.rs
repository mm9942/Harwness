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
//! This module also defines the sibling operation [`provider_concurrency`]
//! (`/provider-concurrency`, Welle 6b), which **does** carry a `model_tool`
//! surface (`approval = "always"`) — it adjusts an already-selected
//! provider's client-side concurrency cap, never which provider is active,
//! so it does not fall under the rule above. See its own doc for the
//! approval-policy rationale.
//!
//! # Sub-Commands
//! - `show` (default): reports the *runtime-active* provider from the controller snapshot;
//!   falls back to `harness.default_provider` from config if no switch has occurred.
//!   Also reports credential status (variant type, not value) for the active provider.
//! - `list`: enumerates every provider in the live resolved configuration and marks each as `[active]`,
//!   `[auth-ok]`, or `[auth-missing]`.
//! - `test`: tests the runtime-active (else config-default) provider: auth-ref type
//!   (no secret value) plus a live HTTP connection test through the
//!   [`ProviderConnectionCheck`] service (`GET /models`); without a registered
//!   service the live line says so and only the configuration is reported.
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
//! - [`ProviderConcurrencyArgs`] — argument struct for
//!   `/provider-concurrency` (Welle 6b).
//!
//! # Error Types
//! - [`harw_operations::OpError::Execution`]: controller not available in context.
//! - [`harw_operations::OpError::InvalidArguments`]: unknown subcommand, unknown
//!   provider ID, missing credentials, incompatible active model, or (for
//!   `/provider-concurrency`) a malformed `<n|unlimited>` value.
//! - [`harw_operations::OpError::NotAvailable`]: (`/provider-concurrency`
//!   only) no `ProviderLoadRegistry` service registered, or the resolved
//!   provider has no registered load-control handle.
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
use harw_operations::session_control::UiaSelection;
use harw_operations::{OpContext, OpError, OpOutput, SharedSessionController};
use harw_provider_http::discovery::DiscoveryError;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

/// Ergebnis eines erfolgreichen Live-Verbindungstests gegen einen Provider.
///
/// # Fields
/// - `model_count` (`usize`): Anzahl der vom `/models`-Endpunkt gemeldeten
///   Modelle. `0` ist zwar eine HTTP-Erfolgsantwort, deutet aber fast immer
///   auf ein Auth-/Endpunkt-Problem hin (siehe `harw models scan`).
/// - `credential_sent` (`bool`): ob die Anfrage mit einem aufgelösten
///   Credential gesendet wurde (`false` = unauthentifiziert, etwa lokales
///   Ollama oder eine nicht auflösbare `auth`-Referenz).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionCheckReport {
    /// Anzahl der gemeldeten Modelle.
    pub model_count: usize,
    /// Ob ein Credential mitgesendet wurde.
    pub credential_sent: bool,
}

/// Rückgabe-Future von [`ProviderConnectionCheck::check`].
pub type ConnectionCheckFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ConnectionCheckReport, DiscoveryError>> + Send + 'a>>;

/// Live-Verbindungstest (HTTP) gegen einen konfigurierten Provider.
///
/// # Description
/// `/provider test` und `/uia-provider test` suchen diesen Dienst als
/// [`SharedProviderConnectionCheck`] (`Arc<dyn ProviderConnectionCheck>`) im
/// [`OpContext`]. Fehlt er, melden beide Ops nur den statischen
/// Konfigurationsbefund und sagen ausdrücklich, dass kein Live-Test lief —
/// die Operation schlägt dann **nicht** fehl.
///
/// Die Produktions-Implementierung ist [`DiscoveryConnectionCheck`] (dieselbe
/// `/models`-Abfrage wie `harw models scan`); Tests setzen eigene Attrappen
/// ein, damit sie nie ins Netz gehen.
///
/// # Concurrency
/// `Send + Sync`, damit der Dienst in der `ServiceMap` liegen und über
/// `.await` hinweg geborgt werden kann.
pub trait ProviderConnectionCheck: Send + Sync {
    /// Prüft die Erreichbarkeit und Anmeldung von `provider`.
    ///
    /// # Arguments
    /// - `provider_name` (`&str`): kanonischer Provider-Name (Diagnose).
    /// - `provider` (`&harw_config::ProviderToml`): Konfigurationseintrag.
    /// - `config` (`&harw_config::ResolvedConfig`): liefert den Env-Layer für
    ///   `env:`-Credential-Referenzen.
    ///
    /// # Errors
    /// [`DiscoveryError`] klassifiziert Auth-, API-, Netzwerk-, Decode- und
    /// „nicht unterstützt"-Fehler; der Text enthält nie ein Geheimnis.
    fn check<'a>(
        &'a self,
        provider_name: &'a str,
        provider: &'a harw_config::ProviderToml,
        config: &'a harw_config::ResolvedConfig,
    ) -> ConnectionCheckFuture<'a>;
}

/// Die Form, in der [`ProviderConnectionCheck`] im [`OpContext`] registriert
/// wird (`ServiceMap` indiziert nach konkretem Typ).
pub type SharedProviderConnectionCheck = Arc<dyn ProviderConnectionCheck>;

/// Produktions-[`ProviderConnectionCheck`] über
/// [`harw_provider_http::discovery::list_models`].
///
/// # Description
/// Löst das Credential exakt wie `harw models scan` über
/// [`harw_provider_http::discovery::resolve_provider_api_key`] auf (mit
/// harw-Home für `file:`-Referenzen und optionalem sealed-secret-Resolver für
/// `secrets:`-Referenzen); ein Klartext-`api_key` dient als Rückfall. Danach
/// wird `GET {base_url}/models` (bzw. `/v1/models` für
/// `anthropic-messages`) mit 20-s-Zeitlimit abgefragt. Der Schlüssel wird nie
/// geloggt oder ausgegeben.
pub struct DiscoveryConnectionCheck {
    home: Option<PathBuf>,
    resolver: Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>,
}

impl DiscoveryConnectionCheck {
    /// Baut den Test mit harw-Home und optionalem `secrets:`-Resolver.
    ///
    /// # Arguments
    /// - `home` (`Option<PathBuf>`): harw-Home für `file:`-Credentials;
    ///   `None` lässt solche Referenzen unaufgelöst.
    /// - `resolver`: sealed-secret-Resolver der Runtime; `None` lässt
    ///   `secrets:`-Referenzen unaufgelöst.
    #[must_use]
    pub fn new(
        home: Option<PathBuf>,
        resolver: Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>,
    ) -> Self {
        Self { home, resolver }
    }
}

impl ProviderConnectionCheck for DiscoveryConnectionCheck {
    fn check<'a>(
        &'a self,
        provider_name: &'a str,
        provider: &'a harw_config::ProviderToml,
        config: &'a harw_config::ResolvedConfig,
    ) -> ConnectionCheckFuture<'a> {
        // Synchron vor dem ersten `.await` auflösen: der Resolver muss so
        // nicht über einen Suspend-Punkt hinweg geborgt werden.
        let resolver = self
            .resolver
            .as_deref()
            .map(|resolver| resolver as &dyn harw_provider_http::SecretResolver);
        let api_key = harw_provider_http::discovery::resolve_provider_api_key(
            provider_name,
            provider,
            config,
            self.home.as_deref(),
            resolver,
        )
        .or_else(|| provider.api_key.clone().filter(|key| !key.is_empty()));
        Box::pin(async move {
            let models = harw_provider_http::discovery::list_models(
                provider_name,
                provider,
                api_key.as_deref(),
            )
            .await?;
            Ok(ConnectionCheckReport {
                model_count: models.len(),
                credential_sent: api_key.is_some(),
            })
        })
    }
}

/// Argument struct for the `/provider` command.
///
/// # Fields
/// - `cmd` (`Option<String>`): optional sub-command. Valid values:
///   - `"show"` (default) — shows the runtime-active provider.
///   - `"list"` — enumerates all configured providers (no secret values).
///   - `"test"` — auth-ref type (no secret value) plus live connection test of the
///     runtime-active, else default provider.
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
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
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
pub(crate) fn resolved_config(
    ctx: &OpContext,
) -> Result<Arc<harw_config::ResolvedConfig>, OpError> {
    // Live-Stand zuerst: er enthält alle seit dem Start gespeicherten
    // Änderungen (auch solche aus demselben Kontext).
    if let Some(live) = ctx.service::<crate::live_config::SharedLiveConfig>() {
        return Ok(live.current());
    }
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
/// - **`test`**: auth-ref type plus live connection test (see [`ProviderConnectionCheck`])
///   for the runtime-active, else config-default provider.
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
/// - [`OpError::Execution`]: when the [`SharedSessionController`] is not registered in context.
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
/// // /provider test       → auth-ref type + live connection test of the active provider
/// // /model switch <id>   → atomically switches provider+model (see harw-ops::model)
/// ```
#[operation(
    name = "provider",
    summary = "Zeigt aktiven Provider; listet/testet konfigurierte Provider.",
    domain = "catalog_config",
    permission = "operator",
    aliases = ["p"],
    category = "model",
    command(
        path = "/provider",
        visibility = "tui_only",
        busy = "immediate",
        busy_subcommands = "test=deferred"
    ),
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
        "test" => handle_test(ctx).await,
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
                let mut text = format!(
                    "Active provider : {canonical_id}  (runtime, explicitly switched)\n\
                     Name            : {display_name}\n\
                     Credentials     : {auth}",
                    display_name = provider.name,
                );
                append_load_status(ctx, canonical_id, &mut text);
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
            Some(name) => match configured_provider(&config, name) {
                Some((canonical_id, _)) => {
                    let mut text = format!(
                        "Active provider : {canonical_id}  (config default — no runtime switch in this session)\n\
                     Use `/model switch <id>` to change provider and model together."
                    );
                    append_load_status(ctx, canonical_id, &mut text);
                    text
                }
                None => format!(
                    "Default provider '{name}' is not present in the configured provider catalog. \
                     Use `harw onboard` to repair the configuration."
                ),
            },
            None => "No default provider configured. Use `harw onboard` to set one up.".to_owned(),
        };
        Ok(OpOutput::from(text))
    }
}

/// Appends a concurrency/rate-limit status block to `text`, if a
/// [`harw_provider_http::ProviderLoadRegistry`] is registered in the
/// [`OpContext`] **and** it has a
/// [`harw_provider_http::ProviderLoadControl`] handle for `canonical_id`.
///
/// # Description
/// Silently a no-op when either is missing. The runtime
/// (`harw_runtime::services::RuntimeServices`) registers a
/// [`harw_provider_http::ProviderLoadRegistry`] on every surface, so a missing
/// registry only occurs in standalone/test contexts; a missing handle means
/// `canonical_id` was not built as a model provider in this run (the registry
/// only holds the root and UIA providers, OpenAI-compatible and
/// `anthropic-messages` alike). `/provider show` must keep working (with only
/// the base info) either way — this is purely additive.
///
/// # Arguments
/// - `ctx` (`&OpContext`): execution context, queried for the registry service.
/// - `canonical_id` (`&str`): the provider's canonical name/registry key.
/// - `text` (`&mut String`): appended to in place.
fn append_load_status(ctx: &OpContext, canonical_id: &str, text: &mut String) {
    let Some(registry) = ctx.service::<harw_provider_http::ProviderLoadRegistry>() else {
        return;
    };
    let Some(control) = registry.get(canonical_id) else {
        return;
    };
    text.push('\n');
    text.push_str(&format_load_status(&control.provider_status()));
}

/// Formats a [`harw_provider_http::ProviderLoadStatus`] as a compact,
/// human-readable multi-line block (shared by `/provider show` and
/// `/provider-concurrency`).
fn format_load_status(status: &harw_provider_http::ProviderLoadStatus) -> String {
    let concurrency = match status.max_concurrency {
        Some(n) => n.to_string(),
        None => "unlimited".to_owned(),
    };
    let wait = match status.rate_limit_wait {
        Some(duration) => format!("{:.1}s", duration.as_secs_f64()),
        None => "none".to_owned(),
    };
    format!(
        "Concurrency     : {concurrency}  (available: {available})\n\
         Rate limit wait : {wait}\n\
         Rate limited    : {count}x observed since start\
         {advice}",
        available = display_permits(status.available_permits),
        count = status.recent_rate_limited,
        advice = if status.recent_rate_limited > 0 {
            "\nHint: repeated HTTP 429 is a signal to lower concurrency, not raise it \
             (`/provider-concurrency <provider> <n>`)."
        } else {
            ""
        },
    )
}

/// Renders `available_permits` for display — `usize::MAX` means "no
/// [`harw_provider_http::DynamicConcurrencyLimiter`] installed" (see
/// [`harw_provider_http::ProviderLoadStatus::available_permits`] doc), shown
/// as `n/a` instead of a meaningless huge number.
fn display_permits(available_permits: usize) -> String {
    if available_permits == usize::MAX {
        "n/a".to_owned()
    } else {
        available_permits.to_string()
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
            let configured =
                crate::model::configured_model(&config, requested_model).ok_or_else(|| {
                    OpError::InvalidArguments(format!("unknown model: {requested_model}"))
                })?;
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
                    model.id == *active_model
                        || model.aliases.iter().any(|alias| alias == active_model)
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

    // ── Step 3b: make sure the running assembly can actually reach it ────────
    // A provider client that could not be built at startup (e.g. its
    // credential was only resolvable later) is rebuilt now; if that still
    // fails, the switch is rejected and the previous model stays active.
    crate::live_model::ensure_provider_ready(ctx, &canonical_target)?;

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
            let configured =
                crate::model::configured_model(&config, &requested_model).ok_or_else(|| {
                    OpError::InvalidArguments(format!("unknown model: {requested_model}"))
                })?;
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
            },
        },
    };

    // Wie beim generischen Wechsel: ein beim Start nicht baubarer Client
    // wird jetzt gebaut, sonst bleibt die bisherige UIA-Auswahl aktiv.
    crate::live_model::ensure_provider_ready(ctx, &canonical_target)?;

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

/// Beschreibt die konfigurierte Auth-Methode eines Providers — nur den
/// Referenztyp (Präfix vor `:`), nie den Wert.
fn auth_method_line(provider: &harw_config::ProviderToml) -> String {
    match &provider.auth {
        Some(secret_ref) => {
            let ref_string = secret_ref.as_ref_string();
            let ref_type = ref_string
                .split_once(':')
                .map(|(prefix, _)| prefix)
                .unwrap_or("unknown");
            format!("Auth method: {ref_type}:  [ref present — value not shown]")
        }
        None if provider.has_plaintext_secret() => {
            "Auth method: api_key (plaintext) — WARNING: insecure; \
             use an `auth` SecretRef instead."
                .to_owned()
        }
        None => "Auth method: (none) — provider has no `auth` field configured.".to_owned(),
    }
}

/// Führt den Live-Verbindungstest über den registrierten
/// [`ProviderConnectionCheck`] aus und liefert eine einzeilige Befundzeile.
///
/// # Description
/// Degradiert statt zu scheitern: ein deaktivierter Provider wird nicht
/// kontaktiert, und ohne registrierten Dienst meldet die Zeile klar, dass
/// nur die Konfiguration geprüft wurde. Fehler des Tests (Auth, Netzwerk,
/// HTTP-Status) sind ein *Befund* und damit Teil der Ausgabe, kein
/// [`OpError`].
async fn live_connection_line(
    ctx: &OpContext,
    provider_name: &str,
    provider: &harw_config::ProviderToml,
    config: &harw_config::ResolvedConfig,
) -> String {
    const LABEL: &str = "Live test        :";
    if !provider.enabled {
        return format!("{LABEL} skipped — provider is disabled in configuration");
    }
    let Some(check) = ctx.service::<SharedProviderConnectionCheck>() else {
        return format!(
            "{LABEL} unavailable — this runtime has no provider connection-check service \
             registered; only the configuration above was checked"
        );
    };
    match check.check(provider_name, provider, config).await {
        Ok(report) => {
            let auth = if report.credential_sent {
                "authenticated"
            } else {
                "unauthenticated"
            };
            if report.model_count == 0 {
                format!(
                    "{LABEL} reachable ({auth}), but 0 models reported — \
                     check credentials and endpoint"
                )
            } else {
                format!(
                    "{LABEL} OK — reachable ({auth}), {count} models reported",
                    count = report.model_count
                )
            }
        }
        Err(DiscoveryError::Unsupported { api }) => {
            format!("{LABEL} not supported for provider API '{api}'")
        }
        Err(error) => format!("{LABEL} FAILED — {error}"),
    }
}

/// Implements `/provider test` — static auth check plus live connection test.
///
/// # Description
/// Tests the runtime-active provider (controller snapshot) if one was
/// switched in this session, otherwise `harness.default_provider`. Reports
/// the configured auth method (variant type only — never the secret value),
/// whether the provider is enabled, and the result of a live HTTP connection
/// test via the [`ProviderConnectionCheck`] service (see
/// [`live_connection_line`] — degrades with a clear note when no service is
/// registered).
///
/// # Returns
/// [`OpOutput`] with auth-type label, provider status and live-test line.
///
/// # Errors
/// - [`OpError::Execution`]: config discovery failed.
///
/// # Concurrency
/// Stateless; thread-safe. Awaits at most one HTTP request (20 s timeout).
///
/// # Spec Reference
/// harwness Plan v2 — `/provider test`.
async fn handle_test(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let config = resolved_config(ctx)?;

    let runtime_active = ctx
        .service::<SharedSessionController>()
        .and_then(|controller| controller.snapshot().active_provider);
    let (label, target_name) = match (runtime_active, &config.harness.default_provider) {
        (Some(active), _) => ("Active provider  :", active),
        (None, Some(default_name)) => ("Default provider :", default_name.clone()),
        (None, None) => {
            return Ok(OpOutput::from(
                "No default provider configured — no test possible. \
                 Use `harw onboard` to set one up."
                    .to_owned(),
            ));
        }
    };

    let Some((canonical, provider_toml)) = configured_provider(&config, &target_name) else {
        return Ok(OpOutput::from(format!(
            "Provider '{target_name}' is not present in the configured provider catalog.\n\
             Run `harw onboard` again or check your config layers."
        )));
    };

    let status = if provider_toml.enabled {
        "enabled"
    } else {
        "disabled"
    };
    let live = live_connection_line(ctx, canonical, provider_toml, &config).await;

    Ok(OpOutput::from(format!(
        "{label} {canonical}  [{status}]\n\
         API type         : {api}\n\
         {auth_info}\n\
         {live}",
        api = provider_toml.api,
        auth_info = auth_method_line(provider_toml),
    )))
}

/// Argument struct for the `/provider-concurrency` command/model-tool.
///
/// # Fields
/// - `provider` (`Option<String>`): the canonical provider ID/name (first
///   token). Required — [`provider_concurrency`] rejects a missing value.
/// - `value` (`Option<String>`): either an unsigned integer (new hard
///   concurrency cap) or the literal `"unlimited"` (second token). Optional:
///   without it the operation only **shows** the provider's current load
///   state (read-only form). The model-tool surface also accepts a JSON
///   integer (`{"value": 1}`), see [`deserialize_concurrency_value`].
///
/// # Schema
/// Hand-written [`harw_operations::OpArgsSchema`] (not derived): the derive
/// can only say `value: string` without descriptions, and the model called
/// the tool with `{}` and `{"provider": "anthropic"}` before guessing the
/// field names (export 429). The schema now marks `provider` as required,
/// types `value` as `integer ≥ 1 | "unlimited"` and describes both.
///
/// # Spec Reference
/// Plan v2, Welle 6b — UIA-Sichtbarkeit auf Provider-Concurrency/
/// Rate-Limit-Zustand + Live-Anpassung.
#[derive(Debug, Default, serde::Deserialize)]
pub struct ProviderConcurrencyArgs {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default, deserialize_with = "deserialize_concurrency_value")]
    pub value: Option<String>,
}

/// Accepts `value` as JSON string (`"3"`, `"unlimited"`) **or** unsigned
/// integer (`3`) and normalises it to the token form parsed by
/// [`parse_concurrency_value`].
///
/// # Errors
/// A serde error for any other JSON type (negative/fractional numbers,
/// booleans, objects, …).
fn deserialize_concurrency_value<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text)),
        Some(serde_json::Value::Number(number)) => {
            number.as_u64().map(|n| Some(n.to_string())).ok_or_else(|| {
                serde::de::Error::custom("value must be a positive integer or \"unlimited\"")
            })
        }
        Some(_) => Err(serde::de::Error::custom(
            "value must be a positive integer or \"unlimited\"",
        )),
    }
}

impl harw_operations::OpArgsSchema for ProviderConcurrencyArgs {
    /// `provider` (required string) plus `value` (`integer ≥ 1` or the
    /// string `"unlimited"`, optional — omitted means "show only").
    fn json_schema() -> harw_tools::JsonSchema {
        use harw_operations::op_schema::{
            described_object_schema, enum_string_schema, integer_schema, string_schema,
        };
        let value = harw_tools::JsonSchema {
            description: Some(
                "Neue Grenze: positive Ganzzahl (z. B. 1) oder \"unlimited\". Weglassen = \
                 nur den Zustand (Concurrency, 429-Wartezeit) anzeigen."
                    .to_owned(),
            ),
            any_of: Some(vec![
                integer_schema("Mindestens 1."),
                enum_string_schema("Grenze aufheben.", &["unlimited"]),
            ]),
            ..harw_tools::JsonSchema::default()
        };
        described_object_schema(
            "Nur `provider` → Zustand anzeigen; `provider` + `value` → Grenze setzen.",
            vec![
                (
                    "provider",
                    string_schema("Kanonischer Provider-Name, z. B. \"anthropic\"."),
                ),
                ("value", value),
            ],
            &["provider"],
        )
    }
}

impl harw_operations::FromRawArgs for ProviderConcurrencyArgs {
    /// Assigns the first token to `provider`, the second to `value` —
    /// mirrors [`crate::model::ModelArgs::from_raw_args`], so
    /// `/provider-concurrency <id> <n|unlimited>` needs no join trick.
    fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        Ok(Self {
            provider: tokens.first().cloned(),
            value: tokens.get(1).cloned(),
        })
    }
}

/// Parses the `<n|unlimited>` token into a concurrency target.
///
/// # Returns
/// `Ok(None)` for `"unlimited"` (case-insensitive), `Ok(Some(n))` for a
/// positive integer.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: empty/missing value, `0`, a negative
///   number, or anything that does not parse as `usize`.
fn parse_concurrency_value(raw: &str) -> Result<Option<usize>, OpError> {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("unlimited") {
        return Ok(None);
    }
    match trimmed.parse::<usize>() {
        Ok(0) => Err(OpError::InvalidArguments(
            "concurrency must be a positive integer (use `unlimited` to remove the cap, not 0)"
                .to_owned(),
        )),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err(OpError::InvalidArguments(format!(
            "'{trimmed}' is not a valid concurrency value — expected a positive integer or \
             `unlimited`"
        ))),
    }
}

/// Implements `/provider-concurrency` — live-adjusts a provider's hard,
/// client-side concurrency cap and reports its resulting load status.
///
/// # Description
/// Resolves `args.provider` against the configured provider catalog (same
/// [`configured_provider`] lookup as `/provider show`/`switch`), then looks
/// the canonical ID up in the [`harw_provider_http::ProviderLoadRegistry`]
/// registered in the [`OpContext`] (see [`ServiceMap`][sm]). Calling
/// [`harw_provider_http::ProviderLoadControl::set_max_concurrency`] takes
/// effect immediately: raising the cap frees permits right away, lowering it
/// only stops new permits from being handed out once in-flight requests
/// return theirs (see `harw_provider_http::DynamicConcurrencyLimiter` doc —
/// no in-flight request is ever aborted).
///
/// # Dual surface & approval
/// Exposed both as an operator command (`/provider-concurrency`) and as a
/// `model_tool`, so the UIA can lower its own provider's concurrency in
/// response to repeated HTTP 429 without operator round-trip. The
/// `#[operation(...)]` macro only supports a single static `approval` for
/// the whole `model_tool` surface (no per-argument distinction), so this
/// operation is declared `approval = "always"`: **raising** concurrency can
/// increase cost and the chance of hitting the provider's own rate limit
/// harder, which must not happen unattended; the macro cannot exempt
/// lowering from that gate, so lowering pays the same (harmless) approval
/// cost as a deliberate, conservative default. See module doc for the
/// broader `/provider` "no unattended provider changes" rule, which this
/// mirrors.
///
/// [sm]: harw_operations::context::ServiceMap
///
/// # Arguments
/// - `ctx` (`&OpContext`): execution context, used for `resolved_config` and
///   the `ProviderLoadRegistry` service lookup.
/// - `args` (`ProviderConcurrencyArgs`): provider ID + optional
///   `<n|unlimited>`; without a value the call is read-only.
///
/// # Returns
/// [`OpOutput`] confirming the new target with the resulting load status
/// (see [`format_load_status`]), or — without `value` — just the current
/// load status.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: missing provider, unknown provider, or a
///   malformed value (see [`parse_concurrency_value`]).
/// - [`OpError::NotAvailable`]: no [`harw_provider_http::ProviderLoadRegistry`]
///   is registered in the [`OpContext`] (the runtime registers one on every
///   surface, so this only happens in standalone/test contexts), or the
///   resolved provider has no registered handle (it was not built as the
///   root or UIA model provider in this run — see
///   `harw_provider_http::build_named_provider` doc).
///
/// # Spec Reference
/// Plan v2, Welle 6b — "Live-Anpassung".
#[operation(
    name = "provider-concurrency",
    summary = "Zeigt/verstellt die harte Nebenläufigkeitsgrenze eines Providers live. Bei wiederholten HTTP-429-Antworten die Concurrency senken, nicht erhöhen.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(
        path = "/provider-concurrency",
        visibility = "tui_only",
        busy = "immediate"
    ),
    model_tool(approval = "always")
)]
async fn provider_concurrency(
    ctx: &OpContext,
    args: ProviderConcurrencyArgs,
) -> Result<OpOutput, OpError> {
    let provider_arg = args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            OpError::InvalidArguments(
                "usage: /provider-concurrency <provider> [<n|unlimited>] — `provider` is \
                 required (e.g. {\"provider\": \"anthropic\", \"value\": 1}); without \
                 `value` the current state is shown"
                    .to_owned(),
            )
        })?;
    // Ohne `value`: nur anzeigen (Read-only-Form), nichts verstellen.
    let target = match args.value.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => Some(parse_concurrency_value(value)?),
        _ => None,
    };

    let config = resolved_config(ctx)?;
    let (canonical_id, _provider) = configured_provider(&config, provider_arg)
        .ok_or_else(|| OpError::InvalidArguments(format!("unknown provider: {provider_arg}")))?;

    let registry = ctx
        .service::<harw_provider_http::ProviderLoadRegistry>()
        .ok_or_else(|| {
            OpError::NotAvailable(
                "provider load-control registry is not available in this execution context \
                 (no ProviderLoadRegistry registered in the ServiceMap)"
                    .to_owned(),
            )
        })?;
    let control = registry.get(canonical_id).ok_or_else(|| {
        OpError::NotAvailable(format!(
            "provider '{canonical_id}' has no live load-control handle \
             (only providers built as the root or UIA model provider in this run are \
             registered)"
        ))
    })?;

    let Some(target) = target else {
        let text = format!(
            "provider '{canonical_id}': current load state\n{status}",
            status = format_load_status(&control.provider_status()),
        );
        return Ok(OpOutput::from(text));
    };

    let applied = control.set_max_concurrency(target);
    if !applied {
        return Err(OpError::Execution(format!(
            "provider '{canonical_id}' has no concurrency limiter installed; the request had no effect"
        )));
    }

    let target_label = match target {
        Some(n) => n.to_string(),
        None => "unlimited".to_owned(),
    };
    let text = format!(
        "provider '{canonical_id}': concurrency target set to {target_label}\n{status}",
        status = format_load_status(&control.provider_status()),
    );
    Ok(OpOutput::from(text))
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
/// - **`test`**: reports the effective UIA provider's configured auth variant
///   and runs the live connection test (see [`ProviderConnectionCheck`]).
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
    summary = "Zeigt/listet/testet den für die UIA gepinnten Provider (uia_provider), unabhängig vom Default.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(
        path = "/uia-provider",
        visibility = "tui_only",
        busy = "staged",
        busy_subcommands = "-=immediate, show=immediate, list=immediate, test=deferred"
    )
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
        "test" => handle_uia_test(ctx).await,
        other => Err(OpError::InvalidArguments(format!(
            "Unknown /uia-provider sub-command: '{other}'. \
             Supported: show, list, test. Use `/uia-model` to change the active provider and model together."
        ))),
    }
}

/// Implements `/uia-provider show` — reports the UIA's pinned provider.
///
/// # Description
/// Reports the effective UIA selection via
/// [`crate::model::effective_uia_selection`]: the live UIA selection from the
/// [`SharedSessionController`] (set by `/uia-model switch`) if present,
/// otherwise `harness.uia_provider`/`harness.uia_model` from config. The
/// generic `/model switch` never changes this selection.
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
        lines.push(format!(
            "  {}  id={}  {status}",
            provider.name, provider.name
        ));
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

/// Implements `/uia-provider test` — tests the effective UIA provider
/// (static auth check plus live connection test, see [`live_connection_line`]).
async fn handle_uia_test(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let config = resolved_config(ctx)?;
    let controller = ctx.service::<SharedSessionController>();
    let selection = crate::model::effective_uia_selection(controller, &config);

    let Some(provider_name) = selection.provider() else {
        return Ok(OpOutput::from(
            "No effective UIA provider configured — no test possible. \
             Use `/uia-model switch <id>` to set one."
                .to_owned(),
        ));
    };
    let Some((canonical, provider)) = configured_provider(&config, provider_name) else {
        return Ok(OpOutput::from(format!(
            "Effective UIA provider '{provider_name}' is not present in the configured provider catalog."
        )));
    };

    let status = if provider.enabled {
        "enabled"
    } else {
        "disabled"
    };
    let live = live_connection_line(ctx, canonical, provider, &config).await;
    Ok(OpOutput::from(format!(
        "Effective UIA provider : {canonical}  [{status}]\nAPI type               : {}\n{}\n{live}",
        provider.api,
        auth_method_line(provider)
    )))
}

#[cfg(test)]
mod tests {
    use super::{ProviderArgs, configured_provider};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::{
        FromRawArgs, NullSessionController, OpContext, OpError, SharedSessionController,
        context::ServiceMap,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;

    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;

    // ── Context builder ───────────────────────────────────────────────────────

    /// Builds a minimal `OpContext` with optional controller and config services.
    fn make_test_ctx(
        ctrl: Option<SharedSessionController>,
        config: Option<Arc<harw_config::ResolvedConfig>>,
    ) -> TestResult<(OpContext, std::path::PathBuf)> {
        make_test_ctx_with_check(ctrl, config, None)
    }

    /// Like [`make_test_ctx`], additionally registering an optional
    /// [`super::SharedProviderConnectionCheck`].
    fn make_test_ctx_with_check(
        ctrl: Option<SharedSessionController>,
        config: Option<Arc<harw_config::ResolvedConfig>>,
        check: Option<super::SharedProviderConnectionCheck>,
    ) -> TestResult<(OpContext, std::path::PathBuf)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("harw-provider-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(tmp.join("ws")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry::build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve binding"))?;
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
        if let Some(check) = check {
            services.insert(check);
        }
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        Ok((ctx, tmp))
    }

    // ── Existing arg-parsing tests (preserved) ────────────────────────────────

    #[test]
    fn test_provider_args_from_raw_args_sets_cmd() -> TestResult {
        let args = ProviderArgs::from_raw_args(&toks(&["list"]));
        match args {
            Ok(a) => assert_eq!(a.cmd.as_deref(), Some("list")),
            Err(e) => return Err(TestError::Unexpected(format!("Unexpected error: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_provider_args_from_raw_args_empty_tokens_sets_cmd_none() -> TestResult {
        let args = ProviderArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.cmd.is_none()),
            Err(e) => return Err(TestError::Unexpected(format!("Unexpected error: {e}"))),
        }
        Ok(())
    }

    /// `switch` without a provider ID must return `OpError::InvalidArguments`.
    #[tokio::test]
    async fn test_provider_switch_requires_target_id() -> TestResult {
        let args = ProviderArgs {
            cmd: Some("switch ".to_string()),
        };
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None)?;

        let result = super::provider(&ctx, args).await;

        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(
                    msg.contains("switch"),
                    "Error message must contain 'switch': {msg}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Expected OpError::InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// `FromRawArgs` joins multiple tokens so that `["switch", "anthropic"]`
    /// becomes `"switch anthropic"`, which `strip_prefix("switch ")` recognises.
    #[test]
    fn test_provider_args_recognizes_switch_prefix() -> TestResult {
        let args = ProviderArgs::from_raw_args(&toks(&["switch", "anthropic"]))
            .map_err(ctx("from_raw_args must not fail"))?;
        let cmd = args.cmd.ok_or(TestError::Missing("cmd must be set"))?;
        assert!(
            cmd.strip_prefix("switch ").is_some(),
            "cmd must start with 'switch ', got: {cmd:?}"
        );
        assert_eq!(
            cmd.strip_prefix("switch ")
                .ok_or(TestError::Missing("switch prefix"))?
                .trim(),
            "anthropic",
            "Provider ID after 'switch ' must be 'anthropic'"
        );
        Ok(())
    }

    #[test]
    fn configured_provider_canonicalizes_a_configured_alias() -> TestResult {
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "operator-alias".to_owned(),
            harw_config::ProviderToml {
                stream: None,
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
                default_reasoning_effort: None,
                gateway_identity_headers: false,
                request_timeout_secs: None,
                stream_idle_timeout_secs: None,
                retry_timeouts: None,
                max_tokens_field: None,
                send_reasoning_effort: None,
                strict_tools: None,
                parallel_tool_calls: None,
                allow_insecure_lan: false,
            },
        );

        let (canonical, _) = configured_provider(&config, "operator-alias")
            .ok_or(TestError::Missing("configured alias must resolve"))?;
        assert_eq!(canonical, "canonical-provider");
        Ok(())
    }

    #[test]
    fn resolved_config_prefers_context_service() -> TestResult {
        let config = Arc::new(harw_config::ResolvedConfig::default());
        let (ctx, _tmp) = make_test_ctx(None, Some(Arc::clone(&config)))?;

        let resolved = super::resolved_config(&ctx)
            .map_err(crate::test_support::ctx("context config must resolve"))?;

        assert!(
            Arc::ptr_eq(&resolved, &config),
            "the context-scoped resolved config must be authoritative"
        );
        Ok(())
    }

    // ── Task D: provider_unknown_subcommand_rejected ──────────────────────────

    /// An unrecognised sub-command must return `OpError::InvalidArguments`.
    #[tokio::test]
    async fn provider_unknown_subcommand_rejected() -> TestResult {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None)?;

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
                return Err(TestError::Unexpected(format!(
                    "Expected OpError::InvalidArguments for unknown sub-command, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── Welle 2 (2d), Teil 1: `switch` retired as a `/provider` sub-command ───

    /// `/provider switch ...` must no longer be recognised as its own
    /// sub-command: it falls into the unknown-sub-command catchall, whose
    /// message must redirect the operator to `/model` (the sole atomic
    /// provider+model switch entry point since this node).
    #[tokio::test]
    async fn provider_switch_subcommand_no_longer_supported() -> TestResult {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None)?;

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "Expected OpError::InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Same as [`provider_switch_subcommand_no_longer_supported`] but for
    /// `/uia-provider`, whose message must point to `/uia-model` instead.
    #[tokio::test]
    async fn uia_provider_switch_subcommand_no_longer_supported() -> TestResult {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None)?;

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "Expected OpError::InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── `/provider-concurrency` (Welle 6b) ─────────────────────────────────────

    fn openai_provider_config(name: &str) -> harw_config::ResolvedConfig {
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            name.to_owned(),
            harw_config::ProviderToml {
                stream: None,
                name: name.to_owned(),
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
                default_reasoning_effort: None,
                gateway_identity_headers: false,
                request_timeout_secs: None,
                stream_idle_timeout_secs: None,
                retry_timeouts: None,
                max_tokens_field: None,
                send_reasoning_effort: None,
                strict_tools: None,
                parallel_tool_calls: None,
                allow_insecure_lan: false,
            },
        );
        config
    }

    #[test]
    fn parse_concurrency_value_accepts_unlimited_case_insensitively() {
        assert_eq!(super::parse_concurrency_value("unlimited"), Ok(None));
        assert_eq!(super::parse_concurrency_value("UNLIMITED"), Ok(None));
        assert_eq!(super::parse_concurrency_value("  Unlimited  "), Ok(None));
    }

    #[test]
    fn parse_concurrency_value_accepts_positive_integers() {
        assert_eq!(super::parse_concurrency_value("1"), Ok(Some(1)));
        assert_eq!(super::parse_concurrency_value("42"), Ok(Some(42)));
    }

    #[test]
    fn parse_concurrency_value_rejects_zero() {
        assert!(matches!(
            super::parse_concurrency_value("0"),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn parse_concurrency_value_rejects_negative_and_non_numeric() {
        assert!(matches!(
            super::parse_concurrency_value("-1"),
            Err(OpError::InvalidArguments(_))
        ));
        assert!(matches!(
            super::parse_concurrency_value("not-a-number"),
            Err(OpError::InvalidArguments(_))
        ));
        assert!(matches!(
            super::parse_concurrency_value(""),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn provider_concurrency_args_from_raw_args_assigns_provider_and_value() -> TestResult {
        let args = super::ProviderConcurrencyArgs::from_raw_args(&toks(&["openai", "3"]))
            .map_err(ctx("from_raw_args must not fail"))?;
        assert_eq!(args.provider.as_deref(), Some("openai"));
        assert_eq!(args.value.as_deref(), Some("3"));
        Ok(())
    }

    /// Export 429: das Modell riet die Feldnamen, weil das Schema weder
    /// Pflichtfelder noch Beschreibungen nannte.
    #[test]
    fn provider_concurrency_schema_requires_provider_and_types_value() -> TestResult {
        use harw_operations::OpArgsSchema as _;
        let schema = super::ProviderConcurrencyArgs::json_schema();
        assert_eq!(schema.required, Some(vec!["provider".to_owned()]));
        let properties = schema
            .properties
            .as_ref()
            .ok_or(TestError::Missing("properties"))?;
        let provider = properties
            .get("provider")
            .ok_or(TestError::Missing("provider property"))?;
        assert!(provider.description.is_some());
        let value = properties
            .get("value")
            .ok_or(TestError::Missing("value property"))?;
        assert!(
            value
                .description
                .as_deref()
                .is_some_and(|d| d.contains("unlimited"))
        );
        let variants = value.any_of.as_ref().ok_or(TestError::Missing("anyOf"))?;
        assert!(
            variants
                .iter()
                .any(|v| v.schema_type == Some(harw_tools::JsonSchemaType::Integer))
        );
        assert!(
            variants.iter().any(|v| {
                v.enum_values.as_deref() == Some(&[serde_json::json!("unlimited")][..])
            })
        );
        Ok(())
    }

    /// `value` darf als JSON-Zahl oder als String kommen.
    #[test]
    fn provider_concurrency_args_accept_integer_and_string_values() -> TestResult {
        let numeric: super::ProviderConcurrencyArgs =
            serde_json::from_value(serde_json::json!({"provider": "anthropic", "value": 1}))
                .map_err(ctx("integer value"))?;
        assert_eq!(numeric.value.as_deref(), Some("1"));
        let unlimited: super::ProviderConcurrencyArgs = serde_json::from_value(
            serde_json::json!({"provider": "anthropic", "value": "unlimited"}),
        )
        .map_err(ctx("string value"))?;
        assert_eq!(unlimited.value.as_deref(), Some("unlimited"));
        let read_only: super::ProviderConcurrencyArgs =
            serde_json::from_value(serde_json::json!({"provider": "anthropic"}))
                .map_err(ctx("read-only form"))?;
        assert!(read_only.value.is_none());
        assert!(
            serde_json::from_value::<super::ProviderConcurrencyArgs>(
                serde_json::json!({"provider": "anthropic", "value": -1})
            )
            .is_err()
        );
        Ok(())
    }

    /// Nur `provider` ist die Read-only-Form: kein „usage"-Fehler mehr, sondern
    /// (hier mangels Registry) `NotAvailable` wie beim Setzen.
    #[tokio::test]
    async fn provider_concurrency_without_value_is_read_only_not_a_usage_error() -> TestResult {
        let config = Arc::new(openai_provider_config("openai"));
        let (ctx, _tmp) = make_test_ctx(None, Some(config))?;
        let args = super::ProviderConcurrencyArgs {
            provider: Some("openai".to_owned()),
            value: None,
        };
        let result = super::provider_concurrency(&ctx, args).await;
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn provider_concurrency_rejects_missing_provider_argument() -> TestResult {
        let config = Arc::new(openai_provider_config("openai"));
        let (ctx, _tmp) = make_test_ctx(None, Some(config))?;

        let args = super::ProviderConcurrencyArgs {
            provider: None,
            value: Some("2".to_owned()),
        };
        let result = super::provider_concurrency(&ctx, args).await;
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn provider_concurrency_rejects_unknown_provider() -> TestResult {
        let config = Arc::new(openai_provider_config("openai"));
        let (ctx, _tmp) = make_test_ctx(None, Some(config))?;

        let args = super::ProviderConcurrencyArgs {
            provider: Some("does-not-exist".to_owned()),
            value: Some("2".to_owned()),
        };
        let result = super::provider_concurrency(&ctx, args).await;
        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(msg.contains("unknown provider"), "message was: {msg}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Expected OpError::InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn provider_concurrency_without_registered_load_registry_is_not_available() -> TestResult
    {
        // No `ProviderLoadRegistry` service inserted — simulates a runtime
        // execution context without one in its `ServiceMap` (standalone/test)
        // (see `harw_provider_http::build_provider_with_load_registry` doc).
        let config = Arc::new(openai_provider_config("openai"));
        let (ctx, _tmp) = make_test_ctx(None, Some(config))?;

        let args = super::ProviderConcurrencyArgs {
            provider: Some("openai".to_owned()),
            value: Some("2".to_owned()),
        };
        let result = super::provider_concurrency(&ctx, args).await;
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
        Ok(())
    }

    // ── Live connection test (`/provider test`, `/uia-provider test`) ─────────

    /// Scripted outcome of [`FakeCheck`] (`DiscoveryError` is not `Clone`).
    enum FakeOutcome {
        Reachable {
            model_count: usize,
            credential_sent: bool,
        },
        AuthRejected,
        Unsupported,
    }

    /// Test double for [`super::ProviderConnectionCheck`]; never touches the network.
    struct FakeCheck {
        outcome: FakeOutcome,
        calls: std::sync::atomic::AtomicUsize,
        last_provider: std::sync::Mutex<Option<String>>,
    }

    impl FakeCheck {
        fn new(outcome: FakeOutcome) -> Arc<Self> {
            Arc::new(Self {
                outcome,
                calls: std::sync::atomic::AtomicUsize::new(0),
                last_provider: std::sync::Mutex::new(None),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn last_provider(&self) -> Option<String> {
            self.last_provider
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
        }
    }

    impl super::ProviderConnectionCheck for FakeCheck {
        fn check<'a>(
            &'a self,
            provider_name: &'a str,
            _provider: &'a harw_config::ProviderToml,
            _config: &'a harw_config::ResolvedConfig,
        ) -> super::ConnectionCheckFuture<'a> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Ok(mut guard) = self.last_provider.lock() {
                *guard = Some(provider_name.to_owned());
            }
            let result = match self.outcome {
                FakeOutcome::Reachable {
                    model_count,
                    credential_sent,
                } => Ok(super::ConnectionCheckReport {
                    model_count,
                    credential_sent,
                }),
                FakeOutcome::AuthRejected => Err(super::DiscoveryError::Auth {
                    status: 401,
                    detail: "rejected".to_owned(),
                }),
                FakeOutcome::Unsupported => Err(super::DiscoveryError::Unsupported {
                    api: "exotic-api".to_owned(),
                }),
            };
            Box::pin(async move { result })
        }
    }

    /// Two enabled providers, `openai` as default and UIA provider.
    fn test_config_with_defaults() -> harw_config::ResolvedConfig {
        let mut config = openai_provider_config("openai");
        let second = openai_provider_config("other");
        config.providers.extend(second.providers);
        config.harness.default_provider = Some("openai".to_owned());
        config.harness.uia_provider = Some("openai".to_owned());
        config
    }

    async fn run_provider_test(
        ctrl: Option<SharedSessionController>,
        config: harw_config::ResolvedConfig,
        check: Option<super::SharedProviderConnectionCheck>,
    ) -> TestResult<String> {
        let (ctx, _tmp) = make_test_ctx_with_check(ctrl, Some(Arc::new(config)), check)?;
        let output = super::provider(
            &ctx,
            ProviderArgs {
                cmd: Some("test".to_owned()),
            },
        )
        .await
        .map_err(ctx_err("/provider test must succeed"))?;
        Ok(output.text)
    }

    fn ctx_err<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
        crate::test_support::ctx(context)
    }

    #[tokio::test]
    async fn provider_test_without_check_service_degrades_with_clear_note() -> TestResult {
        let text = run_provider_test(None, test_config_with_defaults(), None).await?;
        assert!(text.contains("Default provider : openai"), "text: {text}");
        assert!(text.contains("Live test"), "text: {text}");
        assert!(text.contains("unavailable"), "text: {text}");
        assert!(
            !text.contains("not yet"),
            "stale wording must be gone: {text}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn provider_test_reports_successful_live_check() -> TestResult {
        let fake = FakeCheck::new(FakeOutcome::Reachable {
            model_count: 3,
            credential_sent: true,
        });
        let check: super::SharedProviderConnectionCheck = fake.clone();
        let text = run_provider_test(None, test_config_with_defaults(), Some(check)).await?;
        assert!(text.contains("OK"), "text: {text}");
        assert!(text.contains("3 models"), "text: {text}");
        assert!(text.contains("authenticated"), "text: {text}");
        assert_eq!(fake.calls(), 1);
        assert_eq!(fake.last_provider().as_deref(), Some("openai"));
        Ok(())
    }

    #[tokio::test]
    async fn provider_test_flags_zero_models_as_suspicious() -> TestResult {
        let fake = FakeCheck::new(FakeOutcome::Reachable {
            model_count: 0,
            credential_sent: false,
        });
        let check: super::SharedProviderConnectionCheck = fake;
        let text = run_provider_test(None, test_config_with_defaults(), Some(check)).await?;
        assert!(text.contains("0 models"), "text: {text}");
        assert!(text.contains("unauthenticated"), "text: {text}");
        Ok(())
    }

    #[tokio::test]
    async fn provider_test_reports_failed_live_check_as_finding_not_error() -> TestResult {
        let fake = FakeCheck::new(FakeOutcome::AuthRejected);
        let check: super::SharedProviderConnectionCheck = fake;
        let text = run_provider_test(None, test_config_with_defaults(), Some(check)).await?;
        assert!(text.contains("FAILED"), "text: {text}");
        assert!(text.contains("401"), "text: {text}");
        Ok(())
    }

    #[tokio::test]
    async fn provider_test_reports_unsupported_api() -> TestResult {
        let fake = FakeCheck::new(FakeOutcome::Unsupported);
        let check: super::SharedProviderConnectionCheck = fake;
        let text = run_provider_test(None, test_config_with_defaults(), Some(check)).await?;
        assert!(text.contains("not supported"), "text: {text}");
        assert!(text.contains("exotic-api"), "text: {text}");
        Ok(())
    }

    #[tokio::test]
    async fn provider_test_skips_live_check_for_disabled_provider() -> TestResult {
        let mut config = test_config_with_defaults();
        if let Some(provider) = config.providers.get_mut("openai") {
            provider.enabled = false;
        }
        let fake = FakeCheck::new(FakeOutcome::Reachable {
            model_count: 1,
            credential_sent: true,
        });
        let check: super::SharedProviderConnectionCheck = fake.clone();
        let text = run_provider_test(None, config, Some(check)).await?;
        assert!(text.contains("[disabled]"), "text: {text}");
        assert!(text.contains("skipped"), "text: {text}");
        assert_eq!(fake.calls(), 0, "a disabled provider must not be contacted");
        Ok(())
    }

    #[tokio::test]
    async fn provider_test_prefers_runtime_active_provider_over_default() -> TestResult {
        use harw_operations::session_control::SessionController;
        let null = NullSessionController::new();
        null.set_active_provider("other".to_owned())
            .map_err(ctx_err("set_active_provider"))?;
        let ctrl: SharedSessionController = Arc::new(null);
        let fake = FakeCheck::new(FakeOutcome::Reachable {
            model_count: 2,
            credential_sent: true,
        });
        let check: super::SharedProviderConnectionCheck = fake.clone();
        let text = run_provider_test(Some(ctrl), test_config_with_defaults(), Some(check)).await?;
        assert!(text.contains("Active provider  : other"), "text: {text}");
        assert_eq!(fake.last_provider().as_deref(), Some("other"));
        Ok(())
    }

    #[tokio::test]
    async fn uia_provider_test_runs_live_check_for_effective_uia_provider() -> TestResult {
        let fake = FakeCheck::new(FakeOutcome::Reachable {
            model_count: 5,
            credential_sent: true,
        });
        let check: super::SharedProviderConnectionCheck = fake.clone();
        let (ctx, _tmp) = make_test_ctx_with_check(
            None,
            Some(Arc::new(test_config_with_defaults())),
            Some(check),
        )?;
        let output = super::uia_provider(
            &ctx,
            ProviderArgs {
                cmd: Some("test".to_owned()),
            },
        )
        .await
        .map_err(ctx_err("/uia-provider test must succeed"))?;
        assert!(
            output.text.contains("Effective UIA provider : openai"),
            "text: {}",
            output.text
        );
        assert!(output.text.contains("5 models"), "text: {}", output.text);
        assert!(!output.text.contains("not yet"), "text: {}", output.text);
        assert_eq!(fake.last_provider().as_deref(), Some("openai"));
        Ok(())
    }

    #[tokio::test]
    async fn uia_provider_test_without_check_service_degrades() -> TestResult {
        let (ctx, _tmp) =
            make_test_ctx_with_check(None, Some(Arc::new(test_config_with_defaults())), None)?;
        let output = super::uia_provider(
            &ctx,
            ProviderArgs {
                cmd: Some("test".to_owned()),
            },
        )
        .await
        .map_err(ctx_err("/uia-provider test must succeed"))?;
        assert!(output.text.contains("unavailable"), "text: {}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn provider_show_default_text_points_to_model_switch() -> TestResult {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), Some(Arc::new(test_config_with_defaults())))?;
        let output = super::provider(&ctx, ProviderArgs { cmd: None })
            .await
            .map_err(ctx_err("/provider show must succeed"))?;
        assert!(
            output.text.contains("config default"),
            "text: {}",
            output.text
        );
        assert!(
            output.text.contains("/model switch"),
            "text: {}",
            output.text
        );
        assert!(
            !output.text.contains("/provider switch"),
            "text: {}",
            output.text
        );
        assert!(!output.text.contains("not yet"), "text: {}", output.text);
        Ok(())
    }

    /// End-to-end check of [`super::DiscoveryConnectionCheck`] against a
    /// local one-shot HTTP server (no external network): it must call
    /// `GET {base_url}/models` with the plaintext key as bearer and count
    /// the reported models.
    #[tokio::test]
    async fn discovery_connection_check_queries_models_endpoint() -> TestResult {
        use std::io::{Read, Write};
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(ctx_err("bind listener"))?;
        let addr = listener.local_addr().map_err(ctx_err("local_addr"))?;
        let server = std::thread::spawn(move || -> TestResult<String> {
            let (mut stream, _) = listener.accept().map_err(ctx_err("accept"))?;
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .map_err(ctx_err("set_read_timeout"))?;
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buffer).map_err(ctx_err("read"))?;
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(buffer.get(..n).ok_or(TestError::Missing("buffer"))?);
            }
            let body = r#"{"data":[{"id":"m1"},{"id":"m2"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .map_err(ctx_err("write response"))?;
            Ok(String::from_utf8_lossy(&bytes).to_lowercase())
        });

        let mut config = openai_provider_config("local");
        if let Some(provider) = config.providers.get_mut("local") {
            provider.base_url = format!("http://{addr}/v1");
            provider.api_key = Some("test-key".to_owned());
        }
        let provider = config
            .providers
            .get("local")
            .ok_or(TestError::Missing("local provider"))?;

        let check = super::DiscoveryConnectionCheck::new(None, None);
        let report = super::ProviderConnectionCheck::check(&check, "local", provider, &config)
            .await
            .map_err(ctx_err("live check must succeed"))?;
        assert_eq!(
            report,
            super::ConnectionCheckReport {
                model_count: 2,
                credential_sent: true,
            }
        );

        let request = server
            .join()
            .map_err(|_| TestError::Unexpected("server thread panicked".to_owned()))??;
        assert!(
            request.starts_with("get /v1/models http/1.1"),
            "request: {request}"
        );
        assert!(
            request.contains("authorization: bearer test-key"),
            "request: {request}"
        );
        Ok(())
    }
}
