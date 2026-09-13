//! Idempotenter Workflow-State für `maintain()`.
//!
//! # Verantwortungsbereich
//! Persistente State-Machine für einen Konsolidierungslauf, damit ein Crash
//! oder ein Lease-Wechsel zwischen zwei Schritten nicht zu inkonsistenten
//! Zwischenzuständen führt. Jeder Übergang wird atomar (tmp + rename) auf die
//! Platte geschrieben; ein Wiederanlauf liest den letzten commiteten Schritt
//! und macht dort weiter.
//!
//! # State-Machine
//! ```text
//! Idle
//!  → Claimed (Lease erworben, Job-ID vergeben)
//!  → WorkspaceSynced (Signals gelesen, WARM/COLD-Kandidaten berechnet)
//!  → AgentCompleted (LLM/Regel-Konsolidierung fertig — optional in M1)
//!  → BaselineCommitted (Dateien geschrieben, alter Zustand rotiert)
//!  → DbCommitted (state.json geschrieben, Zähler aktualisiert)
//!  → Idle
//! ```
//!
//! # Idempotenz
//! Wird der Prozess in Zustand *X* neu gestartet, muss `resume(X)` die Arbeit
//! ab *X* neu ausführen können, ohne bereits committete Effekte doppelt
//! anzuwenden. Die Übergänge sind deshalb monoton und commit-basiert.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Ein Schritt im Konsolidierungs-Workflow.
///
/// # Bedeutung der Varianten
/// Jeder Schritt beschreibt, was **bereits committet** wurde. Der Wiederanlauf
/// beginnt am Schritt danach.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStep {
    /// Es läuft aktuell kein Konsolidierungs-Job.
    Idle,
    /// Lease wurde erworben — Job-ID + Owner sind persistiert.
    Claimed,
    /// Signals gelesen und Kandidaten-Menge berechnet.
    WorkspaceSynced,
    /// (Optional) LLM-/Regel-Konsolidierung ausgeführt.
    AgentCompleted,
    /// Neue Baseline auf Platte, alte rotiert (`HOT.md.bak`).
    BaselineCommitted,
    /// `state.json` geschrieben; Zähler aktualisiert.
    DbCommitted,
}

impl WorkflowStep {
    /// Der nachfolgende Schritt in der Kette.
    ///
    /// # Rückgabe
    /// - `Some(next)` für nicht-terminale Schritte.
    /// - `None` für `DbCommitted` (Endzustand; nächster Lauf beginnt bei `Idle`).
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Idle => Some(Self::Claimed),
            Self::Claimed => Some(Self::WorkspaceSynced),
            Self::WorkspaceSynced => Some(Self::AgentCompleted),
            Self::AgentCompleted => Some(Self::BaselineCommitted),
            Self::BaselineCommitted => Some(Self::DbCommitted),
            Self::DbCommitted => None,
        }
    }

    /// Kanonischer Kurzname für Logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Claimed => "claimed",
            Self::WorkspaceSynced => "workspace_synced",
            Self::AgentCompleted => "agent_completed",
            Self::BaselineCommitted => "baseline_committed",
            Self::DbCommitted => "db_committed",
        }
    }
}

/// Persistenter Job-Marker eines laufenden oder abgebrochenen Konsolidierungslaufs.
///
/// # Beschreibung
/// Wird in `<memory-root>/workflow.json` gespeichert. Nach jedem erfolgreichen
/// Schritt-Übergang wird die Datei atomar (tmp + rename) aktualisiert. Ein
/// abgebrochener Lauf hinterlässt einen nicht-`DbCommitted`-Zustand — der
/// nächste Aufruf von `maintain()` erkennt das und startet den Wiederanlauf.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowMarker {
    /// Job-ID (UUID- oder Ulid-ähnliche Zeichenkette).
    pub job_id: String,
    /// Zeitpunkt der letzten Aktualisierung (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Letzter erfolgreich committeter Schritt.
    pub step: WorkflowStep,
    /// Owner-Kennung (Prozess-ID, Host, o. ä.) für Lease-Diagnostik.
    pub owner: String,
    /// Zusätzliche Notiz für Debugging.
    pub note: Option<String>,
}

impl WorkflowMarker {
    /// Erstellt einen frischen Marker im Zustand `Idle`.
    #[must_use]
    pub fn idle(job_id: impl Into<String>, owner: impl Into<String>) -> Self {
        Self {
            job_id: job_id.into(),
            updated_at: OffsetDateTime::now_utc(),
            step: WorkflowStep::Idle,
            owner: owner.into(),
            note: None,
        }
    }

    /// Rückt auf den nächsten Schritt vor. Gibt `false` zurück, wenn bereits am Ende.
    pub fn advance(&mut self) -> bool {
        match self.step.next() {
            Some(next) => {
                self.step = next;
                self.updated_at = OffsetDateTime::now_utc();
                true
            }
            None => false,
        }
    }
}
