//! `harw-step-list` — Step-Tracking mit stabilen IDs.
//!
//! Enthält `StepId`, `StepStatus` (Übergangsregeln inkl. Terminalzuständen),
//! `Step`, die `StepList` mit Mutations- und Persistenz-API sowie
//! `StepListError`.

use std::fmt;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Stabile, monoton steigende Identität eines Steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StepId(u64);

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl StepId {
    /// Numerischer Wert der ID.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Lebenszyklus eines Steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StepStatus {
    /// Noch nicht begonnen.
    Open,
    /// In Arbeit.
    InProgress,
    /// Abgeschlossen (terminal).
    Done,
    /// Blockiert (terminal-los, aber nicht fortsetzbar ohne Ursache).
    Blocked,
    /// Abgebrochen (terminal).
    Cancelled,
}

impl StepStatus {
    /// Terminal: kein Weg mehr heraus.
    pub fn is_terminal(&self) -> bool {
        matches!(self, StepStatus::Done | StepStatus::Cancelled)
    }
}

/// Übergangsregel: alles erlaubt, außer aus `Done`/`Cancelled` heraus.
pub fn can_transition_to(from: StepStatus, to: StepStatus) -> bool {
    !from.is_terminal()
}

/// Ein einzelner Schritt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    id: StepId,
    title: String,
    status: StepStatus,
    evidence: Option<String>,
    created_at: u64,
    updated_at: u64,
    version: u64,
}

impl Step {
    pub fn id(&self) -> StepId {
        self.id
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn status(&self) -> &StepStatus {
        &self.status
    }
    pub fn evidence(&self) -> Option<&str> {
        self.evidence.as_deref()
    }
    pub fn created_at(&self) -> u64 {
        self.created_at
    }
    pub fn updated_at(&self) -> u64 {
        self.updated_at
    }
    pub fn version(&self) -> u64 {
        self.version
    }
}

/// Geordnete Liste von Steps mit Persistenz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepList {
    steps: Vec<Step>,
    next_id: u64,
    schema_version: u32,
}

impl Default for StepList {
    fn default() -> Self {
        Self::new()
    }
}

impl StepList {
    /// Leere Liste mit `schema_version` 1.
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            next_id: 1,
            schema_version: 1,
        }
    }

    /// Neuen Step anlegen; gibt die vergebene stabile ID zurück.
    pub fn add(&mut self, title: impl Into<String>, now: u64) -> StepId {
        let id = StepId(self.next_id);
        self.next_id += 1;
        self.steps.push(Step {
            id,
            title: title.into(),
            status: StepStatus::Open,
            evidence: None,
            created_at: now,
            updated_at: now,
            version: 1,
        });
        id
    }

    fn step_mut(&mut self, id: StepId) -> Result<&mut Step, StepListError> {
        self.steps
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StepListError::UnknownStep(id))
    }

    /// Status setzen — Übergänge aus Terminalzuständen heraus sind illegal.
    pub fn set_status(
        &mut self,
        id: StepId,
        status: StepStatus,
        now: u64,
    ) -> Result<(), StepListError> {
        let step = self.step_mut(id)?;
        if !can_transition_to(step.status.clone(), status.clone()) {
            return Err(StepListError::IllegalTransition {
                from: step.status.clone(),
                to: status,
            });
        }
        if step.status == status {
            return Ok(()); // idempotent, keine Version-Erhöhung
        }
        step.status = status;
        step.updated_at = now;
        step.version += 1;
        Ok(())
    }

    /// Step als Done abschließen — Evidence ist Pflicht (kein reines Whitespace).
    pub fn done(&mut self, id: StepId, evidence: &str, now: u64) -> Result<(), StepListError> {
        if evidence.trim().is_empty() {
            return Err(StepListError::EvidenceRequired);
        }
        self.set_status(id, StepStatus::Done, now)?;
        let step = self.step_mut(id)?;
        step.evidence = Some(evidence.to_string());
        step.updated_at = now;
        Ok(())
    }

    /// Evidence nachtragen oder aktualisieren (auch ohne Statuswechsel).
    pub fn update_evidence(
        &mut self,
        id: StepId,
        evidence: &str,
        now: u64,
    ) -> Result<(), StepListError> {
        let step = self.step_mut(id)?;
        step.evidence = Some(evidence.to_string());
        step.updated_at = now;
        step.version += 1;
        Ok(())
    }

    /// `(done, total)`: `total` zählt alle Steps, `done` nur `Status::Done`.
    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Done)
            .count();
        (done, self.steps.len())
    }

    /// Version des ersten Steps (0 wenn leer) — Mutationen erhöhen sie.
    pub fn version(&self) -> u64 {
        self.steps.first().map(|s| s.version).unwrap_or(0)
    }

    /// Alle Steps.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Seriell in Datei schreiben.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let f = std::fs::File::create(path)?;
        serde_json::to_writer(&f, self).map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    /// Aus Datei laden; unbekannte `schema_version` wird abgelehnt.
    pub fn load(path: &Path) -> Result<StepList, StepListError> {
        let f = std::fs::File::open(path).map_err(|e| StepListError::Io(e.to_string()))?;
        let list: StepList = serde_json::from_reader(f)?;
        if list.schema_version != 1 {
            return Err(StepListError::UnsupportedSchema(list.schema_version));
        }
        Ok(list)
    }
}

/// Fehler der StepList-API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepListError {
    /// Step-ID existiert nicht in der Liste.
    UnknownStep(StepId),
    /// Illegaler Übergang aus einem Terminalzustand.
    IllegalTransition {
        /// Quellstatus.
        from: StepStatus,
        /// Zielstatus.
        to: StepStatus,
    },
    /// Version stimmt nicht mehr mit der erwarteten überein.
    StaleVersion(u64),
    /// Evidence fehlt oder ist nur Whitespace.
    EvidenceRequired,
    /// Persistiertes Schema wird nicht unterstützt.
    UnsupportedSchema(u32),
    /// Seriellisierungs-/Dateifehler.
    Io(String),
}

impl fmt::Display for StepListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownStep(id) => write!(f, "unbekannter Step {id}"),
            Self::IllegalTransition { from, to } => {
                write!(f, "illegaler Übergang {from:?} -> {to:?}")
            }
            Self::StaleVersion(v) => write!(f, "veraltete Version {v}"),
            Self::EvidenceRequired => write!(f, "Evidence erforderlich"),
            Self::UnsupportedSchema(v) => write!(f, "nicht unterstützte Schema-Version {v}"),
            Self::Io(msg) => write!(f, "I/O-Fehler: {msg}"),
        }
    }
}

impl std::error::Error for StepListError {}

impl From<serde_json::Error> for StepListError {
    fn from(e: serde_json::Error) -> Self {
        StepListError::Io(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_yields_stable_ids() {
        let mut list = StepList::new();
        let a = list.add("erster", 10);
        let b = list.add("zweiter", 11);
        assert_eq!(a.as_u64(), 1);
        assert_eq!(b.as_u64(), 2);
        assert_eq!(list.steps().len(), 2);
        assert_eq!(list.steps()[0].id(), a);
        assert_eq!(list.steps()[1].id(), b);
        assert_eq!(list.steps()[0].status(), &StepStatus::Open);
        assert_eq!(list.steps()[0].created_at(), 10);
    }

    #[test]
    fn set_status_transitions() {
        let mut list = StepList::new();
        let id = list.add("s", 0);
        assert!(list.set_status(id, StepStatus::InProgress, 1).is_ok());
        assert!(list.set_status(id, StepStatus::Blocked, 2).is_ok());
        assert!(list.set_status(id, StepStatus::InProgress, 3).is_ok());
        assert!(list.set_status(id, StepStatus::Done, 4).is_ok());
        // Terminalzustände: kein Weg mehr heraus.
        let err = list.set_status(id, StepStatus::Open, 5).unwrap_err();
        assert_eq!(
            err,
            StepListError::IllegalTransition {
                from: StepStatus::Done,
                to: StepStatus::Open
            }
        );
        assert!(list.set_status(id, StepStatus::InProgress, 5).is_err());
    }

    #[test]
    fn cancelled_is_terminal_too() {
        let mut list = StepList::new();
        let id = list.add("s", 0);
        assert!(list.set_status(id, StepStatus::Cancelled, 1).is_ok());
        assert!(list.set_status(id, StepStatus::Open, 2).is_err());
    }

    #[test]
    fn unknown_step_is_reported() {
        let mut list = StepList::new();
        let err = list
            .set_status(StepId(42), StepStatus::Done, 0)
            .unwrap_err();
        assert_eq!(err, StepListError::UnknownStep(StepId(42)));
    }

    #[test]
    fn done_requires_evidence() {
        let mut list = StepList::new();
        let id = list.add("s", 0);
        assert_eq!(list.done(id, "", 1), Err(StepListError::EvidenceRequired));
        assert_eq!(
            list.done(id, "   ", 1),
            Err(StepListError::EvidenceRequired)
        );
        assert!(list.done(id, "Testlauf grün", 2).is_ok());
        assert_eq!(list.steps()[0].evidence(), Some("Testlauf grün"));
        assert_eq!(list.steps()[0].status(), &StepStatus::Done);
        assert!(list.update_evidence(id, "erneut bestätigt", 3).is_ok());
        assert_eq!(list.steps()[0].evidence(), Some("erneut bestätigt"));
    }

    #[test]
    fn progress_counts() {
        let mut list = StepList::new();
        let a = list.add("a", 0);
        let b = list.add("b", 0);
        let c = list.add("c", 0);
        assert_eq!(list.progress(), (0, 3));
        list.done(a, "evidenz a", 1).unwrap();
        list.set_status(b, StepStatus::InProgress, 1).unwrap();
        assert_eq!(list.progress(), (1, 3));
        // Cancelled zählt zu total, aber nicht zu done.
        list.set_status(c, StepStatus::Cancelled, 1).unwrap();
        assert_eq!(list.progress(), (1, 3));
        list.set_status(b, StepStatus::Done, 2).unwrap();
        // done() verlangt Evidence; set_status nicht.
        assert_eq!(list.progress(), (2, 3));
    }

    #[test]
    fn version_increases_on_mutation() {
        let mut list = StepList::new();
        assert_eq!(list.version(), 0);
        let id = list.add("s", 0);
        let v0 = list.steps()[0].version();
        list.set_status(id, StepStatus::InProgress, 1).unwrap();
        assert_eq!(list.steps()[0].version(), v0 + 1);
        list.update_evidence(id, "x", 2).unwrap();
        assert_eq!(list.steps()[0].version(), v0 + 2);
        list.done(id, "y", 3).unwrap();
        assert!(list.steps()[0].version() > v0);
    }

    #[test]
    fn save_load_roundtrip() {
        let mut list = StepList::new();
        let a = list.add("alpha", 10);
        let b = list.add("beta", 11);
        list.done(a, "belegt", 12).unwrap();
        list.set_status(b, StepStatus::Cancelled, 13).unwrap();

        let path = std::env::temp_dir().join(format!(
            "harw-step-list-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        list.save(&path).unwrap();
        let loaded = StepList::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(loaded.progress(), list.progress());
        assert_eq!(loaded.steps().len(), 2);
        assert_eq!(loaded.steps()[0].id(), a);
        assert_eq!(loaded.steps()[0].evidence(), Some("belegt"));
        assert_eq!(loaded.steps()[1].status(), &StepStatus::Cancelled);
        // IDs bleiben stabil, next_id wird mitgeliefert.
        let mut loaded = loaded;
        let c = loaded.add("gamma", 20);
        assert_eq!(c.as_u64(), 3);
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let mut list = StepList::new();
        let _ = list.add("x", 0);
        // schema_version manipulieren via JSON.
        let json = serde_json::to_string(&list).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v["schema_version"] = serde_json::json!(99);
        let path = std::env::temp_dir().join(format!(
            "harw-step-list-schema-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, v.to_string()).unwrap();
        let err = StepList::load(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(err, StepListError::UnsupportedSchema(99));
    }
}
