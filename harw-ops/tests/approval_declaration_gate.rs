//! Deklarations-Gate: Freigabe-Metadaten der Operationen gegen die Auto-Freigabe.
//!
//! # Warum dieser Test hier liegt
//! `harw-registry-defaults` besitzt `AUTO_APPROVED_TOOLS`, kennt aber keine
//! Operationen — und darf `harw-ops` nicht einmal als Dev-Dependency ziehen
//! (`harw-ops` hängt normal von `harw-registry-defaults` ab, das wäre ein
//! Zyklus). Integrationstests dieses Crates sehen dagegen beide Seiten: die
//! echte Operations-Registry (`register_all` + `register_plan_tools`) und die
//! öffentliche Allowlist samt `DefaultApprovalPolicy`.
//!
//! # Was geprüft wird (W1-05, Register F-014, G-003, F-043)
//! Jede Operation mit `Surface::ModelTool { approval != None }` muss vor der
//! Ausführung nachfragen: sie darf weder in `AUTO_APPROVED_TOOLS` stehen noch
//! von `DefaultApprovalPolicy::requires_explicit_approval` durchgewunken
//! werden. Vorher standen `plan` und `goal` trotz `approval = "always"` in der
//! Liste und mutierten im Modus `Delegated` ohne Rückfrage. Zusätzlich gilt
//! dasselbe für jede Modell-Tool-Fläche mit `readonly = false`.
//!
//! Die Prüfung nutzt bewusst die reine Namensprüfung statt
//! `ApprovalHandler::review`: `review` liest den prozessweiten
//! Freigabemodus, und parallel laufende Tests desselben Binaries dürfen das
//! Ergebnis nicht beeinflussen.

use harw_extension_api::{ToolCall, ToolName};
use harw_operations::registry::OperationRegistry;
use harw_operations::{ApprovalPolicy, Surface};
use harw_plan::config::PlanToolConfig;
use harw_registry_defaults::{AUTO_APPROVED_TOOLS, DefaultApprovalPolicy};

/// Registry mit der Grundausstattung und der gegateten Planungsfläche —
/// derselbe Operationssatz, den eine Root-Session mit `[tools.plan] enabled`
/// als Modell-Werkzeuge anbietet.
fn full_registry() -> OperationRegistry {
    let mut registry = OperationRegistry::new();
    harw_ops::register_all(&mut registry);
    let added = harw_ops::register_plan_tools(&mut registry, &PlanToolConfig::enabled_defaults());
    assert_eq!(
        added,
        harw_ops::PLAN_TOOL_COUNT,
        "Planungsfläche muss registriert sein"
    );
    let added =
        harw_ops::register_work_driver_tools(&mut registry, &PlanToolConfig::enabled_defaults());
    assert_eq!(
        added,
        harw_ops::WORK_DRIVER_TOOL_COUNT,
        "WorkDriver-Fläche muss registriert sein"
    );
    // R18: die `gateway.*`-Fläche einer UIA-Wurzel (mit und ohne Port).
    let added = harw_ops::gateway_ops::register_gateway(&mut registry);
    assert_eq!(added.ok(), Some(harw_ops::gateway_ops::GATEWAY_OP_COUNT));
    let added = harw_ops::gateway_ops::register_gateway_diagnostics(&mut registry);
    assert_eq!(
        added.ok(),
        Some(harw_ops::gateway_ops::GATEWAY_DIAGNOSTICS_OP_COUNT)
    );
    registry
}

/// Baut den Aufruf, den das Modell für eine Operation absetzen würde. Der
/// Werkzeugname der Modell-Tool-Fläche ist `OperationMeta::name`
/// (`ModelToolAdapter::tool_name`).
fn model_call(name: &str) -> ToolCall {
    ToolCall {
        id: Default::default(),
        name: ToolName::new(name),
        arguments: Default::default(),
    }
}

/// `(Operationsname, readonly, approval)` jeder deklarierten Modell-Tool-Fläche.
fn model_tool_surfaces(registry: &OperationRegistry) -> Vec<(&'static str, bool, ApprovalPolicy)> {
    let mut surfaces = Vec::new();
    for op in registry.iter() {
        let meta = op.meta();
        for surface in &meta.surfaces {
            if let Surface::ModelTool { readonly, approval } = surface {
                surfaces.push((meta.name, *readonly, *approval));
            }
        }
    }
    surfaces
}

#[test]
fn no_model_tool_with_a_declared_approval_is_auto_approved() {
    let registry = full_registry();
    let surfaces = model_tool_surfaces(&registry);

    let mut gated: Vec<&str> = Vec::new();
    for &(name, _readonly, approval) in &surfaces {
        if approval == ApprovalPolicy::None {
            continue;
        }
        gated.push(name);
        assert!(
            !AUTO_APPROVED_TOOLS.contains(&name),
            "{name} deklariert ModelTool-approval {approval:?}, steht aber in AUTO_APPROVED_TOOLS"
        );
        assert!(
            DefaultApprovalPolicy::requires_explicit_approval(&model_call(name)),
            "{name} deklariert ModelTool-approval {approval:?}, die DefaultApprovalPolicy \
             winkt den Aufruf aber ohne Rückfrage durch"
        );
    }

    // Gegen einen leeren Durchlauf absichern: verschwänden die Deklarationen
    // (etwa durch ein geändertes Makro), wäre der Test sonst still grün.
    for expected in ["plan", "goal", "stop"] {
        assert!(
            gated.contains(&expected),
            "{expected} muss eine ModelTool-Fläche mit approval != None deklarieren \
             (gefunden: {gated:?})"
        );
    }
}

#[test]
fn no_mutating_model_tool_is_auto_approved() {
    let registry = full_registry();
    for (name, readonly, approval) in model_tool_surfaces(&registry) {
        if readonly {
            continue;
        }
        assert!(
            DefaultApprovalPolicy::requires_explicit_approval(&model_call(name)),
            "{name} ist als ModelTool nicht readonly (approval {approval:?}) und darf nicht \
             auto-freigegeben sein"
        );
    }
}

#[test]
fn every_auto_approved_operation_declares_a_read_only_model_tool_without_approval() {
    // Umkehrung: steht ein Operationsname in der Allowlist, muss seine
    // Modell-Tool-Fläche read-only und ohne Freigabe deklariert sein. Namen
    // ohne Operation (fs.*, deps.*, web.*, lens.ask) sind Werkzeuge der
    // Tool-Provider und werden in `harw-registry-defaults` selbst geprüft.
    let registry = full_registry();
    let surfaces = model_tool_surfaces(&registry);
    for tool in AUTO_APPROVED_TOOLS {
        let Some(op) = registry.find_by_name(tool) else {
            continue;
        };
        let declared: Vec<_> = surfaces
            .iter()
            .filter(|(name, _, _)| name == tool)
            .collect();
        assert!(
            !declared.is_empty(),
            "{tool} ist auto-freigegeben, hat aber keine ModelTool-Fläche \
             (surfaces: {:?})",
            op.meta().surfaces
        );
        for (_, readonly, approval) in declared {
            assert!(
                *readonly && *approval == ApprovalPolicy::None,
                "{tool} ist auto-freigegeben, deklariert aber readonly={readonly}, \
                 approval={approval:?}"
            );
        }
    }
}
