//! `[tools.plan]` — Konfiguration für das Planning-Tool (AP W1-21..24).
//!
//! Spiegelt die in `docs/design/planning-tool-v1.md` §6 skizzierte
//! `PlanToolConfig` auf der Harness-Ebene (`.harw/config.toml`), damit der
//! Runtime-Wirt entscheiden kann, ob das Plan-Tool überhaupt registriert
//! wird, bevor `harw-plan` instanziiert ist.
//!
//! **Bewusst keine Abhängigkeit zu `harw-plan`**: `harw-plan` darf niemals
//! auf `harw-config` zeigen (das wäre eine neue, teure Zyklus-Kante). Aus
//! diesem Grund bleibt [`PlanSection::require_exploration_for`] als
//! `Vec<String>` typisiert statt als `Vec<harw_plan::PlanNodeKind>`; die
//! Umwandlung in die konkrete Enum-Repräsentation ist Aufgabe des
//! Konsumenten (der Stelle, die sowohl `harw-config` als auch `harw-plan`
//! kennt). [`PlanSection::validate`] prüft trotzdem, dass nur bekannte
//! Knotenart-Namen auftauchen, damit Tippfehler früh auffallen.
//!
//! Der Planmodus ist standardmäßig aktiv. `enabled = false` entfernt die
//! Registrierung weiterhin vollständig; dieses Modul liefert nur die
//! deklarative Konfiguration, das Entfernen selbst obliegt dem Consumer.

use serde::{Deserialize, Serialize};

/// `[tools]` — Container-Sektion für werkzeugspezifische Konfigurationen.
/// Aktuell nur `plan`; künftige Tool-Sektionen (z. B. `[tools.search]`)
/// werden hier als weitere Felder ergänzt, sobald sie gebraucht werden.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    #[serde(default)]
    pub plan: PlanSection,
}

/// `[tools.plan]` — Sichtbarkeit, Persistenz und Validierungs-Policy des
/// Planning-Tools. Alle Felder haben hart-codierte Defaults, sodass eine
/// `config.toml` ohne `[tools.plan]` weiterhin gültig ist und das Tool
/// standardmäßig aktiv bleibt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanSection {
    /// Schaltet das Plan-Tool frei. Der Default ist `true`; `false` entfernt
    /// die Registrierung beim Consumer vollständig, nicht nur "leer".
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Persistiert Pläne über Prozessgrenzen hinweg (`FilePlanStore` statt
    /// `InMemoryPlanStore`).
    #[serde(default = "default_true")]
    pub persist: bool,
    /// Erzwingt die Nutzung des Plan-Tools, sobald die Runtime eine Aufgabe
    /// als "komplex" einstuft.
    #[serde(default)]
    pub require_for_complex_work: bool,
    /// Prüft `AddDependency`-Aktionen per DFS gegen Zyklenbildung im
    /// Dependency-Graph, bevor sie angewendet werden.
    #[serde(default = "default_true")]
    pub validate_dependency_cycles: bool,
    /// Prüft `AddNode`-Aktionen gegen überlappende `write_scope`-Bereiche
    /// aktiver Nodes (`Ready`/`InProgress`).
    #[serde(default = "default_true")]
    pub validate_write_conflicts: bool,
    /// Obergrenze der Knoten in einem einzelnen Plan; verhindert
    /// unbegrenztes Wachstum durch fehlerhafte oder böswillige Aktionen.
    #[serde(default = "default_max_nodes")]
    pub max_nodes: usize,
    /// Knotenarten (als Name, siehe Modul-Doku), für die vor der Aufnahme
    /// in den Plan eine abgeschlossene Exploration-Phase verlangt wird.
    #[serde(default = "default_require_exploration_for")]
    pub require_exploration_for: Vec<String>,
    /// Gültigkeitsdauer einer abgeschlossenen Exploration in Sekunden,
    /// bevor sie für `require_exploration_for`-Zwecke erneut verlangt wird.
    #[serde(default = "default_exploration_ttl")]
    pub exploration_ttl_secs: u64,
    /// Maximale Tiefe, bis zu der ein zusammengesetzter (`composite`)
    /// Knoten rekursiv in Unterknoten expandiert werden darf.
    #[serde(default = "default_max_expand_depth")]
    pub max_expand_depth: u32,
}

impl Default for PlanSection {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            persist: default_true(),
            require_for_complex_work: false,
            validate_dependency_cycles: default_true(),
            validate_write_conflicts: default_true(),
            max_nodes: default_max_nodes(),
            require_exploration_for: default_require_exploration_for(),
            exploration_ttl_secs: default_exploration_ttl(),
            max_expand_depth: default_max_expand_depth(),
        }
    }
}

/// Bekannte Knotenarten-Namen. Muss synchron zu `harw_plan::PlanNodeKind`
/// gepflegt werden; da dieses Modul keine Abhängigkeit zu `harw-plan`
/// eingehen darf (siehe Modul-Doku), ist dies eine bewusst duplizierte,
/// reine String-Liste statt eines gemeinsamen Enums.
const KNOWN_NODE_KINDS: &[&str] = &[
    "research",
    "explore",
    "analysis",
    "synthesis",
    "contract",
    "coding",
    "integration",
    "verification",
    "docs",
    "composite",
];

impl PlanSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `max_nodes == 0`, `max_expand_depth == 0`, oder
    /// `require_exploration_for` einen unbekannten Knotenart-Namen enthält.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_nodes == 0 {
            return Err("tools.plan.max_nodes muss größer als 0 sein".to_owned());
        }
        if self.max_expand_depth == 0 {
            return Err("tools.plan.max_expand_depth muss größer als 0 sein".to_owned());
        }
        for kind in &self.require_exploration_for {
            if !KNOWN_NODE_KINDS.contains(&kind.as_str()) {
                return Err(format!(
                    "tools.plan.require_exploration_for enthält unbekannte Knotenart {kind:?}; erlaubt sind {KNOWN_NODE_KINDS:?}"
                ));
            }
        }
        Ok(())
    }
}

fn default_true() -> bool {
    true
}
fn default_max_nodes() -> usize {
    256
}
fn default_require_exploration_for() -> Vec<String> {
    vec!["coding".to_owned(), "integration".to_owned()]
}
fn default_exploration_ttl() -> u64 {
    86_400
}
fn default_max_expand_depth() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_section_defaults_from_empty_toml() {
        let section: PlanSection = toml::from_str("").unwrap();
        assert!(section.enabled);
        assert!(section.persist);
        assert!(!section.require_for_complex_work);
        assert!(section.validate_dependency_cycles);
        assert!(section.validate_write_conflicts);
        assert_eq!(section.max_nodes, 256);
        assert_eq!(
            section.require_exploration_for,
            vec!["coding".to_owned(), "integration".to_owned()]
        );
        assert_eq!(section.exploration_ttl_secs, 86_400);
        assert_eq!(section.max_expand_depth, 3);
        assert_eq!(section, PlanSection::default());
    }

    #[test]
    fn test_tools_section_defaults_when_absent() {
        let tools: ToolsSection = toml::from_str("").unwrap();
        assert_eq!(tools.plan, PlanSection::default());
    }

    #[test]
    fn test_plan_section_full_toml_round_trip() {
        let src = r#"
            enabled = true
            persist = true
            require_for_complex_work = true
            validate_dependency_cycles = false
            validate_write_conflicts = false
            max_nodes = 64
            require_exploration_for = ["research", "analysis"]
            exploration_ttl_secs = 120
            max_expand_depth = 5
        "#;
        let section: PlanSection = toml::from_str(src).unwrap();
        assert!(section.enabled);
        assert!(section.persist);
        assert!(section.require_for_complex_work);
        assert!(!section.validate_dependency_cycles);
        assert!(!section.validate_write_conflicts);
        assert_eq!(section.max_nodes, 64);
        assert_eq!(
            section.require_exploration_for,
            vec!["research".to_owned(), "analysis".to_owned()]
        );
        assert_eq!(section.exploration_ttl_secs, 120);
        assert_eq!(section.max_expand_depth, 5);

        let encoded = toml::to_string(&section).unwrap();
        let decoded: PlanSection = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, section);
    }

    #[test]
    fn test_tools_plan_nested_table_round_trip() {
        let src = r#"
            [plan]
            enabled = true
            max_nodes = 10
        "#;
        let tools: ToolsSection = toml::from_str(src).unwrap();
        assert!(tools.plan.enabled);
        assert_eq!(tools.plan.max_nodes, 10);
    }

    #[test]
    fn test_plan_section_rejects_unknown_field() {
        let src = r#"
            enabled = true
            enalbed = true
        "#;
        let error = toml::from_str::<PlanSection>(src).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn test_tools_section_rejects_unknown_field() {
        let src = r#"
            [pln]
            enabled = true
        "#;
        assert!(toml::from_str::<ToolsSection>(src).is_err());
    }

    #[test]
    fn test_validate_rejects_zero_max_nodes() {
        let section = PlanSection {
            max_nodes: 0,
            ..Default::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("max_nodes"));
    }

    #[test]
    fn test_validate_rejects_zero_max_expand_depth() {
        let section = PlanSection {
            max_expand_depth: 0,
            ..Default::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("max_expand_depth"));
    }

    #[test]
    fn test_validate_rejects_unknown_exploration_node_kind() {
        let section = PlanSection {
            require_exploration_for: vec!["not_a_real_kind".to_owned()],
            ..Default::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("not_a_real_kind"));
    }

    #[test]
    fn test_validate_accepts_all_known_node_kinds() {
        let section = PlanSection {
            require_exploration_for: KNOWN_NODE_KINDS.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
        };
        assert!(section.validate().is_ok());
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(PlanSection::default().validate().is_ok());
    }
}
