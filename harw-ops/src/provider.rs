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
//! - `catalog`, `add`, `add-custom`, `remove`, `enable`, `disable` (Welle 3 Scheibe 3):
//!   Verwaltung bekannter und eigener Provider; siehe Abschnitt
//!   `Verwaltung` weiter unten und Vertrag K-W3b.
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
//! - [`ProviderArgs`] — argument struct for the `/provider` command. Remote
//!   execution (phone) runs through [`REMOTE`] (lookup by path in
//!   `command_meta::REMOTE_POLICIES`).
//! - [`ProviderConcurrencyArgs`] — argument struct for
//!   `/provider-concurrency` (Welle 6b).
//!
//! # Error Types
//! - [`harw_operations::OpError::Execution`]: controller not available in context.
//! - [`harw_operations::OpError::InvalidArguments`]: unknown subcommand, unknown
//!   provider ID, missing credentials, incompatible active model, or (for
//!   `/provider-concurrency`) a malformed `<n|unlimited>` value.
//! - [`harw_operations::OpError::NotAvailable`]: neither a config service nor
//!   a bound root space (`harw_home::ResolvedHomeContext`) in the context —
//!   there is no fallback to `HARW_HOME`; or (`/provider-concurrency` only) no
//!   `ProviderLoadRegistry` service registered, or the resolved provider has
//!   no registered load-control handle.
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
use harw_operations::operation::{RemoteEffect, RemoteInvocation, RemotePolicy, RemoteVerb};
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
    ///   `None` lässt solche Referenzen unaufgelöst. Die Laufzeit übergibt
    ///   den an die Sitzung gebundenen Root-Space, nie `HARW_HOME`.
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
///   - `"catalog"` — known providers of the built-in catalog (fixed address).
///   - `"add <id> [--auth api-key|oauth] [--name n] [--key handle]"`,
///     `"add-custom <name> --api a --base-url u (--key handle | --no-auth)"`,
///     `"remove|enable|disable <name>"` — management (Welle 3 Scheibe 3; remote only
///     behind a step-up, see [`RemoteManage`]).
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
/// Resolution order: the live configuration
/// ([`crate::live_config::SharedLiveConfig`]), then a context-scoped
/// `Arc<harw_config::ResolvedConfig>`, then config discovery over the root
/// space bound to the session (`Arc<harw_home::ResolvedHomeContext>`, see
/// [`crate::config_util::load_bound_config`]). It never reads `HARW_HOME` or
/// `~/.harw`: a session started with a different root space (e.g. `--home`)
/// must not silently see the process's configuration.
///
/// `pub(crate)` so [`crate::model::handle_switch_core`],
/// [`crate::model::handle_uia_model_switch`],
/// [`crate::model::handle_uia_worker_model_switch`] and the `show`/`list`
/// paths of `/model` and `/uia-model` resolve the configuration through the
/// same authority that this function's own `handle_switch_core`/
/// `handle_uia_switch_core` already use.
///
/// # Errors
/// - [`OpError::NotAvailable`]: neither a config service nor a bound root
///   space is registered in `ctx`.
/// - [`OpError::Execution`]: config discovery over the bound root space
///   failed.
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

    crate::config_util::load_bound_config(ctx, "Config-Discovery fehlgeschlagen").map(Arc::new)
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
/// - [`OpError::NotAvailable`]: neither a config service nor a bound root
///   space in context (see [`resolved_config`]).
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
    summary = "Zeigt aktiven Provider; listet/testet/verwaltet Provider (catalog, add, add-custom, remove, enable, disable).",
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
        "catalog" => Ok(handle_catalog(ctx)),
        other => {
            let mut parts = other.split_whitespace();
            let first = parts.next().unwrap_or("");
            if MANAGE_SUBCOMMANDS.contains(&first) {
                let rest: Vec<String> = parts.map(str::to_owned).collect();
                handle_manage(ctx, first, &rest).await
            } else {
                Err(OpError::InvalidArguments(format!(
                    "Unknown /provider sub-command: '{other}'. \
                     Supported: show, list, test, catalog, add, add-custom, remove, enable, disable. \
                     Use `/model` to change the active provider and model together."
                )))
            }
        }
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
/// - [`OpError::NotAvailable`]: no configuration resolvable (see [`resolved_config`]).
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
/// - [`OpError::NotAvailable`]: no configuration resolvable (see [`resolved_config`]).
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
///
///    An explicit `model` additionally passes the provider lifecycle
///    ([`crate::live_model::enforce_lifecycle`]) **before** anything is
///    mutated or persisted: a retired model is rejected with
///    [`OpError::InvalidArguments`] (naming the suggested replacement) and
///    leaves controller, root session, child route and the persisted default
///    untouched; a deprecated model switches normally and the warning is
///    appended to the confirmation text.
/// 4. Only when all checks pass, calls `controller.set_active_provider(id)`
///    and, if a model argument was given, `controller.set_active_model(id)`.
/// 5. Best-effort persists the resulting active provider/model as the
///    on-disk default of the bound root space's profile via the injected
///    `persist` closure, so future sessions start with the same selection.
///    A persistence failure never fails the operation — it is appended to
///    the success text as a clear note instead.
///
/// `pub(crate)` because this is now the sole atomic provider(+model) switch
/// path: `/provider switch`/`/uia-provider switch` were retired as
/// sub-commands (an operator now always goes through `/model switch` or
/// `/uia-model switch`, which resolve the target model's configured provider
/// and delegate here so a provider+model pair never passes through a moment
/// of incompatibility). `/model switch` passes the
/// `persist_default_selection` of [`crate::config_util::selection_persistence`]
/// (an injected service, else the profile of the bound root space, never
/// `HARW_HOME`).
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
/// - [`OpError::NotAvailable`]: no configuration resolvable (see [`resolved_config`]).
/// - [`OpError::InvalidArguments`]: unknown provider, missing credentials, unknown
///   model, provider/model incompatibility, or a retired model (lifecycle).
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
    let mut lifecycle_warning: Option<String> = None;
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
            // Lifecycle of the explicit choice: a retired offering is refused
            // here, before the provider client is rebuilt, the controller is
            // mutated or anything is persisted. A deprecated one only warns.
            lifecycle_warning =
                crate::live_model::enforce_lifecycle(&canonical_target, &configured.id)?;
            Some(configured.id.clone())
        }
        None => {
            let snap = controller.snapshot();
            if let Some(ref active_model) = snap.active_model
                && let Some(model) = config.models.values().find(|model| {
                    model.id == *active_model
                        || model.aliases.iter().any(|alias| alias == active_model)
                })
            {
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
            None
        }
    };

    // ── Step 3b: make sure the running assembly can actually reach it ────────
    // A provider client that could not be built at startup (e.g. its
    // credential was only resolvable later) is rebuilt now; if that still
    // fails, the switch is rejected and the previous model stays active.
    crate::live_model::ensure_provider_ready(ctx, &canonical_target)?;

    let before = controller.snapshot();

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
    if let Some(warning) = &lifecycle_warning {
        text.push_str("\nWarnung: ");
        text.push_str(warning);
    }
    let note = persist(
        Some(canonical_target.as_str()),
        snap_after.active_model.as_deref(),
    );
    match note.as_deref() {
        Some(note) => {
            text.push('\n');
            text.push_str(note);
        }
        None => text.push_str("\n(als Standard für künftige Sitzungen gespeichert)"),
    }

    Ok(OpOutput {
        text,
        data: Some(
            serde_json::json!({"ui_feedback": {"kind":"model_selection", "target":"session", "changed":before.active_provider != snap_after.active_provider || before.active_model != snap_after.active_model, "provider":canonical_target, "model":snap_after.active_model, "effective":"next_turn", "warning":lifecycle_warning, "note":note}}),
        ),
    })
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
/// Aufrufer ([`crate::model::handle_uia_model_switch`]) übergibt
/// `persist_uia_selection` aus [`crate::config_util::selection_persistence`]
/// (injizierter Dienst, sonst das Profil des gebundenen Root-Space, nie
/// `HARW_HOME`); die Injektion existiert, damit Tests einen No-op-Abschluss
/// einsetzen können und **niemals** eine echte `config.toml` berühren.
///
/// # Errors
/// - [`OpError::NotAvailable`]: keine Config auflösbar (siehe [`resolved_config`]).
/// - [`OpError::Execution`]: Controller fehlt oder Mutation schlug fehl.
/// - [`OpError::InvalidArguments`]: unbekannter oder deaktivierter Provider,
///   fehlende Zugangsdaten, unbekanntes oder inkompatibles Modell, oder ein
///   abgeschaltetes (retired) Modell. Der Lebenszyklus einer ausdrücklich
///   genannten Modell-Wahl wird wie beim generischen Wechsel
///   ([`crate::live_model::enforce_lifecycle`]) vor jeder Mutation und
///   Persistenz geprüft; ein abgekündigtes Modell hängt eine Warnung an die
///   Bestätigung.
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
    let mut lifecycle_warning: Option<String> = None;
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
            // Wie beim generischen Wechsel: ein abgeschaltetes Modell wird vor
            // jeder Mutation abgelehnt, ein abgekündigtes nur mit Warnung.
            lifecycle_warning =
                crate::live_model::enforce_lifecycle(&canonical_target, &configured.id)?;
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
        None => match &cleared_incompatible_model {
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
    if let Some(warning) = &lifecycle_warning {
        text.push_str("\nWarnung: ");
        text.push_str(warning);
    }
    let note = persist(selection.provider.as_deref(), selection.model.as_deref());
    match note.as_deref() {
        Some(note) => {
            text.push('\n');
            text.push_str(note);
        }
        None => text.push_str("\n(UIA-Auswahl für künftige Sitzungen gespeichert)"),
    }

    Ok(OpOutput {
        text,
        data: Some(
            serde_json::json!({"ui_feedback": {"kind":"model_selection", "target":"uia", "changed":current != selection, "provider":canonical_target, "model":resolved_model, "cleared_model":cleared_incompatible_model, "effective":"next_uia_turn", "warning":lifecycle_warning, "note":note}}),
        ),
    })
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
/// - [`OpError::Execution`]: config discovery over the bound root space failed.
/// - [`OpError::NotAvailable`]: neither a config service nor a bound root
///   space in context (see [`resolved_config`]).
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
/// - [`OpError::NotAvailable`]: no configuration resolvable (see
///   [`resolved_config`]), no [`harw_provider_http::ProviderLoadRegistry`]
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
/// - [`OpError::Execution`]: controller not in context, or config discovery
///   over the bound root space failed.
/// - [`OpError::NotAvailable`]: neither a config service nor a bound root
///   space in context (see [`resolved_config`]).
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
/// - [`OpError::Execution`]: config discovery over the bound root space
///   failed, or the configured provider catalog is unavailable.
/// - [`OpError::NotAvailable`]: no configuration resolvable (see [`resolved_config`]).
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

/// Remote allow-list of `/provider` (contract K-W3b 2.8, integration manage-a D-1).
///
/// Reads (`show`, `list`, `catalog`, bare) need `observe`. Every write is a global
/// change behind a step-up. `test` stays TUI-only. `--allow-insecure-lan` is
/// refused at any position; `plan_change` additionally rejects `--auth <ref>` remotely.
pub const REMOTE: RemotePolicy = RemotePolicy {
    path: "/provider",
    verbs: &[
        RemoteVerb::read(&["-"]),
        RemoteVerb::read(&["show", "-"]),
        RemoteVerb::read(&["list", "-"]),
        RemoteVerb::read(&["catalog", "-"]),
        RemoteVerb::global(&["add", "*"], RemoteEffect::Applied),
        RemoteVerb::global(&["add-custom", "*"], RemoteEffect::Applied),
        RemoteVerb::global(&["remove", "*"], RemoteEffect::Applied),
        RemoteVerb::global(&["enable", "*"], RemoteEffect::Applied),
        RemoteVerb::global(&["disable", "*"], RemoteEffect::Applied),
    ],
    denied_tokens: &["--allow-insecure-lan"],
};

// ===== Verwaltung (Welle 3 Scheibe 3) =====
//
// Verwaltung bekannter und eigener Provider (`/provider catalog|add|add-custom|
// remove|enable|disable`). Vertrag K-W3b, Abschnitt 2. Sicherheitsregeln R1-R9:
//
// - Bekannte Provider (eingebauter Katalog): Adresse und API fest, kein
//   `--base-url`/`--api` (R3). `api-key` und `oauth` sind zwei getrennte Eintraege
//   (`<id>-key`, `<id>-oauth`).
// - Ein Schluessel wird nur geschrieben, nie gelesen: dieses Modul kennt ihn nur
//   als undurchsichtigen Griff ([`StagedKeyRef`]) und als grobe Form
//   ([`StagedKeyShape`]). Kein Port liefert den Wert.
// - Remote (Tenant gesetzt) nur mit Step-up-Dienst ([`RemoteManage`]); fehlt er,
//   wird jeder Schreib-Unterbefehl abgelehnt (R9, fail closed).
// - Ein bestehender Provider wird nie ueberschrieben (R1); die Datei entsteht per
//   Hard-Link aus einer privaten Temp-Datei (atomar, scheitert bei Existenz).

/// Standard-Namenssuffix fuer einen Katalog-Provider mit API-Schluessel.
pub const API_KEY_NAME_SUFFIX: &str = "-key";
/// Standard-Namenssuffix fuer einen Katalog-Provider mit OAuth-Anmeldung.
pub const OAUTH_NAME_SUFFIX: &str = "-oauth";
/// Hoechstlaenge eines remote angelegten Provider-Namens (Zeichen `[A-Za-z0-9_-]`).
pub const MAX_REMOTE_PROVIDER_NAME: usize = 48;
/// Hoechstzahl Modell-Ids aus dem Katalog, die `add` in `models` uebernimmt.
pub const MAX_CATALOG_MODELS: usize = 64;
/// Kopfzeile des kanonischen Step-up-Texts (Version des Textformats).
pub const STEPUP_TEXT_HEADER: &str = "harw step-up v1";
/// API-Name der ChatGPT-Plan-Route; MUSS gleich `harw_provider_http::chatgpt_plan::API` sein (Test T11).
pub const CHATGPT_PLAN_API: &str = "openai-chatgpt-plan";
/// Basis-URL der ChatGPT-Plan-Route; MUSS gleich `harw_provider_http::chatgpt_plan::BASE_URL` sein.
pub const CHATGPT_PLAN_BASE_URL: &str = "https://api.openai.com/v1";

/// Spiegel von `harw_session_host::stepup::MAX_TEXT_BYTES` (harw-ops haengt nicht von
/// harw-session-host ab).
const MAX_STEPUP_TEXT_BYTES: usize = 4096;
/// Hoechstlaenge eines Schluesselnamens im Step-up-Text.
const MAX_KEY_NAME_LEN: usize = 128;
/// Hoechstlaenge eines Schluessel-Griffs (Token der Befehlszeile).
const MAX_HANDLE_LEN: usize = 128;
/// Hoechstlaenge eines Provider-Namens lokal.
const MAX_LOCAL_PROVIDER_NAME: usize = 128;
/// APIs fuer remote angelegte eigene Provider (kein `ollama`, keine ChatGPT-Plan-Route).
const REMOTE_CUSTOM_APIS: &[&str] = &["openai-chat", "openai-responses", "anthropic-messages"];
/// APIs fuer lokal angelegte eigene Provider (wie `harw provider add`).
const LOCAL_CUSTOM_APIS: &[&str] = &[
    "openai-chat",
    "openai-responses",
    "anthropic-messages",
    "ollama",
];
/// Erlaubte Werte fuer `--auth-header`.
const AUTH_HEADERS: &[&str] = &["bearer", "x-api-key", "api-key", "none"];
/// Unterbefehle von `/provider`, die schreiben.
const MANAGE_SUBCOMMANDS: &[&str] = &["add", "add-custom", "remove", "enable", "disable"];

/// Anmeldeart eines Providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderAuthKind {
    /// Statischer API-Schluessel (neu per `secret.put`, lokal auch `--auth <ref>`).
    ApiKey,
    /// OAuth: Anthropic-Setup-Token (per `secret.put`) oder verknuepftes ChatGPT-Konto.
    Oauth,
    /// Kein Schluessel (nur eigene Provider).
    None,
}

impl ProviderAuthKind {
    /// `"api-key"`, `"oauth"`, `"none"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api-key",
            Self::Oauth => "oauth",
            Self::None => "none",
        }
    }
}

impl std::str::FromStr for ProviderAuthKind {
    type Err = OpError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "api-key" => Ok(Self::ApiKey),
            "oauth" => Ok(Self::Oauth),
            "none" => Ok(Self::None),
            _ => Err(OpError::InvalidArguments(
                "Anmeldeart unbekannt; erlaubt: api-key, oauth, none".to_owned(),
            )),
        }
    }
}

/// Woher die OAuth-Anmeldung eines Katalog-Providers kommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OAuthCredential {
    /// Setup-/OAuth-Token per `secret.put`; Wert muss mit `required_prefix` beginnen.
    StagedToken {
        /// Verlangtes Praefix des Tokens (Pruefung ueber [`StagedKeyShape`]).
        required_prefix: &'static str,
    },
    /// Am Server verknuepftes Konto (ChatGPT-Plan); `auth` bleibt leer, die Route
    /// bindet das Konto an die feste URL.
    LinkedAccount,
}

/// Eine OAuth-Route eines Katalog-Providers (fest im Code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OAuthRoute {
    /// Katalog-Id des Providers.
    pub catalog_id: &'static str,
    /// API-Name der Route.
    pub api: &'static str,
    /// Feste Basis-URL der Route.
    pub base_url: &'static str,
    /// Herkunft der Zugangsdaten.
    pub credential: OAuthCredential,
}

/// Feste OAuth-Tabelle. Genau diese zwei Eintraege.
pub const OAUTH_ROUTES: &[OAuthRoute] = &[
    OAuthRoute {
        catalog_id: "anthropic",
        api: "anthropic-messages",
        base_url: "https://api.anthropic.com/v1",
        credential: OAuthCredential::StagedToken {
            required_prefix: "sk-ant-oat",
        },
    },
    OAuthRoute {
        catalog_id: "openai",
        api: CHATGPT_PLAN_API,
        base_url: CHATGPT_PLAN_BASE_URL,
        credential: OAuthCredential::LinkedAccount,
    },
];

/// Ein waehlbarer bekannter Provider (geschwaerzt, ohne Schluessel).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct KnownProvider {
    /// Katalog-Id, z. B. `anthropic`, `openai`.
    pub id: String,
    /// Anzeigename aus dem Katalog.
    pub display_name: String,
    /// API des API-Key-Eintrags (`ProviderSpec::api`, kebab-case).
    pub api: String,
    /// Feste Basis-URL des API-Key-Eintrags.
    pub base_url: String,
    /// Angebotene Anmeldearten in fester Reihenfolge: `ApiKey` zuerst, dann `Oauth` falls Route vorhanden.
    pub auth_kinds: Vec<ProviderAuthKind>,
    /// `<id>-key`.
    pub default_name_api_key: String,
    /// `Some("<id>-oauth")`, wenn eine OAuth-Route existiert.
    pub default_name_oauth: Option<String>,
    /// Katalog-Modelle (hoechstens [`MAX_CATALOG_MODELS`]).
    pub models: Vec<String>,
    /// `true`, wenn bereits ein Provider mit `default_name_api_key` existiert (nur in `catalog`-Ausgabe gesetzt).
    #[serde(default)]
    pub api_key_taken: bool,
    /// Wie oben fuer `default_name_oauth`.
    #[serde(default)]
    pub oauth_taken: bool,
}

/// Verweis auf einen per `secret.put` vorgemerkten Schluessel. Traegt NIE den Wert.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StagedKeyRef {
    /// Undurchsichtiger Griff aus `secret.put`.
    pub handle: String,
}

/// Grobe, oeffentliche Form eines vorgemerkten Schluessels (Spiegel von
/// `harw_secrets::write_only::KeyShape`, damit harw-ops nicht von harw-secrets
/// abhaengt). Nur Praefix-Klasse, nie Bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedKeyShape {
    /// Anthropic-OAuth-/Setup-Token (`sk-ant-oat...`).
    AnthropicOAuthToken,
    /// Anthropic-API-Schluessel.
    AnthropicApiKey,
    /// Schluessel im OpenAI-Stil (`sk-...`).
    OpenAiStyleKey,
    /// Alles andere.
    Other,
}

/// Was [`StagedSecretPort::describe`] liefert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedKeyInfo {
    /// Name, unter dem der Schluessel im Secret-Store abgelegt wird.
    pub key_name: String,
    /// Provider-Name, an den `secret.put` den Schluessel gebunden hat.
    pub for_provider: String,
    /// Grobe Form des Schluessels.
    pub shape: StagedKeyShape,
}

/// Eine angefragte Aenderung (geparst, noch nicht geprueft).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ProviderChange {
    /// `/provider add <catalog_id> [--auth api-key|oauth] [--name <n>] [--key <handle>]`
    AddKnown {
        /// Katalog-Id.
        catalog_id: String,
        /// Anmeldeart.
        auth: ProviderAuthKind,
        /// Eigener Name; sonst `<id>-key` bzw. `<id>-oauth`.
        name: Option<String>,
        /// Vorgemerkter Schluessel.
        key: Option<StagedKeyRef>,
    },
    /// `/provider add-custom <name> --api <api> --base-url <url> (--key <handle> | --no-auth)
    /// [--auth-header bearer|x-api-key|api-key]`; lokal zusaetzlich `--auth <SecretRef>`
    /// und `--allow-insecure-lan`.
    AddCustom {
        /// Provider-Name.
        name: String,
        /// API-Name.
        api: String,
        /// Volle Basis-URL.
        base_url: String,
        /// Credential-Transport; `Some("none")` = ohne Anmeldung.
        auth_header: Option<String>,
        /// Vorgemerkter Schluessel.
        key: Option<StagedKeyRef>,
        /// Nur lokal (TUI/CLI); remote ist das ein Fehler.
        local_auth_ref: Option<String>,
        /// Nur lokal; remote ist das ein Fehler.
        allow_insecure_lan: bool,
    },
    /// `/provider remove <name>`
    Remove {
        /// Provider-Name.
        name: String,
    },
    /// `/provider enable <name>`
    Enable {
        /// Provider-Name.
        name: String,
    },
    /// `/provider disable <name>`
    Disable {
        /// Provider-Name.
        name: String,
    },
}

/// Herkunft des Aufrufs. Entscheidet nur der Server (Kontextdienst), nie die Befehlszeile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOrigin {
    /// TUI/CLI am Server selbst.
    Local,
    /// Handy ueber `command.run`.
    Remote,
}

/// Gepruefter Plan; wird bei Remote im Step-up-Registry geparkt.
#[derive(Debug, Clone)]
pub struct ProviderPlan {
    /// Die urspruengliche Aenderung.
    pub change: ProviderChange,
    /// Herkunft.
    pub origin: PlanOrigin,
    /// Endgueltiger Provider-Name (nach Standardnamen-Wahl).
    pub name: String,
    /// `"provider.add"`, `"provider.remove"`, `"provider.enable"`, `"provider.disable"`.
    pub action: &'static str,
    /// Fertige Datei fuer `add*` (`auth` noch `None`, wenn `key` gesetzt; wird bei `apply` gefuellt).
    pub toml: Option<harw_config::ProviderToml>,
    /// Name des neuen Schluessels (fuer Text und Audit), falls `key` gesetzt.
    pub key_name: Option<String>,
    /// Kanonischer Text. Gleicher Plan ergibt byte-gleichen Text.
    pub canonical_text: String,
}

/// Ergebnis von [`apply_plan`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProviderApplied {
    /// Aktion des Plans.
    pub action: &'static str,
    /// Provider-Name.
    pub name: String,
    /// `true`, wenn ein neuer Schluessel versiegelt wurde.
    pub key_stored: bool,
    /// Kurzer deutscher Hinweis.
    pub note: String,
}

/// Write-only-Zugang zu vorgemerkten Schluesseln. KEINE Methode liefert den Wert.
pub trait StagedSecretPort: Send + Sync {
    /// Beschreibt einen Griff ohne ihn zu verbrauchen. Fehler bei unbekanntem/abgelaufenem
    /// Griff oder wenn `for_provider` nicht passt (Text ohne Griff und ohne Wert).
    ///
    /// # Errors
    /// Text ohne Geheimnis, wenn der Griff nicht verwendbar ist.
    fn describe(&self, handle: &str, for_provider: &str) -> Result<StagedKeyInfo, String>;
    /// Verbraucht den Griff (einmalig), versiegelt den Wert im Secret-Store und liefert
    /// die Referenz (`secrets:<uuid>`).
    ///
    /// # Errors
    /// Text ohne Geheimnis.
    fn commit(&self, handle: &str, for_provider: &str) -> Result<harw_config::SecretRef, String>;
    /// Entfernt einen gerade versiegelten Schluessel wieder (Rollback, bestes Bemuehen, nie panic).
    fn rollback(&self, reference: &harw_config::SecretRef);
}

/// Parkt einen Plan hinter einem Step-up (Adapter auf `harw_session_host::stepup::StepUpRegistry`).
pub trait ProviderStepUpPort: Send + Sync {
    /// Liefert die Step-up-Daten; der Plan ist danach nur noch ueber `stepup.confirm` anwendbar.
    ///
    /// # Errors
    /// Text ohne Geheimnis (Rate-Limit, Kapazitaet).
    fn stage(&self, plan: ProviderPlan) -> Result<StagedStepUp, String>;
}

/// Antwortdaten eines geparkten Plans (gehen in `OpOutput::data`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StagedStepUp {
    /// Id des Step-ups.
    pub stepup_id: String,
    /// RFC 3339, UTC.
    pub expires_at: String,
}

/// Kontextdienst, den NUR der Remote-Pfad (`command.run`) registriert: `Arc<RemoteManage>`.
pub struct RemoteManage {
    /// Step-up-Ablage.
    pub stepup: Arc<dyn ProviderStepUpPort>,
    /// Write-only-Schluesselablage.
    pub secrets: Arc<dyn StagedSecretPort>,
    /// DNS-Aufloesung fuer den URL-Guard.
    pub resolver: Arc<dyn harw_provider_http::url_guard::HostResolver>,
}

/// Ob `id` als Teil eines Provider-Namens taugt (`[A-Za-z0-9_-]`, nicht leer).
fn is_name_charset(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Katalog-Api als kebab-case-Text.
const fn api_name(api: harw_model_catalog::ProviderApi) -> &'static str {
    match api {
        harw_model_catalog::ProviderApi::OpenAiResponses => "openai-responses",
        harw_model_catalog::ProviderApi::OpenAiChat => "openai-chat",
        harw_model_catalog::ProviderApi::AnthropicMessages => "anthropic-messages",
        harw_model_catalog::ProviderApi::Ollama => "ollama",
    }
}

/// Bekannte Provider aus dem eingebauten Katalog.
///
/// Gefiltert auf `base_url` mit `https://` ohne `<`/`>`, `api != Ollama` und
/// (`ApiKey`-Anmeldung ODER Id in [`OAUTH_ROUTES`]); Id muss als Namensteil taugen.
/// Sortiert nach `id`.
#[must_use]
pub fn known_providers() -> Vec<KnownProvider> {
    let mut out: Vec<KnownProvider> = harw_model_catalog::embedded_catalog()
        .into_iter()
        .filter(|spec| {
            let has_route = OAUTH_ROUTES.iter().any(|route| route.catalog_id == spec.id);
            let has_api_key = spec
                .auth
                .iter()
                .any(|method| matches!(method, harw_model_catalog::AuthMethod::ApiKey { .. }));
            spec.base_url.starts_with("https://")
                && !spec.base_url.contains(['<', '>'])
                && spec.api != harw_model_catalog::ProviderApi::Ollama
                && (has_api_key || has_route)
                && is_name_charset(&spec.id)
                && spec.id.len() + OAUTH_NAME_SUFFIX.len() <= MAX_REMOTE_PROVIDER_NAME
        })
        .map(|spec| {
            let has_route = OAUTH_ROUTES.iter().any(|route| route.catalog_id == spec.id);
            let mut auth_kinds = vec![ProviderAuthKind::ApiKey];
            if has_route {
                auth_kinds.push(ProviderAuthKind::Oauth);
            }
            KnownProvider {
                default_name_api_key: format!("{}{API_KEY_NAME_SUFFIX}", spec.id),
                default_name_oauth: has_route.then(|| format!("{}{OAUTH_NAME_SUFFIX}", spec.id)),
                display_name: spec.name,
                api: api_name(spec.api).to_owned(),
                base_url: spec.base_url,
                auth_kinds,
                models: spec.models.into_iter().take(MAX_CATALOG_MODELS).collect(),
                api_key_taken: false,
                oauth_taken: false,
                id: spec.id,
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Ein bekannter Provider nach Katalog-Id.
#[must_use]
pub fn known_provider(id: &str) -> Option<KnownProvider> {
    known_providers().into_iter().find(|p| p.id == id)
}

/// Ergebnis der Flag-Zerlegung eines Unterbefehls.
#[derive(Debug, Default)]
struct ParsedFlags {
    positional: Vec<String>,
    values: std::collections::HashMap<&'static str, String>,
    switches: std::collections::HashSet<&'static str>,
}

impl ParsedFlags {
    fn value(&self, flag: &str) -> Option<&String> {
        self.values.get(flag)
    }

    fn switch(&self, flag: &str) -> bool {
        self.switches.contains(flag)
    }
}

/// Zerlegt `tokens` strikt: unbekannte und doppelte Flags sind Fehler. Die Eingabe wird
/// nie im Fehlertext wiederholt (nur der Flag-Name vor einem `=`, gekuerzt).
fn parse_flags(
    tokens: &[String],
    spec: &[(&'static str, bool)],
) -> Result<ParsedFlags, OpError> {
    let mut parsed = ParsedFlags::default();
    let mut iter = tokens.iter();
    while let Some(token) = iter.next() {
        if !token.starts_with("--") {
            parsed.positional.push(token.clone());
            continue;
        }
        let Some(&(flag, takes_value)) = spec.iter().find(|(name, _)| *name == token.as_str())
        else {
            let shown: String = token
                .split('=')
                .next()
                .unwrap_or("")
                .chars()
                .take(24)
                .collect();
            return Err(OpError::InvalidArguments(format!(
                "Unbekannte Option '{shown}'"
            )));
        };
        if parsed.values.contains_key(flag) || parsed.switches.contains(flag) {
            return Err(OpError::InvalidArguments(format!(
                "Option {flag} doppelt angegeben"
            )));
        }
        if takes_value {
            match iter.next() {
                Some(value) if !value.starts_with("--") => {
                    parsed.values.insert(flag, value.clone());
                }
                _ => {
                    return Err(OpError::InvalidArguments(format!(
                        "Option {flag} braucht einen Wert"
                    )));
                }
            }
        } else {
            parsed.switches.insert(flag);
        }
    }
    Ok(parsed)
}

/// Genau ein Positionsargument.
fn single_positional(parsed: &ParsedFlags, what: &str) -> Result<String, OpError> {
    match parsed.positional.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(OpError::InvalidArguments(format!("{what} fehlt"))),
        _ => Err(OpError::InvalidArguments(
            "Zu viele Argumente".to_owned(),
        )),
    }
}

/// Prueft einen Schluessel-Griff (nur Form, nie Inhalt).
fn parse_key_ref(raw: Option<&String>) -> Result<Option<StagedKeyRef>, OpError> {
    let Some(handle) = raw else { return Ok(None) };
    if handle.is_empty()
        || handle.len() > MAX_HANDLE_LEN
        || !handle.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(OpError::InvalidArguments(
            "Schluessel-Griff ungueltig".to_owned(),
        ));
    }
    Ok(Some(StagedKeyRef {
        handle: handle.clone(),
    }))
}

/// Parst die Tokens NACH dem Unterbefehl (`add`, `add-custom`, `remove`, `enable`, `disable`).
///
/// Unbekannte Flags, doppelte Flags, `--auth` UND `--key`, `--key` UND `--no-auth` sind
/// [`OpError::InvalidArguments`]. Es gibt kein `--base-url`/`--api` fuer `add` (R3).
///
/// # Errors
/// [`OpError::InvalidArguments`] bei jedem Verstoss.
pub fn parse_manage_command(sub: &str, tokens: &[String]) -> Result<ProviderChange, OpError> {
    match sub {
        "add" => {
            let parsed = parse_flags(tokens, &[("--auth", true), ("--name", true), ("--key", true)])?;
            let catalog_id = single_positional(&parsed, "Katalog-Id")?;
            let auth = parsed
                .value("--auth")
                .map(|raw| raw.parse::<ProviderAuthKind>())
                .transpose()?
                .unwrap_or(ProviderAuthKind::ApiKey);
            Ok(ProviderChange::AddKnown {
                catalog_id,
                auth,
                name: parsed.value("--name").cloned(),
                key: parse_key_ref(parsed.value("--key"))?,
            })
        }
        "add-custom" => {
            let parsed = parse_flags(
                tokens,
                &[
                    ("--api", true),
                    ("--base-url", true),
                    ("--key", true),
                    ("--no-auth", false),
                    ("--auth", true),
                    ("--auth-header", true),
                    ("--allow-insecure-lan", false),
                ],
            )?;
            let name = single_positional(&parsed, "Provider-Name")?;
            let key = parse_key_ref(parsed.value("--key"))?;
            let no_auth = parsed.switch("--no-auth");
            let local_auth_ref = parsed.value("--auth").cloned();
            if key.is_some() && local_auth_ref.is_some() {
                return Err(OpError::InvalidArguments(
                    "--auth und --key schliessen sich aus".to_owned(),
                ));
            }
            if key.is_some() && no_auth {
                return Err(OpError::InvalidArguments(
                    "--key und --no-auth schliessen sich aus".to_owned(),
                ));
            }
            if local_auth_ref.is_some() && no_auth {
                return Err(OpError::InvalidArguments(
                    "--auth und --no-auth schliessen sich aus".to_owned(),
                ));
            }
            let mut auth_header = parsed.value("--auth-header").cloned();
            if no_auth {
                if auth_header.as_deref().is_some_and(|h| h != "none") {
                    return Err(OpError::InvalidArguments(
                        "--no-auth und --auth-header schliessen sich aus".to_owned(),
                    ));
                }
                auth_header = Some("none".to_owned());
            }
            let api = parsed
                .value("--api")
                .cloned()
                .ok_or_else(|| OpError::InvalidArguments("--api fehlt".to_owned()))?;
            let base_url = parsed
                .value("--base-url")
                .cloned()
                .ok_or_else(|| OpError::InvalidArguments("--base-url fehlt".to_owned()))?;
            Ok(ProviderChange::AddCustom {
                name,
                api,
                base_url,
                auth_header,
                key,
                local_auth_ref,
                allow_insecure_lan: parsed.switch("--allow-insecure-lan"),
            })
        }
        "remove" | "enable" | "disable" => {
            let parsed = parse_flags(tokens, &[])?;
            let name = single_positional(&parsed, "Provider-Name")?;
            Ok(match sub {
                "remove" => ProviderChange::Remove { name },
                "enable" => ProviderChange::Enable { name },
                _ => ProviderChange::Disable { name },
            })
        }
        _ => Err(OpError::InvalidArguments(
            "Unbekannter Verwaltungs-Unterbefehl; erlaubt: add, add-custom, remove, enable, disable"
                .to_owned(),
        )),
    }
}

/// Umgebung der Planung (nur gelesen).
struct PlanEnv<'a> {
    config: &'a harw_config::ResolvedConfig,
    profile_dir: &'a std::path::Path,
    origin: PlanOrigin,
    secrets: Option<&'a dyn StagedSecretPort>,
}

fn provider_file(profile_dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    profile_dir.join("providers").join(format!("{name}.toml"))
}

/// Regel 1: Namenszeichen und Laenge.
fn validate_provider_name(name: &str, origin: PlanOrigin) -> Result<(), OpError> {
    let limit = match origin {
        PlanOrigin::Remote => MAX_REMOTE_PROVIDER_NAME,
        PlanOrigin::Local => MAX_LOCAL_PROVIDER_NAME,
    };
    if !is_name_charset(name) || name.len() > limit {
        return Err(OpError::InvalidArguments(format!(
            "Ungueltiger Provider-Name: nur ASCII-Buchstaben, Ziffern, '-' und '_', hoechstens {limit} Zeichen"
        )));
    }
    Ok(())
}

/// Regel 4 (Existenz): Name in `config.providers` ODER Datei vorhanden.
fn provider_exists(env: &PlanEnv<'_>, name: &str) -> bool {
    env.config
        .providers
        .iter()
        .any(|(key, p)| key.eq_ignore_ascii_case(name) || p.name.eq_ignore_ascii_case(name))
        || provider_file(env.profile_dir, name).exists()
}

fn already_exists_error(name: &str) -> OpError {
    OpError::InvalidArguments(format!(
        "Provider {name} existiert bereits; remote nicht aenderbar"
    ))
}

/// Wert fuer den kanonischen Text: Steuerzeichen werden abgelehnt, nicht ersetzt.
fn text_value(value: &str) -> Result<&str, OpError> {
    if value.chars().any(char::is_control) {
        return Err(OpError::InvalidArguments(
            "Steuerzeichen in einem Wert".to_owned(),
        ));
    }
    Ok(value)
}

fn finish_text(lines: &[String]) -> Result<String, OpError> {
    let text = lines.join("\n");
    if text.len() > MAX_STEPUP_TEXT_BYTES {
        return Err(OpError::InvalidArguments(
            "Beschreibung der Aenderung zu lang".to_owned(),
        ));
    }
    Ok(text)
}

/// Kanonischer Text fuer `add*` (Format 2.5).
fn canonical_add_text(
    name: &str,
    source: &str,
    api: &str,
    base_url: &str,
    login: &str,
    key_line: &str,
    models: usize,
) -> Result<String, OpError> {
    finish_text(&[
        STEPUP_TEXT_HEADER.to_owned(),
        "aktion: provider.add".to_owned(),
        format!("provider: {}", text_value(name)?),
        format!("quelle: {}", text_value(source)?),
        format!("api: {}", text_value(api)?),
        format!("adresse: {}", text_value(base_url)?),
        format!("anmeldung: {login}"),
        format!("schluessel: {key_line}"),
        format!("modelle: {models}"),
        "aktiv: ja".to_owned(),
    ])
}

/// Kanonischer Text fuer `remove|enable|disable`.
fn canonical_simple_text(action: &str, name: &str, base_url: &str) -> Result<String, OpError> {
    finish_text(&[
        STEPUP_TEXT_HEADER.to_owned(),
        format!("aktion: {action}"),
        format!("provider: {}", text_value(name)?),
        format!("adresse: {}", text_value(base_url)?),
    ])
}

/// Beschreibt den vorgemerkten Schluessel; verbraucht ihn NICHT (Regel 5).
fn describe_key(
    env: &PlanEnv<'_>,
    key: &StagedKeyRef,
    name: &str,
) -> Result<StagedKeyInfo, OpError> {
    let secrets = env
        .secrets
        .ok_or_else(|| OpError::InvalidArguments("Schluessel-Ablage fehlt".to_owned()))?;
    // Der Fehlertext des Ports wird bewusst nicht weitergereicht (kein Griff, kein Wert).
    let info = secrets.describe(&key.handle, name).map_err(|_| {
        OpError::InvalidArguments(
            "Schluessel unbekannt, abgelaufen oder fuer diesen Provider nicht vorgemerkt"
                .to_owned(),
        )
    })?;
    if info.for_provider != name {
        return Err(OpError::InvalidArguments(
            "Schluessel gehoert zu anderem Provider".to_owned(),
        ));
    }
    if info.key_name.is_empty()
        || info.key_name.len() > MAX_KEY_NAME_LEN
        || info.key_name.chars().any(|c| c.is_control() || c == '"')
    {
        return Err(OpError::InvalidArguments(
            "Schluesselname ungueltig".to_owned(),
        ));
    }
    Ok(info)
}

/// Leere Provider-Datei mit allen Feldern explizit (wie `harw-cli` `add_provider`).
fn new_provider_toml(
    name: &str,
    api: &str,
    base_url: &str,
    auth_header: Option<String>,
    models: Vec<String>,
    allow_insecure_lan: bool,
) -> harw_config::ProviderToml {
    harw_config::ProviderToml {
        stream: None,
        name: name.to_owned(),
        api: api.to_owned(),
        base_url: base_url.to_owned(),
        auth: None,
        auth_header,
        api_key: None,
        originator: None,
        headers: std::collections::HashMap::new(),
        models,
        enabled: true,
        origin_allowlist: harw_config::OriginAllowlistToml::default(),
        rate_limit: None,
        max_concurrency: None,
        default_reasoning_effort: None,
        gateway_identity_headers: false,
        request_timeout_secs: None,
        stream_idle_timeout_secs: None,
        retry_timeouts: None,
        max_tokens_field: None,
        send_reasoning_effort: None,
        strict_tools: None,
        parallel_tool_calls: None,
        allow_insecure_lan,
    }
}

fn plan_add_known(env: &PlanEnv<'_>, change: &ProviderChange) -> Result<ProviderPlan, OpError> {
    let ProviderChange::AddKnown {
        catalog_id,
        auth,
        name,
        key,
    } = change
    else {
        return Err(OpError::Execution("Planfehler: falscher Aenderungstyp".to_owned()));
    };
    let known = known_provider(catalog_id).ok_or_else(|| {
        OpError::InvalidArguments(
            "Unbekannter Katalog-Provider; /provider catalog zeigt die Auswahl".to_owned(),
        )
    })?;
    // Regel 2 (Auswahl): Anmeldeart, Standardname, feste Adresse und API (R3).
    let (default_name, api, base_url, credential) = match auth {
        ProviderAuthKind::None => {
            return Err(OpError::InvalidArguments(
                "Anmeldeart none gibt es nur fuer eigene Provider".to_owned(),
            ));
        }
        ProviderAuthKind::ApiKey => (
            known.default_name_api_key.clone(),
            known.api.clone(),
            known.base_url.clone(),
            None,
        ),
        ProviderAuthKind::Oauth => {
            let route = OAUTH_ROUTES
                .iter()
                .find(|route| route.catalog_id == known.id)
                .ok_or_else(|| {
                    OpError::InvalidArguments(
                        "Fuer diesen Provider gibt es keine OAuth-Anmeldung".to_owned(),
                    )
                })?;
            (
                format!("{}{OAUTH_NAME_SUFFIX}", known.id),
                route.api.to_owned(),
                route.base_url.to_owned(),
                Some(route.credential),
            )
        }
    };
    let name = name.clone().unwrap_or(default_name);
    validate_provider_name(&name, env.origin)?;

    // Regel 2 (Schluessel-Pflicht).
    let needs_key = matches!(
        (auth, credential),
        (ProviderAuthKind::ApiKey, _)
            | (ProviderAuthKind::Oauth, Some(OAuthCredential::StagedToken { .. }))
    );
    if needs_key && key.is_none() {
        return Err(OpError::InvalidArguments(match env.origin {
            PlanOrigin::Remote => {
                "Schluessel fehlt: zuerst secret.put, dann --key <griff>".to_owned()
            }
            PlanOrigin::Local => {
                "Schluessel fehlt: lokal ueber `harw provider add-known --key-stdin`".to_owned()
            }
        }));
    }
    if !needs_key && key.is_some() {
        return Err(OpError::InvalidArguments(
            "Das verknuepfte Konto braucht keinen Schluessel".to_owned(),
        ));
    }

    // Regel 4 (Existenz, R1).
    if provider_exists(env, &name) {
        return Err(already_exists_error(&name));
    }

    // Regel 5 und R7 (Form-Pruefung).
    let info = key
        .as_ref()
        .map(|key_ref| describe_key(env, key_ref, &name))
        .transpose()?;
    if let Some(info) = &info {
        let is_oauth_token = info.shape == StagedKeyShape::AnthropicOAuthToken;
        match auth {
            ProviderAuthKind::ApiKey if is_oauth_token => {
                return Err(OpError::InvalidArguments(
                    "Ein OAuth-Token gehoert nicht als API-Schluessel angelegt; Anmeldeart oauth waehlen"
                        .to_owned(),
                ));
            }
            ProviderAuthKind::Oauth if !is_oauth_token => {
                return Err(OpError::InvalidArguments(
                    "Fuer oauth wird ein Setup-Token erwartet".to_owned(),
                ));
            }
            _ => {}
        }
    }

    let toml = new_provider_toml(&name, &api, &base_url, None, known.models.clone(), false);
    let login = if *auth == ProviderAuthKind::Oauth {
        "oauth"
    } else {
        "api-key"
    };
    let key_line = match (&info, credential) {
        (Some(info), _) => format!("neu \"{}\" (nur schreiben)", info.key_name),
        (None, Some(OAuthCredential::LinkedAccount)) => "verknuepftes Konto".to_owned(),
        (None, _) => "-".to_owned(),
    };
    let canonical_text = canonical_add_text(
        &name,
        &format!("katalog {}", known.id),
        &api,
        &base_url,
        login,
        &key_line,
        toml.models.len(),
    )?;
    Ok(ProviderPlan {
        change: change.clone(),
        origin: env.origin,
        name,
        action: "provider.add",
        toml: Some(toml),
        key_name: info.map(|i| i.key_name),
        canonical_text,
    })
}

fn plan_add_custom(env: &PlanEnv<'_>, change: &ProviderChange) -> Result<ProviderPlan, OpError> {
    let ProviderChange::AddCustom {
        name,
        api,
        base_url,
        auth_header,
        key,
        local_auth_ref,
        allow_insecure_lan,
    } = change
    else {
        return Err(OpError::Execution("Planfehler: falscher Aenderungstyp".to_owned()));
    };
    validate_provider_name(name, env.origin)?;
    if let Some(header) = auth_header
        && !AUTH_HEADERS.contains(&header.as_str())
    {
        return Err(OpError::InvalidArguments(
            "--auth-header unbekannt; erlaubt: bearer, x-api-key, api-key".to_owned(),
        ));
    }
    let no_auth = auth_header.as_deref() == Some("none");

    let mut auth_ref = None;
    match env.origin {
        PlanOrigin::Remote => {
            if local_auth_ref.is_some() || *allow_insecure_lan {
                return Err(OpError::InvalidArguments(
                    "--auth <Referenz> und --allow-insecure-lan gibt es nur lokal".to_owned(),
                ));
            }
            // R4 ohne DNS; der Fehlertext enthaelt weder URL noch Adresse.
            harw_provider_http::url_guard::check_remote_url_syntax(base_url)
                .map_err(|error| OpError::InvalidArguments(error.to_string()))?;
            if !REMOTE_CUSTOM_APIS.contains(&api.as_str()) {
                return Err(OpError::InvalidArguments(
                    "API fuer eigene Provider remote: openai-chat, openai-responses oder anthropic-messages"
                        .to_owned(),
                ));
            }
            if key.is_some() == no_auth {
                return Err(OpError::InvalidArguments(
                    "Entweder einen neuen Schluessel (--key) oder --no-auth angeben".to_owned(),
                ));
            }
        }
        PlanOrigin::Local => {
            if !LOCAL_CUSTOM_APIS.contains(&api.as_str()) {
                return Err(OpError::InvalidArguments(
                    "API unbekannt; erlaubt: openai-chat, openai-responses, anthropic-messages, ollama"
                        .to_owned(),
                ));
            }
            harw_provider_http::validate_endpoint_with(base_url, *allow_insecure_lan)
                .map_err(|error| OpError::InvalidArguments(format!("Basis-URL ungueltig: {error}")))?;
            if let Some(raw) = local_auth_ref {
                if key.is_some() || no_auth {
                    return Err(OpError::InvalidArguments(
                        "Anmeldung widerspruechlich (--auth, --key, --no-auth)".to_owned(),
                    ));
                }
                let parsed = raw.parse::<harw_config::SecretRef>().map_err(|_| {
                    OpError::InvalidArguments(
                        "--auth erwartet eine Referenz (env:, file:, keyring:, secrets:), keinen Klartext"
                            .to_owned(),
                    )
                })?;
                auth_ref = Some(parsed);
            }
        }
    }

    if provider_exists(env, name) {
        return Err(already_exists_error(name));
    }
    let info = key
        .as_ref()
        .map(|key_ref| describe_key(env, key_ref, name))
        .transpose()?;
    if let Some(info) = &info
        && info.shape == StagedKeyShape::AnthropicOAuthToken
    {
        return Err(OpError::InvalidArguments(
            "Ein OAuth-Token geht nur an den bekannten Provider anthropic mit Anmeldeart oauth"
                .to_owned(),
        ));
    }

    let remote = env.origin == PlanOrigin::Remote;
    let mut toml = new_provider_toml(
        name,
        api,
        base_url,
        auth_header.clone(),
        Vec::new(),
        !remote && *allow_insecure_lan,
    );
    toml.auth = auth_ref;
    if !remote && toml.is_local() {
        if toml.auth.is_none() && toml.auth_header.is_none() && info.is_none() {
            toml.auth_header = Some("none".to_owned());
        }
        toml.max_concurrency = Some(1);
    }
    let login = if info.is_some() || toml.auth.is_some() {
        "api-key"
    } else {
        "keine"
    };
    let key_line = info.as_ref().map_or_else(
        || "-".to_owned(),
        |i| format!("neu \"{}\" (nur schreiben)", i.key_name),
    );
    let canonical_text =
        canonical_add_text(name, "eigen", api, base_url, login, &key_line, 0)?;
    Ok(ProviderPlan {
        change: change.clone(),
        origin: env.origin,
        name: name.clone(),
        action: "provider.add",
        toml: Some(toml),
        key_name: info.map(|i| i.key_name),
        canonical_text,
    })
}

fn plan_simple(env: &PlanEnv<'_>, change: &ProviderChange) -> Result<ProviderPlan, OpError> {
    let (name, action) = match change {
        ProviderChange::Remove { name } => (name, "provider.remove"),
        ProviderChange::Enable { name } => (name, "provider.enable"),
        ProviderChange::Disable { name } => (name, "provider.disable"),
        _ => {
            return Err(OpError::Execution("Planfehler: falscher Aenderungstyp".to_owned()));
        }
    };
    validate_provider_name(name, env.origin)?;
    let file = provider_file(env.profile_dir, name);
    if !file.is_file() {
        return Err(OpError::InvalidArguments(format!(
            "Provider {name} ist unbekannt oder nicht als Datei angelegt"
        )));
    }
    let base_url = env
        .config
        .providers
        .get(name.as_str())
        .map(|p| p.base_url.clone())
        .or_else(|| {
            std::fs::read_to_string(&file)
                .ok()
                .and_then(|text| toml::from_str::<harw_config::ProviderToml>(&text).ok())
                .map(|p| p.base_url)
        })
        .unwrap_or_else(|| "-".to_owned());
    Ok(ProviderPlan {
        change: change.clone(),
        origin: env.origin,
        name: name.clone(),
        action,
        toml: None,
        key_name: None,
        canonical_text: canonical_simple_text(action, name, &base_url)?,
    })
}

/// Prueft synchron (ohne DNS, ohne Schreiben) und baut den Plan samt kanonischem Text.
///
/// `existing` ergibt sich aus `config.providers` UND den Dateien `providers/*.toml`.
/// `secrets` ist Pflicht, wenn `key` gesetzt ist.
///
/// # Errors
/// [`OpError::InvalidArguments`] bei jedem Verstoss gegen R1-R4, R7.
pub fn plan_change(
    config: &harw_config::ResolvedConfig,
    profile_dir: &std::path::Path,
    change: ProviderChange,
    origin: PlanOrigin,
    secrets: Option<&dyn StagedSecretPort>,
) -> Result<ProviderPlan, OpError> {
    let env = PlanEnv {
        config,
        profile_dir,
        origin,
        secrets,
    };
    match &change {
        ProviderChange::AddKnown { .. } => plan_add_known(&env, &change),
        ProviderChange::AddCustom { .. } => plan_add_custom(&env, &change),
        ProviderChange::Remove { .. }
        | ProviderChange::Enable { .. }
        | ProviderChange::Disable { .. } => plan_simple(&env, &change),
    }
}

/// DNS-Pruefung (R4) fuer `AddCustom` mit `origin == Remote`; fuer alle anderen Plaene `Ok(())`.
/// Wird VOR `stage` und noch einmal VOR [`apply_plan`] aufgerufen (DNS kann sich aendern).
///
/// # Errors
/// [`OpError::InvalidArguments`] mit deutschem Text ohne URL und ohne Adresse.
pub async fn guard_plan(
    plan: &ProviderPlan,
    resolver: &dyn harw_provider_http::url_guard::HostResolver,
) -> Result<(), OpError> {
    if plan.origin != PlanOrigin::Remote {
        return Ok(());
    }
    let ProviderChange::AddCustom { base_url, .. } = &plan.change else {
        return Ok(());
    };
    harw_provider_http::url_guard::guard_remote_url_with(base_url, resolver)
        .await
        .map(|_| ())
        .map_err(|error| OpError::InvalidArguments(error.to_string()))
}

/// Eindeutiger Hex-Suffix fuer Temp-Dateien (kein Geheimnis; Schutz liefert `create_new`).
fn unique_hex() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    format!("{:016x}", nanos ^ (pid << 32) ^ count.rotate_left(48))
}

/// Legt eine private Datei NEU an (Modus 0o600 unter Unix); schlaegt bei Existenz fehl.
fn open_private_new(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_private_tmp(dir: &std::path::Path, stem: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let tmp = dir.join(format!(".{stem}.tmp-{}", unique_hex()));
    let mut file = open_private_new(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(tmp)
}

/// Datei-Anlage ohne Ueberschreiben (2.7): Temp-Datei, dann `hard_link` (scheitert atomar
/// mit `AlreadyExists`), dann Temp-Datei entfernen.
fn write_new_provider_file(
    profile_dir: &std::path::Path,
    name: &str,
    provider: &harw_config::ProviderToml,
) -> std::io::Result<()> {
    let dir = profile_dir.join("providers");
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{name}.toml"));
    let rendered = toml::to_string_pretty(provider)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let tmp = write_private_tmp(&dir, &format!("{name}.toml"), rendered.as_bytes())?;
    let linked = std::fs::hard_link(&tmp, &target);
    let _ = std::fs::remove_file(&tmp);
    linked
}

/// Setzt `enabled` in einer vorhandenen Datei (kommentarerhaltend), per Temp-Datei + `rename`.
fn set_enabled_in_file(path: &std::path::Path, enabled: bool) -> Result<(), OpError> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            OpError::InvalidArguments("Provider-Datei nicht gefunden".to_owned())
        } else {
            OpError::Execution(format!("Provider-Datei nicht lesbar: {}", error.kind()))
        }
    })?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| OpError::Execution("Provider-Datei ist kein gueltiges TOML".to_owned()))?;
    doc.as_table_mut()
        .insert("enabled", toml_edit::value(enabled));
    let dir = path
        .parent()
        .ok_or_else(|| OpError::Execution("Provider-Datei ohne Verzeichnis".to_owned()))?;
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| OpError::Execution("Provider-Dateiname ungueltig".to_owned()))?;
    let tmp = write_private_tmp(dir, stem, doc.to_string().as_bytes())
        .map_err(|error| OpError::Execution(format!("Schreiben fehlgeschlagen: {}", error.kind())))?;
    std::fs::rename(&tmp, path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        OpError::Execution(format!("Schreiben fehlgeschlagen: {}", error.kind()))
    })
}

fn apply_add(
    profile_dir: &std::path::Path,
    plan: &ProviderPlan,
    secrets: Option<&dyn StagedSecretPort>,
    live: Option<&crate::live_config::SharedLiveConfig>,
) -> Result<ProviderApplied, OpError> {
    let mut provider = plan
        .toml
        .clone()
        .ok_or_else(|| OpError::Execution("Plan ohne Provider-Datei".to_owned()))?;
    let key = match &plan.change {
        ProviderChange::AddKnown { key, .. } | ProviderChange::AddCustom { key, .. } => {
            key.as_ref()
        }
        _ => None,
    };
    // Reihenfolge bei Schluessel: commit -> Datei schreiben -> bei Fehler rollback.
    let mut committed: Option<harw_config::SecretRef> = None;
    if let Some(key_ref) = key {
        let port = secrets
            .ok_or_else(|| OpError::InvalidArguments("Schluessel-Ablage fehlt".to_owned()))?;
        let reference = port.commit(&key_ref.handle, &plan.name).map_err(|_| {
            OpError::Execution("Schluessel konnte nicht versiegelt werden".to_owned())
        })?;
        provider.auth = Some(reference.clone());
        committed = Some(reference);
    }
    if let Err(error) = write_new_provider_file(profile_dir, &plan.name, &provider) {
        if let (Some(reference), Some(port)) = (&committed, secrets) {
            port.rollback(reference);
        }
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            already_exists_error(&plan.name)
        } else {
            OpError::Execution(format!("Provider-Datei nicht schreibbar: {}", error.kind()))
        });
    }
    if let Some(live) = live {
        let name = plan.name.clone();
        live.update(move |config| {
            config.providers.insert(name, provider);
        });
    }
    Ok(ProviderApplied {
        action: plan.action,
        name: plan.name.clone(),
        key_stored: committed.is_some(),
        note: "Provider angelegt; gilt ab der naechsten Session.".to_owned(),
    })
}

/// Wendet einen Plan an. `add*`: Datei `providers/<name>.toml` wird NUR neu angelegt (nie
/// ueberschrieben). `remove`/`enable`/`disable`: Datei muss existieren. Danach `live.update`.
///
/// # Errors
/// [`OpError::InvalidArguments`] (Datei existiert/fehlt), [`OpError::Execution`] (I/O,
/// Siegeln); bei Schreibfehler nach dem Siegeln wird der Schluessel zurueckgerollt.
pub fn apply_plan(
    profile_dir: &std::path::Path,
    plan: &ProviderPlan,
    secrets: Option<&dyn StagedSecretPort>,
    live: Option<&crate::live_config::SharedLiveConfig>,
) -> Result<ProviderApplied, OpError> {
    validate_provider_name(&plan.name, plan.origin)?;
    match &plan.change {
        ProviderChange::AddKnown { .. } | ProviderChange::AddCustom { .. } => {
            apply_add(profile_dir, plan, secrets, live)
        }
        ProviderChange::Remove { .. } => {
            std::fs::remove_file(provider_file(profile_dir, &plan.name)).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    OpError::InvalidArguments(format!("Provider {} nicht gefunden", plan.name))
                } else {
                    OpError::Execution(format!("Loeschen fehlgeschlagen: {}", error.kind()))
                }
            })?;
            if let Some(live) = live {
                let name = plan.name.clone();
                live.update(move |config| {
                    config.providers.remove(&name);
                });
            }
            Ok(ProviderApplied {
                action: plan.action,
                name: plan.name.clone(),
                key_stored: false,
                note: "Provider entfernt; ein versiegelter Schluessel bleibt im Store.".to_owned(),
            })
        }
        ProviderChange::Enable { .. } | ProviderChange::Disable { .. } => {
            let enabled = matches!(plan.change, ProviderChange::Enable { .. });
            set_enabled_in_file(&provider_file(profile_dir, &plan.name), enabled)?;
            if let Some(live) = live {
                let name = plan.name.clone();
                live.update(move |config| {
                    if let Some(provider) = config.providers.get_mut(&name) {
                        provider.enabled = enabled;
                    }
                });
            }
            Ok(ProviderApplied {
                action: plan.action,
                name: plan.name.clone(),
                key_stored: false,
                note: "gilt ab der naechsten Session.".to_owned(),
            })
        }
    }
}

/// `/provider catalog` — bekannte Provider zur Auswahl (Adresse fest, kein Schluessel).
fn handle_catalog(ctx: &OpContext) -> OpOutput {
    let config = resolved_config(ctx).ok();
    let home_dir = crate::config_util::bound_home(ctx)
        .ok()
        .map(|home| home.profile_dir.clone());
    let taken = |name: &str| -> bool {
        config.as_ref().is_some_and(|config| {
            config
                .providers
                .iter()
                .any(|(key, p)| key.eq_ignore_ascii_case(name) || p.name.eq_ignore_ascii_case(name))
        }) || home_dir
            .as_ref()
            .is_some_and(|dir| provider_file(dir, name).exists())
    };
    let mut providers = known_providers();
    let mut text = String::from(
        "Bekannte Provider (Adresse fest). Anlegen: /provider add <id> [--auth api-key|oauth]\n",
    );
    for known in &mut providers {
        known.api_key_taken = taken(&known.default_name_api_key);
        known.oauth_taken = known
            .default_name_oauth
            .as_deref()
            .is_some_and(|name| taken(name));
        let mut line = format!(
            "  {} ({}) {} [api-key: {}{}]",
            known.id,
            known.display_name,
            known.base_url,
            known.default_name_api_key,
            if known.api_key_taken { ", vorhanden" } else { "" },
        );
        if let Some(name) = &known.default_name_oauth {
            line.push_str(&format!(
                " [oauth: {name}{}]",
                if known.oauth_taken { ", vorhanden" } else { "" }
            ));
        }
        text.push_str(&line);
        text.push('\n');
    }
    OpOutput {
        text,
        data: Some(serde_json::json!({ "providers": providers })),
    }
}

/// Schreibende Unterbefehle (`add`, `add-custom`, `remove`, `enable`, `disable`).
///
/// Remote (Dienst [`RemoteManage`] im Kontext): planen, DNS pruefen, hinter Step-up parken.
/// Remote ohne Dienst bei gesetztem Tenant: abgelehnt (R9). Lokal: planen und anwenden.
async fn handle_manage(ctx: &OpContext, sub: &str, rest: &[String]) -> Result<OpOutput, OpError> {
    let remote = ctx.service::<Arc<RemoteManage>>();
    let remote_call = ctx.tenant().is_some() || ctx.service::<RemoteInvocation>().is_some();
    if remote.is_none() && remote_call {
        return Err(OpError::NotAvailable(
            "Provider-Aenderungen vom Handy brauchen Step-up".to_owned(),
        ));
    }
    let change = parse_manage_command(sub, rest)?;
    let profile_dir = crate::config_util::bound_home(ctx)?.profile_dir.clone();
    let config = resolved_config(ctx)?;
    if let Some(remote) = remote {
        let plan = plan_change(
            &config,
            &profile_dir,
            change,
            PlanOrigin::Remote,
            Some(remote.secrets.as_ref()),
        )?;
        guard_plan(&plan, remote.resolver.as_ref()).await?;
        let text = plan.canonical_text.clone();
        let action = plan.action;
        let name = plan.name.clone();
        let staged = remote.stepup.stage(plan).map_err(|_| {
            OpError::Execution("Step-up konnte nicht angelegt werden".to_owned())
        })?;
        return Ok(OpOutput {
            text,
            data: Some(serde_json::json!({
                "effect": "staged",
                "stepup_id": staged.stepup_id,
                "expires_at": staged.expires_at,
                "action": action,
                "provider": name,
            })),
        });
    }
    let plan = plan_change(&config, &profile_dir, change, PlanOrigin::Local, None)?;
    let live = ctx.service::<crate::live_config::SharedLiveConfig>();
    let applied = apply_plan(&profile_dir, &plan, None, live)?;
    Ok(OpOutput {
        text: format!("{}: {} ({})", applied.action, applied.name, applied.note),
        data: Some(serde_json::json!({ "effect": "applied", "applied": applied })),
    })
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

    /// Without a config service the configuration is discovered in the root
    /// space bound to the session, not in `HARW_HOME`.
    #[test]
    fn resolved_config_discovers_from_the_bound_home() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(
            dir.path().join("config.toml"),
            "default_model = \"bound\"\n",
        )
        .map_err(ctx("seed bound root config"))?;
        let mut services = ServiceMap::new();
        services.insert(crate::config_util::test_home_context(dir.path())?);
        let context = crate::knowledge_test_support::op_context(services)?;

        let resolved =
            super::resolved_config(&context).map_err(ctx("bound config must resolve"))?;

        assert_eq!(resolved.harness.default_model.as_deref(), Some("bound"));
        Ok(())
    }

    /// Neither a config service nor a bound root space: fail closed instead
    /// of falling back to the process's root space.
    #[test]
    fn resolved_config_without_config_or_bound_home_is_not_available() -> TestResult {
        let (ctx, _tmp) = make_test_ctx(None, None)?;

        let result = super::resolved_config(&ctx);

        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
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

/// Abnahme-Tests der Verwaltung (Vertrag K-W3b 2.9). Nur synthetische Werte.
#[cfg(test)]
mod manage_tests {
    use super::{
        OAUTH_ROUTES, PlanOrigin, ProviderApplied, ProviderArgs, ProviderAuthKind,
        ProviderChange, ProviderPlan, ProviderStepUpPort, RemoteManage, StagedKeyInfo,
        StagedKeyShape, StagedSecretPort, StagedStepUp, apply_plan, guard_plan, known_provider,
        known_providers, parse_manage_command, plan_change,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::operation::{
        RemoteCap, RemoteGrant, RemoteInvocation, RemoteSlice,
    };
    use harw_operations::{OpContext, OpError, PermissionTier, context::ServiceMap};
    use harw_provider_http::url_guard::HostResolver;
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::path::Path;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const FAKE_REF: &str = "00000000-0000-0000-0000-000000000001";

    /// Fake-Schluesselablage: kennt Griffe, liefert nie einen Wert, zaehlt `commit`/`rollback`.
    #[derive(Default)]
    struct FakeSecrets {
        infos: Mutex<HashMap<String, StagedKeyInfo>>,
        commits: AtomicUsize,
        rollbacks: AtomicUsize,
    }

    impl FakeSecrets {
        fn with(handle: &str, key_name: &str, for_provider: &str, shape: StagedKeyShape) -> Self {
            let fake = Self::default();
            fake.add(handle, key_name, for_provider, shape);
            fake
        }

        fn add(&self, handle: &str, key_name: &str, for_provider: &str, shape: StagedKeyShape) {
            if let Ok(mut map) = self.infos.lock() {
                map.insert(
                    handle.to_owned(),
                    StagedKeyInfo {
                        key_name: key_name.to_owned(),
                        for_provider: for_provider.to_owned(),
                        shape,
                    },
                );
            }
        }

        fn commits(&self) -> usize {
            self.commits.load(Ordering::SeqCst)
        }

        fn rollbacks(&self) -> usize {
            self.rollbacks.load(Ordering::SeqCst)
        }
    }

    impl StagedSecretPort for FakeSecrets {
        fn describe(&self, handle: &str, _for_provider: &str) -> Result<StagedKeyInfo, String> {
            self.infos
                .lock()
                .map_err(|_| "gesperrt".to_owned())?
                .get(handle)
                .cloned()
                .ok_or_else(|| "unbekannt".to_owned())
        }

        fn commit(
            &self,
            _handle: &str,
            _for_provider: &str,
        ) -> Result<harw_config::SecretRef, String> {
            self.commits.fetch_add(1, Ordering::SeqCst);
            Ok(harw_config::SecretRef::Secrets(FAKE_REF.to_owned()))
        }

        fn rollback(&self, _reference: &harw_config::SecretRef) {
            self.rollbacks.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Fake-Resolver: jeder Host zeigt auf die festen Adressen.
    struct FakeResolver(Vec<SocketAddr>);

    impl HostResolver for FakeResolver {
        fn resolve<'a>(
            &'a self,
            _host: &'a str,
            _port: u16,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>> {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    /// Fake-Step-up: merkt sich die geparkten Plaene.
    #[derive(Default)]
    struct FakeStepUp {
        staged: Mutex<Vec<ProviderPlan>>,
    }

    impl ProviderStepUpPort for FakeStepUp {
        fn stage(&self, plan: ProviderPlan) -> Result<StagedStepUp, String> {
            self.staged
                .lock()
                .map_err(|_| "gesperrt".to_owned())?
                .push(plan);
            Ok(StagedStepUp {
                stepup_id: "stepup-test-1".to_owned(),
                expires_at: "2000-01-01T00:00:00Z".to_owned(),
            })
        }
    }

    fn change(sub: &str, tokens: &[&str]) -> TestResult<ProviderChange> {
        parse_manage_command(sub, &toks(tokens)).map_err(ctx("parse_manage_command"))
    }

    fn config() -> harw_config::ResolvedConfig {
        harw_config::ResolvedConfig::default()
    }

    fn tempdir() -> TestResult<tempfile::TempDir> {
        tempfile::tempdir().map_err(ctx("tempdir"))
    }

    fn file(dir: &Path, name: &str) -> std::path::PathBuf {
        dir.join("providers").join(format!("{name}.toml"))
    }

    fn read_provider(dir: &Path, name: &str) -> TestResult<harw_config::ProviderToml> {
        let text = std::fs::read_to_string(file(dir, name)).map_err(ctx("read provider file"))?;
        toml::from_str(&text).map_err(ctx("parse provider file"))
    }

    fn expect_err(result: Result<ProviderPlan, OpError>, what: &'static str) -> TestResult {
        match result {
            Err(OpError::InvalidArguments(_)) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "{what}: erwartet InvalidArguments, erhalten {:?}",
                other.map(|p| p.name)
            ))),
        }
    }

    /// Kontext mit optionalem Tenant, Home, `RemoteManage` und `RemoteInvocation`.
    fn manage_ctx(
        tenant: bool,
        remote: Option<Arc<RemoteManage>>,
        bind_home: bool,
        invocation: Option<RemoteInvocation>,
    ) -> TestResult<(OpContext, tempfile::TempDir)> {
        let tmp = tempdir()?;
        std::fs::create_dir_all(tmp.path().join("ws")).map_err(ctx("create ws"))?;
        let registry = WorkspaceRegistry::build(
            tmp.path(),
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
        services.insert(Arc::new(config()));
        if bind_home {
            let home = crate::config_util::test_home_context(&tmp.path().join("home"))?;
            services.insert(home);
        }
        if let Some(remote) = remote {
            services.insert(remote);
        }
        if let Some(invocation) = invocation {
            services.insert(invocation);
        }
        let mut op_ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        if tenant {
            op_ctx = op_ctx.with_tenant(TenantId::from_str("tenant-test"));
        }
        Ok((op_ctx, tmp))
    }

    #[test]
    fn known_providers_lists_claude_and_openai_with_both_auth_kinds() -> TestResult {
        let all = known_providers();
        for (id, key, oauth) in [
            ("anthropic", "anthropic-key", "anthropic-oauth"),
            ("openai", "openai-key", "openai-oauth"),
        ] {
            let known = all
                .iter()
                .find(|p| p.id == id)
                .ok_or(TestError::Missing("bekannter Provider"))?;
            assert_eq!(
                known.auth_kinds,
                vec![ProviderAuthKind::ApiKey, ProviderAuthKind::Oauth]
            );
            assert_eq!(known.default_name_api_key, key);
            assert_eq!(known.default_name_oauth.as_deref(), Some(oauth));
        }
        assert!(
            all.iter().all(|p| !p.base_url.contains(['<', '>'])),
            "kein Platzhalter in einer Adresse"
        );
        assert!(all.iter().all(|p| p.base_url.starts_with("https://")));
        let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "nach id sortiert");
        Ok(())
    }

    #[test]
    fn add_known_ignores_any_url_override() {
        for flag in ["--base-url", "--api"] {
            let result = parse_manage_command(
                "add",
                &toks(&["anthropic", flag, "https://capture.example.test"]),
            );
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "{flag} muss bei add abgelehnt werden"
            );
        }
    }

    #[test]
    fn parse_rejects_duplicate_and_conflicting_flags() {
        for (sub, tokens) in [
            ("add", vec!["anthropic", "--key", "h", "--key", "h2"]),
            (
                "add-custom",
                vec![
                    "x", "--api", "openai-chat", "--base-url", "https://a.example.test",
                    "--key", "h", "--auth", "env:X",
                ],
            ),
            (
                "add-custom",
                vec![
                    "x", "--api", "openai-chat", "--base-url", "https://a.example.test",
                    "--key", "h", "--no-auth",
                ],
            ),
            ("remove", vec![]),
            ("remove", vec!["a", "b"]),
        ] {
            let tokens: Vec<String> = tokens.into_iter().map(str::to_owned).collect();
            assert!(
                matches!(
                    parse_manage_command(sub, &tokens),
                    Err(OpError::InvalidArguments(_))
                ),
                "{sub} {tokens:?}"
            );
        }
    }

    #[test]
    fn add_known_writes_catalog_url() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::with(
            "h1",
            "anthropic-key-secret",
            "anthropic-key",
            StagedKeyShape::AnthropicApiKey,
        );
        let plan = plan_change(
            &config(),
            tmp.path(),
            change("add", &["anthropic", "--key", "h1"])?,
            PlanOrigin::Local,
            Some(&fake),
        )
        .map_err(ctx("plan"))?;
        let applied = apply_plan(tmp.path(), &plan, Some(&fake), None).map_err(ctx("apply"))?;
        assert!(applied.key_stored);
        let written = read_provider(tmp.path(), "anthropic-key")?;
        assert_eq!(written.base_url, "https://api.anthropic.com/v1");
        assert_eq!(written.api, "anthropic-messages");
        assert_eq!(
            written.auth,
            Some(harw_config::SecretRef::Secrets(FAKE_REF.to_owned()))
        );
        assert!(written.api_key.is_none(), "nie ein Klartext-Schluessel");
        Ok(())
    }

    #[test]
    fn api_key_and_oauth_coexist_as_two_entries() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::default();
        fake.add("h-key", "k1", "anthropic-key", StagedKeyShape::AnthropicApiKey);
        fake.add(
            "h-oauth",
            "k2",
            "anthropic-oauth",
            StagedKeyShape::AnthropicOAuthToken,
        );
        let cfg = config();
        let first = plan_change(
            &cfg,
            tmp.path(),
            change("add", &["anthropic", "--key", "h-key"])?,
            PlanOrigin::Local,
            Some(&fake),
        )
        .map_err(ctx("plan key"))?;
        let second = plan_change(
            &cfg,
            tmp.path(),
            change("add", &["anthropic", "--auth", "oauth", "--key", "h-oauth"])?,
            PlanOrigin::Local,
            Some(&fake),
        )
        .map_err(ctx("plan oauth"))?;
        apply_plan(tmp.path(), &first, Some(&fake), None).map_err(ctx("apply key"))?;
        apply_plan(tmp.path(), &second, Some(&fake), None).map_err(ctx("apply oauth"))?;
        assert_eq!(first.name, "anthropic-key");
        assert_eq!(second.name, "anthropic-oauth");
        assert!(file(tmp.path(), "anthropic-key").is_file());
        assert!(file(tmp.path(), "anthropic-oauth").is_file());
        assert_eq!(
            read_provider(tmp.path(), "anthropic-oauth")?.base_url,
            "https://api.anthropic.com/v1"
        );
        Ok(())
    }

    #[test]
    fn linked_account_route_uses_fixed_chatgpt_plan_target_without_key() -> TestResult {
        let tmp = tempdir()?;
        let plan = plan_change(
            &config(),
            tmp.path(),
            change("add", &["openai", "--auth", "oauth"])?,
            PlanOrigin::Remote,
            None,
        )
        .map_err(ctx("plan"))?;
        let provider = plan.toml.ok_or(TestError::Missing("toml"))?;
        assert_eq!(provider.api, super::CHATGPT_PLAN_API);
        assert_eq!(provider.base_url, super::CHATGPT_PLAN_BASE_URL);
        assert!(provider.auth.is_none());
        assert!(plan.key_name.is_none());
        assert!(plan.canonical_text.contains("verknuepftes Konto"));
        // Mit Schluessel ist die Verknuepfung ein Fehler.
        expect_err(
            plan_change(
                &config(),
                tmp.path(),
                change("add", &["openai", "--auth", "oauth", "--key", "h"])?,
                PlanOrigin::Remote,
                None,
            ),
            "verknuepftes Konto mit Schluessel",
        )
    }

    #[test]
    fn remote_add_never_overwrites_existing_provider() -> TestResult {
        let tmp = tempdir()?;
        std::fs::create_dir_all(tmp.path().join("providers")).map_err(ctx("mkdir"))?;
        let path = file(tmp.path(), "anthropic-key");
        std::fs::write(&path, b"# vorhanden\n").map_err(ctx("seed"))?;
        let fake = FakeSecrets::with(
            "h1",
            "k",
            "anthropic-key",
            StagedKeyShape::AnthropicApiKey,
        );
        expect_err(
            plan_change(
                &config(),
                tmp.path(),
                change("add", &["anthropic", "--key", "h1"])?,
                PlanOrigin::Remote,
                Some(&fake),
            ),
            "vorhandene Datei",
        )?;
        assert_eq!(
            std::fs::read(&path).map_err(ctx("reread"))?,
            b"# vorhanden\n"
        );
        assert_eq!(fake.commits(), 0, "commit nie gerufen");

        // Auch ein Provider nur in der Config (ohne Datei) blockiert den Namen.
        let other = tempdir()?;
        let mut cfg = config();
        let toml = super::new_provider_toml("anthropic-key", "anthropic-messages", "https://a.example.test", None, vec![], false);
        cfg.providers.insert("anthropic-key".to_owned(), toml);
        expect_err(
            plan_change(
                &cfg,
                other.path(),
                change("add", &["anthropic", "--key", "h1"])?,
                PlanOrigin::Remote,
                Some(&fake),
            ),
            "Provider nur in config",
        )?;
        assert_eq!(fake.commits(), 0);
        Ok(())
    }

    #[test]
    fn remote_rejects_existing_secret_reference() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::default();
        let parsed = change(
            "add-custom",
            &[
                "x",
                "--api",
                "openai-chat",
                "--base-url",
                "https://api.example.test",
                "--auth",
                "secrets:00000000-0000-0000-0000-000000000000",
            ],
        )?;
        expect_err(
            plan_change(&config(), tmp.path(), parsed, PlanOrigin::Remote, Some(&fake)),
            "bestehende Referenz remote",
        )?;
        assert_eq!(fake.commits(), 0);
        Ok(())
    }

    #[test]
    fn remote_rejects_http_and_private_literal() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "k", "x", StagedKeyShape::OpenAiStyleKey);
        for url in [
            "http://api.example.test",
            "https://127.0.0.1",
            "https://10.0.0.1",
            "https://[::1]",
        ] {
            let parsed = change(
                "add-custom",
                &["x", "--api", "openai-chat", "--base-url", url, "--key", "h"],
            )?;
            expect_err(
                plan_change(&config(), tmp.path(), parsed, PlanOrigin::Remote, Some(&fake)),
                "unzulaessige Adresse",
            )?;
        }
        assert_eq!(fake.commits(), 0);
        assert!(!file(tmp.path(), "x").exists());
        Ok(())
    }

    #[test]
    fn remote_rejects_local_only_options_and_bad_api() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "k", "x", StagedKeyShape::OpenAiStyleKey);
        for tokens in [
            vec![
                "x", "--api", "openai-chat", "--base-url", "https://api.example.test",
                "--key", "h", "--allow-insecure-lan",
            ],
            vec![
                "x", "--api", "ollama", "--base-url", "https://api.example.test", "--key", "h",
            ],
            vec![
                "x", "--api", "openai-chat", "--base-url", "https://api.example.test",
            ],
        ] {
            let tokens: Vec<String> = tokens.into_iter().map(str::to_owned).collect();
            let parsed = parse_manage_command("add-custom", &tokens).map_err(ctx("parse"))?;
            expect_err(
                plan_change(&config(), tmp.path(), parsed, PlanOrigin::Remote, Some(&fake)),
                "lokale Option oder API remote",
            )?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn key_never_reaches_capture_server() -> TestResult {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(ctx("bind capture"))?;
        let port = listener.local_addr().map_err(ctx("local_addr"))?.port();
        let resolver = FakeResolver(vec![SocketAddr::from(([127, 0, 0, 1], port))]);
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "evil-key", "evil", StagedKeyShape::OpenAiStyleKey);
        let plan = plan_change(
            &config(),
            tmp.path(),
            change(
                "add-custom",
                &[
                    "evil", "--api", "openai-chat", "--base-url",
                    "https://capture.example.test/v1", "--key", "h",
                ],
            )?,
            PlanOrigin::Remote,
            Some(&fake),
        )
        .map_err(ctx("plan (nur Syntax)"))?;
        let guarded = guard_plan(&plan, &resolver).await;
        assert!(
            matches!(guarded, Err(OpError::InvalidArguments(_))),
            "Loopback-Aufloesung muss abgelehnt werden"
        );
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "Auffang-Server darf keine Verbindung sehen");
        assert_eq!(fake.commits(), 0, "Schluessel nie versiegelt");
        assert!(!file(tmp.path(), "evil").exists());
        Ok(())
    }

    #[tokio::test]
    async fn guard_plan_error_text_has_neither_url_nor_address() -> TestResult {
        let resolver = FakeResolver(vec![SocketAddr::from(([10, 0, 0, 5], 443))]);
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "k", "x", StagedKeyShape::OpenAiStyleKey);
        let plan = plan_change(
            &config(),
            tmp.path(),
            change(
                "add-custom",
                &[
                    "x", "--api", "openai-chat", "--base-url",
                    "https://api.example.test/geheim-pfad", "--key", "h",
                ],
            )?,
            PlanOrigin::Remote,
            Some(&fake),
        )
        .map_err(ctx("plan"))?;
        let err = guard_plan(&plan, &resolver)
            .await
            .err()
            .ok_or(TestError::Missing("Guard-Fehler"))?;
        let text = err.to_string();
        assert!(!text.contains("10.0.0.5"), "{text}");
        assert!(!text.contains("geheim-pfad"), "{text}");
        Ok(())
    }

    #[tokio::test]
    async fn guard_plan_passes_known_and_local_plans_without_dns() -> TestResult {
        let resolver = FakeResolver(Vec::new());
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "k", "anthropic-key", StagedKeyShape::AnthropicApiKey);
        let plan = plan_change(
            &config(),
            tmp.path(),
            change("add", &["anthropic", "--key", "h"])?,
            PlanOrigin::Remote,
            Some(&fake),
        )
        .map_err(ctx("plan"))?;
        guard_plan(&plan, &resolver).await.map_err(ctx("guard known"))
    }

    #[test]
    fn oauth_token_only_to_anthropic_oauth() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::default();
        fake.add("h", "k", "anthropic-key", StagedKeyShape::AnthropicOAuthToken);
        // als api-key abgelehnt
        expect_err(
            plan_change(
                &config(),
                tmp.path(),
                change("add", &["anthropic", "--auth", "api-key", "--key", "h"])?,
                PlanOrigin::Remote,
                Some(&fake),
            ),
            "OAuth-Token als api-key",
        )?;
        // an einen eigenen Provider abgelehnt
        fake.add("hc", "k", "mine", StagedKeyShape::AnthropicOAuthToken);
        expect_err(
            plan_change(
                &config(),
                tmp.path(),
                change(
                    "add-custom",
                    &[
                        "mine", "--api", "anthropic-messages", "--base-url",
                        "https://api.example.test", "--key", "hc",
                    ],
                )?,
                PlanOrigin::Remote,
                Some(&fake),
            ),
            "OAuth-Token an eigenen Provider",
        )?;
        // an anthropic-oauth erlaubt
        fake.add(
            "ho",
            "k",
            "anthropic-oauth",
            StagedKeyShape::AnthropicOAuthToken,
        );
        plan_change(
            &config(),
            tmp.path(),
            change("add", &["anthropic", "--auth", "oauth", "--key", "ho"])?,
            PlanOrigin::Remote,
            Some(&fake),
        )
        .map(|_| ())
        .map_err(ctx("oauth erlaubt"))?;
        // ein normaler Schluessel ist fuer oauth falsch
        fake.add(
            "hn",
            "k",
            "anthropic-oauth",
            StagedKeyShape::AnthropicApiKey,
        );
        expect_err(
            plan_change(
                &config(),
                tmp.path(),
                change("add", &["anthropic", "--auth", "oauth", "--key", "hn"])?,
                PlanOrigin::Remote,
                Some(&fake),
            ),
            "kein Setup-Token fuer oauth",
        )
    }

    #[test]
    fn oauth_route_matches_catalog_url() -> TestResult {
        let route = OAUTH_ROUTES
            .iter()
            .find(|r| r.catalog_id == "anthropic")
            .ok_or(TestError::Missing("anthropic route"))?;
        let known = known_provider("anthropic").ok_or(TestError::Missing("anthropic"))?;
        assert_eq!(route.base_url, known.base_url);
        assert_eq!(route.api, known.api);
        Ok(())
    }

    // T11 (`chatgpt_constants_match_provider_http`) braucht Wiring W5
    // (`pub use chatgpt_plan::{API as CHATGPT_PLAN_API, BASE_URL as CHATGPT_PLAN_BASE_URL};`
    // in harw-provider-http/src/lib.rs) und `pub const API/BASE_URL` in `chatgpt_plan.rs`.
    // Nach dem Wiring `cfg(any())` entfernen.
    #[cfg(any())]
    #[test]
    fn chatgpt_constants_match_provider_http() {
        assert_eq!(super::CHATGPT_PLAN_API, harw_provider_http::CHATGPT_PLAN_API);
        assert_eq!(
            super::CHATGPT_PLAN_BASE_URL,
            harw_provider_http::CHATGPT_PLAN_BASE_URL
        );
    }

    #[test]
    fn canonical_text_shows_full_url_and_key_name() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "mein-schluessel", "mine", StagedKeyShape::OpenAiStyleKey);
        let plan = plan_change(
            &config(),
            tmp.path(),
            change(
                "add-custom",
                &[
                    "mine", "--api", "openai-chat", "--base-url",
                    "https://api.example.test/v1/sub", "--key", "h",
                ],
            )?,
            PlanOrigin::Remote,
            Some(&fake),
        )
        .map_err(ctx("plan"))?;
        let expected = [
            "harw step-up v1",
            "aktion: provider.add",
            "provider: mine",
            "quelle: eigen",
            "api: openai-chat",
            "adresse: https://api.example.test/v1/sub",
            "anmeldung: api-key",
            "schluessel: neu \"mein-schluessel\" (nur schreiben)",
            "modelle: 0",
            "aktiv: ja",
        ]
        .join("\n");
        assert_eq!(plan.canonical_text, expected);
        Ok(())
    }

    #[test]
    fn canonical_text_is_deterministic() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "k", "anthropic-key", StagedKeyShape::AnthropicApiKey);
        let make = || {
            plan_change(
                &config(),
                tmp.path(),
                change("add", &["anthropic", "--key", "h"])?,
                PlanOrigin::Remote,
                Some(&fake),
            )
            .map_err(ctx("plan"))
        };
        let a = make()?;
        let b = make()?;
        assert_eq!(a.canonical_text, b.canonical_text);
        assert!(a.canonical_text.contains("https://api.anthropic.com/v1"));
        Ok(())
    }

    #[test]
    fn canonical_text_rejects_control_characters() {
        assert!(super::canonical_simple_text("provider.remove", "a", "https://x\nevil").is_err());
        assert!(super::canonical_simple_text("provider.remove", "a", "https://x.example.test").is_ok());
    }

    #[tokio::test]
    async fn remote_tenant_without_remote_service_fails_closed() -> TestResult {
        let (op_ctx, tmp) = manage_ctx(true, None, true, None)?;
        for cmd in [
            "add anthropic --key h",
            "remove anthropic-key",
            "enable anthropic-key",
        ] {
            let result = super::provider(
                &op_ctx,
                ProviderArgs {
                    cmd: Some(cmd.to_owned()),
                },
            )
            .await;
            assert!(
                matches!(result, Err(OpError::NotAvailable(_))),
                "{cmd}: {result:?}"
            );
        }
        assert!(!tmp.path().join("home").join("providers").exists());
        Ok(())
    }

    #[tokio::test]
    async fn remote_add_returns_staged_effect() -> TestResult {
        let stepup = Arc::new(FakeStepUp::default());
        let secrets = Arc::new(FakeSecrets::with(
            "h1",
            "anthropic-key-secret",
            "anthropic-key",
            StagedKeyShape::AnthropicApiKey,
        ));
        let remote = Arc::new(RemoteManage {
            stepup: Arc::clone(&stepup) as Arc<dyn ProviderStepUpPort>,
            secrets: Arc::clone(&secrets) as Arc<dyn StagedSecretPort>,
            resolver: Arc::new(FakeResolver(Vec::new())),
        });
        let (op_ctx, _tmp) = manage_ctx(true, Some(remote), true, None)?;
        let profile_dir = crate::config_util::bound_home(&op_ctx)
            .map_err(ctx("bound_home"))?
            .profile_dir
            .clone();
        let out = super::provider(
            &op_ctx,
            ProviderArgs {
                cmd: Some("add anthropic --key h1".to_owned()),
            },
        )
        .await
        .map_err(ctx("provider add"))?;
        let data = out.data.ok_or(TestError::Missing("data"))?;
        assert_eq!(data["effect"], "staged");
        assert_eq!(data["stepup_id"], "stepup-test-1");
        assert_eq!(data["provider"], "anthropic-key");
        assert!(out.text.contains("adresse: https://api.anthropic.com/v1"));
        assert!(!file(&profile_dir, "anthropic-key").exists(), "nichts geschrieben");
        assert_eq!(secrets.commits(), 0, "Siegeln erst beim bestaetigten apply");
        let staged = stepup.staged.lock().map_err(|_| TestError::Missing("lock"))?;
        assert_eq!(staged.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn remote_custom_to_private_address_is_not_staged() -> TestResult {
        let stepup = Arc::new(FakeStepUp::default());
        let secrets = Arc::new(FakeSecrets::with(
            "h1",
            "k",
            "evil",
            StagedKeyShape::OpenAiStyleKey,
        ));
        let remote = Arc::new(RemoteManage {
            stepup: Arc::clone(&stepup) as Arc<dyn ProviderStepUpPort>,
            secrets: Arc::clone(&secrets) as Arc<dyn StagedSecretPort>,
            resolver: Arc::new(FakeResolver(vec![SocketAddr::from(([192, 168, 1, 10], 443))])),
        });
        let (op_ctx, _tmp) = manage_ctx(true, Some(remote), true, None)?;
        let result = super::provider(
            &op_ctx,
            ProviderArgs {
                cmd: Some(
                    "add-custom evil --api openai-chat --base-url https://api.example.test/v1 --key h1"
                        .to_owned(),
                ),
            },
        )
        .await;
        assert!(matches!(result, Err(OpError::InvalidArguments(_))), "{result:?}");
        let staged = stepup.staged.lock().map_err(|_| TestError::Missing("lock"))?;
        assert!(staged.is_empty(), "nie geparkt");
        assert_eq!(secrets.commits(), 0);
        Ok(())
    }

    #[test]
    fn apply_rolls_back_secret_on_write_failure() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "k", "anthropic-key", StagedKeyShape::AnthropicApiKey);
        let plan = plan_change(
            &config(),
            tmp.path(),
            change("add", &["anthropic", "--key", "h"])?,
            PlanOrigin::Remote,
            Some(&fake),
        )
        .map_err(ctx("plan"))?;
        // Rennen: Das Ziel entsteht zwischen Plan und Apply.
        std::fs::create_dir_all(tmp.path().join("providers")).map_err(ctx("mkdir"))?;
        let path = file(tmp.path(), "anthropic-key");
        std::fs::write(&path, b"# fremd\n").map_err(ctx("race"))?;
        let result = apply_plan(tmp.path(), &plan, Some(&fake), None);
        assert!(matches!(result, Err(OpError::InvalidArguments(_))), "{result:?}");
        assert_eq!(fake.commits(), 1);
        assert_eq!(fake.rollbacks(), 1, "rollback genau einmal");
        assert_eq!(std::fs::read(&path).map_err(ctx("reread"))?, b"# fremd\n");
        // Keine Temp-Reste.
        let leftovers = std::fs::read_dir(tmp.path().join("providers"))
            .map_err(ctx("readdir"))?
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
        Ok(())
    }

    #[test]
    fn secret_bound_to_other_provider_is_rejected() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::with("h", "k", "other", StagedKeyShape::AnthropicApiKey);
        expect_err(
            plan_change(
                &config(),
                tmp.path(),
                change("add", &["anthropic", "--key", "h"])?,
                PlanOrigin::Remote,
                Some(&fake),
            ),
            "Schluessel fuer anderen Provider",
        )?;
        assert_eq!(fake.commits(), 0);
        Ok(())
    }

    #[test]
    fn unknown_handle_and_missing_port_are_rejected_without_echo() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::default();
        let result = plan_change(
            &config(),
            tmp.path(),
            change("add", &["anthropic", "--key", "geheimer-griff"])?,
            PlanOrigin::Remote,
            Some(&fake),
        );
        match result {
            Err(OpError::InvalidArguments(msg)) => assert!(!msg.contains("geheimer-griff"), "{msg}"),
            other => return Err(TestError::Unexpected(format!("{:?}", other.map(|p| p.name)))),
        }
        expect_err(
            plan_change(
                &config(),
                tmp.path(),
                change("add", &["anthropic", "--key", "h"])?,
                PlanOrigin::Remote,
                None,
            ),
            "Ablage fehlt",
        )
    }

    #[test]
    fn unknown_catalog_id_and_bad_names_are_rejected() -> TestResult {
        let tmp = tempdir()?;
        let fake = FakeSecrets::default();
        for tokens in [
            vec!["gibt-es-nicht", "--key", "h"],
            vec!["anthropic", "--name", "../x", "--key", "h"],
            vec!["anthropic", "--auth", "none", "--key", "h"],
        ] {
            let tokens: Vec<String> = tokens.into_iter().map(str::to_owned).collect();
            let parsed = parse_manage_command("add", &tokens).map_err(ctx("parse"))?;
            expect_err(
                plan_change(&config(), tmp.path(), parsed, PlanOrigin::Remote, Some(&fake)),
                "ungueltiger Aufruf",
            )?;
        }
        // Remote-Namenslaenge.
        let long = "a".repeat(super::MAX_REMOTE_PROVIDER_NAME + 1);
        let parsed = parse_manage_command("add", &toks(&["anthropic", "--name", long.as_str(), "--key", "h"]))
            .map_err(ctx("parse"))?;
        expect_err(
            plan_change(&config(), tmp.path(), parsed, PlanOrigin::Remote, Some(&fake)),
            "zu langer Name",
        )
    }

    #[test]
    fn local_add_custom_validates_endpoint_and_secret_reference() -> TestResult {
        let tmp = tempdir()?;
        // Klartext statt Referenz: abgelehnt.
        expect_err(
            plan_change(
                &config(),
                tmp.path(),
                change(
                    "add-custom",
                    &[
                        "lokal", "--api", "openai-chat", "--base-url",
                        "https://api.example.test/v1", "--auth", "sk-test-FAKE-0000000000000000",
                    ],
                )?,
                PlanOrigin::Local,
                None,
            ),
            "Klartext-Schluessel",
        )?;
        // Referenz erlaubt.
        let plan = plan_change(
            &config(),
            tmp.path(),
            change(
                "add-custom",
                &[
                    "lokal", "--api", "openai-chat", "--base-url",
                    "https://api.example.test/v1", "--auth", "env:FAKE_TEST_KEY",
                ],
            )?,
            PlanOrigin::Local,
            None,
        )
        .map_err(ctx("plan"))?;
        let applied = apply_plan(tmp.path(), &plan, None, None).map_err(ctx("apply"))?;
        assert!(!applied.key_stored);
        assert_eq!(
            read_provider(tmp.path(), "lokal")?.auth,
            Some(harw_config::SecretRef::Env("FAKE_TEST_KEY".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn enable_disable_remove_roundtrip() -> TestResult {
        let tmp = tempdir()?;
        let plan = plan_change(
            &config(),
            tmp.path(),
            change(
                "add-custom",
                &[
                    "rt", "--api", "openai-chat", "--base-url", "https://api.example.test/v1",
                    "--no-auth",
                ],
            )?,
            PlanOrigin::Local,
            None,
        )
        .map_err(ctx("plan add"))?;
        let applied: ProviderApplied =
            apply_plan(tmp.path(), &plan, None, None).map_err(ctx("apply add"))?;
        assert_eq!(applied.action, "provider.add");
        assert!(read_provider(tmp.path(), "rt")?.enabled);

        let disable = plan_change(&config(), tmp.path(), change("disable", &["rt"])?, PlanOrigin::Remote, None)
            .map_err(ctx("plan disable"))?;
        assert!(disable.canonical_text.contains("adresse: https://api.example.test/v1"));
        apply_plan(tmp.path(), &disable, None, None).map_err(ctx("apply disable"))?;
        assert!(!read_provider(tmp.path(), "rt")?.enabled);

        let enable = plan_change(&config(), tmp.path(), change("enable", &["rt"])?, PlanOrigin::Local, None)
            .map_err(ctx("plan enable"))?;
        apply_plan(tmp.path(), &enable, None, None).map_err(ctx("apply enable"))?;
        assert!(read_provider(tmp.path(), "rt")?.enabled);

        let remove = plan_change(&config(), tmp.path(), change("remove", &["rt"])?, PlanOrigin::Local, None)
            .map_err(ctx("plan remove"))?;
        apply_plan(tmp.path(), &remove, None, None).map_err(ctx("apply remove"))?;
        assert!(!file(tmp.path(), "rt").exists());
        // Unbekannt: Fehler.
        expect_err(
            plan_change(&config(), tmp.path(), change("remove", &["rt"])?, PlanOrigin::Local, None),
            "bereits entfernt",
        )
    }

    #[test]
    fn apply_updates_live_config() -> TestResult {
        let tmp = tempdir()?;
        let live: crate::live_config::SharedLiveConfig =
            Arc::new(crate::live_config::LiveConfig::new(Arc::new(config())));
        let plan = plan_change(
            &config(),
            tmp.path(),
            change("add", &["openai", "--auth", "oauth"])?,
            PlanOrigin::Local,
            None,
        )
        .map_err(ctx("plan"))?;
        apply_plan(tmp.path(), &plan, None, Some(&live)).map_err(ctx("apply"))?;
        assert!(live.current().providers.contains_key("openai-oauth"));
        let remove = plan_change(
            &live.current(),
            tmp.path(),
            change("remove", &["openai-oauth"])?,
            PlanOrigin::Local,
            None,
        )
        .map_err(ctx("plan remove"))?;
        apply_plan(tmp.path(), &remove, None, Some(&live)).map_err(ctx("apply remove"))?;
        assert!(!live.current().providers.contains_key("openai-oauth"));
        Ok(())
    }

    #[test]
    fn catalog_output_marks_taken_names_and_lists_no_secrets() -> TestResult {
        let (op_ctx, _tmp) = manage_ctx(false, None, true, None)?;
        let out = super::handle_catalog(&op_ctx);
        assert!(out.text.contains("anthropic"));
        let data = out.data.ok_or(TestError::Missing("data"))?;
        let rendered = data.to_string();
        assert!(!rendered.contains("secrets:"));
        assert!(
            data["providers"]
                .as_array()
                .is_some_and(|list| list.iter().any(|p| p["id"] == "openai"))
        );
        Ok(())
    }

    #[test]
    fn provider_remote_policy_shape() -> TestResult {
        assert_eq!(super::REMOTE.path, "/provider");
        for form in [&[][..], &["show"], &["list"], &["catalog"]] {
            let verb = super::REMOTE
                .find_verb(&toks(form))
                .ok_or(TestError::Missing("Lese-Form"))?;
            assert_eq!(verb.cap, RemoteCap::Observe);
            assert_eq!(verb.slice, RemoteSlice::Read);
        }
        for form in [
            &["add", "anthropic"][..],
            &["add-custom", "x"],
            &["remove", "x"],
            &["enable", "x"],
            &["disable", "x"],
        ] {
            let verb = super::REMOTE
                .find_verb(&toks(form))
                .ok_or(TestError::Missing("Schreib-Form"))?;
            assert_eq!(verb.slice, RemoteSlice::Global);
            assert_eq!(verb.cap, RemoteCap::Control);
            assert!(verb.stepup);
        }
        assert!(super::REMOTE.find_verb(&toks(&["test"])).is_none());
        assert!(super::REMOTE.find_verb(&toks(&["show", "x"])).is_none());
        assert_eq!(super::REMOTE.denied_tokens, &["--allow-insecure-lan"]);
        Ok(())
    }

    fn contains_providers_dir(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let path = entry.path();
            path.is_dir()
                && (path.file_name().is_some_and(|n| n == "providers")
                    || contains_providers_dir(&path))
        })
    }

    #[tokio::test]
    async fn remote_invocation_without_remote_service_fails_closed() -> TestResult {
        let verb = super::REMOTE
            .find_verb(&toks(&["add", "anthropic"]))
            .ok_or(TestError::Missing("add-Form"))?;
        let invocation = RemoteInvocation {
            grant: RemoteGrant {
                observe: true,
                steer: true,
                control: true,
                tier: PermissionTier::Owner,
            },
            verb,
        };
        let (op_ctx, tmp) = manage_ctx(false, None, true, Some(invocation))?;
        let result = super::handle_manage(&op_ctx, "add", &toks(&["anthropic"])).await;
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
        assert!(!contains_providers_dir(tmp.path()));
        Ok(())
    }
}
