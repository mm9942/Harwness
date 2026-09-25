//! `fixed_agent` — the mini-TUI: the normal harw TUI fixed to one embedded
//! compiled agent.
//!
//! # Responsibility (R10 wave 3B)
//! A compiled agent binary (`harw-agent-runner`, `iface::tui`) starts the
//! **same** [`crate::run_tui`] event loop as the full `harw` chat, but for
//! exactly one [`harw_runtime::embedded::EmbeddedAgent`] instead of a
//! profile's configured UIA. This module is the seam between the two: it
//! builds the one-agent [`harw_runtime::RuntimeAssembly`] and carries the
//! restriction policy ([`FixedAgentOptions`]) a compiled agent needs —
//! nothing here may fork or re-implement `crate::app`/`crate::runtime_root`.
//!
//! # What is restricted, and how it is wired
//! [`run_fixed_agent`] builds the assembly, derives a
//! [`crate::runtime_root::FixedAgentUiRestrictions`] from `opts` (and the
//! embedded artifact's digest) and hands it to [`run_tui`] via
//! [`crate::runtime_root::TuiRunOptions::fixed_agent`].
//! `crate::runtime_root::build_root_runtime` applies it to the freshly
//! built [`crate::app::ChatApp`] right after construction, through three
//! builders that mirror `with_verbose_tools`'s style:
//! - [`crate::app::ChatApp::with_hidden_commands`] — fed
//!   [`hidden_command_names`] (UIA switch, agent selection, and a
//!   forward-compatible slot for definition-writing commands, plus `model`
//!   when `!opts.allow_model_switch`). Rebuilds `command_registry` with
//!   those names removed, so neither the `/`-popup nor tab-completion offer
//!   them; `crate::local_commands::intercept` additionally refuses them by
//!   name before any of its own unconditional local interceptions (a bare
//!   `/agent`, say) could otherwise run.
//! - [`crate::app::ChatApp::with_model_switch_allowlist`] — fed
//!   `opts.allowed_models` when `opts.allow_model_switch` (`None`
//!   otherwise, which is moot once `hidden_command_names` has already hidden
//!   `/model` outright). `crate::local_commands::intercept` refuses any
//!   `/model switch <target>` whose target [`is_model_switch_target_allowed`]
//!   rejects, restated over a plain slice
//!   (`crate::local_commands::model_switch_target_allowed`) so that module
//!   does not need a whole [`FixedAgentOptions`] just to check one target.
//! - [`crate::app::ChatApp::with_title_override`] — fed [`fixed_agent_title`]
//!   (agent name + short digest), rendered as the leading, highest-priority
//!   segment of `harw-tui/src/status_line.rs`'s status line.
//!
//! [`allowed_models_from_ir`] remains the shared source for both
//! `opts.allowed_models` (this module's callers, e.g.
//! `harw-agent-runner/src/iface/tui.rs`) and [`is_model_switch_target_allowed`]'s
//! test coverage.

use std::sync::Arc;

use harw_agent_dsl::ir_v2::AgentIr;
use harw_core::InMemoryStateStore;
use harw_runtime::{ModelSource, RuntimeAssembly, RuntimeSpec, RuntimeStores};

use crate::app::TuiError;
use crate::command::CommandSpec;
use crate::runtime_root::{FixedAgentUiRestrictions, TuiRunOptions, TuiSessionWiring, run_tui};

/// Restriction policy for a compiled agent's mini-TUI session.
///
/// # Description
/// Carries the three things `iface::tui` (in `harw-agent-runner`) knows
/// about the embedded manifest and the runner's own policy that the TUI
/// needs in order to present itself as fixed to one agent: what to show in
/// the title bar, whether `/model switch` may run at all, and — if it may —
/// which targets are legal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedAgentOptions {
    /// Title-bar text base (agent display name); [`fixed_agent_title`]
    /// appends the short artifact digest.
    pub title: String,
    /// Whether `/model` (and `/uia-model`) may switch the active model at
    /// all. `false` hides the command outright (see [`hidden_command_names`]).
    pub allow_model_switch: bool,
    /// `(provider, model)` pairs a `/model switch` may target when
    /// [`Self::allow_model_switch`] is `true`: the manifest's primary model
    /// followed by its fallbacks, in [`allowed_models_from_ir`] order.
    pub allowed_models: Vec<(String, String)>,
}

/// Command names hidden from a fixed-agent session, in addition to whatever
/// [`hidden_command_names`] adds for `opts`.
///
/// # Description
/// - `"agent"` — child-agent selection and management (`/agent use`,
///   `--agent`'s TUI-side counterpart); a compiled agent runs as exactly one
///   agent, so switching the root agent identity has no meaning here.
/// - `"uia-model"`, `"uia-worker-model"`, `"uia-provider"`, `"uia-effort"` —
///   the UIA axis switches. A compiled agent has no UIA to switch away from;
///   these commands would otherwise let a session silently leave the
///   manifest's pinned identity.
///
/// No operation registered in `harw-ops` today writes an agent *definition*
/// from inside the TUI (checked against every `name = "..."` in
/// `harw-ops/src/*.rs`); the "definition-writing commands" the plan asks to
/// disable therefore have no current member. The name is kept as an empty,
/// documented category — `hidden_command_names` returns exactly these plus
/// (conditionally) `"model"` — so that whichever future command writes
/// definitions only needs to be added to this list, not to a new mechanism.
const ALWAYS_HIDDEN: &[&str] = &[
    "agent",
    "uia-model",
    "uia-worker-model",
    "uia-provider",
    "uia-effort",
];

/// Commands a fixed-agent session must not expose, given `opts`.
///
/// # Description
/// [`ALWAYS_HIDDEN`] plus `"model"` when [`FixedAgentOptions::allow_model_switch`]
/// is `false` (a restricted-but-nonzero `allowed_models` list instead lets
/// `/model` through and relies on [`is_model_switch_target_allowed`] /
/// the configured model catalog to reject any other target).
#[must_use]
pub fn hidden_command_names(opts: &FixedAgentOptions) -> Vec<&'static str> {
    let mut hidden: Vec<&'static str> = ALWAYS_HIDDEN.to_vec();
    if !opts.allow_model_switch {
        hidden.push("model");
    }
    hidden
}

/// Removes every [`CommandSpec`] whose canonical name is hidden for `opts`.
///
/// # Description
/// Pure filter over an already-built command list — it does not know how to
/// build or install a [`crate::CommandRegistry`] itself. A caller with
/// access to one (see this module's doc for the exact spot in `app.rs`)
/// rebuilds it via `CommandRegistry::new(filter_command_specs(registry
/// .specs().to_vec(), opts))`. Aliases are not checked: the specs this
/// crate derives from `harw-ops` never alias one hidden command's name as
/// another visible command's alias.
///
/// # Arguments
/// - `specs`: the full catalog (operation-derived plus TUI-local specs).
/// - `opts`: the fixed-agent restriction policy.
///
/// # Returns
/// `specs`, minus every entry named in [`hidden_command_names`], in the
/// original order.
#[must_use]
pub fn filter_command_specs(specs: Vec<CommandSpec>, opts: &FixedAgentOptions) -> Vec<CommandSpec> {
    let hidden = hidden_command_names(opts);
    specs
        .into_iter()
        .filter(|spec| !hidden.contains(&spec.name.as_str()))
        .collect()
}

/// The manifest's model plus its fallbacks, in preference order.
///
/// # Description
/// The primary `(provider, model)` pair first — only if **both** are set,
/// since a `/model switch` target names a specific provider — then every
/// fallback `ModelRef` in declaration order. `models` being `None`, or
/// having neither a primary pair nor fallbacks, yields an empty list
/// (nothing to switch to; [`hidden_command_names`] should then hide
/// `/model` regardless of [`FixedAgentOptions::allow_model_switch`]).
/// Duplicate `(provider, model)` pairs are removed, keeping the first
/// occurrence, so a fallback that repeats the primary model does not appear
/// twice in a picker built from this list.
///
/// Split out from [`allowed_models_from_ir`] so it can be tested directly
/// against a [`harw_agent_dsl::ir_v2::Models`] fixture without constructing
/// a full [`AgentIr`] (whose other sections this function never reads).
#[must_use]
pub fn allowed_models_from_models(
    models: Option<&harw_agent_dsl::ir_v2::Models>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(models) = models {
        if let (Some(provider), Some(model)) = (models.provider.as_ref(), models.model.as_ref()) {
            out.push((provider.clone(), model.clone()));
        }
        for fallback in &models.fallbacks {
            out.push((fallback.provider.clone(), fallback.model.clone()));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|pair| seen.insert(pair.clone()));
    out
}

/// The manifest's model plus its fallbacks, in preference order.
///
/// # Description
/// Thin wrapper over [`allowed_models_from_models`] for the common case of
/// having a whole [`AgentIr`] (`ir.models`) rather than just its `[models]`
/// section.
#[must_use]
pub fn allowed_models_from_ir(ir: &AgentIr) -> Vec<(String, String)> {
    allowed_models_from_models(ir.models.as_ref())
}

/// Whether a `/model switch <target>` argument names one of `opts`'s
/// allowed models.
///
/// # Description
/// `false` immediately when [`FixedAgentOptions::allow_model_switch`] is
/// `false`. Otherwise `target` matches when it equals, case-insensitively,
/// either a listed model id alone (`"gpt-5"`) or a listed `provider/model`
/// pair (`"openai/gpt-5"`) — the two forms `/model switch` itself accepts
/// (`harw-ops/src/model.rs`).
#[must_use]
pub fn is_model_switch_target_allowed(target: &str, opts: &FixedAgentOptions) -> bool {
    if !opts.allow_model_switch {
        return false;
    }
    opts.allowed_models.iter().any(|(provider, model)| {
        target.eq_ignore_ascii_case(model)
            || target.eq_ignore_ascii_case(&format!("{provider}/{model}"))
    })
}

/// Short, stable digest text for a title bar: the first 12 hex characters of
/// [`harw_agent_artifact::ArtifactDigest::to_hex`].
///
/// # Description
/// 12 hex characters (48 bits) is the same truncation depth commit-style
/// short hashes commonly use; full collision safety is not the point here —
/// the full digest remains available (`--manifest`, `--verify`) for anything
/// that needs it unambiguously.
#[must_use]
pub fn short_digest(digest: &harw_agent_artifact::ArtifactDigest) -> String {
    let hex = digest.to_hex();
    hex.chars().take(12).collect()
}

/// The title-bar text for a fixed-agent session: agent name and short
/// digest.
#[must_use]
pub fn fixed_agent_title(
    opts: &FixedAgentOptions,
    digest: &harw_agent_artifact::ArtifactDigest,
) -> String {
    format!("{} · {}", opts.title, short_digest(digest))
}

/// Starts the mini-TUI for one embedded agent.
///
/// # Description
/// Builds the one-shot [`RuntimeAssembly`] for `spec` (which must carry
/// `spec.embedded` — see `harw_runtime::embedded::EmbeddedAgent`, wave 3
/// contract) and drives it through the same [`run_tui`] event loop as the
/// full `harw` chat: no `/resume` (a compiled agent's mini-TUI has no
/// session store of its own), a fresh [`TuiSessionWiring`], and no explicit
/// keybindings file override.
///
/// `opts` shapes the [`FixedAgentUiRestrictions`] applied to [`ChatApp`] via
/// [`TuiRunOptions::fixed_agent`]: which commands are hidden
/// ([`hidden_command_names`]), the status-line title
/// ([`fixed_agent_title`], falling back to `opts.title` alone when `spec`
/// carries no `embedded` artifact to digest — not expected for a real
/// `EntryKind::CompiledAgent` run, but kept total rather than panicking),
/// and the `/model switch` allowlist (`opts.allowed_models` when
/// `opts.allow_model_switch`, else `None` — `hidden_command_names` already
/// hides `/model` outright in that case, so the allowlist is moot but kept
/// `None` for clarity).
///
/// # Arguments
/// - `spec`: the `EntryKind::CompiledAgent` [`RuntimeSpec`] built by
///   `harw-agent-runner`'s `iface::tui::run` (cwd, principal, and
///   `spec.embedded` already set).
/// - `opts`: the restriction policy (title, model-switch allowance).
///
/// # Returns
/// `Ok(())` on a clean exit, mirroring [`run_tui`].
///
/// # Errors
/// [`TuiError::Core`] if the assembly cannot be built (wraps the
/// [`harw_runtime::RuntimeError`] text); otherwise whatever [`run_tui`]
/// returns.
///
/// # Concurrency
/// Blocks the calling thread, exactly like [`run_tui`].
pub fn run_fixed_agent(spec: RuntimeSpec, opts: FixedAgentOptions) -> Result<(), TuiError> {
    // Digested before `spec` moves into the builder below; `ArtifactDigest`
    // is `Copy`, so this is a cheap snapshot, not a borrow.
    let digest = spec.embedded.as_ref().map(|agent| *agent.digest());

    // A compiled agent binary has no `~/.harw`-style profile session store of
    // its own to persist transcripts into (wave 3 scope); the state store is
    // therefore in-memory for this slice. Wiring a durable store belongs to
    // whichever agent gives the mini-TUI `/resume` (out of scope here: this
    // module never passes a `TuiResume`, so `/resume` shows the existing
    // "not configured" system line).
    let stores = RuntimeStores {
        state_store: Arc::new(InMemoryStateStore::new()),
        job_store: None,
        approval_store: None,
    };

    let assembly = RuntimeAssembly::builder(spec)
        .model(ModelSource::Configured)
        .stores(stores)
        .build()
        .map_err(|error| {
            TuiError::Core(format!(
                "could not assemble the fixed-agent runtime: {error}"
            ))
        })?;

    let title = match digest.as_ref() {
        Some(digest) => fixed_agent_title(&opts, digest),
        None => opts.title.clone(),
    };
    let hidden_commands: Vec<String> = hidden_command_names(&opts)
        .into_iter()
        .map(|name| name.to_owned())
        .collect();
    let model_switch_allowlist = opts.allow_model_switch.then(|| opts.allowed_models.clone());
    let restrictions = FixedAgentUiRestrictions {
        hidden_commands,
        title,
        model_switch_allowlist,
    };

    let wiring = TuiSessionWiring::new();
    let assembly = Arc::new(assembly);
    run_tui(
        assembly,
        TuiRunOptions {
            wiring,
            resume: None,
            verbose_tools: false,
            keybindings_path: None,
            fixed_agent: Some(restrictions),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandDomain, CommandScope, OutputSurface, PermissionTier};
    use harw_agent_artifact::ArtifactDigest;
    use harw_agent_dsl::ir_v2::{ModelRef, Models};

    fn opts(allow_model_switch: bool, allowed_models: Vec<(&str, &str)>) -> FixedAgentOptions {
        FixedAgentOptions {
            title: "reviewer".to_owned(),
            allow_model_switch,
            allowed_models: allowed_models
                .into_iter()
                .map(|(p, m)| (p.to_owned(), m.to_owned()))
                .collect(),
        }
    }

    fn spec(name: &str) -> CommandSpec {
        CommandSpec::new(
            name,
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::Misc,
        )
        .expect("valid command name in test fixture")
    }

    #[test]
    fn hidden_command_names_always_hides_uia_switch_and_agent_selection() {
        let hidden = hidden_command_names(&opts(true, vec![("openai", "gpt-5")]));
        for name in ALWAYS_HIDDEN {
            assert!(hidden.contains(name), "{name} should always be hidden");
        }
        assert!(
            !hidden.contains(&"model"),
            "model switch stays visible when allowed"
        );
    }

    #[test]
    fn hidden_command_names_hides_model_when_switch_disallowed() {
        let hidden = hidden_command_names(&opts(false, Vec::new()));
        assert!(hidden.contains(&"model"));
    }

    #[test]
    fn filter_command_specs_drops_only_hidden_names() {
        let specs = vec![
            spec("agent"),
            spec("model"),
            spec("uia-model"),
            spec("uia-provider"),
            spec("uia-effort"),
            spec("uia-worker-model"),
            spec("diary"),
            spec("plan"),
        ];
        let filtered = filter_command_specs(specs, &opts(true, vec![("openai", "gpt-5")]));
        let names: Vec<&str> = filtered.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, vec!["model", "diary", "plan"]);
    }

    #[test]
    fn filter_command_specs_also_drops_model_when_switch_disallowed() {
        let specs = vec![spec("model"), spec("diary")];
        let filtered = filter_command_specs(specs, &opts(false, Vec::new()));
        let names: Vec<&str> = filtered.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, vec!["diary"]);
    }

    #[test]
    fn filter_command_specs_is_idempotent_and_preserves_order() {
        let specs = vec![spec("diary"), spec("plan"), spec("model")];
        let opts = opts(true, vec![("openai", "gpt-5")]);
        let once = filter_command_specs(specs.clone(), &opts);
        let twice = filter_command_specs(once.clone(), &opts);
        assert_eq!(
            once.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            twice.iter().map(|s| s.name.as_str()).collect::<Vec<_>>()
        );
    }

    fn models_fixture(provider: Option<(&str, &str)>, fallbacks: &[(&str, &str)]) -> Models {
        Models {
            provider: provider.map(|(p, _)| p.to_owned()),
            model: provider.map(|(_, m)| m.to_owned()),
            effort: None,
            fallbacks: fallbacks
                .iter()
                .map(|(provider, model)| ModelRef {
                    provider: (*provider).to_owned(),
                    model: (*model).to_owned(),
                })
                .collect(),
            required_env: Vec::new(),
        }
    }

    #[test]
    fn allowed_models_from_models_orders_primary_then_fallbacks() {
        let models = models_fixture(
            Some(("openai", "gpt-5")),
            &[("anthropic", "claude-sonnet-5"), ("openai", "gpt-4o")],
        );
        assert_eq!(
            allowed_models_from_models(Some(&models)),
            vec![
                ("openai".to_owned(), "gpt-5".to_owned()),
                ("anthropic".to_owned(), "claude-sonnet-5".to_owned()),
                ("openai".to_owned(), "gpt-4o".to_owned()),
            ]
        );
    }

    #[test]
    fn allowed_models_from_models_skips_partial_primary_and_dedupes() {
        let mut models = models_fixture(None, &[("openai", "gpt-5"), ("openai", "gpt-5")]);
        models.provider = Some("openai".to_owned());
        // model left None: an incomplete primary pair must not appear.
        assert_eq!(
            allowed_models_from_models(Some(&models)),
            vec![("openai".to_owned(), "gpt-5".to_owned())]
        );
    }

    #[test]
    fn allowed_models_from_models_empty_without_models_section() {
        assert!(allowed_models_from_models(None).is_empty());
    }

    #[test]
    fn is_model_switch_target_allowed_matches_bare_and_qualified_forms() {
        let policy = opts(true, vec![("openai", "gpt-5")]);
        assert!(is_model_switch_target_allowed("gpt-5", &policy));
        assert!(is_model_switch_target_allowed("GPT-5", &policy));
        assert!(is_model_switch_target_allowed("openai/gpt-5", &policy));
        assert!(!is_model_switch_target_allowed("claude-sonnet-5", &policy));
    }

    #[test]
    fn is_model_switch_target_allowed_false_when_switch_disallowed() {
        let policy = opts(false, vec![("openai", "gpt-5")]);
        assert!(!is_model_switch_target_allowed("gpt-5", &policy));
    }

    #[test]
    fn short_digest_takes_twelve_hex_characters() {
        let digest = ArtifactDigest::from_bytes([0u8; 32]);
        let short = short_digest(&digest);
        assert_eq!(short.len(), 12);
        assert!(digest.to_hex().starts_with(&short));
    }

    #[test]
    fn fixed_agent_title_combines_name_and_digest() {
        let policy = opts(true, Vec::new());
        let digest = ArtifactDigest::from_bytes([0xAB; 32]);
        let title = fixed_agent_title(&policy, &digest);
        assert!(title.starts_with("reviewer · "));
        assert!(title.ends_with(&short_digest(&digest)));
    }
}
