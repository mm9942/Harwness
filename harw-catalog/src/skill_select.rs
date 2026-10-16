//! Reiner Auswahl-Kern für getriggerte Skill-Auszüge (Plan PL-95, S1).
//!
//! # Verantwortung
//! [`select`] beantwortet: „Welche Skill-Abschnitte sollen nach diesen
//! Ereignissen (Aufgabentext, Werkzeugausgabe, erste Werkzeugnutzung,
//! berührter Pfad, Befehl) in den Kontext?“ — **ohne Modellaufruf, ohne
//! Ein-/Ausgabe, ohne Laufzeitverdrahtung**. Gleiche Eingabe (auch bei
//! umgestellter Ereignisreihenfolge) liefert dieselbe [`Selection`].
//!
//! # Ablauf
//! 1. Treffer je Skill über den [`crate::TriggerIndex`] des [`SkillIndex`]
//!    (nur Skills, die der Index führt; ein nicht vertrauter oder
//!    deaktivierter Skill fehlt dort schon und kann nie geliefert werden).
//! 2. Ordnung: höhere `priority` zuerst, dann mehr Treffer, dann Name.
//! 3. Abschnittswahl je Skill (der Normalfall; gebündelte Skills sind im
//!    Median 4,7 KiB groß): der `##`-Abschnitt, dessen Überschrift (dann: dessen
//!    Text) am besten zu den Treffer-Schlüsseln passt; passt keiner, der
//!    ganze Skill, **wenn er unter den Pro-Turn-Deckel passt**, sonst der erste
//!    `##`-Abschnitt (ohne Abschnitte: der Anfang). Was über den Deckel ragt,
//!    wird an einer Zeilengrenze gekürzt und gekennzeichnet.
//! 4. Dedupe: ein Skill/Abschnitt höchstens einmal je Sitzung
//!    ([`InjectionState`]); wieder frei erst nach
//!    [`InjectionState::reset_after_compaction`].
//! 5. Budget: [`InjectionBudget::per_turn_bytes`] je Aufruf,
//!    [`InjectionBudget::per_session_bytes`] je Sitzung; was nicht mehr passt,
//!    steht in [`Selection::dropped`] (niedrigste Priorität zuerst verworfen,
//!    weil in Rangfolge abgearbeitet wird).
//!
//! # Rahmen und Provenienz
//! Jeder Auszug beginnt mit
//! `# Skill: <name> (sha256 <hex>) [triggered by: <Art> <Schlüssel>]` — dieselbe
//! Form wie die fest eingebundenen Skills, plus Grund. Der SHA-256 deckt den
//! ganzen Skilltext ab.
//!
//! # Rechte
//! Ein Skill verleiht nie Rechte; getriggerte Auszüge sind Text, sonst nichts.
//!
//! # L0
//! [`catalog_lines`] liefert die budgetierten Katalogzeilen `name — when`.

use std::collections::BTreeSet;
use std::fmt;

use crate::skill_index::{SkillIndex, SkillIndexEntry, markdown_headings, score_entry, tokens};
use crate::skill_triggers::{TriggerEvent, TriggerReason, word_tokens};

/// Standard-Pro-Turn-Deckel.
pub const DEFAULT_PER_TURN_BYTES: usize = 4096;
/// Standard-Pro-Sitzung-Deckel.
pub const DEFAULT_PER_SESSION_BYTES: usize = 16384;
/// Standard-L0-Budget.
pub const DEFAULT_L0_BYTES: usize = 1536;

/// Kennzeichnung eines gekürzten Auszugs.
const TRUNCATION_MARKER: &str = "\n[… gekürzt; vollständig mit skills.load]";

/// Byte-Budgets für getriggerte Skill-Texte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectionBudget {
    /// Höchstens so viele Bytes Auszugstext je [`select`]-Aufruf.
    pub per_turn_bytes: usize,
    /// Höchstens so viele Bytes je Sitzung (bis zum Zurücksetzen).
    pub per_session_bytes: usize,
    /// Budget der L0-Katalogzeilen ([`catalog_lines`]).
    pub l0_bytes: usize,
}

impl Default for InjectionBudget {
    fn default() -> Self {
        Self {
            per_turn_bytes: DEFAULT_PER_TURN_BYTES,
            per_session_bytes: DEFAULT_PER_SESSION_BYTES,
            l0_bytes: DEFAULT_L0_BYTES,
        }
    }
}

/// Was in einer Sitzung schon geliefert wurde.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InjectionState {
    /// `(Skill, Abschnitt)`; leerer Abschnitt = ganzer Skill.
    delivered: BTreeSet<(String, String)>,
    session_bytes: usize,
}

impl InjectionState {
    /// Ein leerer Zustand (neue Sitzung).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bisher in dieser Sitzung gelieferte Bytes.
    #[must_use]
    pub fn session_bytes(&self) -> usize {
        self.session_bytes
    }

    /// Anzahl gelieferter Skill/Abschnitt-Paare.
    #[must_use]
    pub fn delivered_count(&self) -> usize {
        self.delivered.len()
    }

    /// Ob `skill`/`section` (`None` = ganzer Skill) schon geliefert wurde.
    /// Ein gelieferter ganzer Skill deckt alle seine Abschnitte.
    #[must_use]
    pub fn has_delivered(&self, skill: &str, section: Option<&str>) -> bool {
        let whole = (skill.to_owned(), String::new());
        if self.delivered.contains(&whole) {
            return true;
        }
        self.delivered
            .contains(&(skill.to_owned(), section.unwrap_or_default().to_owned()))
    }

    /// Setzt alles zurück: nach einer Verdichtung, die gelieferte Auszüge aus
    /// dem Kontext entfernt hat, dürfen sie wieder geliefert werden und das
    /// Sitzungsbudget beginnt neu.
    pub fn reset_after_compaction(&mut self) {
        self.delivered.clear();
        self.session_bytes = 0;
    }

    /// Vergisst nur `skill` (alle Abschnitte) — wenn nur dieser Auszug durch
    /// eine Verdichtung verloren ging. Die verbrauchten Bytes bleiben.
    pub fn forget_skill(&mut self, skill: &str) {
        self.delivered.retain(|(name, _)| name != skill);
    }
}

/// Ein ausgewählter Auszug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Injection {
    /// Skill-Name.
    pub skill: String,
    /// Überschrift des Abschnitts; `None` = ganzer Skill.
    pub section: Option<String>,
    /// Gerahmter Text, einsetzbar als Kontextfragment.
    pub text: String,
    /// Der Grund (Trigger-Art und Schlüssel).
    pub reason: TriggerReason,
    /// SHA-256 des ganzen Skilltexts.
    pub sha256: String,
    /// Länge von `text` in Bytes.
    pub bytes: usize,
}

impl Injection {
    /// Wandelt den Auszug in ein Kontextfragment (Sektion `skills.triggered`,
    /// `Stability::Fresh`, Vertrauensklasse `Instruction`) — rein, ohne
    /// Laufzeitverdrahtung. `produced_at` gibt der Aufrufer vor (die Funktion
    /// liest keine Uhr).
    ///
    /// # Errors
    /// [`harw_context::ContextError`], wenn Label oder Sektionsname ungültig
    /// sind (Steuerzeichen in einer Überschrift).
    pub fn into_fragment(
        self,
        produced_at: jiff::Timestamp,
    ) -> Result<harw_context::Fragment, harw_context::ContextError> {
        let label = match &self.section {
            Some(section) => format!("skill:{}:{section}", self.skill),
            None => format!("skill:{}", self.skill),
        };
        Ok(harw_context::Fragment {
            label: harw_context::FragmentLabel::try_new(label)?,
            section: harw_context::SectionName::try_new(FRAGMENT_SECTION)?,
            trust: harw_context::TrustClass::Instruction,
            stability: harw_context::Stability::Fresh,
            origin: harw_context::FragmentOrigin {
                provider: "harw-catalog".to_owned(),
                namespace: FRAGMENT_SECTION.to_owned(),
                produced_at,
            },
            cost: harw_lens_types::CostEstimate(u32::try_from(self.bytes).unwrap_or(u32::MAX)),
            digest: harw_types::ContentDigest::of(self.text.as_bytes()),
            body: self.text,
        })
    }
}

/// Sektionsname der getriggerten Skill-Fragmente.
pub const FRAGMENT_SECTION: &str = "skills.triggered";

/// Warum ein Treffer nicht geliefert wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Schon in dieser Sitzung geliefert.
    AlreadyDelivered,
    /// Pro-Turn-Deckel überschritten.
    TurnBudget,
    /// Pro-Sitzung-Deckel überschritten.
    SessionBudget,
}

impl fmt::Display for DropReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyDelivered => "bereits geliefert",
            Self::TurnBudget => "Pro-Turn-Budget überschritten",
            Self::SessionBudget => "Pro-Sitzung-Budget überschritten",
        })
    }
}

/// Ein getroffener, aber nicht gelieferter Auszug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// Skill-Name.
    pub skill: String,
    /// Gewählter Abschnitt (`None` = ganzer Skill).
    pub section: Option<String>,
    /// Warum nicht.
    pub reason: DropReason,
}

/// Ergebnis von [`select`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Zu liefernde Auszüge in Rangfolge.
    pub injections: Vec<Injection>,
    /// Treffer, die nicht geliefert wurden.
    pub dropped: Vec<Dropped>,
}

impl Selection {
    /// Summe der gelieferten Bytes.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.injections.iter().map(|item| item.bytes).sum()
    }
}

impl SkillIndex {
    /// Siehe [`select`].
    #[must_use]
    pub fn select(
        &self,
        events: &[TriggerEvent<'_>],
        state: &mut InjectionState,
        budget: &InjectionBudget,
    ) -> Selection {
        select(self, events, state, budget)
    }
}

struct Candidate<'a> {
    entry: &'a SkillIndexEntry,
    reasons: BTreeSet<TriggerReason>,
    priority: u8,
}

/// Wählt die Auszüge für `events` (siehe Moduldoku).
///
/// `state` wird fortgeschrieben (Dedupe, verbrauchte Bytes); ein Aufruf ist
/// ein Turn.
#[must_use]
pub fn select(
    index: &SkillIndex,
    events: &[TriggerEvent<'_>],
    state: &mut InjectionState,
    budget: &InjectionBudget,
) -> Selection {
    let triggers = index.trigger_index();
    let mut candidates: Vec<Candidate<'_>> = triggers
        .hits(events)
        .into_iter()
        .filter_map(|(name, reasons)| {
            let entry = index.get(&name)?;
            (entry.name == name).then(|| Candidate {
                priority: triggers.priority(&name),
                entry,
                reasons,
            })
        })
        .collect();
    candidates.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| b.reasons.len().cmp(&a.reasons.len()))
            .then_with(|| a.entry.name.cmp(&b.entry.name))
    });

    let mut selection = Selection::default();
    let mut turn_used = 0usize;
    for candidate in candidates {
        let Some(reason) = candidate.reasons.iter().next().cloned() else {
            continue;
        };
        let chosen = choose_text(candidate.entry, &candidate.reasons, &reason, budget);
        let name = candidate.entry.name.clone();
        let section = chosen.section.clone();
        if state.has_delivered(&name, section.as_deref()) {
            selection.dropped.push(Dropped {
                skill: name,
                section,
                reason: DropReason::AlreadyDelivered,
            });
            continue;
        }
        let bytes = chosen.text.len();
        let turn_left = budget.per_turn_bytes.saturating_sub(turn_used);
        let session_left = budget.per_session_bytes.saturating_sub(state.session_bytes);
        let refusal = if chosen.too_small || bytes > turn_left {
            Some(DropReason::TurnBudget)
        } else if bytes > session_left {
            Some(DropReason::SessionBudget)
        } else {
            None
        };
        if let Some(reason) = refusal {
            selection.dropped.push(Dropped {
                skill: name,
                section,
                reason,
            });
            continue;
        }
        turn_used += bytes;
        state.session_bytes += bytes;
        state
            .delivered
            .insert((name.clone(), section.clone().unwrap_or_default()));
        selection.injections.push(Injection {
            skill: name,
            section,
            text: chosen.text,
            reason,
            sha256: candidate.entry.snapshot().sha256.clone(),
            bytes,
        });
    }
    selection
}

struct Chosen {
    section: Option<String>,
    text: String,
    /// Der Deckel lässt nicht einmal Rahmen und Kennzeichnung zu.
    too_small: bool,
}

/// `(Überschrift, Abschnittstext ab Überschrift)` aller `##`-Abschnitte.
fn level2_sections(text: &str) -> Vec<(String, &str)> {
    let headings = markdown_headings(text);
    headings
        .iter()
        .enumerate()
        .filter(|(_, (_, level, _))| *level == 2)
        .filter_map(|(position, (start, _, heading))| {
            let end = headings
                .iter()
                .skip(position + 1)
                .find(|(_, level, _)| *level <= 2)
                .map_or(text.len(), |(offset, _, _)| *offset);
            Some((heading.clone(), text.get(*start..end)?.trim_end()))
        })
        .collect()
}

fn choose_text(
    entry: &SkillIndexEntry,
    reasons: &BTreeSet<TriggerReason>,
    reason: &TriggerReason,
    budget: &InjectionBudget,
) -> Chosen {
    let snapshot = entry.snapshot();
    let instructions = snapshot.instructions.as_str();
    let frame = format!(
        "# Skill: {} (sha256 {}) [triggered by: {reason}]\n\n",
        entry.name, snapshot.sha256
    );
    let sections = level2_sections(instructions);
    let best = best_section(&sections, reasons);
    let whole = instructions.trim_end();
    let (label, body): (Option<String>, &str) = match best {
        Some((heading, body)) => (Some(heading), body),
        None if frame.len() + whole.len() < budget.per_turn_bytes => (None, whole),
        None => match sections.first() {
            Some((heading, body)) => (Some(heading.clone()), *body),
            None => (None, whole),
        },
    };
    let allowance = budget
        .per_turn_bytes
        .saturating_sub(frame.len() + 1 + TRUNCATION_MARKER.len());
    let (body, cut) = if frame.len() + body.len() < budget.per_turn_bytes {
        (body, false)
    } else {
        (cut_at_line(body, allowance), true)
    };
    let mut text = frame;
    text.push_str(body);
    if cut {
        text.push_str(TRUNCATION_MARKER);
    }
    text.push('\n');
    Chosen {
        section: label,
        too_small: cut && body.is_empty(),
        text,
    }
}

/// Der Abschnitt mit der besten Passung zu den Treffer-Schlüsseln:
/// zuerst Überschrift, dann Text. `None`, wenn keiner passt.
fn best_section<'a>(
    sections: &[(String, &'a str)],
    reasons: &BTreeSet<TriggerReason>,
) -> Option<(String, &'a str)> {
    let keys: Vec<Vec<String>> = reasons
        .iter()
        .map(|reason| {
            word_tokens(&reason.key)
                .into_iter()
                .filter(|token| token.chars().count() >= 2)
                .collect::<Vec<_>>()
        })
        .filter(|tokens: &Vec<String>| !tokens.is_empty())
        .collect();
    let mut best: Option<(usize, usize, usize)> = None;
    for (position, (heading, body)) in sections.iter().enumerate() {
        let heading_tokens: BTreeSet<String> = word_tokens(heading).into_iter().collect();
        let body_tokens: BTreeSet<String> = word_tokens(body).into_iter().collect();
        let primary: usize = keys
            .iter()
            .flatten()
            .filter(|token| heading_tokens.contains(*token))
            .count();
        let secondary = keys
            .iter()
            .filter(|tokens| tokens.iter().all(|token| body_tokens.contains(token)))
            .count();
        if (primary, secondary) == (0, 0) {
            continue;
        }
        if best.is_none_or(|(p, s, _)| (primary, secondary) > (p, s)) {
            best = Some((primary, secondary, position));
        }
    }
    let (_, _, position) = best?;
    sections
        .get(position)
        .map(|(heading, body)| (heading.clone(), *body))
}

/// Kürzt `text` auf höchstens `max` Bytes: an der letzten Zeilengrenze,
/// sonst an einer Zeichengrenze.
fn cut_at_line(text: &str, max: usize) -> &str {
    let cut = floor_char_boundary(text, max);
    let head = text.get(..cut).unwrap_or_default();
    match head.rfind('\n') {
        Some(line_end) if line_end > 0 => head.get(..line_end).unwrap_or(head),
        _ => head,
    }
}

/// Größte Zeichengrenze `<= max` (MSRV 1.85 kennt `floor_char_boundary` nicht).
fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut cut = max.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// Die L0-Katalogzeilen `name — when` der für `task_text` am besten
/// passenden Skills, hart auf `l0_bytes` begrenzt.
///
/// # Beschreibung
/// Rang: Trigger-Treffer im Aufgabentext, dann Suchrelevanz
/// ([`SkillIndex::search`]), dann `priority`, dann Name. Gibt es Treffer, stehen
/// nur sie im Katalog; sonst alle Skills nach `priority` und Name. Der Satz ist
/// `when`, sonst der erste Satz der Beschreibung. Jede Zeile zählt mit ihrem
/// Zeilenumbruch gegen das Budget; passt die erste Zeile nicht, wird sie an einer
/// Zeichengrenze gekürzt (nie mitten in einem UTF-8-Zeichen), weitere Zeilen,
/// die nicht mehr passen, entfallen.
#[must_use]
pub fn catalog_lines(index: &SkillIndex, task_text: &str, l0_bytes: usize) -> Vec<String> {
    let hits = index
        .trigger_index()
        .hits(&[TriggerEvent::TaskText(task_text)]);
    let words: Vec<String> = tokens(
        task_text
            .get(..floor_char_boundary(task_text, 64 * 1024))
            .unwrap_or_default(),
    );
    let mut ranked: Vec<(usize, u32, u8, &SkillIndexEntry)> = index
        .entries()
        .map(|entry| {
            let trigger_hits = hits.get(&entry.name).map_or(0, BTreeSet::len);
            let score = if words.is_empty() {
                0
            } else {
                score_entry(entry, &words)
            };
            (
                trigger_hits,
                score,
                index.trigger_index().priority(&entry.name),
                entry,
            )
        })
        .collect();
    if ranked
        .iter()
        .any(|(hits, score, _, _)| *hits > 0 || *score > 0)
    {
        ranked.retain(|(hits, score, _, _)| *hits > 0 || *score > 0);
    }
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.3.name.cmp(&b.3.name))
    });
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    for (_, _, _, entry) in ranked {
        let sentence = single_line(&entry.catalog_sentence());
        let line = if sentence.is_empty() {
            single_line(&entry.name)
        } else {
            format!("{} — {sentence}", single_line(&entry.name))
        };
        let cost = line.len() + 1;
        if used + cost <= l0_bytes {
            used += cost;
            lines.push(line);
        } else if lines.is_empty() && l0_bytes > 1 {
            let cut = floor_char_boundary(&line, l0_bytes - 1);
            lines.push(line.get(..cut).unwrap_or_default().to_owned());
            break;
        }
    }
    lines
}

/// Steuerzeichen und Zeilenumbrüche zu einem Leerzeichen, Leerraum
/// zusammengefasst.
fn single_line(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::path::Path;
    use std::time::{Duration, Instant};

    const BODY: &str = "# Borrow\n\nEinleitung.\n\n## Wann nutzen\n\nImmer wenn der Borrow Checker meckert.\n\n## Ursache E0505\n\nDer Wert wird noch geborgt.\n\n## Behebung\n\nBesitze den Wert vorher.\n";

    fn write_skill(layer: &Path, name: &str, triggers: Option<&str>, body: &str) -> TestResult {
        let dir = layer.join("skills").join(name);
        std::fs::create_dir_all(&dir).map_err(ctx("mkdir"))?;
        std::fs::write(
            dir.join("skill.toml"),
            format!(
                "name = \"{name}\"\ndescription = \"Beschreibung von {name}. Zweiter Satz.\"\n"
            ),
        )
        .map_err(ctx("manifest"))?;
        std::fs::write(dir.join("instructions.md"), body).map_err(ctx("body"))?;
        if let Some(triggers) = triggers {
            std::fs::write(dir.join("triggers.toml"), triggers).map_err(ctx("triggers"))?;
        }
        Ok(())
    }

    fn build(layer: &Path) -> SkillIndex {
        SkillIndex::build_with_bundle(&[layer.to_path_buf()], &[])
    }

    fn fixture() -> TestResult<(tempfile::TempDir, SkillIndex)> {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_skill(
            layer.path(),
            "borrow",
            Some(
                "when = \"Rust-Borrow-Fehler\"\n[triggers]\nkeywords = [\"borrow checker\"]\nerror_codes = [\"E0505\"]\npaths = [\"**/*.rs\"]\ntools = [\"process.execute\"]\ncommands = [\"cargo test\"]\npriority = 70\n",
            ),
            BODY,
        )?;
        write_skill(
            layer.path(),
            "other",
            Some("[triggers]\nkeywords = [\"borrow checker\"]\n"),
            BODY,
        )?;
        write_skill(layer.path(), "plain", None, BODY)?;
        let index = build(layer.path());
        Ok((layer, index))
    }

    fn names(selection: &Selection) -> Vec<&str> {
        selection
            .injections
            .iter()
            .map(|i| i.skill.as_str())
            .collect()
    }

    #[test]
    fn every_trigger_kind_hits_and_misses() -> TestResult {
        let (_layer, index) = fixture()?;
        let run = |event: TriggerEvent<'_>| {
            let mut state = InjectionState::new();
            index.select(&[event], &mut state, &InjectionBudget::default())
        };
        let hit = |event| !run(event).injections.is_empty();
        assert!(hit(TriggerEvent::TaskText("Der Borrow Checker meckert")));
        assert!(!hit(TriggerEvent::TaskText("nur ein Gespräch")));
        assert!(hit(TriggerEvent::ToolOutput {
            tool: "process.execute",
            text: "error[E0505]"
        }));
        assert!(!hit(TriggerEvent::ToolOutput {
            tool: "process.execute",
            text: "error E05051 XE0505"
        }));
        assert!(hit(TriggerEvent::ToolFirstUse("process.execute")));
        assert!(!hit(TriggerEvent::ToolFirstUse("fs.read")));
        assert!(hit(TriggerEvent::PathTouched("src/lib.rs")));
        assert!(!hit(TriggerEvent::PathTouched("src/lib.rsx")));
        assert!(hit(TriggerEvent::Command("cargo test -p x")));
        assert!(!hit(TriggerEvent::Command("cargo testfoo")));
        Ok(())
    }

    #[test]
    fn frame_matches_the_bound_skill_form_and_picks_the_section() -> TestResult {
        let (_layer, index) = fixture()?;
        let mut state = InjectionState::new();
        let selection = index.select(
            &[TriggerEvent::ToolOutput {
                tool: "t",
                text: "error[E0505]: x",
            }],
            &mut state,
            &InjectionBudget::default(),
        );
        let first = selection
            .injections
            .first()
            .ok_or(TestError::Missing("Auszug"))?;
        assert_eq!(first.skill, "borrow");
        assert_eq!(first.section.as_deref(), Some("Ursache E0505"));
        let sha = &index
            .get("borrow")
            .ok_or(TestError::Missing("borrow"))?
            .snapshot()
            .sha256;
        assert_eq!(&first.sha256, sha);
        assert!(
            first.text.starts_with(&format!(
                "# Skill: borrow (sha256 {sha}) [triggered by: error_code E0505]\n\n## Ursache E0505"
            )),
            "{}",
            first.text
        );
        assert!(!first.text.contains("Behebung"));
        assert_eq!(first.bytes, first.text.len());
        // Ohne Überschriftentreffer: kleiner ganzer Skill.
        let mut state = InjectionState::new();
        let selection = index.select(
            &[TriggerEvent::ToolFirstUse("process.execute")],
            &mut state,
            &InjectionBudget::default(),
        );
        let first = selection
            .injections
            .first()
            .ok_or(TestError::Missing("Auszug"))?;
        assert_eq!(first.section, None);
        assert!(first.text.contains("## Behebung") && first.text.contains("Einleitung."));
        Ok(())
    }

    #[test]
    fn ordering_is_priority_then_hits_then_name() -> TestResult {
        let (_layer, index) = fixture()?;
        let mut state = InjectionState::new();
        let selection = index.select(
            &[TriggerEvent::TaskText("borrow checker")],
            &mut state,
            &InjectionBudget::default(),
        );
        assert_eq!(names(&selection), ["borrow", "other"]);
        Ok(())
    }

    #[test]
    fn dedupe_across_two_calls_and_reset_after_compaction() -> TestResult {
        let (_layer, index) = fixture()?;
        let mut state = InjectionState::new();
        let budget = InjectionBudget::default();
        let events = [TriggerEvent::ToolOutput {
            tool: "t",
            text: "E0505",
        }];
        let first = index.select(&events, &mut state, &budget);
        assert_eq!(first.injections.len(), 1);
        let used = state.session_bytes();
        assert!(used > 0);
        let second = index.select(&events, &mut state, &budget);
        assert!(second.injections.is_empty());
        assert_eq!(
            second.dropped,
            [Dropped {
                skill: "borrow".to_owned(),
                section: Some("Ursache E0505".to_owned()),
                reason: DropReason::AlreadyDelivered
            }]
        );
        assert_eq!(state.session_bytes(), used, "nichts doppelt gezählt");
        state.reset_after_compaction();
        assert_eq!(state.session_bytes(), 0);
        assert_eq!(state.delivered_count(), 0);
        assert_eq!(
            index.select(&events, &mut state, &budget).injections.len(),
            1
        );
        state.forget_skill("borrow");
        assert!(!state.has_delivered("borrow", Some("Ursache E0505")));
        Ok(())
    }

    #[test]
    fn injection_converts_to_an_instruction_fragment() -> TestResult {
        let (_layer, index) = fixture()?;
        let mut state = InjectionState::new();
        let selection = index.select(
            &[TriggerEvent::ToolOutput {
                tool: "t",
                text: "E0505",
            }],
            &mut state,
            &InjectionBudget::default(),
        );
        let injection = selection
            .injections
            .into_iter()
            .next()
            .ok_or(TestError::Missing("Auszug"))?;
        let text = injection.text.clone();
        let fragment = injection
            .into_fragment(jiff::Timestamp::UNIX_EPOCH)
            .map_err(ctx("fragment"))?;
        assert_eq!(fragment.section.as_str(), FRAGMENT_SECTION);
        assert_eq!(fragment.trust, harw_context::TrustClass::Instruction);
        assert_eq!(fragment.stability, harw_context::Stability::Fresh);
        assert_eq!(fragment.body, text);
        assert_eq!(fragment.label.as_str(), "skill:borrow:Ursache E0505");
        Ok(())
    }

    #[test]
    fn a_delivered_whole_skill_covers_its_sections() {
        let mut state = InjectionState::new();
        state.delivered.insert(("s".to_owned(), String::new()));
        assert!(state.has_delivered("s", Some("Beliebig")));
        assert!(!state.has_delivered("t", None));
    }

    #[test]
    fn budget_cuts_and_reports_dropped() -> TestResult {
        let (_layer, index) = fixture()?;
        let events = [TriggerEvent::TaskText("borrow checker")];
        // Pro Turn: nur Platz für einen Auszug.
        let one = {
            let mut state = InjectionState::new();
            let selection = index.select(&events, &mut state, &InjectionBudget::default());
            selection
                .injections
                .first()
                .map(|i| i.bytes)
                .ok_or(TestError::Missing("Auszug"))?
        };
        let budget = InjectionBudget {
            per_turn_bytes: one + 10,
            ..InjectionBudget::default()
        };
        let mut state = InjectionState::new();
        let selection = index.select(&events, &mut state, &budget);
        assert_eq!(names(&selection), ["borrow"]);
        assert_eq!(selection.dropped.len(), 1);
        assert_eq!(
            selection.dropped.first().map(|d| d.reason),
            Some(DropReason::TurnBudget)
        );
        assert!(selection.bytes() <= budget.per_turn_bytes);
        // Pro Sitzung.
        let budget = InjectionBudget {
            per_session_bytes: one + 10,
            ..InjectionBudget::default()
        };
        let mut state = InjectionState::new();
        let selection = index.select(&events, &mut state, &budget);
        assert_eq!(names(&selection), ["borrow"]);
        assert_eq!(
            selection.dropped.first().map(|d| d.reason),
            Some(DropReason::SessionBudget)
        );
        assert!(state.session_bytes() <= budget.per_session_bytes);
        // Null-Budget liefert nichts, ohne Panic.
        let zero = InjectionBudget {
            per_turn_bytes: 0,
            per_session_bytes: 0,
            l0_bytes: 0,
        };
        let mut state = InjectionState::new();
        let selection = index.select(&events, &mut state, &zero);
        assert!(selection.injections.is_empty());
        assert_eq!(selection.dropped.len(), 2);
        Ok(())
    }

    #[test]
    fn large_skills_are_cut_to_a_section_or_truncated_within_the_cap() -> TestResult {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let filler = "Zeile mit Text.\n".repeat(400);
        let body = format!(
            "# Gross\n\n## Eins\n\n{filler}\n## Zwei\n\nKurz E0505.\n\n## Drei\n\n{filler}"
        );
        write_skill(
            layer.path(),
            "gross",
            Some("[triggers]\nkeywords = [\"zwei\"]\ncommands = [\"make\"]\n"),
            &body,
        )?;
        let index = build(layer.path());
        let budget = InjectionBudget::default();
        // Überschrift „Zwei“ passt.
        let mut state = InjectionState::new();
        let selection = index.select(&[TriggerEvent::TaskText("Zwei")], &mut state, &budget);
        let hit = selection
            .injections
            .first()
            .ok_or(TestError::Missing("Auszug"))?;
        assert_eq!(hit.section.as_deref(), Some("Zwei"));
        assert!(hit.text.contains("Kurz E0505.") && !hit.text.contains("Zeile mit Text"));
        // Kein Überschriftentreffer, Skill größer als der Deckel: erster Abschnitt, gekürzt.
        let mut state = InjectionState::new();
        let selection = index.select(&[TriggerEvent::Command("make all")], &mut state, &budget);
        let hit = selection
            .injections
            .first()
            .ok_or(TestError::Missing("Auszug"))?;
        assert_eq!(hit.section.as_deref(), Some("Eins"));
        assert!(hit.bytes <= budget.per_turn_bytes, "{}", hit.bytes);
        assert!(hit.text.contains("gekürzt"));
        Ok(())
    }

    #[test]
    fn selection_is_deterministic_and_order_independent() -> TestResult {
        let (_layer, index) = fixture()?;
        let a = [
            TriggerEvent::TaskText("borrow checker"),
            TriggerEvent::PathTouched("a.rs"),
            TriggerEvent::Command("cargo test"),
            TriggerEvent::ToolOutput {
                tool: "t",
                text: "E0505",
            },
        ];
        let mut b = a;
        b.reverse();
        let mut reference = None;
        for round in 0..20 {
            let events: &[TriggerEvent<'_>] = if round % 2 == 0 { &a } else { &b };
            let mut state = InjectionState::new();
            let selection = index.select(events, &mut state, &InjectionBudget::default());
            match &reference {
                None => reference = Some(selection),
                Some(first) => assert_eq!(first, &selection, "Runde {round}"),
            }
        }
        Ok(())
    }

    #[test]
    fn select_never_returns_a_skill_outside_the_index() -> TestResult {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_skill(
            layer.path(),
            "off",
            Some("[triggers]\nkeywords = [\"geheim\"]\n"),
            BODY,
        )?;
        let toml = layer.path().join("skills/off/skill.toml");
        std::fs::write(&toml, "name = \"off\"\nenabled = false\n").map_err(ctx("disable"))?;
        let index = build(layer.path());
        assert!(index.get("off").is_none());
        let mut state = InjectionState::new();
        let selection = index.select(
            &[TriggerEvent::TaskText("geheim")],
            &mut state,
            &InjectionBudget::default(),
        );
        assert!(selection.injections.is_empty() && selection.dropped.is_empty());
        // Und allgemein: jeder gelieferte Name steht im Index.
        let (_l, index) = fixture()?;
        let mut state = InjectionState::new();
        let selection = index.select(
            &[TriggerEvent::TaskText("borrow checker E0505")],
            &mut state,
            &InjectionBudget::default(),
        );
        assert!(
            selection
                .injections
                .iter()
                .all(|i| index.get(&i.skill).is_some())
        );
        Ok(())
    }

    #[test]
    fn invalid_triggers_drop_only_the_trigger_not_the_skill() -> TestResult {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_skill(
            layer.path(),
            "halb",
            Some(
                "when = 5\n[triggers]\nkeywords = [\"gut genug\", \"x\"]\nerror_codes = [\"kaputt!\"]\n",
            ),
            BODY,
        )?;
        write_skill(layer.path(), "kaputt", Some("das ist [[[ kein toml"), BODY)?;
        let index = build(layer.path());
        assert_eq!(index.len(), 2);
        assert!(index.skipped().is_empty(), "{:?}", index.skipped());
        let halb = index.get("halb").ok_or(TestError::Missing("halb"))?;
        assert_eq!(halb.triggers.keywords, ["gut genug"]);
        assert!(halb.triggers.error_codes.is_empty());
        assert!(halb.when.is_none());
        assert!(
            index
                .warnings()
                .iter()
                .any(|w| w.skill == "halb" && w.reason.contains("error_codes"))
        );
        assert!(index.warnings().iter().any(|w| w.skill == "kaputt"));
        let kaputt = index.get("kaputt").ok_or(TestError::Missing("kaputt"))?;
        assert!(kaputt.triggers.is_empty());
        Ok(())
    }

    #[test]
    fn symlinked_or_oversized_trigger_files_are_discarded_not_fatal() -> TestResult {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let huge = format!(
            "# {}\n",
            "x".repeat(crate::skill_triggers::MAX_TRIGGERS_FILE_BYTES)
        );
        write_skill(layer.path(), "riesig", Some(&huge), BODY)?;
        let outside = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(
            outside.path().join("t.toml"),
            "[triggers]\nkeywords=[\"leck\"]\n",
        )
        .map_err(ctx("outside"))?;
        write_skill(layer.path(), "link", None, BODY)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join("t.toml"),
            layer.path().join("skills/link/triggers.toml"),
        )
        .map_err(ctx("symlink"))?;
        let index = build(layer.path());
        assert_eq!(index.len(), 2);
        assert!(index.get("riesig").is_some_and(|e| e.triggers.is_empty()));
        #[cfg(unix)]
        assert!(
            index.get("link").is_some_and(|e| e.triggers.is_empty()),
            "Symlink hinaus gelesen"
        );
        assert!(index.warnings().iter().any(|w| w.skill == "riesig"));
        Ok(())
    }

    #[test]
    fn bundled_skills_load_unchanged() {
        let index = SkillIndex::build(&[]);
        assert_eq!(index.len(), 75, "Anzahl gebündelter Skills");
        assert!(index.skipped().is_empty(), "{:?}", index.skipped());
        assert!(index.warnings().is_empty(), "{:?}", index.warnings());
        assert!(
            index.trigger_index().is_empty(),
            "S1 liefert noch keine Trigger"
        );
    }

    #[test]
    fn l0_lines_use_when_then_description_and_respect_the_budget() -> TestResult {
        let (_layer, index) = fixture()?;
        let lines = catalog_lines(&index, "borrow checker", 1536);
        assert_eq!(
            lines.first().map(String::as_str),
            Some("borrow — Rust-Borrow-Fehler")
        );
        assert!(lines.iter().any(|l| l == "other — Beschreibung von other."));
        let all = catalog_lines(&index, "", 1536);
        assert_eq!(all.len(), 3);
        for budget in 0..120 {
            let lines = catalog_lines(&index, "", budget);
            let total: usize = lines.iter().map(|l| l.len() + 1).sum();
            assert!(total <= budget, "{budget}: {total}");
        }
        Ok(())
    }

    #[test]
    fn l0_never_splits_a_utf8_character() -> TestResult {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_skill(
            layer.path(),
            "uml",
            Some("when = \"Übergrößenmäßig äöü 日本語 🦀🦀🦀\"\n"),
            BODY,
        )?;
        let index = build(layer.path());
        for budget in 0..80 {
            let lines = catalog_lines(&index, "", budget);
            for line in &lines {
                assert!(line.len() < budget.max(1), "{budget}");
                assert!(std::str::from_utf8(line.as_bytes()).is_ok());
            }
        }
        let big = catalog_lines(&index, "", 1536);
        assert_eq!(big, ["uml — Übergrößenmäßig äöü 日本語 🦀🦀🦀"]);
        Ok(())
    }

    #[test]
    fn hostile_inputs_neither_panic_nor_hang() -> TestResult {
        let (_layer, index) = fixture()?;
        let started = Instant::now();
        let mb = "borrow checker E0505 ".repeat(50_000);
        let controls = "\u{0}\u{1}\u{7}\r\n\t\u{1b}[31m E0505 \u{202e}";
        let long_path = "a/".repeat(50_000);
        let long_command = "cargo test && ".repeat(50_000);
        let glob_bomb = "a".repeat(4000);
        let events = [
            TriggerEvent::TaskText(""),
            TriggerEvent::TaskText(&mb),
            TriggerEvent::TaskText(controls),
            TriggerEvent::ToolOutput {
                tool: "",
                text: &mb,
            },
            TriggerEvent::ToolOutput {
                tool: "t",
                text: controls,
            },
            TriggerEvent::ToolFirstUse(""),
            TriggerEvent::ToolFirstUse(controls),
            TriggerEvent::PathTouched(""),
            TriggerEvent::PathTouched(&long_path),
            TriggerEvent::PathTouched(&glob_bomb),
            TriggerEvent::Command(""),
            TriggerEvent::Command(&long_command),
            TriggerEvent::Command(controls),
        ];
        for event in events {
            let mut state = InjectionState::new();
            let _ = index.select(&[event], &mut state, &InjectionBudget::default());
            let _ = catalog_lines(
                &index,
                match event {
                    TriggerEvent::TaskText(t) => t,
                    _ => "",
                },
                1536,
            );
        }
        let mut state = InjectionState::new();
        let _ = index.select(&events, &mut state, &InjectionBudget::default());
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
        // Riesiges Glob-Muster im Manifest wird verworfen.
        let pattern = "*a".repeat(5000);
        let (spec, warnings) = crate::skill_triggers::parse_triggers_file(&format!(
            "[triggers]\npaths = [\"{pattern}\"]\n"
        ));
        assert!(spec.triggers.paths.is_empty() && !warnings.is_empty());
        Ok(())
    }
}
