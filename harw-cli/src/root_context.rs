//! Die eine Kontext-Decke, mit der ein CLI-Prozess seine Wurzel-Sitzungen
//! ausstattet (Nachzug AW2-02 auf `harw-cli`).
//!
//! # Verantwortungsbereich
//! `SpawnContext::ceiling` ist hereditär: ein Kind erbt die bereits
//! geschnittene Decke seines Elternteils, nie eine neu erfundene (siehe
//! `harw_core::child_controller::ManagedAgentSpawner::admit` und die
//! Moduldoku von `harw_core::SpawnContext::ceiling`). Genau dieser
//! Vererbungsweg beginnt aber irgendwo — an einer Sitzung ohne Elternteil.
//! Dieses Modul ist die eine Stelle, an der die Wurzel-Decke für alle
//! lokal-vertrauten `harw`-CLI-Einstiege (`chat`, `lifecycle::health`,
//! `main::build_local_spawn_context`, der Plan-Node-Job-Worker) entsteht.
//!
//! # Warum nicht `ContextCeiling::default()`
//! `ContextCeiling` leitet gar kein `Default` ab — ein hartkodiertes
//! Leergerüst an dieser Stelle wäre trotzdem der falsche Reflex: es würde
//! eine Grenze vortäuschen, ohne zu sagen, welche Autorität sie tatsächlich
//! trägt. Die hier gebaute Decke ist stattdessen bewusst benannt und
//! begründet (siehe [`local_root_context_ceiling`]).
//!
//! # Warum die Decke mehr als `history.tail` führt
//! Diese Decke entstand, als noch **keine** Sitzung ein `ContextProgram`
//! trug: `SessionState::context_program()` war ausnahmslos `None`, also
//! wurde nie ein Programm gegen sie geschnitten, und `history.tail` als
//! einziger über Crate-Grenzen erreichbarer Sektionsname reichte aus. Seit
//! `ManagedAgentSpawner::admit` das Kontextprogramm der Rolle an die
//! Kindsitzung bindet und prüft, dass ein mitgebrachtes Programm die
//! geschnittene Decke nicht erweitert
//! (`harw_core::child_controller`, `describe_context_program_ceiling_violation`),
//! trifft dieselbe Decke auf die Programme der eingebauten Rollen. **Falsch
//! war die Decke, nicht die Prüfung und nicht die Programme**: die Prüfung
//! ist die Sicherheitsobergrenze, die Programme sind die Auswahl darin —
//! und die Auswahl der eingebauten read-only Worker ist das denkbar
//! knappste Minimum (siehe
//! `harw-registry-defaults/agents/worker-base.toml`, `[context]`).
//!
//! Die Decke führt deshalb genau die vier Sektionen, die eine lokal
//! vertraute Wurzel-Sitzung ihren Kindern überhaupt geben können muss;
//! jede einzeln begründet in [`LOCAL_ROOT_SECTIONS`].
//!
//! # Was die Decke weiterhin NICHT führt
//! Sie bleibt eine echte Obergrenze, keine Freigabe: `credential.*`,
//! `secret.*`, `full_parent_transcript`, `sibling_transcripts`,
//! `plan.current`, `web.fetch_allowlist`, `knowledge.candidates`,
//! `diff.changeset` und jede andere Sektion liegen außerhalb. Ein
//! Kontextprogramm, das eine davon verlangt, wird nach wie vor abgewiesen —
//! die Decke wurde präzisiert, nicht geöffnet.
//!
//! # Bekannte Lücke
//! Nur `harw_core::HISTORY_TAIL_SECTION` ist über die Crate-Grenzen hinweg
//! als öffentliche Konstante erreichbar; die Sektionsnamen anderer
//! Kontext-Provider (`harw-memory::context_provider`,
//! `harw-plan-bridge::plan_context`, …) sind crate-privat und stehen
//! deshalb unten als String-Literale. Sollten weitere Provider ihre
//! Sektionsnamen öffentlich machen, gehören sie hier ergänzt — das ist eine
//! bewusste, dokumentierte Lücke, keine übersehene.

use std::collections::BTreeMap;

use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};

/// Großzügiges Gesamtbudget für eine lokal-vertraute Wurzel-Sitzung.
///
/// Kein `u32::MAX`, um Überlauf in nachgelagerten Summen (z. B.
/// `ContextBudgetSpec::tighten`) nicht zu riskieren; groß genug, dass ein
/// lokaler CLI-Lauf in der Praxis nie an diese Grenze stößt.
const LOCAL_ROOT_BUDGET_TOTAL: u32 = 1_000_000;

/// Die Sektionen, die eine lokal-vertraute Wurzel-Sitzung ihren Kindern
/// höchstens zugänglich machen darf.
///
/// # Description
/// Diese Liste ist eine Obergrenze, keine Wunschliste: sie nennt genau das,
/// ohne das ein eingebauter read-only Worker seinen Auftrag nicht ausführen
/// könnte. Jeder Eintrag ist einzeln begründet:
///
/// - `task.objective`: der Auftrag selbst. Ohne ihn ist jede weitere Sektion
///   bedeutungslos — eine Decke, die den Auftrag ausschließt, schließt
///   jedes Kind aus. Er stammt vom Elternteil, nicht vom Kind, und ist
///   damit keine Rechteausweitung.
/// - `task.read_scope`: die Lesegrenze, die der Aufrufer dem Kind setzt. Sie
///   ist eine Übermittelte *Einschränkung*: ein Kind, das seine eigene
///   Grenze nicht kennt, kann sie nicht einhalten. Sie gewährt keinen
///   Zugriff, den die Sandbox nicht ohnehin schon zulässt.
/// - `new.trigger_return`: das Rückgabe-Protokoll aus
///   `worker-base.toml`. Ein Worker, der nicht weiß, wie er zurückgibt,
///   endet nur über Timeout oder Abbruch. Reine Ablaufinformation, ohne
///   Nutzdaten.
/// - `history.tail`: der bisherige Bestand, unverändert (siehe
///   [`harw_core::HISTORY_TAIL_SECTION`]).
///
/// Bewusst NICHT enthalten sind Zugangsdaten, Eltern- und
/// Geschwister-Transkripte sowie jede Provider-Sektion, die ein Kind erst
/// über eine eigene, engere Decke anfordern müsste.
const LOCAL_ROOT_SECTIONS: [&str; 4] = [
    "task.objective",
    "task.read_scope",
    "new.trigger_return",
    harw_core::HISTORY_TAIL_SECTION,
];

/// Baut die Wurzel-Kontext-Decke für eine lokal-vertraute `harw`-CLI-Sitzung.
///
/// # Description
/// Diese Sitzung hat keinen Elternteil, dessen bereits geschnittene Decke
/// sie erben könnte (siehe Moduldoku): die Decke entsteht hier genau einmal.
/// `max_trust` ist [`TrustClass::Instruction`] (der höchste Rang, siehe
/// [`TrustClass::trust_rank`]), damit kein Fragment allein wegen seiner
/// Vertrauensklasse abgelehnt wird — ein lokaler Operator, der bereits
/// `ReadWorkspace`/`WriteWorkspace`/`ExecuteProcess` besitzt, ist nicht
/// weniger vertrauenswürdig als das restriktivste Kontextfragment. Das
/// Budget ist großzügig, aber endlich (siehe [`LOCAL_ROOT_BUDGET_TOTAL`]).
///
/// # Returns
/// Eine [`ContextCeiling`], deren `sections` genau [`LOCAL_ROOT_SECTIONS`]
/// enthält — nicht mehr (siehe dort zur Begründung je Sektion und die
/// Moduldoku zur bekannten Lücke bei weiteren Sektionsnamen).
///
/// # Examples
/// ```rust,ignore
/// let ceiling = harw_cli::root_context::local_root_context_ceiling();
/// assert_eq!(ceiling.max_trust, harw_context::TrustClass::Instruction);
/// ```
#[must_use]
pub(crate) fn local_root_context_ceiling() -> ContextCeiling {
    // Ein einzelnes `expect` über alle Namen statt eines je Name: die
    // Literale in `LOCAL_ROOT_SECTIONS` sind konstant und nicht leer, ein
    // Fehlschlag wäre ein Tippfehler in dieser Datei, kein Laufzeitzustand.
    let sections = LOCAL_ROOT_SECTIONS
        .into_iter()
        .map(SectionName::try_new)
        .collect::<Result<_, _>>()
        .expect("LOCAL_ROOT_SECTIONS are valid section names by construction");
    ContextCeiling {
        sections,
        max_trust: TrustClass::Instruction,
        budget: ContextBudgetSpec {
            total: harw_lens_types::BudgetSpec {
                total: LOCAL_ROOT_BUDGET_TOTAL,
            },
            per_section: BTreeMap::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_root_context_ceiling_admits_history_tail_at_any_trust() {
        let ceiling = local_root_context_ceiling();

        assert!(
            ceiling
                .sections
                .contains(&SectionName::try_new("history.tail").expect("valid section name")),
            "the local root ceiling must always admit history.tail"
        );
        assert_eq!(
            ceiling.max_trust,
            TrustClass::Instruction,
            "a fully trusted local operator must not reject fragments on trust class alone"
        );
    }

    #[test]
    fn test_local_root_context_ceiling_admits_the_shared_worker_base_program() {
        // Die drei `must_include`-Sektionen aus
        // `harw-registry-defaults/agents/worker-base.toml` sind fuer ALLE
        // eingebauten Rollen identisch. Ein Kind ohne eigene Decke erbt die
        // Wurzeldecke unveraendert; enthaelt sie eine dieser Sektionen
        // nicht, wird jede eingebaute Rolle abgewiesen.
        let ceiling = local_root_context_ceiling();

        for section in ["task.objective", "task.read_scope", "new.trigger_return"] {
            assert!(
                ceiling
                    .sections
                    .contains(&SectionName::try_new(section).expect("valid section name")),
                "the local root ceiling must admit '{section}', declared by every builtin role"
            );
        }
    }

    #[test]
    fn test_local_root_context_ceiling_stays_a_real_upper_bound() {
        // Die Decke wurde praezisiert, nicht geoeffnet: was ein eingebauter
        // read-only Worker nicht braucht, liegt weiterhin ausserhalb.
        let ceiling = local_root_context_ceiling();

        for section in [
            "credential.token",
            "secret.vault",
            "full_parent_transcript",
            "sibling_transcripts",
            "plan.current",
            "web.fetch_allowlist",
        ] {
            assert!(
                !ceiling
                    .sections
                    .contains(&SectionName::try_new(section).expect("valid section name")),
                "the local root ceiling must never admit '{section}'"
            );
        }
    }

    #[test]
    fn test_local_root_context_ceiling_has_a_finite_nonzero_budget() {
        let ceiling = local_root_context_ceiling();

        assert!(
            ceiling.budget.total.total > 0,
            "a root ceiling must carry a real, positive budget, not an unbounded or zero one"
        );
        assert!(
            ceiling.budget.total.total < u32::MAX,
            "the root budget must stay finite so it cannot silently mask an overflow downstream"
        );
    }
}
