//! Projektion des Journals auf einen Sitz und Leak-Guards
//! (matrix-game.md §2.2, §2.5, §9.1).
//!
//! [`SeatView`] ist die **einzige** Quelle für Prompts: Sie lässt sich nur
//! über [`project`] bzw. [`project_observer`] erzeugen und enthält weder
//! Sequenznummern noch Zeitstempel. Die Nummerierung ist pro Sicht lokal —
//! fremde Gespräche verschieben sie nicht, die Projektion (und damit der
//! gerenderte Prompt) ist byte-identisch, ob andere verhandelt haben oder
//! nicht.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::commitments::canonical_json;
use crate::error::MatrixResult;
use crate::phases::RoundArgument;
use crate::scenario::{LeakGuardMode, UmpireNegotiations, VarVisibility, VisibilitySettings};
use crate::state::{Audience, EntryKind, GameEntry, GameState, Journal, PlayerId, Seat, WorldVar};

/// Wortanzahl der Shingles des Inhaltsscanners.
pub const SHINGLE_WORDS: usize = 5;

/// Sichtbarkeitskonfiguration einer Projektion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VisibilityCfg {
    /// Umpire-Zugriff auf Paar-Kanäle.
    pub umpire_negotiations: UmpireNegotiations,
    /// Bei `cited`: für die laufende Adjudikation zitierte Kanäle.
    pub cited_channels: BTreeSet<String>,
}

impl VisibilityCfg {
    /// Aus den Szenario-Einstellungen (ohne zitierte Kanäle).
    #[must_use]
    pub fn from_settings(settings: &VisibilitySettings) -> Self {
        Self {
            umpire_negotiations: settings.umpire_negotiations,
            cited_channels: BTreeSet::new(),
        }
    }

    /// Mit zitierten Kanälen (nur bei `cited` wirksam).
    #[must_use]
    pub fn with_cited(mut self, channels: BTreeSet<String>) -> Self {
        self.cited_channels = channels;
        self
    }
}

/// Kanäle, die der Umpire im Modus `cited` für diese Adjudikation sehen darf:
/// genau die, die ein Argument zitiert **und** deren Mitglied der Zitierende
/// ist (§9.1 Nr. 3).
#[must_use]
pub fn cited_channels(state: &GameState, args: &[RoundArgument]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for arg in args {
        for channel in &arg.body.cites_negotiation {
            if state
                .channels
                .get(channel)
                .is_some_and(|c| c.has_member(&arg.seat))
            {
                out.insert(channel.clone());
            }
        }
    }
    out
}

/// Sieht `seat` eine Weltvariable dieser Sichtbarkeit?
#[must_use]
pub fn var_visible_to(visibility: &VarVisibility, seat: &Seat) -> bool {
    match (visibility, seat) {
        (VarVisibility::Public, _) | (_, Seat::Umpire) => true,
        (_, Seat::RedCell) | (VarVisibility::Umpire, Seat::Player(_)) => false,
        (VarVisibility::Seat(owner), Seat::Player(p)) => owner == p,
        (VarVisibility::Seats(owners), Seat::Player(p)) => owners.contains(p),
    }
}

/// Alle offengelegten Geheimnisse (aus öffentlichen `SecretRevealed`).
#[must_use]
pub fn revealed_secrets(journal: &Journal) -> BTreeSet<String> {
    journal
        .entries()
        .filter_map(|e| match &e.kind {
            EntryKind::SecretRevealed { secret_id, .. } => Some(secret_id.clone()),
            _ => None,
        })
        .collect()
}

/// Sieht `seat` diesen Eintrag?
#[must_use]
pub fn visible_to(
    entry: &GameEntry,
    seat: &Seat,
    cfg: &VisibilityCfg,
    revealed: &BTreeSet<String>,
) -> bool {
    if matches!(entry.audience, Audience::ObserverOnly) {
        return false;
    }
    if entry
        .secret_id
        .as_ref()
        .is_some_and(|s| revealed.contains(s))
    {
        return true;
    }
    match (&entry.audience, seat) {
        (Audience::Public, _) => true,
        (Audience::Pair(a, b), Seat::Player(p)) => p == a || p == b,
        (Audience::Pair(..), Seat::Umpire) => match cfg.umpire_negotiations {
            UmpireNegotiations::None => false,
            UmpireNegotiations::Full => true,
            UmpireNegotiations::Cited => entry
                .kind
                .channel_id()
                .is_some_and(|c| cfg.cited_channels.contains(c)),
        },
        (Audience::UmpireOnly | Audience::SeatAndUmpire(_), Seat::Umpire) => true,
        (Audience::Seat(q) | Audience::SeatAndUmpire(q), Seat::Player(p)) => p == q,
        // Die Red Cell sieht ausschließlich Öffentliches.
        (_, Seat::RedCell)
        | (Audience::UmpireOnly, Seat::Player(_))
        | (Audience::Seat(_), Seat::Umpire)
        | (Audience::ObserverOnly, _) => false,
    }
}

/// Wer eine Projektion sieht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "viewer", content = "seat", rename_all = "snake_case")]
pub enum Viewer {
    /// Ein Sitz.
    Seat(Seat),
    /// Der menschliche Facilitator/Beobachter (sieht alles).
    Observer,
}

/// Ein Eintrag der Projektion — ohne Journal-Sequenz und Zeitstempel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewEntry {
    /// Lokale Nummer innerhalb dieser Sicht (1-basiert).
    pub n: u32,
    /// Runde.
    pub round: u32,
    /// Audience.
    pub audience: Audience,
    /// In-Game-Offenlegung.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disclosed_by: Option<PlayerId>,
    /// Inhalt.
    pub kind: EntryKind,
}

/// Projektion des Journals auf einen Sitz — einziger zulässiger Prompt-Input.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeatView {
    viewer: Viewer,
    entries: Vec<ViewEntry>,
    world: BTreeMap<String, WorldVar>,
}

impl SeatView {
    /// Betrachter.
    #[must_use]
    pub fn viewer(&self) -> &Viewer {
        &self.viewer
    }

    /// Sichtbare Einträge.
    #[must_use]
    pub fn entries(&self) -> &[ViewEntry] {
        &self.entries
    }

    /// Einträge nach lokaler Nummer `n` (Delta für Multi-Turn-Prompts).
    #[must_use]
    pub fn entries_since(&self, n: u32) -> &[ViewEntry] {
        let start = self
            .entries
            .iter()
            .position(|e| e.n > n)
            .unwrap_or(self.entries.len());
        self.entries.get(start..).unwrap_or(&[])
    }

    /// Sichtbarer Weltzustand.
    #[must_use]
    pub fn world(&self) -> &BTreeMap<String, WorldVar> {
        &self.world
    }

    /// Eigene Paar-Kanäle (IDs).
    #[must_use]
    pub fn own_channels(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::ChannelOpened { channel, .. } => Some(channel.clone()),
                _ => None,
            })
            .collect()
    }

    /// Kanonisches JSON (Grundlage des Byte-Vergleichs und des Prompt-Renderers).
    ///
    /// # Errors
    /// [`crate::MatrixError::Json`].
    pub fn to_canonical_json(&self) -> MatrixResult<String> {
        canonical_json(self)
    }

    /// Kommt `needle` irgendwo in der Sicht vor (Marker-Suche)?
    ///
    /// # Errors
    /// [`crate::MatrixError::Json`].
    pub fn contains_text(&self, needle: &str) -> MatrixResult<bool> {
        let json = serde_json::to_string(self)?;
        Ok(json.contains(needle))
    }
}

fn fold_world(world: &mut BTreeMap<String, WorldVar>, entry: &GameEntry) {
    match &entry.kind {
        EntryKind::VarDeclared { var } => {
            world.insert(var.id.clone(), var.clone());
        }
        EntryKind::WorldDelta { var, to, .. } => {
            if let Some(v) = world.get_mut(var) {
                v.value = to.clone();
            }
        }
        _ => {}
    }
}

fn build_view(journal: &Journal, viewer: Viewer, keep: impl Fn(&GameEntry) -> bool) -> SeatView {
    let mut entries = Vec::new();
    let mut world = BTreeMap::new();
    for entry in journal.entries() {
        if !keep(entry) {
            continue;
        }
        fold_world(&mut world, entry);
        let n = u32::try_from(entries.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        entries.push(ViewEntry {
            n,
            round: entry.round,
            audience: entry.audience.clone(),
            disclosed_by: entry.disclosed_by.clone(),
            kind: entry.kind.clone(),
        });
    }
    SeatView {
        viewer,
        entries,
        world,
    }
}

/// Projektion für einen Sitz.
#[must_use]
pub fn project(journal: &Journal, seat: &Seat, cfg: &VisibilityCfg) -> SeatView {
    let revealed = revealed_secrets(journal);
    build_view(journal, Viewer::Seat(seat.clone()), |e| {
        visible_to(e, seat, cfg, &revealed)
    })
}

/// Projektion für den menschlichen Beobachter (alles).
#[must_use]
pub fn project_observer(journal: &Journal) -> SeatView {
    build_view(journal, Viewer::Observer, |_| true)
}

// ---------------------------------------------------------------------------
// Leak-Guards
// ---------------------------------------------------------------------------

/// Normalisierte Wörter (klein, alphanumerisch).
#[must_use]
pub fn normalize_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Wort-5-Gramme eines Texts.
#[must_use]
pub fn shingles(text: &str) -> BTreeSet<String> {
    normalize_words(text)
        .windows(SHINGLE_WORDS)
        .map(|w| w.join(" "))
        .collect()
}

/// Art eines Leak-Befunds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum LeakKind {
    /// Gemeinsames Wort-5-Gramm mit geschütztem Inhalt.
    Shingle(String),
    /// Nennung einer verdeckten Weltvariablen (ID oder Label).
    HiddenVariable(String),
}

/// Ein Leak-Befund.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeakFinding {
    /// Art.
    pub kind: LeakKind,
    /// Runde der Quelle.
    pub source_round: u32,
    /// Audience der Quelle.
    pub source_audience: Audience,
}

impl LeakFinding {
    /// Kurzbeschreibung für `LeakSuspect`.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.kind {
            LeakKind::Shingle(s) => format!(
                "5-Gramm „{s}“ aus {} (Runde {})",
                self.source_audience, self.source_round
            ),
            LeakKind::HiddenVariable(v) => {
                format!("verdeckte Variable `{v}` ({})", self.source_audience)
            }
        }
    }
}

fn is_protected(entry: &GameEntry, revealed: &BTreeSet<String>) -> bool {
    match entry.audience {
        Audience::Public | Audience::ObserverOnly => false,
        _ => entry
            .secret_id
            .as_ref()
            .is_none_or(|s| !revealed.contains(s)),
    }
}

fn scan_with(
    text: &str,
    journal: &Journal,
    mut classify: impl FnMut(&GameEntry, &BTreeSet<String>) -> bool,
    mut sink: impl FnMut(bool, LeakFinding),
) {
    let revealed = revealed_secrets(journal);
    let candidate = shingles(text);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    if !candidate.is_empty() {
        for entry in journal.entries() {
            if !is_protected(entry, &revealed) {
                continue;
            }
            let class = classify(entry, &revealed);
            for source in entry.kind.texts() {
                for shingle in shingles(source) {
                    if candidate.contains(&shingle) && seen.insert(shingle.clone()) {
                        sink(
                            class,
                            LeakFinding {
                                kind: LeakKind::Shingle(shingle),
                                source_round: entry.round,
                                source_audience: entry.audience.clone(),
                            },
                        );
                    }
                }
            }
        }
    }
    // Verdeckte Weltvariablen: letzte Deklaration je Variable zählt.
    let lowered = text.to_lowercase();
    let mut latest: BTreeMap<&str, (&WorldVar, &GameEntry)> = BTreeMap::new();
    for entry in journal.entries() {
        if let EntryKind::VarDeclared { var } = &entry.kind {
            latest.insert(var.id.as_str(), (var, entry));
        }
    }
    for (id, (var, entry)) in latest {
        if !var.is_hidden() {
            continue;
        }
        let hit = [id, var.label.as_str()].iter().any(|name| {
            let name = name.to_lowercase();
            name.chars().count() >= 4 && lowered.contains(&name)
        });
        if hit {
            let class = classify(entry, &revealed);
            sink(
                class,
                LeakFinding {
                    kind: LeakKind::HiddenVariable(id.to_owned()),
                    source_round: entry.round,
                    source_audience: entry.audience.clone(),
                },
            );
        }
    }
}

/// Scannt einen zur Veröffentlichung vorgesehenen Text (Umpire-Prosa)
/// gegen unveröffentlichte `Pair`-, `Seat`-, `SeatAndUmpire`- und
/// `UmpireOnly`-Inhalte sowie Namen verdeckter Weltvariablen.
#[must_use]
pub fn scan_public_text(text: &str, journal: &Journal) -> Vec<LeakFinding> {
    let mut findings = Vec::new();
    scan_with(text, journal, |_, _| true, |_, f| findings.push(f));
    findings
}

/// Entscheidung des Leak-Guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardDecision {
    /// Kein Befund — veröffentlichen.
    Clean,
    /// Erster Treffer: einmal mit Hinweis neu anfragen.
    Reprompt(Vec<LeakFinding>),
    /// Zweiter Treffer: zurückhalten, Facilitator bekommt `LeakSuspect`.
    Withhold(Vec<LeakFinding>),
    /// Modus `flag_only`: veröffentlichen, aber kennzeichnen.
    Flag(Vec<LeakFinding>),
}

/// Leak-Guard für öffentliche Umpire-Texte (`attempt` 0 = erster Versuch).
#[must_use]
pub fn guard_public_text(
    text: &str,
    journal: &Journal,
    mode: LeakGuardMode,
    attempt: u32,
) -> GuardDecision {
    let findings = scan_public_text(text, journal);
    if findings.is_empty() {
        return GuardDecision::Clean;
    }
    match mode {
        LeakGuardMode::FlagOnly => GuardDecision::Flag(findings),
        LeakGuardMode::Strict if attempt == 0 => GuardDecision::Reprompt(findings),
        LeakGuardMode::Strict => GuardDecision::Withhold(findings),
    }
}

/// `LeakSuspect`-Eintrag (nur Beobachter) für einen zurückgehaltenen Text.
#[must_use]
pub fn leak_suspect_entry(
    round: u32,
    source: &str,
    text: &str,
    findings: &[LeakFinding],
) -> GameEntry {
    GameEntry::new(
        round,
        Audience::ObserverOnly,
        EntryKind::LeakSuspect {
            source: source.to_owned(),
            text: text.to_owned(),
            findings: findings.iter().map(LeakFinding::describe).collect(),
        },
    )
}

/// Ergebnis des Spieler-Scanners (nur Kennzeichnung, keine Blockade).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerScan {
    /// Zitate aus Inhalten, die der Spieler kennt (In-Game-Offenlegung).
    pub disclosures: Vec<LeakFinding>,
    /// Zitate aus Inhalten, die er **nicht** kennen kann (Systemleck-Alarm).
    pub system_leaks: Vec<LeakFinding>,
}

/// Scannt einen öffentlichen Spielertext.
#[must_use]
pub fn scan_player_public(
    text: &str,
    journal: &Journal,
    player: &PlayerId,
    cfg: &VisibilityCfg,
) -> PlayerScan {
    let seat = Seat::Player(player.clone());
    let mut scan = PlayerScan::default();
    scan_with(
        text,
        journal,
        |entry, revealed| visible_to(entry, &seat, cfg, revealed),
        |visible, finding| {
            if visible {
                scan.disclosures.push(finding);
            } else {
                scan.system_leaks.push(finding);
            }
        },
    );
    scan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commitments::sha256_parts;
    use crate::state::{Audience, EntryKind, GameEntry, Journal};
    use crate::test_support::{NORD_REPLY, RAT_OPENING, scripted_game, scripted_game_with};
    use rand_chacha::ChaCha20Rng;
    use rand_chacha::rand_core::{Rng, SeedableRng};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn rng(seed: u64) -> ChaCha20Rng {
        ChaCha20Rng::from_seed(sha256_parts(&[b"visibility-test", &seed.to_be_bytes()]))
    }

    fn pick(rng: &mut ChaCha20Rng, n: usize) -> usize {
        usize::try_from(rng.next_u32()).unwrap_or(0) % n.max(1)
    }

    fn players() -> Vec<PlayerId> {
        ["rat", "gilde", "nord", "mission"]
            .into_iter()
            .map(PlayerId::new)
            .collect()
    }

    fn entry(audience: Audience, marker: &str) -> GameEntry {
        let kind = match &audience {
            Audience::Pair(a, _) => EntryKind::NegotiationPosted {
                channel: "neg-test".to_owned(),
                from: a.clone(),
                text: format!("{marker} vertrauliche Absprache über Tanker und Kräne"),
                proposal: None,
                accept: None,
                decline: false,
            },
            _ => EntryKind::FactAdded {
                text: format!("{marker} eine Tatsache der Welt"),
                sources: Vec::new(),
            },
        };
        GameEntry::new(1, audience, kind)
    }

    /// Zufällige Audience, die `p` sieht (für den Basis-Journal).
    fn visible_audience(rng: &mut ChaCha20Rng, p: &PlayerId, all: &[PlayerId]) -> Audience {
        let others: Vec<&PlayerId> = all.iter().filter(|q| *q != p).collect();
        match pick(rng, 4) {
            0 => Audience::Public,
            1 => Audience::Seat(p.clone()),
            2 => Audience::SeatAndUmpire(p.clone()),
            _ => Audience::pair(p.clone(), others[pick(rng, others.len())].clone()),
        }
    }

    /// Zufällige Audience, die `p` **nicht** sieht.
    fn foreign_audience(rng: &mut ChaCha20Rng, p: &PlayerId, all: &[PlayerId]) -> Audience {
        let others: Vec<&PlayerId> = all.iter().filter(|q| *q != p).collect();
        let q = others[pick(rng, others.len())].clone();
        match pick(rng, 5) {
            0 => Audience::UmpireOnly,
            1 => Audience::Seat(q),
            2 => Audience::SeatAndUmpire(q),
            3 => Audience::ObserverOnly,
            _ => {
                let r = others
                    .iter()
                    .find(|r| ***r != q)
                    .map_or_else(|| q.clone(), |r| (*r).clone());
                Audience::pair(q, r)
            }
        }
    }

    #[test]
    fn property_non_interference_for_players() -> TestResult {
        let all = players();
        let cfg = VisibilityCfg::default();
        for trial in 0..300u64 {
            let mut rng = rng(trial);
            let p = all[pick(&mut rng, all.len())].clone();
            let mut base = Journal::new();
            let mut mixed = Journal::new();
            let mut foreign_markers = Vec::new();
            let n = 5 + pick(&mut rng, 20);
            for i in 0..n {
                // fremde Einträge zufällig einstreuen
                for _ in 0..pick(&mut rng, 3) {
                    let marker = format!("fremd{trial}x{i}x{}", foreign_markers.len());
                    let aud = foreign_audience(&mut rng, &p, &all);
                    mixed.append(entry(aud, &marker), None);
                    foreign_markers.push(marker);
                }
                let aud = visible_audience(&mut rng, &p, &all);
                let e = entry(aud, &format!("basis{trial}x{i}"));
                base.append(e.clone(), None);
                mixed.append(e, None);
            }
            let seat = Seat::Player(p.clone());
            let a = project(&base, &seat, &cfg).to_canonical_json()?;
            let view = project(&mixed, &seat, &cfg);
            let b = view.to_canonical_json()?;
            assert_eq!(
                a, b,
                "Trial {trial}: Projektion von {p} hängt von fremden Einträgen ab"
            );
            for marker in &foreign_markers {
                assert!(
                    !b.contains(marker.as_str()),
                    "Trial {trial}: Marker {marker} sichtbar für {p}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn property_third_seat_never_sees_pair_entries() {
        let all = players();
        for mode in [
            UmpireNegotiations::None,
            UmpireNegotiations::Cited,
            UmpireNegotiations::Full,
        ] {
            let cfg = VisibilityCfg {
                umpire_negotiations: mode,
                cited_channels: ["neg-test".to_owned()].into_iter().collect(),
            };
            for trial in 0..200u64 {
                let mut rng = rng(10_000 + trial);
                let mut journal = Journal::new();
                for i in 0..20 {
                    let a = all[pick(&mut rng, 4)].clone();
                    let b = all[pick(&mut rng, 4)].clone();
                    let aud = if a == b {
                        Audience::Seat(a)
                    } else {
                        Audience::pair(a, b)
                    };
                    journal.append(entry(aud, &format!("m{i}")), None);
                }
                for p in &all {
                    let view = project(&journal, &Seat::Player(p.clone()), &cfg);
                    for e in view.entries() {
                        match &e.audience {
                            Audience::Pair(a, b) => assert!(a == p || b == p),
                            Audience::Seat(q) => assert_eq!(q, p),
                            other => panic!("unerwartete Audience {other}"),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn property_umpire_none_invariant_to_pair_entries() -> TestResult {
        let all = players();
        let cfg = VisibilityCfg::default();
        for trial in 0..200u64 {
            let mut rng = rng(20_000 + trial);
            let mut base = Journal::new();
            let mut mixed = Journal::new();
            for i in 0..15 {
                if pick(&mut rng, 2) == 0 {
                    let a = all[pick(&mut rng, 4)].clone();
                    let others: Vec<&PlayerId> = all.iter().filter(|q| **q != a).collect();
                    let b = others[pick(&mut rng, others.len())].clone();
                    mixed.append(entry(Audience::pair(a, b), &format!("pair{i}")), None);
                }
                let aud = match pick(&mut rng, 3) {
                    0 => Audience::Public,
                    1 => Audience::UmpireOnly,
                    _ => Audience::SeatAndUmpire(all[pick(&mut rng, 4)].clone()),
                };
                let e = entry(aud, &format!("base{i}"));
                base.append(e.clone(), None);
                mixed.append(e, None);
            }
            assert_eq!(
                project(&base, &Seat::Umpire, &cfg).to_canonical_json()?,
                project(&mixed, &Seat::Umpire, &cfg).to_canonical_json()?
            );
        }
        Ok(())
    }

    #[test]
    fn property_monotony_every_projected_entry_is_visible() {
        let all = players();
        let cfg = VisibilityCfg::default();
        let mut rng = rng(30_000);
        let mut journal = Journal::new();
        for i in 0..200 {
            let aud = match pick(&mut rng, 6) {
                0 => Audience::Public,
                1 => Audience::UmpireOnly,
                2 => Audience::ObserverOnly,
                3 => Audience::Seat(all[pick(&mut rng, 4)].clone()),
                4 => Audience::SeatAndUmpire(all[pick(&mut rng, 4)].clone()),
                _ => Audience::pair(all[0].clone(), all[1 + pick(&mut rng, 3)].clone()),
            };
            journal.append(entry(aud, &format!("e{i}")), None);
        }
        let mut seats: Vec<Seat> = all.iter().cloned().map(Seat::Player).collect();
        seats.push(Seat::Umpire);
        for seat in &seats {
            let view = project(&journal, seat, &cfg);
            let expected = journal
                .entries()
                .filter(|e| e.audience.raw_includes(seat))
                .count();
            assert_eq!(view.entries().len(), expected, "Sitz {seat}");
            for e in view.entries() {
                assert!(e.audience.raw_includes(seat));
            }
        }
        assert_eq!(project_observer(&journal).entries().len(), 200);
    }

    #[test]
    fn projection_identical_whether_or_not_others_negotiated() -> TestResult {
        let (_, with) =
            scripted_game_with(&[11u8; 32], &["rat", "gilde", "nord", "mission"], true)?;
        let (_, without) =
            scripted_game_with(&[11u8; 32], &["rat", "gilde", "nord", "mission"], false)?;
        assert_ne!(with.journal, without.journal);
        let cfg = VisibilityCfg::default();
        for seat in [Seat::player("gilde"), Seat::player("mission"), Seat::Umpire] {
            assert_eq!(
                project(&with.journal, &seat, &cfg).to_canonical_json()?,
                project(&without.journal, &seat, &cfg).to_canonical_json()?,
                "Sitz {seat} erkennt fremde Verhandlung"
            );
        }
        // Die Beteiligten sehen ihren Kanal.
        let rat = project(&with.journal, &Seat::player("rat"), &cfg);
        assert_eq!(rat.own_channels().len(), 1);
        assert!(rat.contains_text(NORD_REPLY)?);
        Ok(())
    }

    #[test]
    fn umpire_cited_and_full_modes() -> TestResult {
        let (_, log) = scripted_game(&[12u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let channel = log
            .state
            .channels
            .keys()
            .next()
            .cloned()
            .ok_or("kein Kanal")?;
        let none = project(&log.journal, &Seat::Umpire, &VisibilityCfg::default());
        assert!(!none.contains_text(RAT_OPENING)?);
        let cited = VisibilityCfg {
            umpire_negotiations: UmpireNegotiations::Cited,
            cited_channels: BTreeSet::new(),
        };
        assert!(!project(&log.journal, &Seat::Umpire, &cited).contains_text(RAT_OPENING)?);
        let cited = cited.with_cited([channel].into_iter().collect());
        assert!(project(&log.journal, &Seat::Umpire, &cited).contains_text(RAT_OPENING)?);
        let full = VisibilityCfg {
            umpire_negotiations: UmpireNegotiations::Full,
            cited_channels: BTreeSet::new(),
        };
        assert!(project(&log.journal, &Seat::Umpire, &full).contains_text(NORD_REPLY)?);
        Ok(())
    }

    #[test]
    fn cited_channels_require_membership() -> TestResult {
        let (_, log) = scripted_game(&[13u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let channel = log
            .state
            .channels
            .keys()
            .next()
            .cloned()
            .ok_or("kein Kanal")?;
        let mk = |seat: &str| RoundArgument {
            id: "r2-a1".to_owned(),
            round: 2,
            seat: PlayerId::new(seat),
            body: crate::phases::ArgumentBody {
                action: "x".to_owned(),
                pros: vec!["y".to_owned()],
                cites_negotiation: vec![channel.clone()],
                conflict_target: None,
                project: None,
                use_fail_chit_if_failed: false,
            },
            secret_id: None,
            counters: BTreeMap::new(),
        };
        assert_eq!(cited_channels(&log.state, &[mk("rat")]).len(), 1);
        assert!(cited_channels(&log.state, &[mk("gilde")]).is_empty());
        Ok(())
    }

    #[test]
    fn secrets_hidden_until_revealed_then_public() -> TestResult {
        let (_, log) = scripted_game(&[14u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let cfg = VisibilityCfg::default();
        let secret_action = "Die Gilde schleust nachts zusätzliche Tanker am Zoll vorbei.";
        // Vor Spielende (Fork nach Runde 1) kennt nur gilde + Umpire den Inhalt.
        let before = log.journal.fork_at_round_end(1)?;
        assert!(!project(&before, &Seat::player("rat"), &cfg).contains_text(secret_action)?);
        assert!(!project(&before, &Seat::player("mission"), &cfg).contains_text(secret_action)?);
        assert!(project(&before, &Seat::player("gilde"), &cfg).contains_text(secret_action)?);
        assert!(project(&before, &Seat::Umpire, &cfg).contains_text(secret_action)?);
        // Die Ankündigung ist öffentlich.
        assert!(
            project(&before, &Seat::player("rat"), &cfg)
                .contains_text("geheimes Argument vor (#s1)")?
        );
        // Verdeckte Variable des Eigentümers ist für andere unsichtbar.
        let rat_view = project(&before, &Seat::player("rat"), &cfg);
        assert!(!rat_view.world().contains_key("smuggling_net"));
        assert!(
            project(&before, &Seat::player("gilde"), &cfg)
                .world()
                .contains_key("smuggling_net")
        );
        assert!(
            project(&before, &Seat::Umpire, &cfg)
                .world()
                .contains_key("plant_sabotage_risk")
        );
        assert!(!rat_view.world().contains_key("plant_sabotage_risk"));
        // Nach Spielende ist das geheime Argument für alle sichtbar.
        assert!(project(&log.journal, &Seat::player("rat"), &cfg).contains_text(secret_action)?);
        // Private Notizen bleiben privat, auch für den Umpire.
        assert!(!project(&log.journal, &Seat::Umpire, &cfg).contains_text("Nur für uns")?);
        assert!(project(&log.journal, &Seat::player("gilde"), &cfg).contains_text("Nur für uns")?);
        Ok(())
    }

    #[test]
    fn projection_has_no_sequence_numbers_or_timestamps() -> TestResult {
        let (_, log) = scripted_game(&[15u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let json = project(
            &log.journal,
            &Seat::player("mission"),
            &VisibilityCfg::default(),
        )
        .to_canonical_json()?;
        assert!(!json.contains("\"seq\""));
        assert!(!json.contains("\"at\""));
        assert!(!json.contains("master_seed"));
        assert!(!json.contains("state_hash"));
        Ok(())
    }

    #[test]
    fn leak_guard_flags_pair_content_in_umpire_text() -> TestResult {
        let (_, log) = scripted_game(&[16u8; 32], &["rat", "gilde", "nord", "mission"])?;
        // wörtliches 5-Gramm aus dem Paar-Kanal
        let leaky = "Der Umpire hört: wir brauchen Tankschiffe aber ohne Fahnen, heißt es.";
        let findings = scan_public_text(leaky, &log.journal);
        assert!(
            findings
                .iter()
                .any(|f| matches!(&f.kind, LeakKind::Shingle(_))),
            "{findings:?}"
        );
        assert_eq!(
            guard_public_text(leaky, &log.journal, LeakGuardMode::Strict, 0),
            GuardDecision::Reprompt(findings.clone())
        );
        assert_eq!(
            guard_public_text(leaky, &log.journal, LeakGuardMode::Strict, 1),
            GuardDecision::Withhold(findings.clone())
        );
        assert_eq!(
            guard_public_text(leaky, &log.journal, LeakGuardMode::FlagOnly, 0),
            GuardDecision::Flag(findings.clone())
        );
        let suspect = leak_suspect_entry(1, "umpire:r1-a1:public_rationale", leaky, &findings);
        assert_eq!(suspect.audience, Audience::ObserverOnly);
        // Paraphrase ohne gemeinsames 5-Gramm ist sauber.
        let clean = "Die Lage im Hafen bleibt angespannt, die Anlage läuft weiter.";
        assert_eq!(
            guard_public_text(clean, &log.journal, LeakGuardMode::Strict, 0),
            GuardDecision::Clean
        );
        // Nennung einer verdeckten Variablen.
        let named = scan_public_text("Das Schmuggelnetz der Gilde wächst.", &log.journal);
        assert!(
            named
                .iter()
                .any(|f| f.kind == LeakKind::HiddenVariable("smuggling_net".to_owned()))
        );
        // Öffentliche Inhalte sind kein Leck.
        let public = scan_public_text(
            "Der Inselrat stellt die Entsalzungsanlage unter Polizeischutz.",
            &log.journal,
        );
        assert!(public.is_empty(), "{public:?}");
        Ok(())
    }

    #[test]
    fn player_scanner_distinguishes_disclosure_from_system_leak() -> TestResult {
        let (_, log) = scripted_game(&[17u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let cfg = VisibilityCfg::default();
        let quote = format!("Öffentlich: {NORD_REPLY}");
        let rat = scan_player_public(&quote, &log.journal, &PlayerId::new("rat"), &cfg);
        assert!(!rat.disclosures.is_empty());
        assert!(rat.system_leaks.is_empty());
        let mission = scan_player_public(&quote, &log.journal, &PlayerId::new("mission"), &cfg);
        assert!(!mission.system_leaks.is_empty());
        Ok(())
    }

    #[test]
    fn shingles_normalize() {
        let s = shingles("Wir brauchen, Tankschiffe! aber OHNE Fahnen");
        assert!(s.contains("wir brauchen tankschiffe aber ohne"));
        assert!(s.contains("brauchen tankschiffe aber ohne fahnen"));
        assert_eq!(s.len(), 2);
        assert!(shingles("zu kurz hier").is_empty());
    }

    #[test]
    fn entries_since_returns_delta() -> TestResult {
        let (_, log) = scripted_game(&[18u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let view = project(
            &log.journal,
            &Seat::player("nord"),
            &VisibilityCfg::default(),
        );
        let total = view.entries().len();
        assert_eq!(view.entries_since(0).len(), total);
        assert_eq!(view.entries_since(u32::try_from(total)?).len(), 0);
        assert_eq!(view.entries_since(3).first().map(|e| e.n), Some(4));
        Ok(())
    }
}
