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
//!   Gesamtbudget `1_000_000`.
//! - `harw-tui/src/app.rs:1364` (`local_tui_root_context_ceiling`): nur
//!   `history.tail`, ansonsten identisch.
//!
//! Dieselbe Vertrauensstellung (lokaler Operator mit `{R, W, X}`), zwei
//! Decken: eine TUI-Sitzung wies Kontextprogramme der eingebauten Rollen ab,
//! die in der CLI zugelassen waren. [`root_ceiling`] ist ab W2b die eine
//! Wurzeldecke; die weitere der beiden Fassungen (die CLI-Fassung) ist die
//! richtige — ihre vier Sektionen sind einzeln begründet, siehe die
//! Konstante `LOCAL_ROOT_SECTIONS` unten.
//!
//! # Bekannte Lücke
//! Nur [`harw_core::HISTORY_TAIL_SECTION`] ist über Crate-Grenzen hinweg als
//! öffentliche Konstante erreichbar; die Sektionsnamen der übrigen
//! Kontext-Provider (`harw-memory`, `harw-plan-bridge`, …) sind crate-privat
//! und stehen deshalb als String-Literale hier. Werden sie öffentlich,
//! gehören sie hier ergänzt — eine dokumentierte, keine übersehene Lücke.
//!
//! # Fehler
//! Keine: beide Decken entstehen aus Konstanten dieser Datei.

use std::collections::{BTreeMap, BTreeSet};

use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};

use crate::spec::CeilingPolicy;

/// Gesamtbudget der lokal-vertrauten Wurzeldecke.
///
/// # Beschreibung
/// Übernommen aus `harw-cli/src/root_context.rs` (`LOCAL_ROOT_BUDGET_TOTAL`)
/// und `harw-tui/src/app.rs:1364`, die beide `1_000_000` führten. Kein
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

/// Die Sektionen, die eine lokal-vertraute Wurzelsitzung ihren Kindern
/// höchstens zugänglich machen darf.
///
/// # Beschreibung
/// Eine Obergrenze, keine Wunschliste (Begründungen wörtlich aus
/// `harw-cli/src/root_context.rs`):
///
/// - `task.objective`: der Auftrag selbst. Eine Decke, die ihn ausschließt,
///   schließt jedes Kind aus. Er stammt vom Elternteil, ist also keine
///   Rechteausweitung.
/// - `task.read_scope`: die Lesegrenze, die der Aufrufer dem Kind setzt — eine
///   übermittelte *Einschränkung*. Ein Kind, das seine Grenze nicht kennt,
///   kann sie nicht einhalten.
/// - `new.trigger_return`: das Rückgabeprotokoll aus `worker-base.toml`. Ohne
///   es endet ein Worker nur über Timeout oder Abbruch. Reine
///   Ablaufinformation.
/// - `history.tail`: der bisherige Bestand, unverändert (die einzige Sektion
///   der TUI-Fassung).
///
/// Bewusst **nicht** enthalten: `credential.*`, `secret.*`,
/// `full_parent_transcript`, `sibling_transcripts`, `plan.current`,
/// `web.fetch_allowlist`, `knowledge.candidates`, `diff.changeset` und jede
/// weitere Provider-Sektion.
const LOCAL_ROOT_SECTIONS: [&str; 4] = [
    "task.objective",
    "task.read_scope",
    "new.trigger_return",
    harw_core::HISTORY_TAIL_SECTION,
];

/// Baut die Wurzel-Kontext-Decke einer Politik.
///
/// # Beschreibung
/// [`CeilingPolicy::LocalRoot`] liefert die eine lokal-vertraute Wurzeldecke
/// (siehe Moduldoku): `LOCAL_ROOT_SECTIONS` (siehe dort), `max_trust` =
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
            // Ein einzelnes `expect` über alle Namen statt eines je Name: die
            // Literale sind konstant und nicht leer, ein Fehlschlag wäre ein
            // Tippfehler in dieser Datei, kein Laufzeitzustand.
            sections: LOCAL_ROOT_SECTIONS
                .into_iter()
                .map(SectionName::try_new)
                .collect::<Result<_, _>>()
                .expect("LOCAL_ROOT_SECTIONS are valid section names by construction"),
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

    fn section(name: &str) -> SectionName {
        SectionName::try_new(name).expect("test section name is valid")
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
    fn local_root_ceiling_carries_history_tail_and_the_three_task_sections() {
        let ceiling = root_ceiling(CeilingPolicy::LocalRoot);
        assert!(
            ceiling
                .sections
                .contains(&section(harw_core::HISTORY_TAIL_SECTION))
        );
        for name in ["task.objective", "task.read_scope", "new.trigger_return"] {
            assert!(ceiling.sections.contains(&section(name)), "{name}");
        }
        assert_eq!(ceiling.sections.len(), LOCAL_ROOT_SECTIONS.len());
        assert_eq!(ceiling.max_trust, TrustClass::Instruction);
        assert_eq!(ceiling.budget.total.total, LOCAL_ROOT_BUDGET_TOTAL);
    }

    #[test]
    fn local_root_ceiling_excludes_credentials_and_foreign_transcripts() {
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
            assert!(!ceiling.sections.contains(&section(name)), "{name}");
        }
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
