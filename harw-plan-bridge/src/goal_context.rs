//! Das Ziel überlebt Compact und Modellwechsel.
//!
//! # Verantwortungsbereich
//! [`GoalContextProvider`] ist ein `harw_extension_api::ContextProvider`, der
//! in **jedem** Turn dasselbe kompakte Fragment beisteuert: Zielsatz, offene
//! Akzeptanzkriterien, Invarianten, die aktuell ausführbaren Knoten und die
//! Coverage aus `harw_plan::goal::evaluate_goal`.
//!
//! Das ist der Punkt, an dem philosophy.md §5 praktisch wird: ein
//! Context-Compact, ein Modellwechsel oder ein neuer Worker dürfen das Ziel
//! nicht verlieren. Weil das Fragment bei jedem Turn frisch aus den Stores
//! gebildet wird, kann es weder veralten noch aus dem Fenster fallen.
//!
//! # Warum der `impl` von Hand geschrieben ist
//! Das Attribut-Makro `#[harw_macros::context_provider]` erzeugt aus einer
//! freien `async fn(ctx: &TurnInputContext, state: &StateType)` eine Struct
//! mit **genau einem** Feld `state` und einem `new(state)`-Konstruktor
//! (siehe `harw-macros/src/contributor.rs`, `expand_context_provider`). Der
//! hier verlangte Provider hält drei Werte (Goal-Store, Plan-Store,
//! Zeichenlimit) und soll sie einzeln benennen; er müsste sie sonst in eine
//! künstliche State-Struct verpacken und hieße dann `GoalContextProvider {
//! state: … }`. Die Makro-Signatur passt also nicht — der `impl` steht von
//! Hand, das `Box::pin`-Boilerplate ist genau eine Zeile.
//!
//! # Deckelung
//! Der Inhalt ist auf `max_chars` Zeichen begrenzt (Default
//! [`DEFAULT_MAX_CHARS`]). Gekappt wird auf Zeichen-, nicht auf Byte-Grenzen,
//! und das Ende wird mit `…` markiert, damit ein Leser sieht, dass etwas fehlt.
//!
//! # Exportierte Typen
//! [`GoalContextProvider`], [`DEFAULT_MAX_CHARS`].
//!
//! # Concurrency
//! `Send + Sync`; hält nur `Arc`-Zeiger auf die Stores und erzeugt pro Aufruf
//! einen frischen String. `contribute` blockiert nicht — es liest die Stores
//! synchron und verpackt das Ergebnis in ein bereits fertiges Future.
//!
//! # Fehler
//! Keine. Ist kein Goal oder kein Plan vorhanden, ist das Ergebnis eine leere
//! Fragmentliste — ein fehlendes Ziel ist zur Kontext-Zusammenstellung kein
//! Fehler, sondern schlicht nichts beizutragen.

use std::sync::Arc;

use harw_extension_api::contributors::{ContextProvider, ExtFuture};
use harw_extension_api::types::{ContextFragment, TurnInputContext};
use harw_plan::goal::{GoalStore, evaluate_goal};
use harw_plan::{PlanNodeKind, PlanStore};

/// Voreingestelltes Zeichenlimit eines Ziel-Fragments.
pub const DEFAULT_MAX_CHARS: usize = 2000;

/// Beschriftung des beigesteuerten Fragments.
const FRAGMENT_LABEL: &str = "plan-goal";

/// Injiziert Goal, offene Kriterien und ausführbare Knoten in jeden Turn.
///
/// # Description
/// Siehe Modul-Dokumentation. Der Provider hält keine Kopie des Ziels — er
/// liest bei jedem Turn den aktuellen Zustand, damit ein verfeinertes Goal
/// sofort wirkt.
///
/// # Concurrency
/// `Send + Sync`; klont beim Erzeugen nur `Arc`-Zeiger.
///
/// # Examples
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_plan::{InMemoryPlanStore, PlanStore};
/// use harw_plan_bridge::GoalContextProvider;
///
/// # fn demo(goal: Arc<dyn harw_plan::goal::GoalStore>) {
/// let plan: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
/// let provider = GoalContextProvider::new(goal, plan);
/// assert_eq!(provider.max_chars(), harw_plan_bridge::DEFAULT_MAX_CHARS);
/// # }
/// ```
pub struct GoalContextProvider {
    /// Quelle des Ziels.
    goal: Arc<dyn GoalStore>,
    /// Quelle des Plans (für ausführbare Knoten und Coverage).
    plan: Arc<dyn PlanStore>,
    /// Obergrenze des beigesteuerten Textes in Zeichen.
    max_chars: usize,
}

impl GoalContextProvider {
    /// Erzeugt einen Provider mit dem Standardlimit.
    ///
    /// # Arguments
    /// - `goal` (`Arc<dyn GoalStore>`): Quelle des Ziels.
    /// - `plan` (`Arc<dyn PlanStore>`): Quelle des Plans.
    ///
    /// # Returns
    /// Einen Provider mit `max_chars = ` [`DEFAULT_MAX_CHARS`].
    #[must_use]
    pub fn new(goal: Arc<dyn GoalStore>, plan: Arc<dyn PlanStore>) -> Self {
        Self::with_max_chars(goal, plan, DEFAULT_MAX_CHARS)
    }

    /// Erzeugt einen Provider mit eigenem Zeichenlimit.
    ///
    /// # Arguments
    /// - `goal` (`Arc<dyn GoalStore>`): Quelle des Ziels.
    /// - `plan` (`Arc<dyn PlanStore>`): Quelle des Plans.
    /// - `max_chars` (`usize`): Obergrenze in Zeichen. `0` schaltet den
    ///   Beitrag ab (leere Fragmentliste).
    ///
    /// # Returns
    /// Den konfigurierten Provider.
    #[must_use]
    pub fn with_max_chars(
        goal: Arc<dyn GoalStore>,
        plan: Arc<dyn PlanStore>,
        max_chars: usize,
    ) -> Self {
        Self {
            goal,
            plan,
            max_chars,
        }
    }

    /// Gibt das geltende Zeichenlimit zurück.
    #[must_use]
    pub fn max_chars(&self) -> usize {
        self.max_chars
    }

    /// Baut die Fragmente synchron.
    ///
    /// # Description
    /// Die eigentliche Arbeit von [`ContextProvider::contribute`], als
    /// synchrone Methode: sie macht keine `await`-Punkte, und so ist sie ohne
    /// Runtime testbar.
    ///
    /// Inhalt in dieser Reihenfolge: Zielsatz, nummerierte offene
    /// Akzeptanzkriterien, Invarianten, ausführbare Knoten mit ihrem
    /// `objective`, Coverage. Erfüllte Kriterien werden weggelassen — der
    /// Kontext soll sagen, was noch fehlt, nicht was schon erledigt ist.
    ///
    /// # Returns
    /// Genau ein [`ContextFragment`], oder eine leere Liste, wenn kein Ziel
    /// gebunden ist, der Plan fehlt oder `max_chars == 0`.
    ///
    /// # Concurrency
    /// Liest beide Stores nacheinander; hält danach keine Locks mehr.
    #[must_use]
    pub fn fragments(&self) -> Vec<ContextFragment> {
        if self.max_chars == 0 {
            return Vec::new();
        }

        let goal = match self.goal.current() {
            Ok(goal) => goal,
            Err(error) => {
                tracing::debug!(error = %error, "Kein Goal gebunden — kein Ziel-Fragment");
                return Vec::new();
            }
        };
        let plan = match self.plan.current() {
            Ok(plan) => plan,
            Err(error) => {
                tracing::debug!(error = %error, "Kein Plan vorhanden — kein Ziel-Fragment");
                return Vec::new();
            }
        };

        let report = evaluate_goal(&goal, &plan);
        let mut content = String::new();

        content.push_str("# Ziel\n");
        content.push_str(goal.statement.trim());
        content.push('\n');

        if !report.criteria_open.is_empty() {
            content.push_str("\n## Offene Akzeptanzkriterien\n");
            for (position, &index) in report.criteria_open.iter().enumerate() {
                let Some(criterion) = goal.acceptance_criteria.get(index) else {
                    continue;
                };
                content.push_str(&format!(
                    "{}. {}\n",
                    position + 1,
                    criterion.description.trim()
                ));
            }
        }

        if !goal.invariants.is_empty() {
            content.push_str("\n## Invarianten\n");
            for invariant in &goal.invariants {
                let violated = report.invariants_violated.contains(&invariant.id);
                content.push_str(&format!(
                    "- {}{}\n",
                    invariant.statement.trim(),
                    if violated { " (noch nicht belegt)" } else { "" }
                ));
            }
        }

        let ready = harw_plan::graph::ready_nodes(&plan);
        if !ready.is_empty() {
            content.push_str("\n## Jetzt ausführbar\n");
            for node in ready {
                content.push_str(&format!(
                    "- {} [{}]: {}\n",
                    node.id,
                    kind_label(node.kind),
                    node.objective.trim()
                ));
            }
        }

        content.push_str(&format!(
            "\n## Abdeckung\n{} von {} Akzeptanzkriterien belegt ({:.0} %).\n",
            report.criteria_met.len(),
            goal.acceptance_criteria.len(),
            report.coverage * 100.0
        ));

        vec![ContextFragment {
            label: FRAGMENT_LABEL.to_owned(),
            content: truncate_to_chars(&content, self.max_chars),
        }]
    }
}

impl ContextProvider for GoalContextProvider {
    fn contribute<'a>(&'a self, _ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
        // Der Provider liest nur zwei synchrone Stores; es gibt nichts
        // abzuwarten. Das Future ist beim ersten Poll bereits fertig.
        Box::pin(async move { self.fragments() })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Interne Hilfsfunktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Kürzt einen Text auf höchstens `max_chars` Zeichen (zeichengenau).
///
/// Wird gekürzt, endet das Ergebnis auf `…`, damit ein Leser die Kürzung sieht.
/// Bei `max_chars == 0` ist das Ergebnis leer.
fn truncate_to_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    // Ein Zeichen bleibt für die Ellipse reserviert.
    let keep = max_chars.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

/// Kurzform einer Knotenart für die Kontextzeile.
fn kind_label(kind: PlanNodeKind) -> &'static str {
    match kind {
        PlanNodeKind::Research => "research",
        PlanNodeKind::Explore => "explore",
        PlanNodeKind::Analysis => "analysis",
        PlanNodeKind::Synthesis => "synthesis",
        PlanNodeKind::Contract => "contract",
        PlanNodeKind::Coding => "coding",
        PlanNodeKind::Integration => "integration",
        PlanNodeKind::Verification => "verification",
        PlanNodeKind::Docs => "docs",
        PlanNodeKind::Composite => "composite",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use crate::testing::{
        InMemoryGoalStore, coding_node, goal_with_open_criterion, seeded_plan_store,
    };
    use harw_plan::goal::GoalAction;
    use harw_plan::{InMemoryPlanStore, PlanNodeStatus};

    fn provider(max_chars: usize) -> TestResult<GoalContextProvider> {
        let goal_store = InMemoryGoalStore::new();
        goal_store
            .apply(
                GoalAction::Set {
                    goal: goal_with_open_criterion(),
                },
                "operator",
            )
            .map_err(ctx("Goal setzen"))?;
        let plan = seeded_plan_store(vec![
            coding_node("t-1", PlanNodeStatus::Ready),
            coding_node("t-2", PlanNodeStatus::Completed),
        ])?;

        Ok(GoalContextProvider::with_max_chars(
            Arc::new(goal_store),
            Arc::new(plan),
            max_chars,
        ))
    }

    #[test]
    fn test_fragments_contain_goal_open_criteria_ready_nodes_and_coverage() -> TestResult {
        let fragments = provider(DEFAULT_MAX_CHARS)?.fragments();

        assert_eq!(fragments.len(), 1);
        let fragment = &fragments[0];
        assert_eq!(fragment.label, FRAGMENT_LABEL);

        let content = fragment.content.as_str();
        assert!(content.contains("# Ziel"), "{content}");
        assert!(content.contains("Bridge fertigstellen"), "{content}");
        assert!(
            content.contains("## Offene Akzeptanzkriterien"),
            "{content}"
        );
        assert!(content.contains("1. alle Tests grün"), "{content}");
        assert!(content.contains("## Invarianten"), "{content}");
        assert!(content.contains("## Jetzt ausführbar"), "{content}");
        assert!(content.contains("t-1 [coding]"), "{content}");
        assert!(content.contains("## Abdeckung"), "{content}");
        Ok(())
    }

    #[test]
    fn test_completed_nodes_are_not_listed_as_ready() -> TestResult {
        let fragments = provider(DEFAULT_MAX_CHARS)?.fragments();
        let content = fragments[0].content.as_str();
        assert!(
            !content.contains("t-2 ["),
            "abgeschlossener Knoten gelistet: {content}"
        );
        Ok(())
    }

    #[test]
    fn test_fragments_respect_max_chars() -> TestResult {
        let limit = 80;
        let fragments = provider(limit)?.fragments();

        assert_eq!(fragments.len(), 1);
        let content = fragments[0].content.as_str();
        assert!(
            content.chars().count() <= limit,
            "Fragment ist {} Zeichen lang",
            content.chars().count()
        );
        assert!(content.ends_with('…'), "Kürzung nicht markiert: {content}");
        Ok(())
    }

    #[test]
    fn test_zero_max_chars_contributes_nothing() -> TestResult {
        assert!(provider(0)?.fragments().is_empty());
        Ok(())
    }

    #[test]
    fn test_missing_goal_contributes_nothing() -> TestResult {
        let provider = GoalContextProvider::new(
            Arc::new(InMemoryGoalStore::new()),
            Arc::new(seeded_plan_store(vec![coding_node(
                "t-1",
                PlanNodeStatus::Ready,
            )])?),
        );
        assert!(provider.fragments().is_empty());
        Ok(())
    }

    #[test]
    fn test_missing_plan_contributes_nothing() -> TestResult {
        let goal_store = InMemoryGoalStore::new();
        goal_store
            .apply(
                GoalAction::Set {
                    goal: goal_with_open_criterion(),
                },
                "operator",
            )
            .map_err(ctx("Goal setzen"))?;
        let provider =
            GoalContextProvider::new(Arc::new(goal_store), Arc::new(InMemoryPlanStore::new()));
        assert!(provider.fragments().is_empty());
        Ok(())
    }

    #[test]
    fn test_truncate_to_chars_is_character_safe() {
        let text = "äöüßéè";
        let cut = truncate_to_chars(text, 3);
        assert_eq!(cut.chars().count(), 3);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate_to_chars(text, 100), text);
        assert!(truncate_to_chars(text, 0).is_empty());
    }

    #[test]
    fn test_default_max_chars_is_the_documented_two_thousand() {
        assert_eq!(DEFAULT_MAX_CHARS, 2000);
        let provider = GoalContextProvider::new(
            Arc::new(InMemoryGoalStore::new()),
            Arc::new(InMemoryPlanStore::new()),
        );
        assert_eq!(provider.max_chars(), DEFAULT_MAX_CHARS);
    }
}
