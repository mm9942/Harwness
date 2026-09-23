//! Die eine Wurzel-Kontext-Decke je [`CeilingPolicy`].
//!
//! # Verantwortungsbereich
//! `SpawnContext::ceiling` ist hereditär: ein Kind erbt die bereits
//! geschnittene Decke seines Elternteils, nie eine neu erfundene (siehe
//! `harw_core::child_controller::ManagedAgentSpawner::admit`). Dieser
//! Vererbungsweg beginnt an einer Sitzung ohne Elternteil — und genau dort
//! standen bisher **zwei** verschiedene Wurzeldecken im Workspace:
//!
//! - `harw-cli/src/root_context.rs:62-143` (`local_root_context_ceiling`):
//!   vier Sektionen (`task.objective`, `task.read_scope`,
//!   `new.trigger_return`, `history.tail`), `max_trust = Instruction`,
//!   Gesamtbudget `1_000_000`. Diese Datei ist seit W2d-2 entfernt; ihre
//!   Wurzeldecke lebt seither ausschließlich hier in dieser Datei.
//! - `harw-tui/src/app.rs:1364` (`local_tui_root_context_ceiling`): nur
//!   `history.tail`, ansonsten identisch.
//!
//! Dieselbe Vertrauensstellung (lokaler Operator mit `{R, W, X}`), zwei
//! Decken: eine TUI-Sitzung wies Kontextprogramme der eingebauten Rollen ab,
//! die in der CLI zugelassen waren. [`root_ceiling`] ist ab W2b die eine
//! Wurzeldecke; die weitere der beiden Fassungen (die CLI-Fassung) ist die
//! richtige — ihre Sektionen sind einzeln begründet, siehe
//! [`harw_context::ceiling::ROOT_CONTEXT_SECTIONS`].
//!
//! **Seit C-PROTO-RT (W3):** die Sektionsliste dieser Datei war ein
//! crate-privates Duplikat (`LOCAL_ROOT_SECTIONS`, vier Sektionen ohne
//! `legacy.v1`) der Liste, die `harw-context` unter
//! [`harw_context::ceiling::ROOT_CONTEXT_SECTIONS`] als die eine Quelle
//! führt (F-163: die v1-Brücke stempelte jedes gebrückte Fragment mit
//! `legacy.v1`, keine Wurzeldecke enthielt diese Sektion, jedes v1-Fragment
//! wurde in Kindsitzungen als `BelowCeiling` verworfen). [`root_ceiling`]
//! bezieht die Sektionen jetzt aus `harw-context`; die Duplizierung ist
//! behoben, `legacy.v1` ist ab hier Teil jeder lokal-vertrauten Wurzeldecke.
//!
//! # Bekannte Lücke
//! Nur [`harw_core::HISTORY_TAIL_SECTION`] ist über Crate-Grenzen hinweg als
//! öffentliche Konstante erreichbar; die Sektionsnamen der übrigen
//! Kontext-Provider (`harw-memory`, `harw-plan-bridge`, …) sind crate-privat
//! und stehen deshalb als String-Literale in
//! [`harw_context::ceiling::ROOT_CONTEXT_SECTIONS`]. Werden sie öffentlich,
//! gehören sie dort ergänzt — eine dokumentierte, keine übersehene Lücke.
//!
//! # Fehler
//! [`root_ceiling`] selbst ist unfehlbar (`ContextCeiling`, kein `Result`):
//! die lokal-vertraute Decke entsteht aus
//! [`harw_context::ceiling::ROOT_CONTEXT_SECTIONS`] und
//! `LOCAL_ROOT_BUDGET_TOTAL`, die geschlossene Decke aus Literalen dieser
//! Funktion. Eine ungültige Sektionskonstante (Vertragsbruch von
//! `harw-context`) wird nicht mehr per `expect` paniken gelassen, sondern
//! still übersprungen und mit `tracing::warn!` protokolliert (Bible
//! R087/R165).

use std::collections::{BTreeMap, BTreeSet};

use harw_context::ceiling::ROOT_CONTEXT_SECTIONS;
use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};

use crate::spec::CeilingPolicy;

/// Gesamtbudget der lokal-vertrauten Wurzeldecke.
///
/// # Beschreibung
/// Übernommen aus `harw-cli/src/root_context.rs` (`LOCAL_ROOT_BUDGET_TOTAL`,
/// entfernt in W2d-2) und `harw-tui/src/app.rs:1364`, die beide `1_000_000`
/// führten. Kein
/// `u32::MAX`, damit nachgelagerte Summen (etwa
/// [`ContextBudgetSpec::tighten`]) nicht überlaufen; groß genug, dass ein
/// lokaler Lauf in der Praxis nie anschlägt.
///
/// **Übergangswert bis P1.2:** Die Zahl ist eine Obergrenze *gegen Ausreißer*,
/// keine aus dem Modellfenster abgeleitete Größe — solange keine Sitzung ein
/// Kontextprogramm mit echten Kostenschätzungen gegen sie schneidet, kann sie
/// auch keine sein. P1.2 leitet das Wurzelbudget aus dem Kontextfenster des
/// aktiven Modells ab und ersetzt diese Konstante.
const LOCAL_ROOT_BUDGET_TOTAL: u32 = 1_000_000;

/// Baut die Wurzel-Kontext-Decke einer Politik.
///
/// # Beschreibung
/// [`CeilingPolicy::LocalRoot`] liefert die eine lokal-vertraute Wurzeldecke
/// (siehe Moduldoku): [`harw_context::ceiling::ROOT_CONTEXT_SECTIONS`]
/// (Sektionen einzeln begründet dort, seit C-PROTO-RT die eine Quelle statt
/// eines crate-privaten Duplikats — F-163), `max_trust` =
/// [`TrustClass::Instruction`] (höchster Rang, damit kein Fragment allein
/// wegen seiner Vertrauensklasse abgewiesen wird — ein lokaler Operator mit
/// `{R, W, X}` ist nicht weniger vertrauenswürdig als das restriktivste
/// Fragment), Budget `LOCAL_ROOT_BUDGET_TOTAL`.
///
/// [`CeilingPolicy::Closed`] liefert die vollständig geschlossene Decke:
/// keine Sektion, [`TrustClass::Data`] (niedrigster Rang), Budget `0`.
/// Identisch zu `harw_core::child_controller`s `closed_ceiling`, dem
/// fail-closed Ergebnis für eine Wurzel ohne eigene Decke — ein fehlendes
/// Feld darf nie zu mehr Autorität führen als ein gesetztes.
///
/// # Argumente
/// - `policy` ([`CeilingPolicy`]): aus [`crate::spec::EntryKind::profile`].
///
/// # Rückgabe
/// Die Decke, die der Einstieg genau einmal an seine Wurzelsitzung gibt.
///
/// # Examples
/// ```rust
/// use harw_runtime::CeilingPolicy;
/// use harw_runtime::ceiling::root_ceiling;
///
/// assert!(root_ceiling(CeilingPolicy::Closed).sections.is_empty());
/// assert!(!root_ceiling(CeilingPolicy::LocalRoot).sections.is_empty());
/// ```
#[must_use]
pub fn root_ceiling(policy: CeilingPolicy) -> ContextCeiling {
    match policy {
        CeilingPolicy::LocalRoot => ContextCeiling {
            // Kein `expect` mehr über alle Namen (Bible R087/R165): die
            // Literale sind konstant und nicht leer (harw-context prüft dies
            // selbst in `test_root_context_sections_are_valid_unique_and_contain_legacy_v1`),
            // ein Fehlschlag hier wäre also ein Vertragsbruch von
            // `harw-context`, kein Laufzeitzustand dieser Datei — deshalb
            // still übersprungen und geloggt statt paniken zu lassen.
            sections: ROOT_CONTEXT_SECTIONS
                .iter()
                .copied()
                .filter_map(|name| match SectionName::try_new(name) {
                    Ok(section) => Some(section),
                    Err(error) => {
                        tracing::warn!(
                            name,
                            %error,
                            "runtime.ceiling.invalid_root_section_skipped"
                        );
                        None
                    }
                })
                .collect(),
            max_trust: TrustClass::Instruction,
            budget: ContextBudgetSpec {
                total: harw_lens_types::BudgetSpec {
                    total: LOCAL_ROOT_BUDGET_TOTAL,
                },
                per_section: BTreeMap::new(),
            },
        },
        CeilingPolicy::Closed => ContextCeiling {
            sections: BTreeSet::new(),
            max_trust: TrustClass::Data,
            budget: ContextBudgetSpec {
                total: harw_lens_types::BudgetSpec { total: 0 },
                per_section: BTreeMap::new(),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::EntryKind;
    use crate::test_support::{TestResult, ctx};

    const ALL_ENTRIES: [EntryKind; 11] = [
        EntryKind::Tui,
        EntryKind::OneShot,
        EntryKind::LocalEcho,
        EntryKind::Analyze,
        EntryKind::Doctor,
        EntryKind::Web,
        EntryKind::McpServe,
        EntryKind::JobPrompt,
        EntryKind::JobPlanNode,
        EntryKind::GatewayTelegram,
        EntryKind::GatewayDream,
    ];

    fn section(name: &str) -> TestResult<SectionName> {
        SectionName::try_new(name).map_err(ctx("test section name is valid"))
    }

    #[test]
    fn closed_ceiling_is_empty() {
        let ceiling = root_ceiling(CeilingPolicy::Closed);
        assert!(ceiling.sections.is_empty());
        assert_eq!(ceiling.max_trust, TrustClass::Data);
        assert_eq!(ceiling.budget.total.total, 0);
        assert!(ceiling.budget.per_section.is_empty());
    }

    #[test]
    fn local_root_ceiling_carries_exactly_root_context_sections_including_legacy_v1() -> TestResult
    {
        let ceiling = root_ceiling(CeilingPolicy::LocalRoot);
        let expected: BTreeSet<SectionName> = ROOT_CONTEXT_SECTIONS
            .iter()
            .map(|name| section(name))
            .collect::<TestResult<_>>()?;
        assert_eq!(ceiling.sections, expected);
        assert!(
            ceiling
                .sections
                .contains(&section(harw_context::ceiling::LEGACY_V1_SECTION)?)
        );
        assert!(
            ceiling
                .sections
                .contains(&section(harw_core::HISTORY_TAIL_SECTION)?)
        );
        for name in ["task.objective", "task.read_scope", "new.trigger_return"] {
            assert!(ceiling.sections.contains(&section(name)?), "{name}");
        }
        assert_eq!(ceiling.sections.len(), ROOT_CONTEXT_SECTIONS.len());
        assert_eq!(ceiling.max_trust, TrustClass::Instruction);
        assert_eq!(ceiling.budget.total.total, LOCAL_ROOT_BUDGET_TOTAL);
        Ok(())
    }

    #[test]
    fn local_root_ceiling_excludes_credentials_and_foreign_transcripts() -> TestResult {
        let ceiling = root_ceiling(CeilingPolicy::LocalRoot);
        for name in [
            "credential.tokens",
            "secret.values",
            "full_parent_transcript",
            "sibling_transcripts",
            "plan.current",
            "web.fetch_allowlist",
            "knowledge.candidates",
            "diff.changeset",
        ] {
            assert!(!ceiling.sections.contains(&section(name)?), "{name}");
        }
        Ok(())
    }

    #[test]
    fn closed_is_a_reduction_of_local_root() {
        let local = root_ceiling(CeilingPolicy::LocalRoot);
        let closed = root_ceiling(CeilingPolicy::Closed);
        assert_eq!(local.intersect(&closed), closed);
    }

    #[test]
    fn every_entry_gets_exactly_one_of_the_two_ceilings() {
        let local = root_ceiling(CeilingPolicy::LocalRoot);
        let closed = root_ceiling(CeilingPolicy::Closed);
        for entry in ALL_ENTRIES {
            let ceiling = root_ceiling(entry.profile().ceiling);
            assert!(ceiling == local || ceiling == closed, "{entry:?}");
        }
    }
}
