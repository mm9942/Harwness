//! Roh geparste TOML-Struktur einer Agentendefinition (§8 DSL-Spec).
//!
//! Dieses Modul enthält [`RawAgentDefinition`], die direkte Spiegelung einer
//! TOML-Definitionsdatei. Die Struktur ist noch nicht auf ID-Konsistenz,
//! Versions-Kompatibilität oder Authority-Monotonie geprüft.
//!
//! # Schlüsseltypen
//! - [`RawAgentDefinition`] — TOML-nahe, ungeparste Definition
//! - [`RawMixin`] — Mixin-Deklaration (aktuell als Alias für [`DefinitionRef`])
//! - [`RawPatch`] — Patch-Deklaration (aktuell als Alias für [`toml::Table`])
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt nur die Deserialisierung; Validierung und Auflösung
//! sind Aufgaben von `parse.rs` bzw. `resolve.rs`.
//!
//! # Nebenläufigkeit
//! `RawAgentDefinition` ist `Send + Sync` und klonierbar.

use serde::{Deserialize, Serialize};

use crate::ids::{DefinitionId, DefinitionRef, Version};
use crate::roles::AgentRoleId;

/// Direktes TOML-Abbild einer Agentendefinitions-Datei (§8).
///
/// # Beschreibung
/// Enthält alle Felder einer `.toml`-Definition in ihrer rohen Form.
/// Unbekannte Felder (wie `[work]`, `[context]`, `[tools]`) werden in der
/// Tabelle `tables` gesammelt. Patch-Operationen landen in `patch`.
///
/// # Argumente
/// Keine — wird via `serde` deserialisiert.
///
/// # Fehler
/// Keine direkt; Fehler entstehen beim Aufruf von [`crate::parse::parse_toml`].
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::parse::parse_toml;
///
/// let src = r#"
/// schema = "harwness.agent/v1"
/// id = "harwness.agent.test@1"
/// version = "1.0.0"
/// role = "worker"
/// specialization = "test-worker"
/// "#;
/// let raw = parse_toml(src).unwrap();
/// assert_eq!(raw.schema, "harwness.agent/v1");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawAgentDefinition {
    /// Schema-Bezeichner der Definition, z. B. `"harwness.agent/v1"`.
    pub schema: String,

    /// Eindeutige namensraum-ID dieser Definition (§5).
    pub id: DefinitionId,

    /// Vollständige semantische Version dieser Definition.
    pub version: Version,

    /// Optionale Basis-Definition (§6, `extends`).
    #[serde(default)]
    pub extends: Option<DefinitionRef>,

    /// Geordnete Liste von Mixins (§6).
    #[serde(default)]
    pub mixins: Vec<DefinitionRef>,

    /// Autoritative Rolle dieses Agenten (§3).
    pub role: AgentRoleId,

    /// Spezialisierungsbezeichner, der das Arbeitsvertrag bestimmt (§10).
    pub specialization: String,

    /// Optionaler menschenlesbarer Name.
    #[serde(default)]
    pub name: Option<String>,

    /// Optionale Beschreibung der Aufgabe dieses Agenten.
    #[serde(default)]
    pub description: Option<String>,

    /// Alle weiteren TOML-Tabellen (z. B. `[work]`, `[context]`, `[tools]`).
    /// Compiler-Erweiterungen deserialisieren diese Felder später.
    #[serde(flatten)]
    pub tables: toml::Table,

    /// Patch-Operationen aus `[patch.*]`-Sektionen (§7).
    #[serde(default)]
    pub patch: toml::Table,
}

/// Mixin-Deklaration: Alias für [`DefinitionRef`] für semantische Klarheit.
pub type RawMixin = DefinitionRef;

/// Patch-Deklaration: freie TOML-Tabelle mit Merge-Operationen (§7).
pub type RawPatch = toml::Table;

#[cfg(test)]
mod tests {
    use crate::parse::parse_toml;

    #[test]
    fn test_parse_minimal_toml() {
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.test-min@1"
version = "1.0.0"
role = "worker"
specialization = "focused-pure-coding"
"#;
        let raw = parse_toml(src).unwrap();
        assert_eq!(raw.schema, "harwness.agent/v1");
        assert_eq!(raw.role, crate::roles::AgentRoleId::Worker);
        assert_eq!(raw.specialization, "focused-pure-coding");
        assert!(raw.extends.is_none());
        assert!(raw.mixins.is_empty());
    }

    #[test]
    fn test_parse_full_example() {
        // Vollständiges Beispiel aus §8 der DSL-Spec
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.focused-pure-coding@1"
version = "1.0.0"

extends = { id = "harwness.agent.worker-base@1" }

name = "Focused Pure Coding Task Agent"
description = "Implements one bounded coding task without redesigning architecture."

role = "worker"
specialization = "focused-pure-coding"

[compatibility]
min_harwness = "0.1.0"

[binding]
required = "plan-node"
requires_parent_orchestrator = true

[work]
mode = "implementation"
may_research_web = false
may_change_plan = false
may_change_architecture = false
may_spawn_agents = false
may_invoke_agent_tools = true

[context]
policy = "harwness.context.focused-worker@1"
budget_tokens = 18000
include_full_transcript = false
load_details = "on-demand"

[limits]
max_tool_calls = 40
max_agent_tool_calls = 4
max_wall_time_seconds = 1800
"#;
        let raw = parse_toml(src).unwrap();
        assert_eq!(raw.id.name, "focused-pure-coding");
        assert_eq!(raw.id.namespace, "harwness");
        assert_eq!(raw.version.0.major, 1);
        assert!(raw.extends.is_some());
        assert_eq!(raw.name.as_deref(), Some("Focused Pure Coding Task Agent"));
    }
}
