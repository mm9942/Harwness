//! Capability-Tabelle der Anthropic-Modelle für Thinking, Effort, Ausgabe-Limit
//! und Prompt-Caching (W4a/A-ANTH).
//!
//! ## Verantwortung
//! Dieses Modul beantwortet für eine Modell-ID genau drei Fragen, die der
//! Wire-Builder in [`crate::anthropic`] braucht:
//! 1. Welcher `thinking`-Modus wird akzeptiert (`adaptive` oder nur das
//!    Legacy-`enabled`+`budget_tokens`)?
//! 2. Wird `output_config.effort` akzeptiert, und welche Stufen (`xhigh`,
//!    `max`) kennt das Modell?
//! 3. Wie viele Ausgabe-Tokens (`max_tokens`) erlaubt die synchrone
//!    Messages-API höchstens?
//!
//! Unbekannte IDs (Foundry-Deployment-Namen, Gateway-Aliase, zurückgezogene
//! Modelle) liefern `None`: der Wire-Builder sendet dann **keine**
//! Reasoning-Felder und klemmt `max_tokens` nicht (fail closed — ein
//! falsches `thinking`-Feld erzeugt HTTP 400).
//!
//! ## Quellen (Stand 2026-09-13, offizielle Anthropic-Doku)
//! - Modelle, IDs, Aliase, Max-Output:
//!   <https://platform.claude.com/docs/en/about-claude/models/overview>
//! - Effort-Modelle und `xhigh`/`max`-Verfügbarkeit:
//!   <https://platform.claude.com/docs/en/build-with-claude/effort>
//! - Thinking-Modi und abgelehnte Konfigurationen je Modell:
//!   <https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting>
//! - 128k-Ausgabe für alle 1M-Kontext-Modelle:
//!   <https://platform.claude.com/docs/en/build-with-claude/context-windows>
//!
//! ## Nebenläufigkeit
//! Reine `'static`-Daten und reine Funktionen; `Send + Sync`, lock-frei.
//!
//! ## Fehler
//! Keine — alle Funktionen sind total.

use harw_types::ReasoningEffort;

/// Akzeptierter `thinking`-Modus eines Modells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThinkingSupport {
    /// `thinking: {"type": "adaptive"}` wird akzeptiert (Claude 4.6 und neuer,
    /// Mythos Preview).
    Adaptive,
    /// Nur Legacy-`{"type": "enabled", "budget_tokens": N}`; `adaptive`
    /// liefert HTTP 400 („adaptive thinking is not supported on this model“).
    /// Der Harness sendet für diese Modelle kein `thinking`-Feld, weil er
    /// kein Token-Budget aus einem Effort-Level ableitet.
    ExtendedOnly,
}

/// Eine Zeile der Capability-Tabelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnthropicModelCaps {
    /// Claude-API-ID (bei Modellen vor 4.6 die datierte Snapshot-ID).
    pub(crate) id: &'static str,
    /// Weitere akzeptierte Schreibweisen (Claude-API-Alias).
    pub(crate) aliases: &'static [&'static str],
    /// Akzeptierter `thinking`-Modus.
    pub(crate) thinking: ThinkingSupport,
    /// `output_config.effort` mit `low`/`medium`/`high` wird akzeptiert.
    pub(crate) effort: bool,
    /// Stufe `xhigh` wird akzeptiert.
    pub(crate) xhigh: bool,
    /// Stufe `max` wird akzeptiert.
    pub(crate) max: bool,
    /// Obergrenze für `max_tokens` der synchronen Messages-API; `None`, wenn
    /// die offizielle Doku für dieses Modell keinen Wert mehr ausweist.
    pub(crate) max_output_tokens: Option<u32>,
}

// 128k Ausgabe-Tokens: context-windows-Seite, gilt für alle 1M-Kontext-Modelle.
const OUT_128K: Option<u32> = Some(128_000);

// Die Tabelle. Jede Zeile ist gegen die im Modulkopf genannten Seiten geprüft.
const MODELS: &[AnthropicModelCaps] = &[
    AnthropicModelCaps {
        id: "claude-fable-5-1",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: true,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-mythos-5-1",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: true,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-fable-5",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: true,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-mythos-5",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: true,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-mythos-preview",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: false,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-opus-5",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: true,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-opus-4-8",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: true,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-opus-4-7",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: true,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-opus-4-6",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: false,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-sonnet-5",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: true,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-sonnet-4-6",
        aliases: &[],
        thinking: ThinkingSupport::Adaptive,
        effort: true,
        xhigh: false,
        max: true,
        max_output_tokens: OUT_128K,
    },
    AnthropicModelCaps {
        id: "claude-opus-4-5-20251101",
        aliases: &["claude-opus-4-5"],
        thinking: ThinkingSupport::ExtendedOnly,
        effort: true,
        xhigh: false,
        max: false,
        max_output_tokens: None,
    },
    AnthropicModelCaps {
        id: "claude-sonnet-4-5-20250929",
        aliases: &["claude-sonnet-4-5"],
        thinking: ThinkingSupport::ExtendedOnly,
        effort: false,
        xhigh: false,
        max: false,
        max_output_tokens: None,
    },
    AnthropicModelCaps {
        id: "claude-haiku-4-5-20251001",
        aliases: &["claude-haiku-4-5"],
        thinking: ThinkingSupport::ExtendedOnly,
        effort: false,
        xhigh: false,
        max: false,
        max_output_tokens: Some(64_000),
    },
];

/// Sucht die Capability-Zeile zu einer Modell-ID (exakt, inkl. Alias).
///
/// # Arguments
/// - `model` (`&str`): Modell-/Deployment-Name aus dem Request.
///
/// # Returns
/// `Some(&caps)` für eine dokumentierte ID, sonst `None` (fail closed).
#[must_use]
pub(crate) fn lookup(model: &str) -> Option<&'static AnthropicModelCaps> {
    MODELS
        .iter()
        .find(|caps| caps.id == model || caps.aliases.contains(&model))
}

/// Übersetzt ein internes Effort-Level in den `output_config.effort`-Wert
/// eines Modells.
///
/// # Description
/// - `Minimal` → `None` (Feld weglassen; Anthropic kennt keine Stufe
///   „minimal“).
/// - `Low`/`Medium`/`High` → gleichnamige Stufe.
/// - `Xhigh` → `xhigh`, sonst `high` (nie über die Anforderung hinaus).
/// - `Max` → `max`, sonst `xhigh`, sonst `high`.
/// - Modelle ohne Effort-Unterstützung → immer `None`.
///
/// # Returns
/// `Some(wire_value)` oder `None` (Feld weglassen).
#[must_use]
pub(crate) fn effort_wire_value(
    caps: &AnthropicModelCaps,
    effort: ReasoningEffort,
) -> Option<&'static str> {
    if !caps.effort {
        return None;
    }
    match effort {
        ReasoningEffort::Minimal => None,
        ReasoningEffort::Low => Some("low"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        ReasoningEffort::Xhigh if caps.xhigh => Some("xhigh"),
        ReasoningEffort::Xhigh => Some("high"),
        ReasoningEffort::Max if caps.max => Some("max"),
        ReasoningEffort::Max if caps.xhigh => Some("xhigh"),
        ReasoningEffort::Max => Some("high"),
    }
}

/// Klemmt eine gewünschte Ausgabe-Obergrenze auf das Modell-Limit.
///
/// # Returns
/// `min(requested, max_output_tokens)` für bekannte Modelle mit Limit, sonst
/// `requested` unverändert. Nie `0`: ein Wunsch von `0` wird auf `1` gehoben,
/// weil die API `max_tokens >= 1` verlangt.
#[must_use]
pub(crate) fn clamp_max_tokens(model: &str, requested: u32) -> u32 {
    let requested = requested.max(1);
    match lookup(model).and_then(|caps| caps.max_output_tokens) {
        Some(limit) => requested.min(limit),
        None => requested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_finds_current_models_and_aliases() {
        for id in [
            "claude-fable-5-1",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
            "claude-haiku-4-5",
            "claude-opus-4-5",
        ] {
            assert!(lookup(id).is_some(), "{id}");
        }
        assert_eq!(
            lookup("claude-haiku-4-5").map(|caps| caps.id),
            Some("claude-haiku-4-5-20251001")
        );
    }

    #[test]
    fn test_lookup_unknown_deployment_alias_is_none() {
        assert!(lookup("anthropic-deployment-alias").is_none());
        assert!(lookup("claude-opus-5 ").is_none());
        assert!(lookup("anthropic.claude-opus-5").is_none());
    }

    #[test]
    fn test_lookup_thinking_modes_match_docs() {
        let adaptive = [
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "claude-fable-5",
            "claude-mythos-5",
            "claude-mythos-preview",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
        ];
        for id in adaptive {
            assert_eq!(
                lookup(id).map(|caps| caps.thinking),
                Some(ThinkingSupport::Adaptive),
                "{id}"
            );
        }
        for id in [
            "claude-opus-4-5-20251101",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
        ] {
            assert_eq!(
                lookup(id).map(|caps| caps.thinking),
                Some(ThinkingSupport::ExtendedOnly),
                "{id}"
            );
        }
    }

    #[test]
    fn test_effort_wire_value_maps_levels_per_model() {
        let opus5 = lookup("claude-opus-5").expect("opus 5 row");
        assert_eq!(effort_wire_value(opus5, ReasoningEffort::Minimal), None);
        assert_eq!(effort_wire_value(opus5, ReasoningEffort::Low), Some("low"));
        assert_eq!(effort_wire_value(opus5, ReasoningEffort::Medium), Some("medium"));
        assert_eq!(effort_wire_value(opus5, ReasoningEffort::High), Some("high"));
        assert_eq!(effort_wire_value(opus5, ReasoningEffort::Xhigh), Some("xhigh"));
        assert_eq!(effort_wire_value(opus5, ReasoningEffort::Max), Some("max"));

        let sonnet46 = lookup("claude-sonnet-4-6").expect("sonnet 4.6 row");
        assert_eq!(effort_wire_value(sonnet46, ReasoningEffort::Xhigh), Some("high"));
        assert_eq!(effort_wire_value(sonnet46, ReasoningEffort::Max), Some("max"));

        let opus45 = lookup("claude-opus-4-5").expect("opus 4.5 row");
        assert_eq!(effort_wire_value(opus45, ReasoningEffort::Max), Some("high"));

        let haiku = lookup("claude-haiku-4-5").expect("haiku row");
        assert_eq!(effort_wire_value(haiku, ReasoningEffort::High), None);
    }

    #[test]
    fn test_clamp_max_tokens_respects_model_limits() {
        assert_eq!(clamp_max_tokens("claude-opus-5", 500_000), 128_000);
        assert_eq!(clamp_max_tokens("claude-haiku-4-5", 100_000), 64_000);
        assert_eq!(clamp_max_tokens("claude-opus-5", 4096), 4096);
        assert_eq!(clamp_max_tokens("custom-deployment", 500_000), 500_000);
        assert_eq!(clamp_max_tokens("claude-opus-5", 0), 1);
    }
}
