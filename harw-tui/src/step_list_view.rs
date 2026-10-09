//! Read-only StepList-Ansicht für das Explorer-Fenster (h22).
//!
//! Reine Projektion einer [`harw_step_list::StepList`] in Textzeilen:
//! Status-Marke, Titel und Fortschritt (`done/total`). Die TUI mutiert hier
//! nichts — Änderungen laufen über den Step-Owner (Bibliotheks-API), die
//! Ansicht rendert nur den aktuellen Zustand neu. Der Bibliothekszustand
//! (`StepList`) bleibt von flüchtigem UI-Zustand getrennt: `StepListView`
//! hält nur eine Anzeige-Kopie des Fortschritts, keine eigene Liste.

use harw_step_list::{StepList, StepStatus};

/// Format-Marke je Status (keine Farben hier — die übernimmt das Theme am
/// Render-Ort über `ratatui`-Styling; hier bleibt es reiner Text, damit die
/// Funktion ohne UI-Abhängigkeit testbar bleibt).
fn status_mark(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Open => "[ ]",
        StepStatus::InProgress => "[~]",
        StepStatus::Done => "[x]",
        StepStatus::Blocked => "[!]",
        StepStatus::Cancelled => "[-]",
    }
}

/// Eine Zeile der StepList-Ansicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLine {
    /// Status-Marke (z. B. `[x]`).
    pub mark: &'static str,
    /// Titel des Steps.
    pub title: String,
}

/// Wandelt eine [`StepList`] in reine Anzeigezeilen um (read-only, h22).
///
/// Nur Anzeige: die TUI ruft das bei jedem Render; Mutationen bleiben beim
/// Step-Owner. `None` für eine leere Liste, damit der Renderzweig den Bereich
/// weglassen kann.
#[must_use]
pub fn step_lines(list: &StepList) -> Option<(Vec<StepLine>, usize, usize)> {
    let steps = list.steps();
    if steps.is_empty() {
        return None;
    }
    let lines = steps
        .iter()
        .map(|step| StepLine {
            mark: status_mark(step.status()),
            title: step.title().to_owned(),
        })
        .collect();
    let (done, total) = list.progress();
    Some((lines, done, total))
}

/// Fortschrittszeile (`Schritte: done/total`) für den Kopfbereich.
#[must_use]
pub fn progress_line(done: usize, total: usize) -> String {
    format!("Schritte: {done}/{total}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_renders_nothing() {
        let list = StepList::new();
        assert!(step_lines(&list).is_none());
    }

    #[test]
    fn steps_render_with_marks_and_progress() {
        let mut list = StepList::new();
        let first = list.add("h7 Scheibe 1", 1);
        let _second = list.add("h7 Scheibe 2", 2);
        list.done(first, "diff:status_line.rs", 3)
            .expect("done transition");
        let (lines, done, total) = step_lines(&list).expect("non-empty");
        assert_eq!(total, 2);
        assert_eq!(done, 1);
        assert_eq!(lines[0].mark, "[x]");
        assert_eq!(lines[0].title, "h7 Scheibe 1");
        assert_eq!(lines[1].mark, "[ ]");
        assert_eq!(progress_line(done, total), "Schritte: 1/2");
    }

    #[test]
    fn blocked_and_cancelled_have_their_marks() {
        let mut list = StepList::new();
        let blocked = list.add("blockiert", 1);
        let cancelled = list.add("abgebrochen", 2);
        list.set_status(blocked, StepStatus::Blocked, 3)
            .expect("block transition");
        list.set_status(cancelled, StepStatus::Cancelled, 4)
            .expect("cancel transition");
        let (lines, _, _) = step_lines(&list).expect("non-empty");
        assert_eq!(lines[0].mark, "[!]");
        assert_eq!(lines[1].mark, "[-]");
    }
}
