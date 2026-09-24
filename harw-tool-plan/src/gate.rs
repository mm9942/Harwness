//! Sofort wirkende Plan-Sperre als Freigabe-Handler (Runde 5, Teil F, Punkt 3).
//!
//! # Beschreibung
//! Der Kern übernimmt `InteractionMode::Plan` (Werkzeugfläche + Rechte-Decke)
//! erst an der nächsten Turn-Grenze. Damit ein Wechsel auf die Stufe `plan`
//! **sofort** wirkt — auch mitten in einem Turn —, hängt die Montage diesen
//! Handler an die Freigabekette der Wurzel. Solange die [`PlanModeLock`]
//! gesetzt ist, lehnt er jeden Werkzeugaufruf ab, dessen Name nicht in der
//! Plan-Positivliste steht (`harw_core::mode::InteractionMode::Plan`
//! `.allowed_tools()`, von der Montage übergeben): nichts Schreibendes außer
//! `plan.write`, keine Ausführung.
//!
//! `check_approval` aggregiert `Deny` > `AskUser` > `Allow`; ein `Deny`
//! dieses Handlers gewinnt deshalb über jeden Freigabemodus, auch
//! `FullAccess`, und über jede Allow-Regel. Ohne Sperre liefert er `Allow`
//! und schränkt nichts ein.
//!
//! # Nebenläufigkeit
//! `Send + Sync`; liest die Sperre bei **jedem** Aufruf frisch.

use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{ApprovalDecision, ApprovalHandler, ExtFuture, ToolCall};

use crate::session::PlanModeLock;

/// Kurzname des Handlers (Diagnose, Rechte-Schnappschuss).
pub const PLAN_MODE_GATE_LABEL: &str = "plan-mode-gate";

/// Lehnt im Plan-Modus alles außerhalb der Plan-Positivliste ab.
#[derive(Debug, Clone)]
pub struct PlanModeGate {
    lock: PlanModeLock,
    allowed: Vec<String>,
}

/// Präfix der Übergabe-Werkzeuge (`harw_core::turn_loop::HANDOFF_PREFIX`).
const HANDOFF_TOOL_PREFIX: &str = "transfer_to_";

impl PlanModeGate {
    /// Baut den Handler.
    ///
    /// # Arguments
    /// - `lock` ([`PlanModeLock`]): die geteilte Sperre der Sitzung.
    /// - `allowed` (`impl IntoIterator<Item = impl Into<String>>`): die
    ///   Plan-Positivliste (`InteractionMode::Plan.allowed_tools()`).
    #[must_use]
    pub fn new(lock: PlanModeLock, allowed: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            lock,
            allowed: allowed.into_iter().map(Into::into).collect(),
        }
    }

    /// Die reine Entscheidung (testbar ohne Laufzeit).
    ///
    /// # Returns
    /// `None` für „kein Einwand“, sonst die Ablehnungsbegründung.
    #[must_use]
    pub fn denial_for(&self, tool: &str) -> Option<String> {
        // Plan R9, E1: eine Übergabe `transfer_to_<ziel>` ist im Plan-Modus
        // erlaubt — welche Ziele (nur lesende) entscheiden Sichtbarkeit und
        // Admission des Spawners (`harw_core::delegation_visibility::
        // delegable_in_mode`), nicht diese Namensliste.
        if !self.lock.is_locked()
            || tool.starts_with(HANDOFF_TOOL_PREFIX)
            || self.allowed.iter().any(|name| name == tool)
        {
            return None;
        }
        Some(format!(
            "Plan-Modus aktiv: „{tool}“ ist gesperrt. Im Plan-Modus sind nur lesende \
             Werkzeuge, Recherche, plan.write (nur .harw/plans/) und ask_user erlaubt. \
             Schreibe den Plan mit plan.write und lege ihn mit plan.exit zur Freigabe vor."
        ))
    }
}

impl ApprovalHandler for PlanModeGate {
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        let decision = match self.denial_for(call.name.as_str()) {
            Some(reason) => ApprovalDecision::Deny(reason),
            None => ApprovalDecision::Allow,
        };
        Box::pin(async move { decision })
    }

    fn kind(&self) -> ApprovalHandlerKind {
        // Eingebaute Grenze ohne Konfiguration, aber keine Standardpolitik im
        // Sinne der Kette (`DefaultApprovalPolicy`) — deshalb `Other`.
        ApprovalHandlerKind::Other
    }

    fn label(&self) -> &'static str {
        PLAN_MODE_GATE_LABEL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use harw_extension_api::ToolName;
    use harw_types::ToolCallId;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn locked_gate_denies_writing_and_executing_tools_mid_turn() -> TestResult {
        let lock = PlanModeLock::new(false);
        let gate = PlanModeGate::new(lock.clone(), ["fs.read", "plan.write", "ask_user"]);
        // Offen: nichts wird eingeschränkt.
        assert!(matches!(
            gate.review(&call("shell.exec")).await,
            ApprovalDecision::Allow
        ));
        // Mitten im Turn umgeschaltet: der nächste Aufruf ist gesperrt.
        lock.set(true);
        for forbidden in [
            "fs.write",
            "fs.edit",
            "shell.exec",
            "host.sudo_exec",
            "agents.write_uia",
        ] {
            assert!(
                matches!(
                    gate.review(&call(forbidden)).await,
                    ApprovalDecision::Deny(_)
                ),
                "{forbidden}"
            );
        }
        for allowed in ["fs.read", "plan.write", "ask_user"] {
            assert!(
                matches!(gate.review(&call(allowed)).await, ApprovalDecision::Allow),
                "{allowed}"
            );
        }
        // Plan R9, E1: Übergaben passieren das Tor; die Zielauswahl (nur
        // lesende Ziele) trifft der Spawner.
        assert!(gate.denial_for("transfer_to_explorer").is_none());
        // Zurück: wieder offen.
        lock.set(false);
        assert!(gate.denial_for("fs.write").is_none());
        Ok(())
    }
}
