//! Reiner, rein anzeigebezogener Live-Aktivitäts-Ranker (PL-94 W01 M2b).
//!
//! Er ordnet laufende Agenten-Runs für die Inline-Chat-Ansicht. Der Ranker hat
//! keinen Einfluss auf Ausführung, Scheduling oder Persistenz: er bekommt eine
//! Momentaufnahme von Kandidaten plus eine Uhrzeit (`now_ms`) und liefert eine
//! Reihenfolge von `run_id`s. Es gibt keine Uhr, keine I/O und keine
//! Abhängigkeit auf Monitor- oder Stream-Module; alles ist deterministisch.
#![allow(
    dead_code,
    reason = "PL-94 W01 M2b: the inline-chat renderer that consumes the ranker is not mounted yet"
)]

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// Skala des Recency-Scores: ein frisches Ereignis (Alter 0) bekommt `2^32`.
const RECENCY_SCALE: u64 = 1 << 32;

/// Anzeige-Stufe eines Runs, absteigend nach Priorität.
///
/// Die Deklarationsreihenfolge ist die Prioritätsreihenfolge: der kleinste
/// Wert (`NeedsOperator`) steht in der Anzeige ganz oben.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ActivityTier {
    /// Der Run wartet auf eine Entscheidung oder Eingabe des Operators.
    NeedsOperator,
    /// Der Run hat einen Meilenstein erreicht.
    Milestone,
    /// Der Run arbeitet und hat kürzlich bedeutsame Ereignisse erzeugt.
    Active,
    /// Der Run läuft, hat aber kein kürzliches bedeutsames Ereignis. Das
    /// bedeutet UNBEKANNT, nicht „fertig“.
    Quiet,
    /// Der Run ist beendet; er wird nur noch kurz nachlaufend angezeigt.
    Terminal,
}

/// Eingabe des Rankers: Momentaufnahme eines einzelnen Runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveCandidate {
    /// Stabile Kennung des Runs.
    pub(crate) run_id: String,
    /// Ob der Run in der aktuellen Sitzung sichtbar ist. Unsichtbare
    /// Kandidaten erscheinen nie in der Ausgabe.
    pub(crate) session_visible: bool,
    /// Anzeige-Stufe des Runs.
    pub(crate) tier: ActivityTier,
    /// Zeitpunkt (ms) des letzten bedeutsamen Ereignisses.
    pub(crate) last_meaningful_ms: u64,
    /// Anzahl Reasoning-Deltas seit der letzten Auswertung. Wird bewusst
    /// ignoriert: geschwätzige Runs dürfen nicht nach oben rutschen.
    pub(crate) reasoning_deltas_since_last_eval: u32,
    /// Zeitpunkt (ms), seit dem der Run terminal ist; `None` solange er läuft.
    /// Fehlt der Wert bei einem terminalen Run, gilt `last_meaningful_ms`.
    pub(crate) terminal_since_ms: Option<u64>,
}

/// Parameter des Rankers.
///
/// Alle Werte sind vorgeschlagene UX-Parameter (proposed), keine
/// abgesicherten Messwerte; sie werden nach Nutzerfeedback nachjustiert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RankerConfig {
    /// Mindestabstand zwischen zwei Auswertungen; dazwischen bleibt die
    /// Reihenfolge stehen (Vorschlag: 750 ms).
    pub(crate) coalesce_ms: u64,
    /// Haltezeit nach einem Wechsel an Position 0 (Vorschlag: 3000 ms).
    pub(crate) lead_dwell_ms: u64,
    /// Halbwertszeit der Recency-Gewichtung (Vorschlag: 10 000 ms).
    pub(crate) recency_halflife_ms: u64,
    /// Nachlaufzeit terminaler Runs bis zum Ausblenden (Vorschlag: 5000 ms).
    pub(crate) terminal_display_ms: u64,
}

impl Default for RankerConfig {
    fn default() -> Self {
        Self {
            coalesce_ms: 750,
            lead_dwell_ms: 3000,
            recency_halflife_ms: 10_000,
            terminal_display_ms: 5000,
        }
    }
}

/// Zustandsbehafteter Ranker mit Anti-Flackern (Coalescing und Lead-Dwell).
///
/// Sortierschlüssel einer vollen Auswertung, in dieser Reihenfolge:
/// 1. angepinnter, sichtbarer, nicht terminaler Run zuerst,
/// 2. [`ActivityTier`],
/// 3. Recency-Score (ganzzahliger Halbwertszerfall, siehe
///    [`recency_score`]; höher = neuer),
/// 4. vorheriger Rang,
/// 5. `run_id` aufsteigend.
#[derive(Clone, Debug)]
pub(crate) struct LiveActivityRanker {
    config: RankerConfig,
    pinned: Option<String>,
    prev_order: Vec<String>,
    last_eval_ms: Option<u64>,
    lead: Option<String>,
    lead_since_ms: Option<u64>,
}

impl LiveActivityRanker {
    /// Erzeugt einen Ranker ohne Historie.
    pub(crate) fn new(config: RankerConfig) -> Self {
        Self {
            config,
            pinned: None,
            prev_order: Vec::new(),
            last_eval_ms: None,
            lead: None,
            lead_since_ms: None,
        }
    }

    /// Pinnt einen Run (`Some`) oder hebt das Pinnen auf (`None`).
    ///
    /// Eine Änderung des Pins verwirft das Coalescing-Fenster, damit die
    /// nächste Auswertung sofort wirkt.
    pub(crate) fn pin(&mut self, run_id: Option<String>) {
        if self.pinned != run_id {
            self.pinned = run_id;
            self.last_eval_ms = None;
        }
    }

    /// Liefert die Anzeigereihenfolge der `run_id`s zum Zeitpunkt `now_ms`.
    ///
    /// Unsichtbare Kandidaten und abgelaufene terminale Runs werden vor dem
    /// Bewerten entfernt. Liegt die letzte Auswertung weniger als
    /// `coalesce_ms` zurück, bleibt die alte Reihenfolge erhalten
    /// (verschwundene Runs entfallen, neue sichtbare werden angehängt).
    pub(crate) fn rank(&mut self, now_ms: u64, candidates: &[LiveCandidate]) -> Vec<String> {
        let visible = self.eligible(now_ms, candidates);

        let coalescing = self
            .last_eval_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < self.config.coalesce_ms);
        if coalescing {
            return self.coalesced(now_ms, visible);
        }

        let mut ordered = self.sorted(now_ms, visible);
        self.apply_lead_dwell(now_ms, &mut ordered);

        let new_lead = ordered.first().map(|c| c.run_id.clone());
        if new_lead != self.lead {
            self.lead = new_lead;
            self.lead_since_ms = Some(now_ms);
        }

        let ids: Vec<String> = ordered.iter().map(|c| c.run_id.clone()).collect();
        self.prev_order.clone_from(&ids);
        self.last_eval_ms = Some(now_ms);
        ids
    }

    /// Sichtbare, nicht abgelaufene und eindeutige Kandidaten (Regeln 1 und 2).
    fn eligible<'a>(&self, now_ms: u64, candidates: &'a [LiveCandidate]) -> Vec<&'a LiveCandidate> {
        let mut seen: HashSet<&str> = HashSet::new();
        candidates
            .iter()
            .filter(|c| c.session_visible)
            .filter(|c| !self.terminal_expired(now_ms, c))
            .filter(|c| seen.insert(c.run_id.as_str()))
            .collect()
    }

    /// Ob ein terminaler Run seine Nachlaufzeit überschritten hat.
    fn terminal_expired(&self, now_ms: u64, c: &LiveCandidate) -> bool {
        if c.tier != ActivityTier::Terminal {
            return false;
        }
        let since = c.terminal_since_ms.unwrap_or(c.last_meaningful_ms);
        now_ms.saturating_sub(since) >= self.config.terminal_display_ms
    }

    /// Ob der Kandidat der aktuell wirksame Pin ist (nicht terminal).
    fn is_pinned(&self, c: &LiveCandidate) -> bool {
        c.tier != ActivityTier::Terminal && self.pinned.as_deref() == Some(c.run_id.as_str())
    }

    /// Sortiert nach Pin, Tier, Recency, vorherigem Rang und `run_id`.
    fn sorted<'a>(&self, now_ms: u64, cands: Vec<&'a LiveCandidate>) -> Vec<&'a LiveCandidate> {
        let prev_rank: HashMap<&str, usize> = self
            .prev_order
            .iter()
            .enumerate()
            .map(|(rank, id)| (id.as_str(), rank))
            .collect();
        let mut keyed: Vec<_> = cands
            .into_iter()
            .map(|c| {
                let age = now_ms.saturating_sub(c.last_meaningful_ms);
                let score = recency_score(age, self.config.recency_halflife_ms);
                let rank = prev_rank
                    .get(c.run_id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX);
                let key = (
                    u8::from(!self.is_pinned(c)),
                    c.tier,
                    Reverse(score),
                    rank,
                    c.run_id.as_str(),
                );
                (key, c)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        keyed.into_iter().map(|(_, c)| c).collect()
    }

    /// Anti-Thrash (Regel 5): innerhalb von `lead_dwell_ms` nach einem
    /// Lead-Wechsel behält der bisherige Lead Position 0, außer der
    /// Herausforderer ist `NeedsOperator` oder angepinnt. Ein terminaler
    /// bisheriger Lead wird nicht gehalten.
    fn apply_lead_dwell(&self, now_ms: u64, ordered: &mut Vec<&LiveCandidate>) {
        let mut hold_pos: Option<usize> = None;
        if let (Some(lead), Some(since), Some(first)) = (
            self.lead.as_deref(),
            self.lead_since_ms,
            ordered.first().copied(),
        ) {
            let in_dwell = now_ms.saturating_sub(since) < self.config.lead_dwell_ms;
            let challenger_wins = first.tier == ActivityTier::NeedsOperator
                || self.is_pinned(first)
                || first.run_id == lead;
            if in_dwell && !challenger_wins {
                hold_pos = ordered
                    .iter()
                    .position(|c| c.run_id == lead && c.tier != ActivityTier::Terminal);
            }
        }
        if let Some(pos) = hold_pos {
            let held = ordered.remove(pos);
            ordered.insert(0, held);
        }
    }

    /// Coalescing (Regel 6): alte Reihenfolge, Verschwundene entfernt, neue
    /// sichtbare Runs (in Schlüsselreihenfolge) angehängt.
    fn coalesced(&mut self, now_ms: u64, visible: Vec<&LiveCandidate>) -> Vec<String> {
        let present: HashSet<&str> = visible.iter().map(|c| c.run_id.as_str()).collect();
        let mut out: Vec<String> = self
            .prev_order
            .iter()
            .filter(|id| present.contains(id.as_str()))
            .cloned()
            .collect();
        let fresh: Vec<&LiveCandidate> = visible
            .iter()
            .copied()
            .filter(|c| !self.prev_order.contains(&c.run_id))
            .collect();
        out.extend(
            self.sorted(now_ms, fresh)
                .into_iter()
                .map(|c| c.run_id.clone()),
        );
        self.prev_order.clone_from(&out);
        out
    }
}

/// Ganzzahliger Halbwertszerfall: Alter 0 ergibt `2^32`, jede volle
/// Halbwertszeit halbiert den Wert, dazwischen wird linear interpoliert.
/// Der Score ist monoton fallend im Alter und ab 32 Halbwertszeiten null.
/// Keine Gleitkommazahlen, daher plattformunabhängig deterministisch.
fn recency_score(age_ms: u64, halflife_ms: u64) -> u64 {
    let half = halflife_ms.max(1);
    let periods = age_ms / half;
    if periods >= 32 {
        return 0;
    }
    let rem = age_ms % half;
    let start = RECENCY_SCALE >> periods;
    let drop = u128::from(start - (start >> 1)) * u128::from(rem) / u128::from(half);
    start.saturating_sub(u64::try_from(drop).unwrap_or(start))
}

#[cfg(test)]
mod tests {
    use super::{ActivityTier, LiveActivityRanker, LiveCandidate, RankerConfig, recency_score};
    use crate::test_support::{TestError, TestResult};

    fn cand(id: &str, tier: ActivityTier, last: u64) -> LiveCandidate {
        LiveCandidate {
            run_id: id.to_owned(),
            session_visible: true,
            tier,
            last_meaningful_ms: last,
            reasoning_deltas_since_last_eval: 0,
            terminal_since_ms: None,
        }
    }

    fn terminal(id: &str, since: u64) -> LiveCandidate {
        LiveCandidate {
            terminal_since_ms: Some(since),
            ..cand(id, ActivityTier::Terminal, since)
        }
    }

    fn ranker() -> LiveActivityRanker {
        LiveActivityRanker::new(RankerConfig::default())
    }

    fn expect(
        ranker: &mut LiveActivityRanker,
        now: u64,
        cands: &[LiveCandidate],
        want: &[&str],
    ) -> TestResult {
        let got = ranker.rank(now, cands);
        let want: Vec<String> = want.iter().map(|s| (*s).to_owned()).collect();
        if got == want {
            Ok(())
        } else {
            Err(TestError::Unexpected(format!(
                "t={now}: erwartet {want:?}, erhalten {got:?}"
            )))
        }
    }

    #[test]
    fn milestone_outranks_chatty_active() -> TestResult {
        let mut chatty = cand("a-chatty", ActivityTier::Active, 1000);
        chatty.reasoning_deltas_since_last_eval = 500;
        let milestone = cand("z-milestone", ActivityTier::Milestone, 0);
        expect(
            &mut ranker(),
            1000,
            &[chatty, milestone],
            &["z-milestone", "a-chatty"],
        )
    }

    #[test]
    fn reasoning_deltas_never_matter() -> TestResult {
        let a = cand("a", ActivityTier::Active, 0);
        let mut b = cand("b", ActivityTier::Active, 0);
        b.reasoning_deltas_since_last_eval = 500;
        expect(&mut ranker(), 0, &[b, a], &["a", "b"])
    }

    #[test]
    fn dwell_keeps_previous_lead() -> TestResult {
        let mut r = ranker();
        expect(&mut r, 0, &[cand("a", ActivityTier::Active, 0)], &["a"])?;
        let both = [
            cand("a", ActivityTier::Active, 0),
            cand("b", ActivityTier::Active, 1000),
        ];
        expect(&mut r, 1000, &both, &["a", "b"])?;
        expect(&mut r, 3000, &both, &["b", "a"])
    }

    #[test]
    fn needs_operator_takes_lead_immediately() -> TestResult {
        let mut r = ranker();
        expect(&mut r, 0, &[cand("a", ActivityTier::Active, 0)], &["a"])?;
        let both = [
            cand("a", ActivityTier::Active, 0),
            cand("c", ActivityTier::NeedsOperator, 1000),
        ];
        expect(&mut r, 1000, &both, &["c", "a"])
    }

    #[test]
    fn pin_unpin_and_terminal_pin() -> TestResult {
        let mut r = ranker();
        let cands = [
            cand("a", ActivityTier::Milestone, 0),
            cand("b", ActivityTier::Active, 0),
        ];
        r.pin(Some("b".to_owned()));
        expect(&mut r, 0, &cands, &["b", "a"])?;
        r.pin(None);
        expect(&mut r, 5000, &cands, &["a", "b"])?;

        let mut r2 = ranker();
        r2.pin(Some("t".to_owned()));
        let with_terminal = [cand("a", ActivityTier::Active, 0), terminal("t", 0)];
        expect(&mut r2, 1000, &with_terminal, &["a", "t"])
    }

    #[test]
    fn invisible_never_appear() -> TestResult {
        let mut r = ranker();
        let mut hidden = cand("hidden", ActivityTier::NeedsOperator, 0);
        hidden.session_visible = false;
        r.pin(Some("hidden".to_owned()));
        let cands = [hidden, cand("a", ActivityTier::Quiet, 0)];
        expect(&mut r, 0, &cands, &["a"])?;
        expect(&mut r, 5000, &cands, &["a"])
    }

    #[test]
    fn terminal_lingers_then_drops() -> TestResult {
        let mut r = ranker();
        let cands = [cand("a", ActivityTier::Active, 0), terminal("x", 0)];
        expect(&mut r, 0, &cands, &["a", "x"])?;
        expect(&mut r, 4999, &cands, &["a", "x"])?;
        expect(&mut r, 5000, &cands, &["a"])
    }

    #[test]
    fn ties_are_deterministic() -> TestResult {
        let abc = [
            cand("a", ActivityTier::Active, 0),
            cand("b", ActivityTier::Active, 0),
            cand("c", ActivityTier::Active, 0),
        ];
        let cba = [abc[2].clone(), abc[1].clone(), abc[0].clone()];
        expect(&mut ranker(), 0, &abc, &["a", "b", "c"])?;
        expect(&mut ranker(), 0, &cba, &["a", "b", "c"])
    }

    #[test]
    fn previous_rank_beats_run_id() -> TestResult {
        let mut r = ranker();
        expect(&mut r, 0, &[cand("z", ActivityTier::Active, 0)], &["z"])?;
        let both = [
            cand("a", ActivityTier::Active, 0),
            cand("z", ActivityTier::Active, 0),
        ];
        expect(&mut r, 1000, &both, &["z", "a"])
    }

    #[test]
    fn coalescing_keeps_order_and_syncs_membership() -> TestResult {
        let mut r = ranker();
        let start = [
            cand("a", ActivityTier::Active, 0),
            cand("b", ActivityTier::Active, 0),
        ];
        expect(&mut r, 0, &start, &["a", "b"])?;
        let changed = [
            cand("a", ActivityTier::Active, 0),
            cand("b", ActivityTier::NeedsOperator, 0),
        ];
        expect(&mut r, 749, &changed, &["a", "b"])?;
        let vanished_and_new = [
            cand("b", ActivityTier::Milestone, 0),
            cand("c", ActivityTier::Active, 0),
        ];
        expect(&mut r, 500, &vanished_and_new, &["b", "c"])?;
        expect(&mut r, 750, &changed, &["b", "a"])
    }

    #[test]
    fn quiet_ranks_below_active() -> TestResult {
        let quiet = cand("a-quiet", ActivityTier::Quiet, 1000);
        let active = cand("z-active", ActivityTier::Active, 0);
        expect(
            &mut ranker(),
            1000,
            &[quiet, active],
            &["z-active", "a-quiet"],
        )
    }

    #[test]
    fn recency_score_is_monotone_and_halves() {
        assert_eq!(recency_score(0, 10_000), 1 << 32);
        assert_eq!(recency_score(10_000, 10_000), 1 << 31);
        assert_eq!(recency_score(20_000, 10_000), 1 << 30);
        assert_eq!(recency_score(u64::MAX, 10_000), 0);
        assert!(recency_score(4000, 10_000) > recency_score(4001, 10_000));
        assert!(recency_score(1, 0) <= recency_score(0, 0));
    }
}
