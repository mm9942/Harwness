//! Plan-Knoten als Fragmente — begrenzt auf das eigene Revier.
//!
//! # Verantwortungsbereich
//! [`PlanContextProvider`] ist die erste der drei AW3-03-Quellenbindungen:
//! sie liefert [`harw_context::Fragment`]e aus dem aktuellen
//! [`harw_plan::PlanStore`]. Anders als [`crate::goal_context::GoalContextProvider`]
//! (der immer *ein* zusammengefasstes Ziel-Fragment liefert) liefert dieser
//! Provider *ein Fragment je Plan-Knoten* — und zwar nur für Knoten im
//! eigenen Revier: ein Clan-Arbeiter sieht die Knoten seines Reviers, nicht
//! die der anderen.
//!
//! # Das Revier ist ein Glob
//! Das Revier wird als einzelnes Glob-Muster konstruiert (typischerweise das
//! `members_from_plan`-Muster der eigenen Zelle, siehe [`crate::cells::CellPlan`]
//! — optional bereits mit dem `plan_scope` des eigenen Clans geschnitten,
//! genau wie [`crate::cells::CellPlan::from_cell`] es tut). Ein Knoten gehört
//! zum Revier, wenn das Muster entweder seine `TaskId` **oder** einen
//! Eintrag seines `write_scope` trifft — dieselbe Zwei-Wege-Prüfung wie
//! `crate::cells::node_matches`.
//!
//! **Wiederverwendung statt einer dritten Glob-Implementierung:** der
//! Workspace trägt bereits zwei gleichbedeutende Glob-Abgleiche
//! (`harw_plan::admission::glob_matches`, exponiert über
//! [`harw_plan::ScopeMatcher::matches_glob`], und `harw_context::selector`s
//! bewusst zweite, unabhängige Neuimplementierung derselben Mini-Syntax für
//! ein anderes Namensvokabular). Diese Datei fügt keine dritte hinzu — sie
//! ruft [`harw_plan::ScopeMatcher::matches_glob`] auf, exakt wie
//! `crate::cells::node_matches` es bereits tut.
//!
//! # Warum je Knoten ein eigenes Fragment
//! Ein Plan kann Dutzende Knoten enthalten; ein einzelnes, alles
//! zusammenfassendes Fragment ließe sich weder gezielt budgetieren
//! (`harw_context::ContextBudgetSpec::section_budget` wirkt pro Sektion, hier
//! `"plan.nodes"`) noch gezielt ausschließen (`must_include`/`exclude` in
//! `harw_agent_dsl::executable::ContextProgram` matcht Selektoren gegen
//! `Fragment::label`, die hier die `TaskId` ist). Ein Fragment je Knoten
//! macht beides möglich.
//!
//! # Wo die Sichtbarkeit durchgesetzt wird
//! Genau einmal: in [`PlanContextProvider::node_in_scope`], aufgerufen aus
//! [`PlanContextProvider::fragments`], bevor ein Knoten überhaupt zu
//! [`PlanContextProvider::fragment_for_node`] gelangt. Ein Knoten außerhalb
//! des Reviers erreicht die Fragment-Konstruktion nie — es gibt keinen
//! zweiten Pfad, der ungefiltert auf `plan.nodes` zugreift.
//!
//! # Vertrauen und Namensraum
//! [`PLAN_CONTEXT_NAMESPACE`] (`"plan"`), [`PLAN_CONTEXT_MAX_TRUST`]
//! ([`harw_context::TrustClass::Evidence`]) und
//! [`PLAN_CONTEXT_MAY_CARRY_USER_CONTENT`] (`true` — ein `objective`-Text
//! kann von einem Modell oder Operator während der Planung frei verfasst
//! worden sein) sind die Deklaration, mit der dieser Provider sich bei
//! [`crate::fragment_registry::FragmentProviderRegistry`] anmeldet. `Evidence`
//! statt `Data`, weil Plan-Knoten durch den kontrollierten Plan-/Goal-Prozess
//! laufen (nicht roh übernommener externer Text) — aber nie `Instruction`,
//! weil `PLAN_CONTEXT_MAY_CARRY_USER_CONTENT == true` das strukturell
//! verbietet.
//!
//! # Exportierte Typen
//! [`PlanContextProvider`], [`PLAN_CONTEXT_NAMESPACE`],
//! [`PLAN_CONTEXT_MAX_TRUST`], [`PLAN_CONTEXT_MAY_CARRY_USER_CONTENT`].
//!
//! # Concurrency
//! `Send + Sync`; hält nur einen `Arc<dyn PlanStore>` und einen `String`.
//! `fragments` liest den Store synchron und macht keine I/O jenseits dessen.
//!
//! # Fehler
//! [`crate::error::PlanBridgeError::CellMemberPattern`] beim Konstruieren mit
//! einem leeren oder nur aus Leerzeichen bestehenden Revier-Muster —
//! derselbe Fehler, den ein leeres `members_from_plan` bereits in
//! [`crate::cells::CellPlan::from_cell`] auslöst.

use std::sync::Arc;

use harw_context::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};
use harw_lens_types::{BytesOverFour, CostEstimator};
use harw_plan::{PlanNode, PlanStore, ScopeMatcher};

use crate::error::PlanBridgeError;

/// Sektionspräfix, unter dem [`PlanContextProvider`] liefert.
pub const PLAN_CONTEXT_NAMESPACE: &str = "plan";

/// Höchste Vertrauensklasse, die [`PlanContextProvider`] behaupten darf.
pub const PLAN_CONTEXT_MAX_TRUST: TrustClass = TrustClass::Evidence;

/// `true`: `objective`-Texte können von einem Modell oder Operator während
/// der Planung frei verfasst worden sein — [`PlanContextProvider`] darf
/// deshalb niemals `TrustClass::Instruction` behaupten (siehe Moduldoku).
pub const PLAN_CONTEXT_MAY_CARRY_USER_CONTENT: bool = true;

/// Einzige Sektion, unter der dieser Provider Fragmente liefert.
const SECTION: &str = "plan.nodes";

/// Name dieses Providers, für [`FragmentOrigin::provider`] und
/// Registrierungs-Deklarationen.
const PROVIDER_NAME: &str = "PlanContextProvider";

/// Liefert Plan-Knoten als [`harw_context::Fragment`]e, begrenzt auf das
/// eigene Revier.
///
/// # Description
/// Siehe Moduldokumentation. Ein Knoten wird zu genau einem Fragment; Knoten
/// außerhalb des Reviers werden nie erreicht (siehe [`Self::node_in_scope`]).
///
/// # Concurrency
/// `Send + Sync`; hält nur einen `Arc<dyn PlanStore>` und einen `String`.
///
/// # Examples
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_plan::{InMemoryPlanStore, PlanStore};
/// use harw_plan_bridge::plan_context::PlanContextProvider;
///
/// let plan: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
/// let provider = PlanContextProvider::try_new(plan, "harw-tui/**")
///     .expect("nicht-leeres Muster konstruiert immer erfolgreich");
/// let fragments = provider.fragments(jiff::Timestamp::UNIX_EPOCH);
/// assert!(fragments.is_empty(), "leerer Store liefert nichts");
/// ```
pub struct PlanContextProvider {
    /// Quelle der Plan-Knoten.
    plan: Arc<dyn PlanStore>,
    /// Das Revier-Glob: trifft `TaskId` oder einen `write_scope`-Eintrag.
    scope_pattern: String,
}

/// Zeigt das Revier-Glob, nicht den Plan-Speicher.
///
/// Ein abgeleitetes `Debug` scheitert am `Arc<dyn PlanStore>`. Das Glob ist
/// die Angabe, die einen Provider unterscheidbar macht; der Speicherinhalt
/// wäre Plandaten, die in einem Log nichts zu suchen haben.
impl std::fmt::Debug for PlanContextProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanContextProvider")
            .field("scope_pattern", &self.scope_pattern)
            .finish_non_exhaustive()
    }
}

impl PlanContextProvider {
    /// Konstruiert einen Provider für ein gegebenes Revier.
    ///
    /// # Arguments
    /// - `plan` (`Arc<dyn PlanStore>`): Quelle der Plan-Knoten.
    /// - `scope_pattern` (`impl Into<String>`): das Revier-Glob, typischerweise
    ///   das `members_from_plan`-Muster der eigenen Zelle (optional bereits
    ///   mit dem `plan_scope` des Clans geschnitten).
    ///
    /// # Returns
    /// `Ok(Self)`, wenn das getrimmte Muster nicht leer ist.
    ///
    /// # Errors
    /// [`PlanBridgeError::CellMemberPattern`], wenn `scope_pattern` leer oder
    /// nur aus Leerzeichen besteht — ein Muster, das nichts aussagt, darf
    /// nicht stillschweigend „alles" oder „nichts" bedeuten (derselbe
    /// Grundsatz wie bei einem leeren `members_from_plan`).
    ///
    /// # Examples
    /// ```rust
    /// use std::sync::Arc;
    /// use harw_plan::InMemoryPlanStore;
    /// use harw_plan_bridge::plan_context::PlanContextProvider;
    ///
    /// assert!(PlanContextProvider::try_new(Arc::new(InMemoryPlanStore::new()), "   ").is_err());
    /// ```
    pub fn try_new(
        plan: Arc<dyn PlanStore>,
        scope_pattern: impl Into<String>,
    ) -> Result<Self, PlanBridgeError> {
        let scope_pattern = scope_pattern.into();
        if scope_pattern.trim().is_empty() {
            return Err(PlanBridgeError::CellMemberPattern {
                pattern: scope_pattern,
            });
        }
        Ok(Self {
            plan,
            scope_pattern,
        })
    }

    /// Das geltende Revier-Muster.
    #[must_use]
    pub fn scope_pattern(&self) -> &str {
        &self.scope_pattern
    }

    /// Baut die Registrierungs-Deklaration dieses Providers.
    ///
    /// # Description
    /// Bündelt [`PLAN_CONTEXT_NAMESPACE`], [`PLAN_CONTEXT_MAX_TRUST`] und
    /// [`PLAN_CONTEXT_MAY_CARRY_USER_CONTENT`] zu einer
    /// [`crate::fragment_registry::FragmentProviderDeclaration`], die direkt
    /// an [`crate::fragment_registry::FragmentProviderRegistry::try_register`]
    /// übergeben werden kann.
    ///
    /// # Returns
    /// Die Deklaration dieses Providers.
    ///
    /// # Examples
    /// ```rust
    /// use harw_plan_bridge::fragment_registry::FragmentProviderRegistry;
    /// use harw_plan_bridge::plan_context::PlanContextProvider;
    ///
    /// let mut registry = FragmentProviderRegistry::new();
    /// registry
    ///     .try_register(PlanContextProvider::declaration())
    ///     .expect("PlanContextProvider claims a fresh namespace with a legal trust ceiling");
    /// ```
    #[must_use]
    pub fn declaration() -> crate::fragment_registry::FragmentProviderDeclaration {
        crate::fragment_registry::FragmentProviderDeclaration {
            provider_name: PROVIDER_NAME,
            namespace: PLAN_CONTEXT_NAMESPACE.to_owned(),
            max_trust: PLAN_CONTEXT_MAX_TRUST,
            may_carry_user_content: PLAN_CONTEXT_MAY_CARRY_USER_CONTENT,
            cost_estimator: Arc::new(BytesOverFour),
        }
    }

    /// Prüft, ob ein Knoten im eigenen Revier liegt.
    ///
    /// # Description
    /// Trifft entweder die `TaskId` des Knotens oder einen Eintrag seines
    /// `write_scope` auf [`Self::scope_pattern`], über
    /// [`ScopeMatcher::matches_glob`] — dieselbe Zwei-Wege-Prüfung wie
    /// `crate::cells::node_matches`. Das ist die einzige Stelle, an der diese
    /// Sichtbarkeitsentscheidung getroffen wird (siehe Moduldoku).
    ///
    /// # Arguments
    /// - `node` (`&PlanNode`): der zu prüfende Knoten.
    ///
    /// # Returns
    /// `true`, wenn der Knoten zum Revier gehört.
    #[must_use]
    fn node_in_scope(&self, node: &PlanNode) -> bool {
        if ScopeMatcher::matches_glob(&self.scope_pattern, node.id.as_str()) {
            return true;
        }
        node.write_scope
            .iter()
            .any(|scope| ScopeMatcher::matches_glob(&self.scope_pattern, scope.as_str()))
    }

    /// Baut die Fragmente für alle Knoten im eigenen Revier.
    ///
    /// # Description
    /// Liest den Plan einmal synchron; fehlt er, ist das Ergebnis eine leere
    /// Liste (kein Fehler — ein fehlender Plan hat schlicht nichts
    /// beizutragen, wie bei [`crate::goal_context::GoalContextProvider`]).
    /// Jeder Knoten im Revier ([`Self::node_in_scope`]) wird zu genau einem
    /// Fragment ([`Self::fragment_for_node`]); ein Knoten, dessen `TaskId`
    /// kein gültiges [`FragmentLabel`] ergibt, wird protokolliert und
    /// übersprungen statt die gesamte Liste scheitern zu lassen.
    ///
    /// # Arguments
    /// - `produced_at` (`jiff::Timestamp`): Zeitstempel für
    ///   [`FragmentOrigin::produced_at`]. Injiziert statt aus der Systemuhr
    ///   gelesen, damit derselbe Plan immer dasselbe Ergebnis liefert.
    ///
    /// # Returns
    /// Ein [`harw_context::Fragment`] je Knoten im Revier, in
    /// `plan.nodes`-Reihenfolge.
    ///
    /// # Concurrency
    /// Liest den Store einmal synchron; hält danach keine Locks mehr.
    #[must_use]
    pub fn fragments(&self, produced_at: jiff::Timestamp) -> Vec<Fragment> {
        let plan = match self.plan.current() {
            Ok(plan) => plan,
            Err(error) => {
                tracing::debug!(error = %error, "Kein Plan vorhanden — keine Plan-Fragmente");
                return Vec::new();
            }
        };

        let Ok(section) = SectionName::try_new(SECTION) else {
            // SECTION ist ein fester, nicht-leerer, steuerzeichenfreier
            // Literal-String — dieser Zweig ist unerreichbar, existiert aber,
            // weil `try_new` fehlbar ist und dieser Provider nie `unwrap()`
            // verwendet.
            return Vec::new();
        };

        plan.nodes
            .iter()
            .filter(|node| self.node_in_scope(node))
            .filter_map(|node| self.fragment_for_node(node, &section, produced_at))
            .collect()
    }

    /// Baut ein einzelnes Fragment aus einem Plan-Knoten.
    ///
    /// # Description
    /// `label` ist die `TaskId` des Knotens, `body` fasst Art, Status und
    /// Ziel zusammen. `trust` ist immer [`PLAN_CONTEXT_MAX_TRUST`]; dieser
    /// Provider liefert keine abgestufte Vertrauensklasse je Knoten, weil
    /// jeder Plan-Knoten durch denselben kontrollierten Plan-Prozess läuft.
    /// `stability` ist immer [`Stability::Fresh`] — ein Knotenstatus ändert
    /// sich zwischen Turns, und diese Datei liest den Plan bei jedem Aufruf
    /// neu, statt eine Beständigkeit zu behaupten, die niemand garantiert.
    fn fragment_for_node(
        &self,
        node: &PlanNode,
        section: &SectionName,
        produced_at: jiff::Timestamp,
    ) -> Option<Fragment> {
        let label = match FragmentLabel::try_new(node.id.as_str()) {
            Ok(label) => label,
            Err(error) => {
                tracing::warn!(
                    task = %node.id,
                    error = %error,
                    "Plan-Knoten ergibt kein gültiges Fragment-Label — übersprungen"
                );
                return None;
            }
        };

        let body = format!(
            "{} [{:?}] ({:?}): {}",
            node.id,
            node.kind,
            node.status,
            node.objective.trim()
        );
        let cost = BytesOverFour.estimate(&body);
        let digest = harw_types::ContentDigest::of(body.as_bytes());

        Some(Fragment {
            label,
            section: section.clone(),
            trust: PLAN_CONTEXT_MAX_TRUST,
            stability: Stability::Fresh,
            origin: FragmentOrigin {
                provider: PROVIDER_NAME.to_owned(),
                namespace: PLAN_CONTEXT_NAMESPACE.to_owned(),
                produced_at,
            },
            cost,
            digest,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testing::coding_node;
    use harw_plan::{PathOrSymbol, PlanNodeStatus};

    fn node_writing(id: &str, paths: &[&str]) -> PlanNode {
        let mut node = coding_node(id, PlanNodeStatus::Ready);
        node.write_scope = paths.iter().map(|path| PathOrSymbol::new(*path)).collect();
        node
    }

    fn provider_with(pattern: &str, nodes: Vec<PlanNode>) -> TestResult<PlanContextProvider> {
        let store = crate::testing::seeded_plan_store(nodes)?;
        PlanContextProvider::try_new(Arc::new(store), pattern)
            .map_err(ctx("nicht-leeres Muster konstruiert immer erfolgreich"))
    }

    #[test]
    fn test_try_new_rejects_blank_scope_pattern() -> TestResult {
        let plan: Arc<dyn PlanStore> = Arc::new(harw_plan::InMemoryPlanStore::new());
        match PlanContextProvider::try_new(plan, "   ") {
            Err(PlanBridgeError::CellMemberPattern { pattern }) => {
                assert_eq!(pattern, "   ");
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "erwartet CellMemberPattern, bekommen: {other:?}"
            ))),
        }
    }

    /// Der wichtigste Test dieser Aufgabe: ein Knoten eines fremden Clans
    /// (anderes `write_scope`-Revier) erscheint nicht in den Fragmenten.
    #[test]
    fn test_fragments_exclude_nodes_of_a_foreign_clan() -> TestResult {
        let provider = provider_with(
            "harw-tui/**",
            vec![
                node_writing("tui-1", &["harw-tui/src/a.rs"]),
                node_writing("cli-1", &["harw-cli/src/b.rs"]),
            ],
        )?;

        let fragments = provider.fragments(jiff::Timestamp::UNIX_EPOCH);

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].label.as_str(), "tui-1");
        assert!(
            fragments.iter().all(|f| f.label.as_str() != "cli-1"),
            "fremder Clan-Knoten ist durchgerutscht: {fragments:?}"
        );
        Ok(())
    }

    #[test]
    fn test_fragments_match_by_task_id_glob_too() -> TestResult {
        let provider = provider_with(
            "tui-*",
            vec![
                node_writing("tui-1", &["harw-cli/src/a.rs"]),
                node_writing("cli-1", &["harw-cli/src/b.rs"]),
            ],
        )?;

        let fragments = provider.fragments(jiff::Timestamp::UNIX_EPOCH);

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].label.as_str(), "tui-1");
        Ok(())
    }

    #[test]
    fn test_fragment_fields_are_fully_populated() -> TestResult {
        let provider = provider_with("t-*", vec![node_writing("t-1", &["src/a.rs"])])?;
        let produced_at = jiff::Timestamp::UNIX_EPOCH;

        let fragments = provider.fragments(produced_at);
        assert_eq!(fragments.len(), 1);
        let fragment = &fragments[0];

        assert_eq!(fragment.section.as_str(), SECTION);
        assert_eq!(fragment.trust, PLAN_CONTEXT_MAX_TRUST);
        assert_eq!(fragment.stability, Stability::Fresh);
        assert_eq!(fragment.origin.provider, PROVIDER_NAME);
        assert_eq!(fragment.origin.namespace, PLAN_CONTEXT_NAMESPACE);
        assert_eq!(fragment.origin.produced_at, produced_at);
        assert!(fragment.cost.0 > 0, "cost muss aus dem Body geschätzt sein");
        assert_ne!(
            fragment.digest,
            harw_types::ContentDigest::of(b""),
            "digest muss aus dem tatsächlichen Body berechnet sein"
        );
        assert!(fragment.body.contains("t-1"));
        Ok(())
    }

    #[test]
    fn test_no_plan_contributes_nothing() -> TestResult {
        let plan: Arc<dyn PlanStore> = Arc::new(harw_plan::InMemoryPlanStore::new());
        let provider = PlanContextProvider::try_new(plan, "t-*")
            .map_err(ctx("nicht-leeres Muster konstruiert immer erfolgreich"))?;
        assert!(provider.fragments(jiff::Timestamp::UNIX_EPOCH).is_empty());
        Ok(())
    }

    #[test]
    fn test_pattern_without_matches_yields_no_fragments() -> TestResult {
        let provider = provider_with("nichts-*", vec![node_writing("t-1", &["src/a.rs"])])?;
        assert!(provider.fragments(jiff::Timestamp::UNIX_EPOCH).is_empty());
        Ok(())
    }

    #[test]
    fn test_declaration_registers_successfully() -> TestResult {
        let mut registry = crate::fragment_registry::FragmentProviderRegistry::new();
        registry
            .try_register(PlanContextProvider::declaration())
            .map_err(ctx(
                "PlanContextProvider's own declaration must pass its own registry",
            ))?;
        assert_eq!(
            registry.max_trust(PLAN_CONTEXT_NAMESPACE),
            Some(PLAN_CONTEXT_MAX_TRUST)
        );
        Ok(())
    }
}
