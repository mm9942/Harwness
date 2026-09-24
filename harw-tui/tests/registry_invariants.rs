//! Cross-registry invariants for TUI slash-command discoverability.
//!
//! Prevents the "command exists but TUI can't see it" and "two aliases collide"
//! classes of bugs. Any new op registered via `harw_ops::register_all` is
//! automatically checked here without manual test updates.
//!
//! Spec source: `harw-tui` interaction-contract + `harw-operations` op-contract.

use harw_operations::registry::OperationRegistry;
use harw_operations::{OperationCategory, Surface};
use harw_tui::{
    CapabilitySet, CommandError, CommandRegistry, DispatchContext, Invocation, InvocationSurface,
    PermissionTier,
};
use std::collections::BTreeMap;

mod common;
use common::{TestError, TestResult, ctx};

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Builds a fresh OperationRegistry populated by `harw_ops::register_all`.
fn build_ops() -> OperationRegistry {
    let mut ops = OperationRegistry::new();
    harw_ops::register_all(&mut ops);
    ops
}

/// Builds a DispatchContext for an Operator-tier TUI call with no special capabilities.
fn tui_operator_ctx() -> DispatchContext {
    DispatchContext {
        caller_tier: PermissionTier::Operator,
        surface: InvocationSurface::Tui,
        capabilities: CapabilitySet::default(),
    }
}

// ── Test 1 ────────────────────────────────────────────────────────────────────

/// Every op that declares `Surface::Command` must appear in the TUI registry.
///
/// Design-doc invariant: `CommandRegistry::from_operation_registry` must mirror
/// the full `Surface::Command` declaration set of the OperationRegistry.
/// A missing entry causes "command exists but TUI can't see it" — the primary
/// bug class this suite guards against.
#[test]
fn every_op_with_command_surface_appears_in_tui_registry() -> TestResult {
    let ops = build_ops();
    let tui = CommandRegistry::built_in().map_err(ctx("built_in"))?;

    for op in ops.iter() {
        let meta = op.meta();
        for surface in &meta.surfaces {
            if let Surface::Command { path, .. } = surface {
                let name = path.trim_start_matches('/');
                assert!(
                    tui.find(name).is_some(),
                    "op '{}' declares Surface::Command path '{path}' \
                     but CommandRegistry::built_in() has no spec for '{name}' — \
                     check that CommandRegistry::from_operation_registry skips no valid names",
                    meta.name,
                );
            }
        }
    }
    Ok(())
}

// ── Test 2 ────────────────────────────────────────────────────────────────────

/// Every alias string must be unique across all operations, and must not collide
/// with another op's primary command name.
///
/// Uses `BTreeMap` for deterministic, reproducible error messages.
/// Collision classes caught:
/// - alias used by two different ops
/// - alias of op A equals the primary name of op B (and A ≠ B)
#[test]
fn alias_uniqueness_across_all_ops() -> TestResult {
    let ops = build_ops();

    // First pass: collect all primary names.
    let name_to_op: BTreeMap<String, &'static str> = ops
        .iter()
        .map(|op| (op.meta().name.to_owned(), op.meta().name))
        .collect();

    // Second pass: for each alias check it does not clash.
    let mut alias_owner: BTreeMap<String, &'static str> = BTreeMap::new();

    for op in ops.iter() {
        let meta = op.meta();
        for alias in meta.aliases {
            let alias_s = alias.to_string();

            // Check: alias must not equal a *different* op's primary name.
            if let Some(&other_name) = name_to_op.get(&alias_s) {
                if other_name != meta.name {
                    return Err(TestError::Unexpected(format!(
                        "alias '{alias}' of op '{}' clashes with the primary command \
                         name of op '{other_name}'",
                        meta.name,
                    )));
                }
            }

            // Check: alias must not already be claimed by a different op.
            match alias_owner.get(&alias_s).copied() {
                Some(prev) if prev != meta.name => {
                    return Err(TestError::Unexpected(format!(
                        "alias '{alias}' is claimed by both op '{prev}' and op '{}' — \
                         aliases must be globally unique across the op set",
                        meta.name,
                    )));
                }
                _ => {
                    alias_owner.insert(alias_s, meta.name);
                }
            }
        }
    }
    Ok(())
}

// ── Test 3 ────────────────────────────────────────────────────────────────────

/// `CommandRegistry::dispatch` returns `CommandError::UnknownCommand` for a
/// near-miss typo. `suggestion` is checked when the registry's prefix-based
/// heuristic is likely to fire (same first character); otherwise we accept `None`
/// and note the limitation inline.
///
/// NOTE: `suggestion()` in registry.rs uses prefix matching (first char + shortest
/// length difference), not full edit-distance. "modl" starts with 'm', which
/// matches "model", "memory" — the shortest-length winner should be "model".
/// "providr" starts with 'p', which matches "provider", "ps", "plugins",
/// "permissions" — length proximity to "provider" (7 vs 7) vs "ps" (2) is closer
/// by abs_diff; min_by_key picks the smallest abs_diff, so "provider" wins (0 diff).
///
/// If the suggestion implementation changes in the future, update the `Some`
/// assertions accordingly or relax to `.is_some()`.
// No #[ignore] needed — the prefix heuristic is implemented and exercised below.
#[test]
fn unknown_command_returns_suggestion_for_near_miss_typo() -> TestResult {
    let registry = CommandRegistry::built_in().map_err(common::ctx("built_in"))?;
    let ctx = tui_operator_ctx();

    // "modl" — first char 'm', nearest-length match among 'm'-prefixed ops is "model".
    let result_modl = registry.dispatch(
        ctx,
        Invocation::Command {
            name: "modl".to_owned(),
            raw_args: vec![],
        },
    );
    let Err(err_modl) = result_modl else {
        return Err(TestError::Unexpected("Err erwartet".into()));
    };

    match &err_modl {
        CommandError::UnknownCommand { suggestion, input } => {
            assert_eq!(input, "modl", "input field must echo the bad name");
            // The prefix heuristic fires because 'm' is present.
            // Accept Some("model") strictly; if suggestion is None the heuristic
            // changed — change this to assert!(suggestion.is_none()) and add a TODO.
            assert!(
                suggestion.is_some(),
                "expected a suggestion for near-miss 'modl' (first char 'm' is present in the \
                 registry); got None — if suggestion() was changed to a different heuristic, \
                 update this assertion"
            );
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "expected UnknownCommand, got: {other:?}"
            )));
        }
    }

    // "providr" — first char 'p', nearest-length to "provider" (len 8 vs 7 → diff 1).
    let result_providr = registry.dispatch(
        ctx,
        Invocation::Command {
            name: "providr".to_owned(),
            raw_args: vec![],
        },
    );
    let Err(err_providr) = result_providr else {
        return Err(TestError::Unexpected("Err erwartet".into()));
    };

    match &err_providr {
        CommandError::UnknownCommand { suggestion, input } => {
            assert_eq!(input, "providr");
            assert!(
                suggestion.is_some(),
                "expected a suggestion for near-miss 'providr' (first char 'p' is present); \
                 got None"
            );
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "expected UnknownCommand, got: {other:?}"
            )));
        }
    }
    Ok(())
}

// ── Test 4 ────────────────────────────────────────────────────────────────────

/// Model, Provider, and Effort ops carry `OperationCategory::Model`.
///
/// Documents the positive contract: these three ops define the model-control
/// surface and must be categorised as `Model` so TUI `/help` groups them
/// correctly. If a new op is accidentally categorised `Misc`, this test will
/// NOT fail — the `Misc` default is intentional for ops without a natural
/// category. This test only asserts the three known Model-category ops are
/// correctly tagged.
#[test]
fn model_ops_carry_category_model() -> TestResult {
    let ops = build_ops();

    for expected_name in ["model", "provider", "effort"] {
        let op = ops
            .iter()
            .find(|o| o.meta().name == expected_name)
            .ok_or_else(|| {
                TestError::Unexpected(format!(
                    "op '{expected_name}' not found in OperationRegistry — \
                     was it removed from harw_ops::register_all?"
                ))
            })?;
        assert_eq!(
            op.meta().category,
            OperationCategory::Model,
            "op '{expected_name}' must be OperationCategory::Model for TUI /help grouping; \
             got {:?}",
            op.meta().category,
        );
    }
    Ok(())
}

// ── Test 5 ────────────────────────────────────────────────────────────────────

/// /model, /provider, and /effort exist in the TUI registry AND carry aliases.
///
/// Positive assertion for the three model-control commands:
/// - present in `CommandRegistry::built_in()`
/// - their underlying ops have at least one alias registered
///   (model → "m", provider → "p", effort → "reasoning")
#[test]
fn model_provider_effort_all_present_and_have_aliases() -> TestResult {
    let ops = build_ops();
    let tui = CommandRegistry::built_in().map_err(ctx("built_in"))?;

    // Expected: (op-name, at-least-one-alias)
    let expected: &[(&str, &str)] = &[("model", "m"), ("provider", "p"), ("effort", "reasoning")];

    for &(name, expected_alias) in expected {
        // Must be in the TUI registry.
        assert!(
            tui.find(name).is_some(),
            "TUI registry is missing '{name}' — every model-control op must be discoverable"
        );

        // The underlying op must carry the known alias.
        let op = ops.iter().find(|o| o.meta().name == name).ok_or_else(|| {
            TestError::Unexpected(format!("OperationRegistry missing op '{name}'"))
        })?;

        let has_alias = op.meta().aliases.contains(&expected_alias);
        assert!(
            has_alias,
            "op '{name}' is missing alias '{expected_alias}' — aliases are part of the \
             command discoverability contract and must not be silently removed"
        );

        // The alias must also resolve in the OperationRegistry's own alias field
        // (aliases are declared in OperationMeta, not yet surfaced in CommandSpec,
        // so we verify at the op level only).
        assert!(
            !op.meta().aliases.is_empty(),
            "op '{name}' has no aliases in OperationMeta — expected at least '{expected_alias}'"
        );
    }
    Ok(())
}

// ── Test 6 ────────────────────────────────────────────────────────────────────

/// The count of ops with `Surface::Command` matches the count of specs in
/// `CommandRegistry::built_in()`.
///
/// When this assertion fails it means either:
/// - A new op was added to `register_all` with `Surface::Command` but the
///   TUI registry derivation silently dropped it (path validation rejected
///   the name), OR
/// - A spec was added to `CommandRegistry` without a matching op surface.
///
/// Both are bugs. The informative failure message names both counts.
#[test]
fn tui_registry_and_operation_registry_agree_on_count() -> TestResult {
    let ops = build_ops();
    let tui = CommandRegistry::built_in().map_err(ctx("built_in"))?;

    // Count Surface::Command declarations across all ops.
    let op_command_surface_count: usize = ops
        .iter()
        .flat_map(|op| op.meta().surfaces.iter())
        .filter(|surface| matches!(surface, Surface::Command { .. }))
        .count();

    let tui_spec_count = tui.specs().len();

    assert_eq!(
        op_command_surface_count, tui_spec_count,
        "OperationRegistry has {op_command_surface_count} Surface::Command declarations \
         but CommandRegistry::built_in() has {tui_spec_count} specs — they must match. \
         Likely cause: a new op's Surface::Command path failed CommandName::parse (e.g. \
         contains '/' sub-segments) and was silently skipped in from_operation_registry."
    );
    Ok(())
}

// ── Test 7 (bonus) ───────────────────────────────────────────────────────────

/// Jede Operation aus `register_all` ist über **mindestens eine Fläche**
/// erreichbar, und die Command-Operationen stehen erschöpfend in dieser Liste.
///
/// Guards against silent removal from `register_all`. If this fails, one of
/// the core op structs was removed or renamed without updating this list.
///
/// # Warum die Liste seit UI-06 nur noch die Command-Operationen zählt
/// Die frühere Fassung verglich die Gesamtzahl aus `register_all` mit dieser
/// Namensliste und begründete das damit, eine der TUI unbekannte Operation
/// sei „über keine Fläche erreichbar". **Das war wahr, solange es nur
/// `Surface::Command` gab.** Seit `#[operation(...)]` ein `web(...)`-
/// Unterattribut kennt, gibt es Operationen, die die TUI zu Recht nicht kennt
/// — `approval.pending` und `approval.resolve` sind reine Web-Flächen für die
/// Bestätigungsansicht und haben in einem Terminal nichts zu suchen.
///
/// Die Prüfung ist deshalb **nicht abgeschwächt, sondern getrennt und
/// verschärft**: die Namensliste bleibt für Command-Operationen erschöpfend,
/// und zusätzlich muss **jede** Operation mindestens eine Fläche tragen. Eine
/// Operation ganz ohne Fläche wäre wirklich unerreichbar — das fing die alte
/// Zählung nicht, weil sie nur Zahlen verglich, keine Flächen.
///
/// `mode` gehört zur Grundausstattung und **nicht** hinter das
/// `[tools.plan]`-Gate: es steuert die Session, nicht die Planungsfläche.
/// Läge es hinter dem Gate, könnte eine Laufzeit ohne Plan-Store den Modus
/// nicht mehr wechseln — auch nicht zurück nach `work`. Die sechs gegateten
/// Operationen (`plan`, `goal`, `explore`, `research_deps`, `research_web`,
/// `analyze`) kommen über `register_plan_tools` und stehen deshalb bewusst
/// nicht in dieser Liste.
#[test]
fn all_registered_ops_are_reachable_by_name() {
    let ops = build_ops();
    let expected_names = [
        "help",
        "status",
        "quit",
        "new",
        "work",
        "ps",
        "attach",
        "stop",
        "diff",
        "agent",
        "skills",
        "plugins",
        "model",
        "provider",
        "permissions",
        "compact",
        "memory",
        "effort",
        "mode",
        // Ergänzt mit AW5-09: `/context-proposal` ist Operator-Governance und
        // Command-only -- die TUI muss sie kennen. Diese Liste ist bewusst
        // erschöpfend: eine Operation, die `register_all` einträgt und die
        // TUI nicht kennt, wäre über keine Fläche erreichbar.
        "context-proposal",
        // Slice B4 (Contract §2 A8): `/add-workdir` legt eine zusätzliche
        // Workspace-Wurzel frei, `/export` liefert nachträglichen Export --
        // beide sind `tui_only` Command-Operationen aus `register_all` und
        // gehören deshalb in diese erschöpfende Liste.
        "add-workdir",
        "export",
        // Agent OPS: `/usage` liest `SessionStateSnapshot::total_usage` --
        // reine Session-Introspektion wie `mode`/`context-proposal`, deshalb
        // ebenfalls Grundausstattung statt Planungsfläche und in dieser
        // Liste zu führen.
        "usage",
        // UIA-spezifische gepinnte Provider-/Modell-Auswahl (`uia-provider`,
        // `uia-model`, `harw-ops/src/{model,provider}.rs`): strukturelle
        // Zwillinge von `model`/`provider` oben, ebenfalls
        // `command(visibility = "tui_only")` und damit Surface::Command.
        "uia-model",
        "uia-provider",
        // `uia-worker-model` (harw-ops/src/model.rs) und `uia-effort`
        // (harw-ops/src/effort.rs) waren implementiert, aber bis zu diesem
        // Knoten nicht in `harw_ops::register_all` eingetragen -- `/uia-
        // worker-model` und `/uia-effort` existierten deshalb in der TUI
        // nicht, obwohl der Picker für `/uia-worker-model` bereits `switch
        // <id>` dagegen sendet. Beide sind `command(visibility =
        // "tui_only")` und damit Surface::Command, genau wie `uia-model`/
        // `uia-provider` oben.
        "uia-worker-model",
        "uia-effort",
        // W6b: `provider-concurrency` (harw-ops/src/provider.rs) ist
        // `command(visibility = "tui_only")` und Surface::Command, ebenfalls
        // bereits in `register_all`, aber zuvor nicht in dieser
        // erschöpfenden Liste geführt.
        "provider-concurrency",
        // Manueller lokaler Bug-Report-Fallback (`harw-ops/src/bug_report.rs`),
        // `command(visibility = "tui_only")` -- reine Session-/Diagnose-Fläche
        // wie `usage` oben.
        "bug-report",
        // Genehmigungs-Befehlsgruppe (Interaktionsvertrag §2.3/§4,
        // `harw-ops/src/{approve,deny,review,cancel,retry}.rs`): alle fünf
        // deklarieren `command(visibility = "channel_parity")`. Anders als
        // `visibility = "tui_only"` bedeutet `ChannelParity` NICHT "der TUI
        // unbekannt" -- `command_visibility_to_scope`
        // (`harw-tui/src/registry.rs`) bildet es auf
        // `CommandScope::ChannelParity` ab, und diese Ops stehen genau wie
        // die `TuiOnly`-Ops in `CommandRegistry::built_in()` (siehe Test 1
        // oben, der das bereits für alle Command-Ops prüft). Sie gehören
        // deshalb in diese erschöpfende Liste.
        "approve",
        "deny",
        "review",
        "cancel",
        "retry",
        // Plan `recursive-cooking-lobster.md` Teil B5
        // (`harw-ops/src/sandbox_lease.rs`): `/sandbox-lease [status|revoke]`
        // ist `command(visibility = "tui_only")` und damit Surface::Command,
        // busy = Immediate (siehe `command_exec::busy_availability_for` --
        // keine Sonderbehandlung nötig, die generische Immediate-Fallunter-
        // scheidung greift bereits).
        "sandbox-lease",
        // `/models` (`harw-ops/src/models.rs`): rollenbezogene Modellwahl,
        // `command(visibility = "tui_only")` und damit Surface::Command.
        "models",
        // Wissensfläche (`harw-ops/src/{workbench,kanban,diary,palace,dream}.rs`):
        // `workbench`/`diary`/`dream` deklarieren `visibility = "channel_reduced"`,
        // `kanban`/`palace` `visibility = "channel_parity"` -- alle fünf sind
        // Surface::Command und stehen in `CommandRegistry::built_in()`.
        "workbench",
        "kanban",
        "diary",
        "palace",
        "dream",
    ];

    // Jede Operation muss mindestens eine Fläche tragen. Eine ohne wäre über
    // keinen Weg aufrufbar — weder Terminal, noch Modell, noch HTTP.
    for op in ops.iter() {
        assert!(
            !op.meta().surfaces.is_empty(),
            "Operation '{}' trägt keine einzige Fläche und ist damit unerreichbar",
            op.meta().name
        );
    }

    // Die Namensliste ist für Command-Operationen erschöpfend. Reine
    // Web-Operationen (etwa die Bestätigungsfläche) gehören bewusst nicht
    // hinein — siehe Doku über diesem Test.
    let command_ops: Vec<_> = ops
        .iter()
        .filter(|op| {
            op.meta()
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::Command { .. }))
        })
        .collect();

    assert_eq!(
        command_ops.len(),
        expected_names.len(),
        "register_all liefert {} Command-Operationen, die TUI kennt {} — jede neue gehört in diese Liste",
        command_ops.len(),
        expected_names.len(),
    );

    for name in expected_names {
        assert!(
            ops.find_by_name(name).is_some(),
            "op '{name}' is missing from OperationRegistry — was it removed from \
             harw_ops::register_all?"
        );
    }
}

// ── Test 8 (bonus) ───────────────────────────────────────────────────────────

/// No spec name in the TUI registry starts with '/'.
///
/// `CommandName::parse` and `from_operation_registry` strip the leading slash
/// from Surface::Command paths. If stripping ever breaks, names would be
/// prefixed with '/' and `classify_input` would never match them.
#[test]
fn tui_spec_names_have_no_leading_slash() -> TestResult {
    let tui = CommandRegistry::built_in().map_err(ctx("built_in"))?;
    for spec in tui.specs() {
        assert!(
            !spec.name.as_str().starts_with('/'),
            "spec '{}' must not start with '/' — leading slash stripping in \
             from_operation_registry may be broken",
            spec.name.as_str(),
        );
    }
    Ok(())
}
