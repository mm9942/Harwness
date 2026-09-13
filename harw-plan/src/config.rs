//! Konfiguration des Plan-Tools.
//!
//! Verantwortungsbereich: `PlanToolConfig` mit `enabled = false` als Default
//! und `enabled_defaults()` für Tests (Design-Doc §6).
//!
//! Exportierte Typen: [`PlanToolConfig`].

use serde::{Deserialize, Serialize};

use crate::actions::PlanAction;
pub use crate::error::PlanToolConfigError;
use crate::types::{Plan, PlanNodeKind};

/// Konfiguration für das harw-plan-Tool.
///
/// # Description
/// Steuert, ob der `PlanStore` überhaupt konstruiert und exponiert wird.
/// Wenn `enabled = false`, darf kein Store erzeugt und kein Registry-Eintrag
/// vorgenommen werden (Design-Doc §6).
///
/// `Default::default()` liefert eine **deaktivierte** Konfiguration.
/// Für Tests steht `PlanToolConfig::enabled_defaults()` bereit.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::config::PlanToolConfig;
/// let cfg = PlanToolConfig::default();
/// assert!(!cfg.is_enabled());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanToolConfig {
    /// Aktiviert oder deaktiviert das Plan-Tool vollständig.
    #[serde(default)]
    pub enabled: bool,

    /// Aktiviert persistente Speicherung via `FilePlanStore`.
    #[serde(default)]
    pub persist: bool,

    /// Zwingt zur Planerstellung bei komplexer Arbeit.
    #[serde(default)]
    pub require_for_complex_work: bool,

    /// Aktiviert Zyklus-Erkennung in der Validation.
    #[serde(default = "PlanToolConfig::default_true")]
    pub validate_dependency_cycles: bool,

    /// Aktiviert WriteSet-Konflikt-Prüfung in der Validation.
    #[serde(default = "PlanToolConfig::default_true")]
    pub validate_write_conflicts: bool,

    /// Maximale Anzahl Knoten pro Plan.
    #[serde(default = "PlanToolConfig::default_max_nodes")]
    pub max_nodes: usize,

    /// Knotenarten, die vor der Ausführung eine **frische Exploration**
    /// verlangen (coding-philosophy §3: Arbeit beginnt mit Recherche).
    ///
    /// Ein Knoten dieser Art wird nur dann `Ready`, wenn entweder eine
    /// abgeschlossene `Explore`/`Research`-Abhängigkeit existiert oder ein
    /// eigenes `EvidenceRef` der Art `Finding` jünger als
    /// [`Self::exploration_ttl_secs`] anhängt. Leer = Regel deaktiviert.
    #[serde(default = "PlanToolConfig::default_require_exploration_for")]
    pub require_exploration_for: Vec<PlanNodeKind>,

    /// Höchstalter eines `Finding`-Nachweises in Sekunden, damit er als
    /// „frische Exploration" gilt (Default: 24 h).
    #[serde(default = "PlanToolConfig::default_exploration_ttl_secs")]
    pub exploration_ttl_secs: u64,

    /// Maximale Verschachtelungstiefe von `Expand` (Composite-Ketten).
    /// Verhindert, dass ein Plan sich unbegrenzt nach innen aufspaltet.
    #[serde(default = "PlanToolConfig::default_max_expand_depth")]
    pub max_expand_depth: u32,
}

impl PlanToolConfig {
    fn default_true() -> bool {
        true
    }

    /// Standard: Coding- und Integrationsknoten verlangen Vor-Exploration.
    fn default_require_exploration_for() -> Vec<PlanNodeKind> {
        vec![PlanNodeKind::Coding, PlanNodeKind::Integration]
    }

    /// Standard: 24 Stunden.
    fn default_exploration_ttl_secs() -> u64 {
        86_400
    }

    /// Standard: drei Ebenen `Expand`.
    fn default_max_expand_depth() -> u32 {
        3
    }

    fn default_max_nodes() -> usize {
        256
    }

    /// Gibt zurück, ob das Plan-Tool aktiviert ist.
    ///
    /// # Returns
    /// `true` wenn `enabled == true`.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Prüft strukturelle Invarianten der Konfiguration.
    ///
    /// Ein Knotenlimit von null wäre nicht durchsetzbar, da bereits ein
    /// `Create`-Plan einen späteren `AddNode` niemals zulassen könnte.
    pub fn validate(&self) -> Result<(), PlanToolConfigError> {
        if self.max_nodes == 0 {
            return Err(PlanToolConfigError::ZeroMaxNodes);
        }

        Ok(())
    }

    /// Stellt sicher, dass das Tool aktiviert und seine Konfiguration gültig ist.
    ///
    /// Store-Fabriken und Adapter sollen diese Prüfung vor dem Exponieren oder
    /// Anwenden von Plan-Operationen aufrufen.
    pub fn require_enabled(&self) -> Result<(), PlanToolConfigError> {
        self.validate()?;

        if !self.enabled {
            return Err(PlanToolConfigError::Disabled);
        }

        Ok(())
    }

    /// Prüft, ob ein vollständiger Plan innerhalb des konfigurierten Limits liegt.
    pub fn validate_plan(&self, plan: &Plan) -> Result<(), PlanToolConfigError> {
        self.require_enabled()?;
        self.validate_node_count(plan.nodes.len())
    }

    /// Prüft eine Aktion gegen die aktuelle Knotenzahl des Plans.
    ///
    /// Nur [`PlanAction::AddNode`] erhöht nach den derzeitigen Aktionstypen
    /// die Knotenzahl. Der Aufrufer liefert die Anzahl *vor* der Aktion, damit
    /// diese Methode ohne Store-Zugriff rein und vor einer Mutation nutzbar ist.
    pub fn validate_action(
        &self,
        action: &PlanAction,
        current_node_count: usize,
    ) -> Result<(), PlanToolConfigError> {
        self.require_enabled()?;

        if matches!(action, PlanAction::AddNode { .. }) {
            let attempted_nodes = current_node_count.saturating_add(1);
            self.validate_node_count(attempted_nodes)?;
        }

        Ok(())
    }

    fn validate_node_count(&self, attempted_nodes: usize) -> Result<(), PlanToolConfigError> {
        if attempted_nodes > self.max_nodes {
            return Err(PlanToolConfigError::NodeLimitExceeded {
                max_nodes: self.max_nodes,
                attempted_nodes,
            });
        }

        Ok(())
    }

    /// Erstellt eine vollständig aktivierte Standardkonfiguration für Tests.
    ///
    /// # Description
    /// Alle Features aktiv, `persist = false` (kein Dateisystem in Unit-Tests).
    /// `max_nodes = 256`.
    ///
    /// # Returns
    /// `PlanToolConfig` mit `enabled = true`.
    pub fn enabled_defaults() -> Self {
        Self {
            enabled: true,
            persist: false,
            require_for_complex_work: false,
            validate_dependency_cycles: true,
            validate_write_conflicts: true,
            max_nodes: 256,
            // Bewusst leer: dieser Test-/Bootstrap-Helfer soll einen Plan ohne
            // vorgelagerte Exploration zulassen. Die *produktive* Konfiguration
            // aus `[tools.plan]` erbt dagegen den serde-Default
            // (`default_require_exploration_for`) und erzwingt die Regel.
            require_exploration_for: Vec::new(),
            exploration_ttl_secs: Self::default_exploration_ttl_secs(),
            max_expand_depth: Self::default_max_expand_depth(),
        }
    }
}

impl Default for PlanToolConfig {
    /// Liefert eine **deaktivierte** Konfiguration (Design-Doc §6).
    ///
    /// # Returns
    /// `PlanToolConfig` mit `enabled = false`.
    fn default() -> Self {
        Self {
            enabled: false,
            persist: false,
            require_for_complex_work: false,
            validate_dependency_cycles: true,
            validate_write_conflicts: true,
            max_nodes: 256,
            require_exploration_for: Self::default_require_exploration_for(),
            exploration_ttl_secs: Self::default_exploration_ttl_secs(),
            max_expand_depth: Self::default_max_expand_depth(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Vier Importe weniger als vorher: `PathOrSymbol`, `TaskId`,
    // `PlanNodeKind` und `PlanNodeStatus` brauchte nur das abgelöste
    // Struct-Literal. Das Fixture-Makro nennt sie intern.
    use crate::ids::{PlanId, RevisionId};
    use crate::types::PlanNode;
    use time::OffsetDateTime;

    /// Erzeugt einen Test-Knoten über das Fixture-Makro.
    ///
    /// Vorher stand hier ein 18-Felder-Struct-Literal — dasselbe, das in zehn
    /// weiteren Modulen wiederholt wurde. Genau dieses Literal war der Grund,
    /// warum die vier neuen `PlanNode`-Felder an mehreren Stellen nachgezogen
    /// werden mussten und eine davon erst bei `--all-targets` auffiel. Über
    /// [`crate::plan_node!`] trägt eine künftige Felderweiterung nur noch eine
    /// Stelle.
    fn node(id: &str) -> PlanNode {
        crate::plan_node!(
            id,
            objective: "test node",
            write: ["src/lib.rs"],
        )
    }

    fn plan_with_nodes(count: usize) -> Plan {
        Plan {
            id: PlanId::new("p-config"),
            revision: RevisionId::new(0),
            parent_revision: None,
            goal_statement: "test plan".to_owned(),
            goal_id: None,
            nodes: (0..count)
                .map(|index| node(&format!("t-{index}")))
                .collect(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_default_is_disabled() {
        let cfg = PlanToolConfig::default();
        assert!(!cfg.is_enabled(), "Default muss deaktiviert sein");
        assert!(!cfg.persist);
        assert_eq!(cfg.max_nodes, 256);
    }

    #[test]
    fn test_enabled_defaults() {
        let cfg = PlanToolConfig::enabled_defaults();
        assert!(cfg.is_enabled(), "enabled_defaults muss aktiviert sein");
        assert!(cfg.validate_dependency_cycles);
        assert!(cfg.validate_write_conflicts);
        assert_eq!(cfg.max_nodes, 256);
    }

    #[test]
    fn test_validate_rejects_zero_max_nodes() {
        let cfg = PlanToolConfig {
            max_nodes: 0,
            ..PlanToolConfig::enabled_defaults()
        };

        assert_eq!(cfg.validate(), Err(PlanToolConfigError::ZeroMaxNodes));
    }

    #[test]
    fn test_validate_plan_rejects_disabled_tool() {
        let cfg = PlanToolConfig::default();

        assert_eq!(
            cfg.validate_plan(&plan_with_nodes(0)),
            Err(PlanToolConfigError::Disabled)
        );
    }

    #[test]
    fn test_validate_plan_rejects_node_limit_exceeded() {
        let cfg = PlanToolConfig {
            max_nodes: 1,
            ..PlanToolConfig::enabled_defaults()
        };

        assert_eq!(
            cfg.validate_plan(&plan_with_nodes(2)),
            Err(PlanToolConfigError::NodeLimitExceeded {
                max_nodes: 1,
                attempted_nodes: 2,
            })
        );
    }

    #[test]
    fn test_validate_action_rejects_add_node_at_limit() {
        let cfg = PlanToolConfig {
            max_nodes: 1,
            ..PlanToolConfig::enabled_defaults()
        };
        let action = PlanAction::AddNode { node: node("t-2") };

        assert_eq!(
            cfg.validate_action(&action, 1),
            Err(PlanToolConfigError::NodeLimitExceeded {
                max_nodes: 1,
                attempted_nodes: 2,
            })
        );
    }

    #[test]
    fn test_validate_action_allows_non_node_mutations_at_limit() {
        let cfg = PlanToolConfig {
            max_nodes: 1,
            ..PlanToolConfig::enabled_defaults()
        };
        let action = PlanAction::Inspect;

        assert_eq!(cfg.validate_action(&action, 1), Ok(()));
    }
}
