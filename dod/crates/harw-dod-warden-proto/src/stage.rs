//! Die Eskalationsleiter: [`EscalationStage`].
//!
//! # Verantwortungsbereich
//! Genau ein Typ: die geordnete Menge der Stufen, ab denen eine
//! [`crate::action::WardenAction`] überhaupt durchgesetzt werden darf (siehe
//! [`crate::action::WardenAction::is_admissible_from`]). Dieses Modul trifft
//! keine Aussage darüber, WANN eine Stufe erreicht wird — das entscheidet
//! `harw-dod-escalate` (AW5-03, `Ladder::admissible`) anhand des laufenden
//! Befundverlaufs. Hier steht nur das Vokabular, das beide Seiten der
//! Leitung (Eskalationsleiter und Durchsetzer) gemeinsam verwenden müssen,
//! damit `stage`-Werte auf der Leitung dasselbe bedeuten.
//!
//! # Warum nur zwei Stufen
//! `harw-macros/tests/warden_actions.rs` (AW5-01, bereits gelandet) benutzt
//! exakt `RuleTriggered` und `Escalated` als Beispielstufen für
//! `admissible_from: [...]`. Das ist die einzige Stelle im Workspace, an der
//! bereits konkrete Stufennamen für dieses Protokoll auftauchen. Diese
//! Deklaration übernimmt sie unverändert, statt eine dritte, hier erfundene
//! Bezeichnung einzuführen, die mit dem, was AW5-03 tatsächlich erwartet,
//! kollidieren könnte. Eine feinere Leiter (mehr als zwei Stufen) ist eine
//! spätere, additive Erweiterung — `#[non_exhaustive]` wird bewusst NICHT
//! gesetzt, weil diese Stufen intern getaggt über die Leitung gehen und ein
//! zusätzlicher Varianten-Zweig ohnehin jede `match`-Stelle zum Compile-Fehler
//! macht, was hier erwünscht ist (siehe `action.rs`, `is_admissible_from`).
//!
//! # Nebenläufigkeit
//! `Copy`, keine interne Veränderlichkeit. Sicher aus jedem Thread lesbar.

use serde::{Deserialize, Serialize};

/// Eine Stufe der Eskalationsleiter.
///
/// # Description
/// Ordnet danach, wie weit ein Befund die Leiter bereits hochgestiegen ist:
/// `RuleTriggered` ist die erste Stufe (eine feste Regel hat unmittelbar
/// angeschlagen), `Escalated` die zweite (der Befund wurde als schwerer
/// eingestuft, z. B. durch Wiederholung oder Verschärfung). Die Reihenfolge
/// der Varianten ist die Ordnung selbst — [`Ord`] leitet daraus ab, dass
/// `RuleTriggered < Escalated` gilt.
///
/// # Wire-Format
/// Kebab-case-String (`"rule-triggered"`, `"escalated"`), damit sie auf der
/// Leitung genauso aussieht wie ein `warden_actions!`-Aktionsname.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::EscalationStage;
///
/// assert!(EscalationStage::RuleTriggered < EscalationStage::Escalated);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EscalationStage {
    /// Eine feste Regel hat unmittelbar angeschlagen (vgl.
    /// `harw_dod_rules::FindingKind::RuleTriggered`, ein verwandtes, aber
    /// getrenntes Konzept: dort beschreibt es die Art des Befundes, hier die
    /// Stufe der Durchsetzungsleiter).
    RuleTriggered,
    /// Der Befund wurde auf die nächste Stufe gehoben.
    Escalated,
}

impl EscalationStage {
    /// Returns the kebab-case wire name of this stage.
    ///
    /// # Description
    /// Identisch mit der serde-Form (`"rule-triggered"`, `"escalated"`);
    /// Proof v2 nimmt diesen festen Wert in die kanonische MAC-Eingabe auf
    /// (siehe `canonical.rs`), statt sich auf eine JSON-Kodierung zu verlassen.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::EscalationStage;
    ///
    /// assert_eq!(EscalationStage::Escalated.wire_name(), "escalated");
    /// ```
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::RuleTriggered => "rule-triggered",
            Self::Escalated => "escalated",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EscalationStage;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_ordering_places_rule_triggered_below_escalated() {
        assert!(EscalationStage::RuleTriggered < EscalationStage::Escalated);
        assert!(EscalationStage::Escalated > EscalationStage::RuleTriggered);
    }

    #[test]
    fn test_serde_roundtrip_uses_kebab_case() -> TestResult {
        let json =
            serde_json::to_string(&EscalationStage::RuleTriggered).map_err(ctx("serializes"))?;
        assert_eq!(json, "\"rule-triggered\"");
        let round_tripped: EscalationStage =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, EscalationStage::RuleTriggered);

        let json = serde_json::to_string(&EscalationStage::Escalated).map_err(ctx("serializes"))?;
        assert_eq!(json, "\"escalated\"");
        Ok(())
    }

    #[test]
    fn test_wire_name_matches_serde_form() -> TestResult {
        for stage in [EscalationStage::RuleTriggered, EscalationStage::Escalated] {
            let json = serde_json::to_string(&stage).map_err(ctx("serializes"))?;
            assert_eq!(json, format!("\"{}\"", stage.wire_name()));
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_rejects_unknown_stage_name() {
        let result: Result<EscalationStage, _> = serde_json::from_str("\"contained\"");
        assert!(result.is_err());
    }
}
