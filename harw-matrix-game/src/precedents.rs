//! Präzedenzregister (matrix-game.md §11.4).
//!
//! Der Umpire kann ein Urteil über ein **öffentliches** Argument als
//! Maßstab markieren (`precedent` im Urteil). Der GameMaster journalisiert
//! es als öffentlichen [`crate::state::EntryKind::PrecedentSet`]-Eintrag;
//! das Journal bleibt die einzige Quelle. Spätere Adjudikations-Prompts
//! erhalten die einschlägigen Maßstäbe — ausgewählt rein deterministisch
//! über Schlagworte (`tags`) und Stichwortüberlappung mit den Argumenten der
//! laufenden Runde, und zwar ausschließlich aus der Projektion des Umpires.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::state::{EntryKind, Journal};
use crate::visibility::{SeatView, normalize_words};

/// Höchstzahl Schlagworte je Präzedenzfall.
pub const MAX_PRECEDENT_TAGS: usize = 5;
/// Zeichenlimit des Maßstabs.
pub const MAX_PRINCIPLE_CHARS: usize = 300;
/// Höchstzahl Maßstäbe im Adjudikations-Prompt.
pub const MAX_RELEVANT_PRECEDENTS: usize = 5;
/// Mindestlänge eines Stichworts für die Überlappung.
const KEYWORD_MIN_CHARS: usize = 6;
/// Punkte je Schlagworttreffer.
const TAG_SCORE: u32 = 3;
/// Mindestpunktzahl, ab der ein Maßstab als einschlägig gilt.
const RELEVANCE_THRESHOLD: u32 = 2;

/// Markierung eines Urteils als Präzedenzfall (Teil von `umpire_adjudication`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentFlag {
    /// Der Maßstab in einem Satz (öffentlich, nur öffentlich Bekanntes).
    pub principle: String,
    /// Schlagworte für das spätere Wiederfinden (1–5).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Journalisierter Präzedenzfall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Precedent {
    /// Kennung (`p1`, `p2` …).
    pub id: String,
    /// Argument, an dem der Maßstab gesetzt wurde.
    pub argument_id: String,
    /// Runde.
    pub round: u32,
    /// Maßstab.
    pub principle: String,
    /// Normalisierte Schlagworte.
    pub tags: Vec<String>,
    /// Netto des Urteils.
    pub net: i32,
    /// Erfolgswahrscheinlichkeit des Urteils in Prozent.
    pub probability_pct: u8,
}

/// Normalisiertes Schlagwort (klein, Wörter mit einem Leerzeichen verbunden).
#[must_use]
pub fn normalize_tag(tag: &str) -> String {
    normalize_words(tag).join(" ")
}

/// Prüft eine Markierung; Befunde landen mit `label` in `errors`.
pub fn check_precedent_flag(flag: &PrecedentFlag, label: &str, errors: &mut Vec<String>) {
    let principle = flag.principle.trim();
    if principle.is_empty() {
        errors.push(format!("{label}: precedent.principle ist leer"));
    }
    let n = principle.chars().count();
    if n > MAX_PRINCIPLE_CHARS {
        errors.push(format!(
            "{label}: precedent.principle hat {n} Zeichen (max. {MAX_PRINCIPLE_CHARS})"
        ));
    }
    if flag.tags.is_empty() || flag.tags.len() > MAX_PRECEDENT_TAGS {
        errors.push(format!(
            "{label}: precedent.tags braucht 1–{MAX_PRECEDENT_TAGS} Schlagworte"
        ));
    }
    let mut seen = BTreeSet::new();
    for (i, tag) in flag.tags.iter().enumerate() {
        let norm = normalize_tag(tag);
        if norm.is_empty() {
            errors.push(format!("{label}: precedent.tags[{i}] ist leer"));
        } else if !seen.insert(norm) {
            errors.push(format!("{label}: precedent.tags[{i}] doppelt"));
        }
    }
}

/// Alle Präzedenzfälle eines Journals in Reihenfolge.
#[must_use]
pub fn precedents_in(journal: &Journal) -> Vec<Precedent> {
    journal
        .entries()
        .filter_map(|e| match &e.kind {
            EntryKind::PrecedentSet { precedent } => Some(precedent.clone()),
            _ => None,
        })
        .collect()
}

fn contains_sequence(words: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && words.windows(needle.len()).any(|w| w == needle)
}

/// Relevanzpunkte eines Maßstabs für einen Text: 3 je Schlagwort, das als
/// Wortfolge vorkommt, plus 1 je gemeinsamem langem Stichwort (≥ 6 Zeichen)
/// aus dem Maßstab.
#[must_use]
pub fn relevance(precedent: &Precedent, text: &str) -> u32 {
    let words = normalize_words(text);
    let mut score = 0u32;
    for tag in &precedent.tags {
        if contains_sequence(&words, &normalize_words(tag)) {
            score += TAG_SCORE;
        }
    }
    let text_words: BTreeSet<&str> = words.iter().map(String::as_str).collect();
    let keywords: BTreeSet<String> = normalize_words(&precedent.principle)
        .into_iter()
        .filter(|w| w.chars().count() >= KEYWORD_MIN_CHARS)
        .collect();
    for k in &keywords {
        if text_words.contains(k.as_str()) {
            score += 1;
        }
    }
    score
}

/// Ein für die laufende Runde einschlägiger Maßstab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelevantPrecedent {
    /// Präzedenzfall.
    pub precedent: Precedent,
    /// Höchste Relevanz über die Argumente der Runde.
    pub score: u32,
    /// Argumente der Runde, für die er einschlägig ist.
    pub arguments: Vec<String>,
}

fn rank(mut found: Vec<RelevantPrecedent>, limit: usize) -> Vec<RelevantPrecedent> {
    found.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.precedent.round.cmp(&b.precedent.round))
            .then(a.precedent.argument_id.cmp(&b.precedent.argument_id))
    });
    found.truncate(limit);
    found
}

fn match_arguments(
    precedents: Vec<Precedent>,
    arguments: &[(String, String)],
    limit: usize,
) -> Vec<RelevantPrecedent> {
    let mut found = Vec::new();
    for precedent in precedents {
        let mut best = 0;
        let mut hits = Vec::new();
        for (id, text) in arguments {
            let score = relevance(&precedent, text);
            if score >= RELEVANCE_THRESHOLD {
                best = best.max(score);
                hits.push(id.clone());
            }
        }
        if !hits.is_empty() {
            found.push(RelevantPrecedent {
                precedent,
                score: best,
                arguments: hits,
            });
        }
    }
    rank(found, limit)
}

/// Einschlägige Maßstäbe früherer Runden für die Argumente der Runde
/// `round` — ausschließlich aus der Projektion `view` (Prompt-Invariante).
#[must_use]
pub fn relevant_precedents(view: &SeatView, round: u32, limit: usize) -> Vec<RelevantPrecedent> {
    let mut precedents = Vec::new();
    let mut arguments: Vec<(String, String)> = Vec::new();
    for e in view.entries() {
        match &e.kind {
            EntryKind::PrecedentSet { precedent } if precedent.round < round => {
                precedents.push(precedent.clone());
            }
            EntryKind::ArgumentRevealed {
                argument_id,
                argument,
                ..
            } if e.round == round && !arguments.iter().any(|(id, _)| id == argument_id) => {
                let text = format!("{} {}", argument.action, argument.pros.join(" "));
                arguments.push((argument_id.clone(), text));
            }
            _ => {}
        }
    }
    match_arguments(precedents, &arguments, limit)
}

/// Für das AAR: zu jedem Maßstab die späteren Argumente (beliebiger
/// Sichtbarkeit), für die er einschlägig gewesen wäre.
#[must_use]
pub fn later_matches(journal: &Journal) -> Vec<RelevantPrecedent> {
    let precedents = precedents_in(journal);
    let arguments: Vec<(u32, String, String)> = journal
        .entries()
        .filter_map(|e| match &e.kind {
            EntryKind::ArgumentRevealed {
                argument_id,
                argument,
                ..
            } => Some((
                e.round,
                argument_id.clone(),
                format!("{} {}", argument.action, argument.pros.join(" ")),
            )),
            _ => None,
        })
        .collect();
    precedents
        .into_iter()
        .map(|p| {
            let mut best = 0;
            let mut hits = Vec::new();
            for (round, id, text) in &arguments {
                if *round <= p.round {
                    continue;
                }
                let score = relevance(&p, text);
                if score >= RELEVANCE_THRESHOLD {
                    best = best.max(score);
                    hits.push(id.clone());
                }
            }
            RelevantPrecedent {
                precedent: p,
                score: best,
                arguments: hits,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn precedent(tags: &[&str], principle: &str) -> Precedent {
        Precedent {
            id: "p1".to_owned(),
            argument_id: "r1-a1".to_owned(),
            round: 1,
            principle: principle.to_owned(),
            tags: tags.iter().map(|t| normalize_tag(t)).collect(),
            net: 2,
            probability_pct: 83,
        }
    }

    #[test]
    fn relevance_counts_tags_and_keywords() {
        let p = precedent(
            &["Blockade", "Hafen Zoll"],
            "Eine Blockade ohne Schiffe vor Ort gelingt nicht.",
        );
        assert_eq!(relevance(&p, "Der Rat verhängt eine Blockade."), 3 + 1);
        assert_eq!(relevance(&p, "Am Hafen Zoll wird kontrolliert."), 3);
        assert_eq!(relevance(&p, "Wir bauen eine Leitung."), 0);
        // Einzelnes langes Stichwort allein liegt unter der Schwelle.
        let q = precedent(&["x"], "Schiffe brauchen Liegeplätze.");
        assert!(relevance(&q, "Unsere Schiffe laufen aus.") < RELEVANCE_THRESHOLD);
    }

    #[test]
    fn flag_validation() {
        let mut errors = Vec::new();
        check_precedent_flag(
            &PrecedentFlag {
                principle: " ".to_owned(),
                tags: vec!["a".to_owned(), "A".to_owned()],
            },
            "r",
            &mut errors,
        );
        assert_eq!(errors.len(), 2, "{errors:?}");
        let mut ok = Vec::new();
        check_precedent_flag(
            &PrecedentFlag {
                principle: "Maßstab".to_owned(),
                tags: vec!["zoll".to_owned()],
            },
            "r",
            &mut ok,
        );
        assert!(ok.is_empty());
    }
}
