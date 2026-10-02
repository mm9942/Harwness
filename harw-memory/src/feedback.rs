//! Rückmeldung zu gelieferten Gedächtnisfakten: genutzt oder korrigiert.
//!
//! # Verantwortungsbereich
//! Der Kontext-Provider liefert Fakten in den Prompt ([`FeedbackTracker::note_delivery`]).
//! Am Turn-Ende erfährt der Tracker den Text der Nutzernachricht und der
//! Assistentenantwort und verbucht daraus in `usage.json`:
//!
//! - **genutzt**, wenn die Antwort einen gelieferten Fakt erkennbar aufgreift
//!   ([`mentions`]);
//! - **korrigiert**, wenn die Nutzernachricht eine Korrektur anzeigt
//!   ([`crate::learning::score_correction`]) und dabei einen gelieferten Fakt
//!   aufgreift.
//!
//! Die Zähler speisen die Verdrängung (`FactStore::decay`): oft geliefert und
//! nie genutzt, oder öfter korrigiert als genutzt, halbiert die Konfidenz.
//!
//! # Grenzen
//! Die Erkennung ist eine **Heuristik** über Stichwörter, kein Verständnis:
//! sie verlangt mehrere gemeinsame Stichwörter und lieber zu wenig als zu
//! viel. Sie schreibt nie Inhalt, nur Zähler.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::facts::FactStore;
use crate::learning::score_correction;

/// Ab diesem Korrektur-Gewicht (`score_correction`, 0–100) gilt eine
/// Nutzernachricht als Korrektur.
pub const CORRECTION_MIN_SCORE: u8 = 60;
/// Obergrenze gemerkter Sitzungen (älteste Einträge fallen heraus).
const MAX_TRACKED_SESSIONS: usize = 64;
/// Mindestlänge eines aussagekräftigen Stichworts.
const MIN_TOKEN_LEN: usize = 5;

/// Wo ein gelieferter Fakt liegt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeliveredScope {
    /// Projekt-Fakten-Wurzel.
    Project,
    /// Globale Fakten-Wurzel.
    Global,
}

/// Ein im Prompt gelieferter Fakt (Name und Beschreibung, kein Inhalt).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveredFact {
    /// Wurzel des Fakts.
    pub scope: DeliveredScope,
    /// Fakt-Name.
    pub name: String,
    /// Einzeilige Beschreibung, wie sie im Prompt stand.
    pub description: String,
}

const STOPWORDS: &[&str] = &[
    "nicht", "dieser", "diese", "dieses", "wurde", "werden", "immer", "sollte", "sollen", "should",
    "always", "which", "their", "there", "about", "using", "would", "could", "before", "after",
    "wenn", "damit", "dabei", "durch", "(project", "(global",
];

fn significant_tokens(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        let token = raw.to_lowercase();
        if token.chars().count() >= MIN_TOKEN_LEN
            && !STOPWORDS.contains(&token.as_str())
            && seen.insert(token.clone())
        {
            out.push(token);
        }
    }
    out
}

/// Ob `text` den Fakt (`name`, `description`) erkennbar aufgreift: mindestens
/// zwei seiner aussagekräftigen Stichwörter und mindestens 40 % davon kommen
/// vor (bei einem einzigen Stichwort genügt es).
#[must_use]
pub fn mentions(name: &str, description: &str, text: &str) -> bool {
    let mut keywords = significant_tokens(&name.replace('-', " "));
    for token in significant_tokens(description) {
        if !keywords.contains(&token) {
            keywords.push(token);
        }
    }
    if keywords.is_empty() {
        return false;
    }
    let present: HashSet<String> = significant_tokens(text).into_iter().collect();
    let hits = keywords.iter().filter(|k| present.contains(*k)).count();
    let need = if keywords.len() == 1 {
        1
    } else {
        // 40 % aufgerundet, mindestens 2.
        (keywords.len() * 2).div_ceil(5).max(2)
    };
    hits >= need
}

#[derive(Debug, Default)]
struct SessionDeliveries {
    /// Lieferung des laufenden Turns.
    current: Vec<DeliveredFact>,
    /// Lieferung des vorherigen Turns (Ziel einer Korrektur).
    previous: Vec<DeliveredFact>,
}

/// Verbucht Rückmeldung zu gelieferten Fakten (siehe Moduldoku).
pub struct FeedbackTracker {
    project: Option<Arc<FactStore>>,
    global: Option<Arc<FactStore>>,
    sessions: Mutex<HashMap<String, SessionDeliveries>>,
}

impl std::fmt::Debug for FeedbackTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedbackTracker").finish_non_exhaustive()
    }
}

impl FeedbackTracker {
    /// Tracker über den beiden Fakt-Speichern (jeder optional).
    #[must_use]
    pub fn new(project: Option<Arc<FactStore>>, global: Option<Arc<FactStore>>) -> Self {
        Self {
            project,
            global,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Merkt die Lieferung des laufenden Turns; die bisherige wird zur
    /// vorherigen. Leere Lieferungen räumen den Turn ebenfalls ab.
    pub fn note_delivery(&self, session_id: &str, facts: Vec<DeliveredFact>) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        if sessions.len() >= MAX_TRACKED_SESSIONS
            && !sessions.contains_key(session_id)
            && let Some(oldest) = sessions.keys().next().cloned()
        {
            sessions.remove(&oldest);
        }
        let entry = sessions.entry(session_id.to_owned()).or_default();
        entry.previous = std::mem::replace(&mut entry.current, facts);
    }

    /// Vergisst eine beendete Sitzung.
    pub fn forget_session(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session_id);
        }
    }

    /// Verbucht `used` für gelieferte Fakten, die `answer` aufgreift.
    pub fn on_assistant_message(&self, session_id: &str, answer: &str) {
        let targets = self.matching(session_id, answer, false);
        self.apply(&targets, |store, names| store.record_used(names));
    }

    /// Verbucht `corrected` für gelieferte Fakten (dieses und des vorherigen
    /// Turns), die eine Korrektur in `message` aufgreift.
    pub fn on_user_message(&self, session_id: &str, message: &str) {
        if score_correction(message) < CORRECTION_MIN_SCORE {
            return;
        }
        let targets = self.matching(session_id, message, true);
        self.apply(&targets, |store, names| store.record_corrected(names));
    }

    fn matching(&self, session_id: &str, text: &str, include_previous: bool) -> Vec<DeliveredFact> {
        let Ok(sessions) = self.sessions.lock() else {
            return Vec::new();
        };
        let Some(entry) = sessions.get(session_id) else {
            return Vec::new();
        };
        let mut pool: Vec<&DeliveredFact> = entry.current.iter().collect();
        if include_previous {
            pool.extend(entry.previous.iter());
        }
        let mut seen = HashSet::new();
        pool.into_iter()
            .filter(|fact| mentions(&fact.name, &fact.description, text))
            .filter(|fact| seen.insert((fact.scope, fact.name.clone())))
            .cloned()
            .collect()
    }

    fn apply(
        &self,
        facts: &[DeliveredFact],
        record: impl Fn(&FactStore, &[&str]) -> crate::error::MemoryResult<()>,
    ) {
        for (scope, store) in [
            (DeliveredScope::Project, &self.project),
            (DeliveredScope::Global, &self.global),
        ] {
            let Some(store) = store else { continue };
            let names: Vec<&str> = facts
                .iter()
                .filter(|fact| fact.scope == scope)
                .map(|fact| fact.name.as_str())
                .collect();
            if names.is_empty() {
                continue;
            }
            if let Err(error) = record(store, &names) {
                tracing::warn!(error = %error, "memory.feedback.record_failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Fact, FactScope};
    use crate::test_support::{TestError, TestResult, ctx};
    use time::OffsetDateTime;

    fn delivered(name: &str, description: &str) -> DeliveredFact {
        DeliveredFact {
            scope: DeliveredScope::Project,
            name: name.to_owned(),
            description: description.to_owned(),
        }
    }

    #[test]
    fn an_answer_that_applies_the_fact_is_a_mention_and_an_unrelated_one_is_not() {
        let desc = "Tests mit cargo nextest ausführen statt mit cargo test";
        assert!(mentions(
            "prefer-nextest",
            desc,
            "Ich führe die Tests mit cargo nextest aus, wie üblich."
        ));
        assert!(!mentions(
            "prefer-nextest",
            desc,
            "Die Datei wurde umbenannt und der Import angepasst."
        ));
        // One shared word is not enough for a multi-word fact.
        assert!(!mentions("prefer-nextest", desc, "Tests laufen gleich."));
    }

    #[test]
    fn short_facts_with_one_keyword_need_only_that_keyword() {
        assert!(mentions("uses-bazel", "bazel", "wir bauen mit Bazel"));
        assert!(!mentions("uses-bazel", "bazel", "wir bauen mit cargo"));
        assert!(
            !mentions("a", "b", "irgendein Text"),
            "no keywords, no match"
        );
    }

    fn store_with(tag: &str, name: &str) -> TestResult<(Arc<FactStore>, std::path::PathBuf)> {
        let root =
            std::env::temp_dir().join(format!("harw-feedback-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = FactStore::open(&root, FactScope::Project).map_err(ctx("open"))?;
        let now = OffsetDateTime::now_utc();
        let fact = Fact {
            name: name.to_owned(),
            description: "Tests mit cargo nextest ausführen".to_owned(),
            fact_type: crate::facts::FactType::Preference,
            scope: FactScope::Project,
            created: now,
            updated: now,
            confidence: 0.8,
            sources: Vec::new(),
            tags: Vec::new(),
            body: "x".to_owned(),
        };
        store.write(&fact).map_err(ctx("write"))?;
        Ok((Arc::new(store), root))
    }

    #[test]
    fn used_and_corrected_are_recorded_for_the_delivered_facts_only() -> TestResult {
        let (store, root) = store_with("used", "prefer-nextest")?;
        let tracker = FeedbackTracker::new(Some(Arc::clone(&store)), None);
        let fact = delivered("prefer-nextest", "Tests mit cargo nextest ausführen");
        tracker.note_delivery("s1", vec![fact]);

        tracker.on_assistant_message("s1", "Ich starte die Tests mit cargo nextest.");
        tracker.on_assistant_message("s1", "Völlig anderer Text über Dateien.");
        tracker.on_user_message("s1", "Das ist falsch, nutze nicht cargo nextest dafür!");
        tracker.on_user_message("s1", "Danke, passt cargo nextest gut.");
        let feedback = store
            .feedback("prefer-nextest")
            .ok_or(TestError::Missing("feedback"))?;
        assert_eq!(feedback.used, 1, "one answer applied the fact");
        assert_eq!(feedback.corrected, 1, "only the correction counts");

        // An unknown session records nothing.
        tracker.on_assistant_message("other", "cargo nextest");
        assert_eq!(store.feedback("prefer-nextest").map(|f| f.used), Some(1));
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn a_correction_also_reaches_the_previous_turns_delivery() -> TestResult {
        let (store, root) = store_with("previous", "prefer-nextest")?;
        let tracker = FeedbackTracker::new(Some(Arc::clone(&store)), None);
        tracker.note_delivery(
            "s1",
            vec![delivered(
                "prefer-nextest",
                "Tests mit cargo nextest ausführen",
            )],
        );
        // Next turn delivers nothing, then the user corrects the earlier answer.
        tracker.note_delivery("s1", Vec::new());
        tracker.on_user_message("s1", "Das ist falsch, nutze nicht cargo nextest dafür!");
        assert_eq!(
            store.feedback("prefer-nextest").map(|f| f.corrected),
            Some(1)
        );
        tracker.forget_session("s1");
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn the_session_map_stays_bounded() {
        let tracker = FeedbackTracker::new(None, None);
        for n in 0..(MAX_TRACKED_SESSIONS * 2) {
            tracker.note_delivery(&format!("s{n}"), Vec::new());
        }
        let len = tracker.sessions.lock().map(|s| s.len()).unwrap_or(0);
        assert!(len <= MAX_TRACKED_SESSIONS, "got {len}");
    }
}
