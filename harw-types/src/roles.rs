//! Semantische Labels und Klassifikations-Enums.
//!
//! `AgentRole`, `RiskLevel`, `ReviewDecision`, `MessagePhase` — reine
//! Daten-Enums ohne Logik, höchste Stabilität.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Rolle eines Nachrichten-Autors. Kein UUID, sondern semantisches Label.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    User,
    Assistant,
    System,
    Tool,
    /// Für Multi-Agent-Szenarien (Collab, Sub-Agent).
    Agent {
        name: String,
    },
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
            Self::System => write!(f, "system"),
            Self::Tool => write!(f, "tool"),
            Self::Agent { name } => write!(f, "agent:{name}"),
        }
    }
}

/// Risiko-Klassifikation für genehmigungspflichtige Aktionen.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        };
        f.write_str(s)
    }
}

/// Entscheidung über eine Genehmigungsanfrage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    Rejected,
    ApprovedOnce,
}

/// Vorgabe-Wartezeit auf eine Freigabeentscheidung, bevor sie als abgelaufen
/// gilt (Interaktionsvertrag §4.4).
///
/// # Einzige Quelle im Fan-in-Vokabular
/// Lebt hier statt in `harw-core` oder `harw-protocol`, weil beide diese
/// Politik brauchen (`harw-core::session::PendingApproval::timeout_at`,
/// `harw-protocol::approvals::ApprovalRequest::timeout_at`) und `harw-core`
/// nicht von `harw-protocol` abhängen darf, ohne einen Zyklus zu erzeugen —
/// `harw-types` ist der einzige gemeinsame, zyklusfreie Ort für beide.
pub const DEFAULT_APPROVAL_TIMEOUT: jiff::SignedDuration = jiff::SignedDuration::from_secs(300);

/// Begründung, die jede Oberfläche für eine durch Zeitablauf erzwungene
/// [`ReviewDecision::Rejected`] verwendet (Interaktionsvertrag §4.4 —
/// „timed-out-denied").
///
/// Eine einzige Zeichenkette statt einer Kopie je Crate (`harw-core`,
/// `harw-protocol`, ein künftiges Channel-Binding), damit ein Betreiber beim
/// Auditieren nicht mehrere Formulierungen für denselben Vorgang sieht.
pub const APPROVAL_TIMEOUT_REASON: &str =
    "no approval decision arrived before the request's timeout_at was reached";

/// Phase einer Assistant-Nachricht innerhalb eines Turns.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    /// Zwischenschritt, noch kein finales Ergebnis.
    Commentary,
    /// Abschließende Antwort des Turns.
    FinalAnswer,
}
