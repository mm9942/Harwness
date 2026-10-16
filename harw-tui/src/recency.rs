//! Reine Recency-Anzeigesicht für Agentenmeldungen (neueste = Slot 1, vorherige
//! Slot 1 = Slot 2); ältere Meldungen bleiben vollständig im Scrollback/Verlauf,
//! nichts wird gelöscht oder umnummeriert.

#[derive(Clone, Debug)]
pub(crate) struct RecencyEntry {
    pub(crate) child: String,
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

    fn entry(text: &str) -> RecencyEntry {
        RecencyEntry {
            child: "agent".to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn push_rota() {
        let mut slots = RecencySlots::new();
        slots.push(entry("A"));
        assert_eq!(slots.slot1().unwrap().text, "A");
        assert!(slots.slot2().is_none());
        slots.push(entry("B"));
        assert_eq!(slots.slot1().unwrap().text, "B");
        assert_eq!(slots.slot2().unwrap().text, "A");
        slots.push(entry("C"));
        assert_eq!(slots.slot1().unwrap().text, "C");
        assert_eq!(slots.slot2().unwrap().text, "B");
    }

    #[test]
    fn empty_slots_initially() {
        let slots = RecencySlots::new();
        assert!(slots.slot1().is_none());
        assert!(slots.slot2().is_none());
    }

    #[test]
    fn discarded_third() {
        let mut slots = RecencySlots::new();
        slots.push(entry("A"));
        slots.push(entry("B"));
        slots.push(entry("C"));
        assert_eq!(slots.slot2().unwrap().text, "B");
    }
}
