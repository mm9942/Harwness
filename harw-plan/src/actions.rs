//! Plan-Aktionen und -Events für `harw-plan`.
//!
//! Verantwortungsbereich: Definiert `PlanAction` (alle zulässigen Mutationen und
//! Lesezugriffe), `NodePatch` (partielles Update-Objekt für `UpdateNode`) sowie
//! `PlanEvent` (persistentes Ergebnis einer angewendeten Aktion).
//!
//! `PlanAction` wird mit `#[serde(tag = "op", rename_all = "snake_case")]`
//! serialisiert (Design-Doc §3).
//!
//! Exportierte Typen: [`PlanAction`], [`NodePatch`], [`PlanEvent`].

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{ContractRef, PathOrSymbol, PlanId, RevisionId, TaskId};

/// Deserialisiert ein `Option<Option<T>>` so, dass „fehlt" und „ist `null`"
/// unterscheidbar bleiben.
///
/// # Beschreibung
///
/// In einem [`NodePatch`] trägt die Schachtelung die Semantik: das äußere
/// `Option` unterscheidet „Feld nicht angefasst" (`None`) von „Feld angefasst"
/// (`Some`), das innere „neuer Wert" (`Some(v)`) von „Wert löschen" (`None`).
///
/// Standard-serde kann das nicht abbilden: es deserialisiert `null` direkt zu
/// `None` und macht damit aus „löschen" ein „nicht anfassen". Da Patches über
/// `history.jsonl` persistiert werden, wäre das ein stiller Datenverlust — ein
/// wiedereröffneter Knoten behielte seinen alten Worker.
///
/// Diese Funktion greift, weil `deserialize_with` **nur bei vorhandenem Feld**
/// aufgerufen wird: fehlt das Feld, liefert `default` das äußere `None`; ist es
/// `null`, entsteht `Some(None)`. Die Gegenrichtung braucht zusätzlich
/// `skip_serializing_if = "Option::is_none"` — sonst schriebe ein „nicht
/// angefasstes" Feld ein `null`, das beim Lesen zu „löschen" würde.
///
/// # Argumente
/// - `deserializer` (`D`): der serde-Deserialisierer des Feldes.
///
/// # Rückgabe
/// `Some(inner)`, wobei `inner` der deserialisierte `Option<T>`-Wert ist.
///
/// # Fehler
/// Gibt den Fehler des inneren Typs weiter, wenn der Wert weder `null` noch ein
/// gültiges `T` ist.
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
use crate::types::{
    Assignment, Criterion, EvidenceRef, InvalidationCondition, PlanNode, PlanNodeKind,
    PlanNodeStatus,
};

/// Partielles Update-Objekt für [`PlanAction::UpdateNode`].
///
/// # Description
/// Jedes Feld ist `Option`, wobei `None` bedeutet "unverändert lassen". Für
/// Felder, die selbst `Option<T>` sind (`wave`, `assignment`), zeigt das
/// äußere `Some` an, dass das Feld angefasst wurde — das innere `Option`
/// trägt den neuen Wert (inklusive `None`, um das Feld explizit zu löschen).
/// Design-Doc §3.
///
/// # Concurrency
/// `Clone + Send + Sync` (nur durch enthaltene Werttypen bedingt).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodePatch {
    /// Neues Ziel des Knotens, falls angefasst.
    #[serde(default)]
    pub objective: Option<String>,
    /// Neue Knotenart, falls angefasst.
    #[serde(default)]
    pub kind: Option<PlanNodeKind>,
    /// Neue Wave-Zuordnung. Äußeres `Some` = Feld angefasst; inneres `None`
    /// löscht die Wave-Zuordnung.
    ///
    /// Zu `deserialize_with`/`skip_serializing_if` siehe [`double_option`] —
    /// ohne beides überlebt „löschen" den JSON-Roundtrip nicht.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub wave: Option<Option<u32>>,
    /// Neue Abhängigkeitsliste, falls angefasst.
    #[serde(default)]
    pub dependencies: Option<Vec<TaskId>>,
    /// Neuer Lesebereich, falls angefasst.
    #[serde(default)]
    pub read_scope: Option<Vec<PathOrSymbol>>,
    /// Neuer Schreibbereich, falls angefasst.
    #[serde(default)]
    pub write_scope: Option<Vec<PathOrSymbol>>,
    /// Neuer verbotener Bereich, falls angefasst.
    #[serde(default)]
    pub forbidden_scope: Option<Vec<PathOrSymbol>>,
    /// Neue Eingangsverträge, falls angefasst.
    #[serde(default)]
    pub input_contracts: Option<Vec<ContractRef>>,
    /// Neue Ausgangsverträge, falls angefasst.
    #[serde(default)]
    pub output_contracts: Option<Vec<ContractRef>>,
    /// Neue Akzeptanzkriterien, falls angefasst.
    #[serde(default)]
    pub acceptance_criteria: Option<Vec<Criterion>>,
    /// Neue Zuweisung. Äußeres `Some` = Feld angefasst; inneres `None`
    /// entfernt die Zuweisung.
    ///
    /// Zu `deserialize_with`/`skip_serializing_if` siehe [`double_option`] —
    /// ohne beides überlebt „Zuweisung entfernen" den JSON-Roundtrip nicht,
    /// und ein wiedereröffneter Knoten behielte seinen alten Worker.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub assignment: Option<Option<Assignment>>,
}

impl NodePatch {
    /// Prüft, ob keines der Felder angefasst wurde.
    ///
    /// # Description
    /// Ein `NodePatch`, für das `is_empty()` `true` liefert, verändert bei
    /// Anwendung keinen Zustand — nützlich, um No-Op-`UpdateNode`-Aktionen
    /// vor der Anwendung zu verwerfen (Design-Doc §3).
    ///
    /// # Returns
    /// `true`, wenn alle Felder `None` sind, sonst `false`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::actions::NodePatch;
    /// let patch = NodePatch::default();
    /// assert!(patch.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        let Self {
            objective,
            kind,
            wave,
            dependencies,
            read_scope,
            write_scope,
            forbidden_scope,
            input_contracts,
            output_contracts,
            acceptance_criteria,
            assignment,
        } = self;
        objective.is_none()
            && kind.is_none()
            && wave.is_none()
            && dependencies.is_none()
            && read_scope.is_none()
            && write_scope.is_none()
            && forbidden_scope.is_none()
            && input_contracts.is_none()
            && output_contracts.is_none()
            && acceptance_criteria.is_none()
            && assignment.is_none()
    }
}

/// Eine Plan-Aktion — die einzige Art, den Store zu mutieren.
///
/// # Description
/// Reine Werttypen; kein `&mut PlanNode` in der API. Jede Mutation wird über
/// `PlanStore::apply(action, actor) -> PlanResult<PlanEvent>` gefahren.
///
/// Das `op`-Tag erlaubt einfaches JSON-Dispatching über das Feld `"op"`.
///
/// # Concurrency
/// `Clone + Send + Sync` (nur durch enthaltene Werttypen bedingt).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PlanAction {
    /// Legt einen neuen Plan an.
    Create {
        /// Bezeichner des neuen Plans.
        plan_id: PlanId,
        /// Ziel-Statement des Plans.
        goal: String,
    },
    /// Fügt einen neuen Knoten zum Plan hinzu.
    AddNode {
        /// Der hinzuzufügende Knoten.
        node: PlanNode,
    },
    /// Aktualisiert ausgewählte Felder eines vorhandenen Knotens.
    UpdateNode {
        /// Bezeichner des zu aktualisierenden Knotens.
        id: TaskId,
        /// Partielles Update; nur angefasste Felder werden übernommen
        /// (Design-Doc §3).
        patch: NodePatch,
    },
    /// Fügt eine Abhängigkeit zwischen zwei Knoten hinzu.
    AddDependency {
        /// Abhängiger Knoten (Child).
        child: TaskId,
        /// Abhängigkeitsquelle (Parent).
        parent: TaskId,
    },
    /// Setzt den Status eines Knotens.
    SetStatus {
        /// Bezeichner des Knotens.
        id: TaskId,
        /// Neuer Status.
        status: PlanNodeStatus,
        /// Optionaler Grund für den Statuswechsel.
        reason: Option<String>,
    },
    /// Hängt einen Evidenz-Nachweis an einen Knoten.
    AttachEvidence {
        /// Bezeichner des Knotens.
        id: TaskId,
        /// Anzuhängender Nachweis.
        evidence: EvidenceRef,
    },
    /// Invalidiert eine Menge von Knoten.
    Invalidate {
        /// Zu invalidierende Knoten.
        ids: Vec<TaskId>,
        /// Grund der Invalidierung.
        condition: InvalidationCondition,
    },
    /// Löst den Plan durch eine neue Revision ab.
    Supersede {
        /// Neue Eltern-Revision (muss größer als aktuelle Revision sein).
        new_parent_revision: RevisionId,
    },
    /// Zerlegt einen Knoten in mehrere Kindknoten.
    ///
    /// # Description
    /// `parent` wird zum Composite-Knoten; die `children` übernehmen dessen
    /// Verantwortung in kleineren Schritten. Der Schreibbereich jedes Kindes
    /// muss eine Teilmenge des Schreibbereichs von `parent` sein — dies wird
    /// in `validate.rs` geprüft, nicht hier (Design-Doc §3, §5).
    Expand {
        /// Zu zerlegender Elternknoten.
        parent: TaskId,
        /// Neue Kindknoten, die die Verantwortung von `parent` übernehmen.
        children: Vec<PlanNode>,
    },
    /// Ersetzt eine Gruppe abgeschlossener Research/Explore-Knoten durch
    /// einen einzelnen Contract-Knoten.
    ///
    /// # Description
    /// Die in `superseded` gelisteten Knoten (typischerweise
    /// `PlanNodeKind::Research` / `PlanNodeKind::Explore`) werden als
    /// `Superseded` markiert; ihre Evidenz wird auf `replacement`
    /// (typischerweise `PlanNodeKind::Contract`) übertragen, ergänzt um
    /// `summary` als Verdichtung des Ergebnisses (Design-Doc §3).
    Condense {
        /// Durch `replacement` ersetzte, abgeschlossene Knoten.
        superseded: Vec<TaskId>,
        /// Neuer Contract-Knoten, der die Verdichtung repräsentiert.
        replacement: PlanNode,
        /// Menschenlesbare Zusammenfassung der verdichteten Ergebnisse.
        summary: String,
    },
    /// Öffnet einen invalidierten Knoten erneut als neuen Ausführungsversuch.
    ///
    /// # Description
    /// Die Task-Identität (`id`) bleibt erhalten; der Store erhöht
    /// `assignment.attempt` und setzt den Status zurück in einen
    /// ausführbaren Zustand. `reason` dokumentiert, warum der Knoten erneut
    /// geöffnet wurde (Design-Doc §3).
    Reopen {
        /// Bezeichner des erneut zu öffnenden Knotens.
        id: TaskId,
        /// Grund für die Wiedereröffnung.
        reason: String,
    },
    /// Bindet den Plan an ein Goal.
    ///
    /// # Description
    /// `goal_id` referenziert ein Goal aus dem separaten `goal`-Modul (eigener
    /// AP); hier als roher `String` geführt, da der `GoalId`-Typ noch nicht
    /// existiert (Design-Doc §3).
    BindGoal {
        /// Bezeichner des zu bindenden Goals.
        goal_id: String,
    },
    /// Lese-Aktion: gibt den aktuellen Plan zurück ohne Mutation.
    Inspect,
}

/// Das persistente Ergebnis einer angewendeten Plan-Aktion.
///
/// # Description
/// Wird vom Store nach jedem `apply`-Aufruf zurückgegeben und an das
/// `history.jsonl`-Log angehängt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEvent {
    /// Sequenznummer des Events (entspricht der Revision nach der Aktion).
    pub revision: RevisionId,
    /// Die angewendete Aktion.
    pub action: PlanAction,
    /// Akteur, der die Aktion ausgelöst hat (runtime-gesetzt).
    pub actor: String,
    /// Zeitpunkt der Anwendung (runtime-gesetzt).
    #[serde(with = "time::serde::rfc3339")]
    pub applied_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PlanError;
    use crate::ids::{PathOrSymbol, RevisionId, TaskId};
    use crate::test_support::{TestError, TestResult};
    use crate::types::{Plan, PlanNode, PlanNodeStatus};
    use time::OffsetDateTime;

    fn test_plan(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            id: PlanId::new("p-actions-test"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "Test".to_owned(),
            goal_id: None,
            nodes,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn test_node(id: &str, status: PlanNodeStatus) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            kind: PlanNodeKind::Coding,
            wave: None,
            objective: "objective".to_owned(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: vec![PathOrSymbol::new(format!("src/{id}.rs"))],
            forbidden_scope: vec![],
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status,
            evidence: vec![],
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_plan_action_serde_roundtrip() -> TestResult {
        let action = PlanAction::SetStatus {
            id: TaskId::new("t-1"),
            status: PlanNodeStatus::Ready,
            reason: Some("alle Deps grün".to_owned()),
        };
        let json = serde_json::to_string(&action)?;
        let back: PlanAction = serde_json::from_str(&json)?;
        match back {
            PlanAction::SetStatus { id, status, .. } => {
                assert_eq!(id, TaskId::new("t-1"));
                assert_eq!(status, PlanNodeStatus::Ready);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Unerwartete Variante: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_plan_action_op_tag() -> TestResult {
        let action = PlanAction::Inspect;
        let json = serde_json::to_string(&action)?;
        assert!(
            json.contains("\"op\":\"inspect\""),
            "op-Tag fehlt oder falsch: {}",
            json
        );

        let create = PlanAction::Create {
            plan_id: PlanId::new("p-1"),
            goal: "Ziel".to_owned(),
        };
        let json2 = serde_json::to_string(&create)?;
        assert!(
            json2.contains("\"op\":\"create\""),
            "op-Tag für Create fehlt: {}",
            json2
        );
        Ok(())
    }

    #[test]
    fn test_update_node_patch_roundtrip() -> TestResult {
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t-1"),
            patch: NodePatch {
                objective: Some("neues Ziel".to_owned()),
                wave: Some(Some(2)),
                assignment: Some(None),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&action)?;
        assert!(
            json.contains("\"op\":\"update_node\""),
            "op-Tag für UpdateNode fehlt: {}",
            json
        );
        let back: PlanAction = serde_json::from_str(&json)?;
        match back {
            PlanAction::UpdateNode { id, patch } => {
                assert_eq!(id, TaskId::new("t-1"));
                assert_eq!(patch.objective, Some("neues Ziel".to_owned()));
                assert_eq!(patch.wave, Some(Some(2)));
                // Kein `PartialEq`-Vergleich auf `Assignment`/`PlanNodeKind`
                // nötig: die Enum-Shape genügt zur Prüfung.
                assert!(matches!(patch.assignment, Some(None)));
                assert!(patch.kind.is_none());
                assert!(patch.dependencies.is_none());
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Unerwartete Variante: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_node_patch_is_empty() {
        assert!(NodePatch::default().is_empty());

        let touched = NodePatch {
            objective: Some("angefasst".to_owned()),
            ..Default::default()
        };
        assert!(!touched.is_empty());

        // Auch das explizite Löschen eines optionalen Feldes (inneres `None`)
        // zählt als "angefasst".
        let cleared_wave = NodePatch {
            wave: Some(None),
            ..Default::default()
        };
        assert!(!cleared_wave.is_empty());
    }

    #[test]
    fn test_expand_action_serde_roundtrip() -> TestResult {
        let child = test_node("child-1", PlanNodeStatus::Draft);
        let action = PlanAction::Expand {
            parent: TaskId::new("parent-1"),
            children: vec![child],
        };
        let json = serde_json::to_string(&action)?;
        assert!(
            json.contains("\"op\":\"expand\""),
            "op-Tag für Expand fehlt: {}",
            json
        );
        let back: PlanAction = serde_json::from_str(&json)?;
        match back {
            PlanAction::Expand { parent, children } => {
                assert_eq!(parent, TaskId::new("parent-1"));
                assert_eq!(children.len(), 1);
                assert_eq!(children[0].id, TaskId::new("child-1"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Unerwartete Variante: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn status_actions_require_completed_dependencies() {
        let dependency = test_node("dependency", PlanNodeStatus::InProgress);
        let mut node = test_node("child", PlanNodeStatus::Draft);
        node.dependencies = vec![TaskId::new("dependency")];
        let plan = test_plan(vec![dependency, node]);

        let action = PlanAction::SetStatus {
            id: TaskId::new("child"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };

        // `DependencyNotCompleted`, nicht `IllegalTransition`: der Übergang
        // selbst ist zulässig, blockiert wird er von der Abhängigkeit. Die
        // Variante benennt beides — welcher Knoten, welche Abhängigkeit, in
        // welchem Status —, während `IllegalTransition` den Grund nur als
        // String trug.
        assert!(matches!(
            crate::validate::validate(&plan, &action),
            Err(PlanError::DependencyNotCompleted { .. })
        ));

        let mut missing_node = test_node("missing-child", PlanNodeStatus::Ready);
        missing_node.dependencies = vec![TaskId::new("missing")];
        let missing_action = PlanAction::SetStatus {
            id: TaskId::new("missing-child"),
            status: PlanNodeStatus::InProgress,
            reason: None,
        };
        assert!(matches!(
            crate::validate::validate(&test_plan(vec![missing_node]), &missing_action),
            Err(PlanError::NodeMissing { .. })
        ));
    }
}
