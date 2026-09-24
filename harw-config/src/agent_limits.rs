//! `[agents]` — Grenzen der Agenten-Orchestrierung (Runde 5, Teil K).
//!
//! # Beschreibung
//! Vier sicherheitsnahe Zahlen, die der `ManagedAgentSpawner` bei jeder
//! Admission durchsetzt (fail-closed, mit klarer Meldung an das Modell):
//!
//! | Schlüssel | Vorgabe | erlaubt | Bedeutung |
//! |---|---|---|---|
//! | `max_root_orchestrators` | 1 | 1–4 | gleichzeitig laufende Orchestratoren **der UIA-Wurzel** (Hintergrund und synchron) |
//! | `max_sub_orchestrators` | 2 | 1–6 | gleichzeitig laufende Sub-Orchestratoren je Root-Orchestrator-Baum |
//! | `max_sub_orchestrator_depth` | 2 | 1–3 | Verschachtelung der Sub-Orchestratoren (direkt unter dem Root-Orchestrator = 1) |
//! | `max_spawn_depth` | 4 | 1–6 | allgemeine Kind-Tiefe unterhalb der Wurzelsitzung (`ChildLimits::max_depth`) |
//!
//! Ungültige Werte werden auf den erlaubten Bereich **geklemmt**, nie
//! abgelehnt. Zusätzlich gilt die Konsistenzregel
//! `max_sub_orchestrator_depth + 1 ≤ max_spawn_depth`: die Sub-Tiefe wird
//! notfalls gekappt (Warnung im Log), siehe [`AgentLimitsToml::effective`].
//!
//! # Merge-Regel
//! Nur vertraute Layer (Home **und** Profil) setzen frei — auch nach oben.
//! Ein nicht vertrautes Projekt darf jede Zahl nur **senken**
//! (`crate::merge`, `merge_agent_limits`); ein Erhöhungsversuch wird
//! ignoriert und als `ScopeDiagnostic` gemeldet.
//!
//! # Nebenläufigkeit
//! Reine Datentypen und Funktionen.

use serde::{Deserialize, Serialize};

/// Vorgabe für `[agents] max_root_orchestrators`.
pub const DEFAULT_MAX_ROOT_ORCHESTRATORS: u32 = 1;
/// Erlaubter Bereich für `[agents] max_root_orchestrators` (Kostendeckel).
pub const MAX_ROOT_ORCHESTRATORS_RANGE: (u32, u32) = (1, 4);

/// Vorgabe für `[agents] max_sub_orchestrators`.
pub const DEFAULT_MAX_SUB_ORCHESTRATORS: u32 = 2;
/// Erlaubter Bereich für `[agents] max_sub_orchestrators`.
pub const MAX_SUB_ORCHESTRATORS_RANGE: (u32, u32) = (1, 6);

/// Vorgabe für `[agents] max_sub_orchestrator_depth`.
pub const DEFAULT_MAX_SUB_ORCHESTRATOR_DEPTH: u32 = 2;
/// Erlaubter Bereich für `[agents] max_sub_orchestrator_depth`.
pub const MAX_SUB_ORCHESTRATOR_DEPTH_RANGE: (u32, u32) = (1, 3);

/// Vorgabe für `[agents] max_spawn_depth` — entspricht dem bisherigen
/// festen `ChildLimits::conservative().max_depth`.
pub const DEFAULT_MAX_SPAWN_DEPTH: u32 = 4;
/// Erlaubter Bereich für `[agents] max_spawn_depth`.
pub const MAX_SPAWN_DEPTH_RANGE: (u32, u32) = (1, 6);

/// `[agents]` — Orchestrierungsgrenzen, wie sie in der TOML stehen.
///
/// # Beschreibung
/// Jedes Feld ist optional; `None` heißt Vorgabe. Die wirksamen, geklemmten
/// und konsistenten Werte liefert [`Self::effective`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLimitsToml {
    /// Höchstzahl gleichzeitig laufender Orchestratoren der UIA-Wurzel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_root_orchestrators: Option<u32>,
    /// Höchstzahl gleichzeitig laufender Sub-Orchestratoren je
    /// Root-Orchestrator-Baum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sub_orchestrators: Option<u32>,
    /// Höchste Verschachtelung der Sub-Orchestratoren.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sub_orchestrator_depth: Option<u32>,
    /// Allgemeine Kind-Tiefe unterhalb der Wurzelsitzung.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_depth: Option<u32>,
}

/// Die wirksamen Orchestrierungsgrenzen (geklemmt und konsistent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveAgentLimits {
    /// Siehe [`AgentLimitsToml::max_root_orchestrators`].
    pub max_root_orchestrators: u32,
    /// Siehe [`AgentLimitsToml::max_sub_orchestrators`].
    pub max_sub_orchestrators: u32,
    /// Siehe [`AgentLimitsToml::max_sub_orchestrator_depth`]; nach der
    /// Konsistenz-Kappung höchstens `max_spawn_depth - 1` (kann dadurch `0`
    /// werden: dann darf kein Sub-Orchestrator entstehen).
    pub max_sub_orchestrator_depth: u32,
    /// Siehe [`AgentLimitsToml::max_spawn_depth`].
    pub max_spawn_depth: u32,
}

impl Default for EffectiveAgentLimits {
    fn default() -> Self {
        AgentLimitsToml::default().effective()
    }
}

/// Klemmt `value` (oder die Vorgabe) in den Bereich `range`.
fn clamp_or_default(value: Option<u32>, default: u32, range: (u32, u32)) -> u32 {
    value.unwrap_or(default).clamp(range.0, range.1)
}

impl AgentLimitsToml {
    /// Wirksame `max_root_orchestrators` (Vorgabe 1, geklemmt auf 1–4).
    #[must_use]
    pub fn effective_max_root_orchestrators(&self) -> u32 {
        clamp_or_default(
            self.max_root_orchestrators,
            DEFAULT_MAX_ROOT_ORCHESTRATORS,
            MAX_ROOT_ORCHESTRATORS_RANGE,
        )
    }

    /// Wirksame `max_sub_orchestrators` (Vorgabe 2, geklemmt auf 1–6).
    #[must_use]
    pub fn effective_max_sub_orchestrators(&self) -> u32 {
        clamp_or_default(
            self.max_sub_orchestrators,
            DEFAULT_MAX_SUB_ORCHESTRATORS,
            MAX_SUB_ORCHESTRATORS_RANGE,
        )
    }

    /// Wirksame `max_spawn_depth` (Vorgabe 4, geklemmt auf 1–6).
    #[must_use]
    pub fn effective_max_spawn_depth(&self) -> u32 {
        clamp_or_default(
            self.max_spawn_depth,
            DEFAULT_MAX_SPAWN_DEPTH,
            MAX_SPAWN_DEPTH_RANGE,
        )
    }

    /// Alle vier wirksamen Grenzen, geklemmt und konsistent.
    ///
    /// # Beschreibung
    /// Klemmt jede Zahl auf ihren Bereich und erzwingt danach
    /// `max_sub_orchestrator_depth + 1 ≤ max_spawn_depth`: ist die Sub-Tiefe
    /// zu groß, wird sie auf `max_spawn_depth - 1` gekappt und das mit einer
    /// Warnung protokolliert. Die allgemeine Tiefe bleibt dabei unberührt —
    /// sie ist die strengere, übergeordnete Grenze.
    ///
    /// # Returns
    /// [`EffectiveAgentLimits`].
    #[must_use]
    pub fn effective(&self) -> EffectiveAgentLimits {
        let max_spawn_depth = self.effective_max_spawn_depth();
        let configured_depth = clamp_or_default(
            self.max_sub_orchestrator_depth,
            DEFAULT_MAX_SUB_ORCHESTRATOR_DEPTH,
            MAX_SUB_ORCHESTRATOR_DEPTH_RANGE,
        );
        let allowed_depth = max_spawn_depth.saturating_sub(1);
        let max_sub_orchestrator_depth = if configured_depth > allowed_depth {
            tracing::warn!(
                max_sub_orchestrator_depth = configured_depth,
                max_spawn_depth,
                capped_to = allowed_depth,
                "config.agents.sub_orchestrator_depth_capped_to_spawn_depth"
            );
            allowed_depth
        } else {
            configured_depth
        };
        EffectiveAgentLimits {
            max_root_orchestrators: self.effective_max_root_orchestrators(),
            max_sub_orchestrators: self.effective_max_sub_orchestrators(),
            max_sub_orchestrator_depth,
            max_spawn_depth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_defaults_match_the_previous_behaviour() {
        let limits = AgentLimitsToml::default().effective();
        assert_eq!(limits.max_root_orchestrators, 1);
        assert_eq!(limits.max_sub_orchestrators, 2);
        assert_eq!(limits.max_sub_orchestrator_depth, 2);
        // Bisher fest: `ChildLimits::conservative().max_depth == 4`.
        assert_eq!(limits.max_spawn_depth, 4);
        assert_eq!(EffectiveAgentLimits::default(), limits);
    }

    #[test]
    fn test_out_of_range_values_are_clamped() {
        let too_big = AgentLimitsToml {
            max_root_orchestrators: Some(99),
            max_sub_orchestrators: Some(99),
            max_sub_orchestrator_depth: Some(99),
            max_spawn_depth: Some(99),
        }
        .effective();
        assert_eq!(too_big.max_root_orchestrators, 4);
        assert_eq!(too_big.max_sub_orchestrators, 6);
        assert_eq!(too_big.max_sub_orchestrator_depth, 3);
        assert_eq!(too_big.max_spawn_depth, 6);

        let zero = AgentLimitsToml {
            max_root_orchestrators: Some(0),
            max_sub_orchestrators: Some(0),
            max_sub_orchestrator_depth: Some(0),
            max_spawn_depth: Some(0),
        }
        .effective();
        assert_eq!(zero.max_root_orchestrators, 1);
        assert_eq!(zero.max_sub_orchestrators, 1);
        assert_eq!(zero.max_spawn_depth, 1);
        // Konsistenz: Sub-Tiefe + 1 ≤ Spawn-Tiefe (1) → 0.
        assert_eq!(zero.max_sub_orchestrator_depth, 0);
    }

    #[test]
    fn test_sub_depth_is_capped_to_spawn_depth_minus_one() {
        let limits = AgentLimitsToml {
            max_sub_orchestrator_depth: Some(3),
            max_spawn_depth: Some(3),
            ..AgentLimitsToml::default()
        }
        .effective();
        assert_eq!(limits.max_spawn_depth, 3);
        assert_eq!(limits.max_sub_orchestrator_depth, 2);

        let untouched = AgentLimitsToml {
            max_sub_orchestrator_depth: Some(3),
            max_spawn_depth: Some(4),
            ..AgentLimitsToml::default()
        }
        .effective();
        assert_eq!(untouched.max_sub_orchestrator_depth, 3);
    }

    #[test]
    fn test_section_parses_and_rejects_unknown_keys() -> TestResult {
        let parsed: AgentLimitsToml =
            toml::from_str("max_root_orchestrators = 2\nmax_spawn_depth = 5")
                .map_err(ctx("gültiger [agents]-Abschnitt"))?;
        assert_eq!(parsed.max_root_orchestrators, Some(2));
        assert_eq!(parsed.max_spawn_depth, Some(5));
        assert!(toml::from_str::<AgentLimitsToml>("max_children = 3").is_err());
        Ok(())
    }
}
