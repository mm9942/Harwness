//! Reine Recency-Anzeigesicht für Agentenmeldungen (neueste = Slot 1, vorherige
//! Slot 1 = Slot 2); ältere Meldungen bleiben vollständig im Scrollback/Verlauf,
//! nichts wird gelöscht oder umnummeriert.

#[derive(Clone, Debug)]
pub(crate) struct RecencyEntry {
    /// Sitzungs-ID des meldenden Kindes (im Betrieb eine 36 Zeichen lange UUID).
    pub(crate) child: String,
    /// Organisatorische Rolle des Kindes (z. B. `uia-writer`); kann leer sein.
    /// Die Anzeige bevorzugt sie als kurze Kennung vor der Sitzungs-ID.
    pub(crate) role: String,
    pub(crate) text: String,
}

#[derive(Clone, Debug)]
pub(crate) struct RecencySlots {
    slot1: Option<RecencyEntry>,
    slot2: Option<RecencyEntry>,
}

impl RecencySlots {
    pub(crate) fn new() -> Self {
        Self {
            slot1: None,
            slot2: None,
        }
    }

    pub(crate) fn push(&mut self, entry: RecencyEntry) {
        self.slot2 = self.slot1.take();
        self.slot1 = Some(entry);
    }

    pub(crate) fn slot1(&self) -> Option<&RecencyEntry> {
        self.slot1.as_ref()
    }

    pub(crate) fn slot2(&self) -> Option<&RecencyEntry> {
        self.slot2.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::{RecencyEntry, RecencySlots};
    use crate::test_support::{TestError, TestResult};

    fn entry(text: &str) -> RecencyEntry {
        RecencyEntry {
            child: "agent".to_owned(),
            role: String::new(),
            text: text.to_owned(),
        }
    }

    fn text_of(entry: Option<&RecencyEntry>, what: &'static str) -> TestResult<String> {
        entry
            .map(|entry| entry.text.clone())
            .ok_or(TestError::Missing(what))
    }

    #[test]
    fn push_rota() -> TestResult {
        let mut slots = RecencySlots::new();
        slots.push(entry("A"));
        assert_eq!(text_of(slots.slot1(), "Slot 1")?, "A");
        assert!(slots.slot2().is_none());
        slots.push(entry("B"));
        assert_eq!(text_of(slots.slot1(), "Slot 1")?, "B");
        assert_eq!(text_of(slots.slot2(), "Slot 2")?, "A");
        slots.push(entry("C"));
        assert_eq!(text_of(slots.slot1(), "Slot 1")?, "C");
        assert_eq!(text_of(slots.slot2(), "Slot 2")?, "B");
        Ok(())
    }

    #[test]
    fn empty_slots_initially() {
        let slots = RecencySlots::new();
        assert!(slots.slot1().is_none());
        assert!(slots.slot2().is_none());
    }

    #[test]
    fn discarded_third() -> TestResult {
        let mut slots = RecencySlots::new();
        slots.push(entry("A"));
        slots.push(entry("B"));
        slots.push(entry("C"));
        assert_eq!(text_of(slots.slot2(), "Slot 2")?, "B");
        Ok(())
    }

    #[test]
    fn role_is_carried_alongside_child_and_text() -> TestResult {
        let mut slots = RecencySlots::new();
        slots.push(RecencyEntry {
            child: "0196f3a2-7b1e-4c8d-9a35-5e2f8d41b6c7".to_owned(),
            role: "uia-writer".to_owned(),
            text: "fertig".to_owned(),
        });
        let newest = slots.slot1().ok_or(TestError::Missing("Slot 1"))?;
        assert_eq!(newest.role, "uia-writer");
        assert_eq!(newest.child, "0196f3a2-7b1e-4c8d-9a35-5e2f8d41b6c7");
        Ok(())
    }
}
