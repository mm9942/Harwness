//! Matrix-Game-Runner: treibt den deterministischen Kern aus
//! `harw-matrix-game` mit echten Kind-Agenten (`docs/design/matrix-game.md`
//! §4, §7).
//!
//! # Verantwortungsbereich
//! [`MatrixRun`] hält einen laufenden Spielstand: Szenario, [`GameLog`]
//! (Zustand + Journal), [`RoundCursor`], die Zuordnung Sitz → Kind-Session,
//! offene Facilitator-Eingriffe, Laufverzeichnis und Status.
//! [`MatrixRun::step`] führt **genau eine Phase** aus:
//!
//! 1. Nächste FSM-Position bestimmen, `PhaseEntered` journalisieren, fällige
//!    Szenario- und Facilitator-Injects anwenden.
//! 2. Die Aufrufe der Phase über [`expected_calls`] in kanonischer
//!    Sitzreihenfolge dispatchen. Jeder Sitz ist ein multi-turn Kind
//!    (`matrix-player`/`matrix-umpire`/`matrix-market`), das beim ersten
//!    Aufruf mit [`prompts::system_prompt`] als Auftrag gestartet und danach
//!    über alle Phasen hinweg zugelassen gehalten wird (`ChildGuard::keep`).
//!    Der Turn-Input entsteht ausschließlich aus der Projektion
//!    `project(journal, seat)` ([`prompts::turn_prompt`], Delta seit dem
//!    letzten Zug, zu Rundenbeginn ein volles Lagebild).
//! 3. Antworten werden mit [`PhaseOutput::parse`] und den `validate_*`-
//!    Funktionen geprüft; bei einem Befund gibt es genau einen Reparatur-Turn
//!    ([`prompts::repair_prompt`]), danach gilt der Sitz als `Forfeit`.
//! 4. Die deterministischen GameMaster-Schritte (Siegeln per
//!    [`ArgumentBox`], `reveal_round`, `resolve_argument`, …) schreiben das
//!    Journal. Öffentliche Umpire-Texte laufen vorher durch
//!    [`guard_public_text`]: `Reprompt` → Reparatur-Turn, `Withhold`/`Flag` →
//!    `LeakSuspect`-Eintrag für den Beobachter.
//! 5. Jede neue Journalzeile wird an `journal.jsonl` angehängt und — falls ein
//!    [`AgentEventHub`] im Kontext liegt — als Live-Event veröffentlicht.
//!
//! # Determinismus
//! Der Master-Seed kommt aus `--seed`, sonst aus dem Szenario, sonst aus dem
//! SHA-256 des Szenario-Quelltexts — derselbe Lauf mit denselben
//! Agenten-Antworten ergibt dieselben Würfel. Die Antworten selbst sind die
//! einzige nicht-deterministische Quelle; sie stehen im Journal, sodass
//! `replay` ohne Modellaufrufe denselben Zustand liefert.
//!
//! # Nicht umgesetzt (offene Punkte des Kerns)
//! Schlussargumente werden öffentlich protokolliert, aber nicht erneut
//! adjudiziert (Knock-out-Würfe fehlen im Kern). Ein Facilitator-Veto
//! verwirft das Argument; einen Neuversuch in derselben Runde gibt es nicht,
//! weil die [`ArgumentBox`] keine Nachbesserung zulässt.
//!
//! # Nebenläufigkeit
//! [`MatrixRun`] ist `Send`, aber nicht intern synchronisiert; der Aufrufer
//! (`super`) nimmt den Lauf für die Dauer eines Schritts aus der Registry.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use harw_authority::{PermissionRequest, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
use harw_core::StateStore;
use harw_core::agent_events::AgentEventHub;
use harw_core::cancel::CancelToken;
use harw_core::child_controller::{AgentBudget, ManagedAgentSpawner};
use harw_core::turn_loop::{TurnInput, TurnOutcome};
use harw_core_bridge::{OpContextCoreExt, parse_budget_hint, tighten_budget};
use harw_matrix_game::MatrixError;
use harw_matrix_game::aar::{AarInput, build_aar, validate_goal_ratings};
use harw_matrix_game::commitments::{DOMAIN, sha256_parts, to_hex};
use harw_matrix_game::events::events_for_entry;
use harw_matrix_game::library;
use harw_matrix_game::phases::{
    ArgumentBox, ContractKind, CounterArgument, EffectOp, ExpectedCall, NegotiationRequest, Phase,
    PhaseConfig, PhaseOutput, PhaseStep, PlayerDebrief, RedCellObjection, RoundArgument,
    RoundCursor, UmpireAdjudication, UmpireNarration, UmpireRuling, UmpireSynthesis, Verdict,
    apply_inject, close_negotiation, close_round_with_market, end_game, enter_phase,
    expected_calls, open_channels, open_game, post_messages, record_briefing, record_narration,
    record_standing, resolve_adjudication, reveal_round, reveal_secret, submit_counters,
    submit_red_cell, validate_counter_argument, validate_negotiation_messages,
    validate_negotiation_request, validate_player_argument, validate_red_cell_objection,
    validate_umpire_adjudication, validate_umpire_narration,
};
use harw_matrix_game::prompts::{self, SeatRole};
use harw_matrix_game::scenario::{
    AdjudicationSystem, LoadedScenario, Rules, Scenario, ScheduledInject, SeedSpec,
    materials_for_seat, pair_folder_members,
};
use harw_matrix_game::state::{
    Audience, EntryKind, FactSource, GameEntry, GameLog, Journal, PlayerId, RevealedBy, Seat,
    replay,
};
use harw_matrix_game::visibility::{
    GuardDecision, LeakFinding, VisibilityCfg, cited_channels, guard_public_text,
    leak_suspect_entry, project, project_observer, revealed_secrets, visible_to,
};
use harw_operations::{OpContext, OpError};
use harw_registry_defaults::profile::role_names;
use harw_types::{SessionId, ToolCallId, WorkspaceId};
use jiff::Timestamp;
use serde_json::{Value, json};

// ── Konstanten ───────────────────────────────────────────────────────────────

/// Dateiname des Journals im Laufverzeichnis.
pub const JOURNAL_FILE: &str = "journal.jsonl";
/// Dateiname der Szenario-Kopie im Laufverzeichnis.
pub const SCENARIO_FILE: &str = "scenario.toml";
/// Dateiname des After-Action-Reviews im Laufverzeichnis.
pub const AAR_FILE: &str = "aar.md";

/// Budget eines einzelnen Sitz-Turns (Text plus Lesezugriffe auf die
/// Unterlagen). Die Rollendefinition (`[spawn.budget]`) kann es nur weiter
/// verschärfen.
const SEAT_TURN_BUDGET: &str = "60k_tokens,16_tool_calls,300s";

/// Höchstwartezeit auf einen freien Admission-Slot beim ersten Aufruf eines Sitzes.
const SEAT_SLOT_WAIT: Duration = Duration::from_secs(120);

/// Reparaturversuche nach einer ungültigen Antwort (danach `Forfeit`).
const REPAIR_ATTEMPTS: u32 = 1;

/// Runde 9, E7: Restbudget (neue Tokens), unter dem ein gehaltener Sitz vor
/// seinem nächsten Zug durch ein frisches Kind ersetzt wird. Das Token-Budget
/// eines Kindes gilt für seine ganze Sitzung; ohne diesen Wechsel liefe ein
/// Sitz mitten im Spiel in sein Budget und passte.
const SEAT_RECYCLE_BELOW_TOKENS: u64 = 15_000;

/// Runde 9, E7: Höchstlänge einer Verzichtsursache in Zusammenfassungen.
const CAUSE_SUMMARY_MAX_CHARS: usize = 180;

/// Kommando der Journal-Notiz eines verwirkten Sitz-Aufrufs.
pub const FORFEIT_NOTE: &str = "forfeit";

/// Kommando der Journal-Notiz eines technischen Abbruchs (Runde 9, E7).
pub const TECHNICAL_STOP_NOTE: &str = "technischer-abbruch";

/// Ursache eines ungültigen Zugs in Zusammenfassungen (die Einzelheiten
/// stehen in der `forfeit`-Notiz des Journals).
const INVALID_ANSWER_CAUSE: &str = "ungültige Antwort nach Reparaturversuch";

/// Mittelteil der `forfeit`-Notiz eines technisch gescheiterten Aufrufs.
const FAILED_CALL: &str = "Aufruf fehlgeschlagen";

/// Mittelteil der `forfeit`-Notiz einer auch nach Reparatur ungültigen Antwort.
const INVALID_ANSWER: &str = "Antwort auch nach Reparaturversuch ungültig";

/// Hartes Zeitlimit eines Sitz-Aufrufs (Runde 7, Teil M): Admission-Warten
/// ([`SEAT_SLOT_WAIT`]) plus Turn-Budget (300 s) plus Reserve. Danach gilt
/// der Sitz für diesen Aufruf als gepasst, sein Kind wird abgebrochen und
/// freigegeben — ein hängender Sitz (z. B. eine nie beantwortete Freigabe)
/// blockiert den Lauf nicht mehr.
pub const SEAT_TURN_TIMEOUT: Duration = Duration::from_secs(480);

/// Dateiname des paper-tauglichen Berichts im Laufverzeichnis (Runde 7, Teil M4).
pub const REPORT_FILE: &str = "report.md";

/// Sitz-Schlüssel des Umpires in `views`/`seats`.
pub const UMPIRE_KEY: &str = "umpire";
/// Sitz-Schlüssel der Red Cell (reservierte Sitz-Id im Szenario).
pub const RED_CELL_KEY: &str = harw_matrix_game::scenario::RED_CELL_KEY;

/// Unterordner des Laufverzeichnisses mit den Unterlagen-Kopien je Sitz.
pub const MATERIALS_DIR: &str = "materials";
/// Höchstgröße einer kopierten Unterlagen-Datei.
pub const MAX_MATERIAL_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Höchstgröße aller Unterlagen einer Sitz-Kopie.
pub const MAX_MATERIAL_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

/// Plan R9: Höchstlänge eines recherchierten Fakts (Zeichen).
pub const MAX_RESEARCH_FACT_CHARS: usize = 1_200;

/// Plan R9: höchstens so viele Belege je recherchiertem Fakt.
pub const MAX_RESEARCH_FACT_SOURCES: usize = 5;
/// Höchste Verzeichnistiefe beim Kopieren.
const MAX_MATERIAL_DEPTH: usize = 16;

// ── Status und Berichte ──────────────────────────────────────────────────────

/// Lebenszyklus eines Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Bereit für den nächsten Schritt.
    Running,
    /// Vom Facilitator angehalten (`/matrix pause`); `step` setzt fort.
    Paused,
    /// AAR geschrieben, Kinder freigegeben.
    Ended,
}

impl RunStatus {
    /// Textform für `show`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Ended => "ended",
        }
    }
}

/// Ergebnis eines [`MatrixRun::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    /// Runde nach dem Schritt.
    pub round: u32,
    /// Ausgeführte Phase.
    pub phase: Phase,
    /// Anzahl der Sitz-Aufrufe (ohne Reparatur-Turns).
    pub calls: usize,
    /// Sitze, die in dieser Phase gepasst haben (ungültig oder Fehler).
    pub forfeits: Vec<String>,
    /// Runde 9, E7: je gepasstem Aufruf Sitz, Ursache und Art (gleiche
    /// Reihenfolge wie [`Self::forfeits`]).
    pub forfeit_causes: Vec<SeatForfeit>,
    /// Anzahl der Leak-Befunde (`LeakSuspect`-Einträge).
    pub leaks: usize,
    /// Spiel nach diesem Schritt beendet.
    pub ended: bool,
    /// Runde 9, E7: jeder Sitz-Aufruf dieser Phase scheiterte technisch; der
    /// Lauf ist pausiert. Inhalt: die (entdoppelten) Ursachen.
    pub technical_stop: Option<String>,
}

/// Art eines verwirkten Sitz-Aufrufs (Runde 9, E7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForfeitKind {
    /// Technischer Fehler: Spawn/Admission, Lauf, Zeitlimit, fehlende Antwort.
    Technical,
    /// Antwort auch nach dem Reparaturversuch ungültig (ein Spielzug).
    InvalidAnswer,
    /// Der Lauf wurde abgebrochen.
    Cancelled,
}

/// Ein verwirkter Sitz-Aufruf mit Ursache (Runde 9, E7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatForfeit {
    /// Sitz-Schlüssel.
    pub seat: String,
    /// Kurze Ursache für Zusammenfassungen (volle Fassung im Journal).
    pub cause: String,
    /// Art des Verzichts.
    pub kind: ForfeitKind,
}

impl StepReport {
    fn new(cursor: RoundCursor) -> Self {
        Self {
            round: cursor.round,
            phase: cursor.phase,
            calls: 0,
            forfeits: Vec::new(),
            forfeit_causes: Vec::new(),
            leaks: 0,
            ended: false,
            technical_stop: None,
        }
    }

    /// Einzeilige deutsche Zusammenfassung; gepasste Sitze erscheinen je
    /// Ursache gruppiert, z. B. `gepasst: rat, gilde (Spawn fehlgeschlagen: …)`.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut line = format!(
            "Runde {} · Phase {} · {} Aufruf(e)",
            self.round,
            self.phase.label(),
            self.calls
        );
        if !self.forfeit_causes.is_empty() {
            line.push_str(&format!(
                " · gepasst: {}",
                group_forfeits(&self.forfeit_causes)
            ));
        } else if !self.forfeits.is_empty() {
            line.push_str(&format!(" · gepasst: {}", self.forfeits.join(", ")));
        }
        if self.leaks > 0 {
            line.push_str(&format!(" · Leak-Verdacht: {}", self.leaks));
        }
        if self.ended {
            line.push_str(" · Spiel beendet");
        }
        if self.technical_stop.is_some() {
            line.push_str(" · technischer Abbruch, Lauf pausiert");
        }
        line
    }

    fn push_forfeit(&mut self, seat: String, cause: String, kind: ForfeitKind) {
        self.forfeits.push(seat.clone());
        self.forfeit_causes.push(SeatForfeit {
            seat,
            cause: shorten_cause(&cause),
            kind,
        });
    }

    /// `Some(ursachen)`, wenn die Phase Sitze aufrief und **jeder** Aufruf
    /// technisch scheiterte (nicht: ungültige Antwort, Abbruch).
    fn all_calls_failed_technically(&self) -> Option<String> {
        let all_technical = self.calls > 0
            && self.forfeit_causes.len() >= self.calls
            && self
                .forfeit_causes
                .iter()
                .all(|forfeit| forfeit.kind == ForfeitKind::Technical);
        all_technical.then(|| distinct_causes(&self.forfeit_causes).join("; "))
    }
}

/// Kürzt eine Ursache für Zusammenfassungen (eine Zeile, begrenzte Länge).
fn shorten_cause(cause: &str) -> String {
    let line = cause.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= CAUSE_SUMMARY_MAX_CHARS {
        return line;
    }
    let mut short: String = line.chars().take(CAUSE_SUMMARY_MAX_CHARS).collect();
    short.push('…');
    short
}

/// Entdoppelte Ursachen in der Reihenfolge ihres ersten Auftretens.
fn distinct_causes(forfeits: &[SeatForfeit]) -> Vec<String> {
    let mut causes: Vec<String> = Vec::new();
    for forfeit in forfeits {
        if !causes.contains(&forfeit.cause) {
            causes.push(forfeit.cause.clone());
        }
    }
    causes
}

/// `sitz, sitz (ursache); sitz (ursache)` — je Ursache einmal, Sitze
/// entdoppelt, Reihenfolge des ersten Auftretens.
#[must_use]
pub fn group_forfeits(forfeits: &[SeatForfeit]) -> String {
    distinct_causes(forfeits)
        .into_iter()
        .map(|cause| {
            let mut seats: Vec<&str> = Vec::new();
            for forfeit in forfeits.iter().filter(|f| f.cause == cause) {
                if !seats.contains(&forfeit.seat.as_str()) {
                    seats.push(&forfeit.seat);
                }
            }
            format!("{} ({cause})", seats.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Verzichte eines Laufs mit gleicher Ursache (Runde 9, E7), aus den
/// `forfeit`-Notizen des Journals gelesen — damit auch ein per Replay
/// geladener Lauf seine Ursachen kennt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForfeitGroup {
    /// Kurze Ursache (technischer Fehler) bzw. „ungültige Antwort …“.
    pub cause: String,
    /// Betroffene Sitze, entdoppelt, in Reihenfolge des ersten Auftretens.
    pub seats: Vec<String>,
    /// Anzahl der verwirkten Aufrufe mit dieser Ursache.
    pub count: usize,
    /// `true` für technische Fehler (nicht: ungültige Antwort).
    pub technical: bool,
}

/// Liest eine `forfeit`-Notiz: `(sitz, ursache, technisch)`.
fn parse_forfeit_note(detail: &str) -> Option<(String, String, bool)> {
    let rest = detail.strip_prefix("Sitz `")?;
    let (seat, rest) = rest.split_once('`')?;
    let (head, cause) = rest.split_once(" — ")?;
    if head.ends_with(INVALID_ANSWER) {
        return Some((seat.to_owned(), INVALID_ANSWER_CAUSE.to_owned(), false));
    }
    let technical = cause.trim() != "Lauf abgebrochen";
    Some((seat.to_owned(), shorten_cause(cause), technical))
}

/// Alle Verzichte eines Journals, je Ursache gruppiert (Reihenfolge des
/// ersten Auftretens).
#[must_use]
pub fn forfeit_groups(journal: &Journal) -> Vec<ForfeitGroup> {
    let mut groups: Vec<ForfeitGroup> = Vec::new();
    for entry in journal.entries() {
        let EntryKind::FacilitatorNote { command, detail } = &entry.kind else {
            continue;
        };
        if command != FORFEIT_NOTE {
            continue;
        }
        let Some((seat, cause, technical)) = parse_forfeit_note(detail) else {
            continue;
        };
        match groups.iter_mut().find(|group| group.cause == cause) {
            Some(group) => {
                group.count += 1;
                if !group.seats.contains(&seat) {
                    group.seats.push(seat);
                }
            }
            None => groups.push(ForfeitGroup {
                cause,
                seats: vec![seat],
                count: 1,
                technical,
            }),
        }
    }
    groups
}

/// Letzter technischer Abbruch eines Journals (Detail der Notiz), falls es
/// einen gab.
#[must_use]
pub fn last_technical_stop(journal: &Journal) -> Option<String> {
    journal
        .entries()
        .filter_map(|entry| match &entry.kind {
            EntryKind::FacilitatorNote { command, detail } if command == TECHNICAL_STOP_NOTE => {
                Some(detail.clone())
            }
            _ => None,
        })
        .last()
}

/// Hängt dem AAR einen Abschnitt mit den Verzichtsursachen an (und, falls
/// der Lauf technisch abbrach, den Hinweis, dass das kein Spielergebnis ist).
fn with_forfeit_causes(mut markdown: String, journal: &Journal) -> String {
    let causes = forfeit_causes_markdown(journal);
    if causes.is_empty() {
        return markdown;
    }
    if !markdown.ends_with('\n') {
        markdown.push('\n');
    }
    markdown.push_str("\n## Verzichte und ihre Ursachen\n\n");
    if let Some(stop) = last_technical_stop(journal) {
        markdown.push_str(&format!(
            "**Technischer Abbruch im Lauf — die betroffenen Phasen sind kein Spielergebnis:** {stop}\n\n"
        ));
    }
    markdown.push_str(&causes);
    markdown
}

/// Markdown-Liste der Verzichtsursachen (leer ohne Verzichte).
#[must_use]
pub fn forfeit_causes_markdown(journal: &Journal) -> String {
    let mut out = String::new();
    for group in forfeit_groups(journal) {
        let kind = if group.technical {
            ", technischer Fehler"
        } else {
            ""
        };
        out.push_str(&format!(
            "- {} — {} ({}×{kind})\n",
            group.cause,
            group.seats.join(", "),
            group.count
        ));
    }
    out
}

// ── Sitz-Treiber ─────────────────────────────────────────────────────────────

/// Eine Anfrage an einen Sitz.
#[derive(Debug, Clone)]
pub struct SeatRequest<'a> {
    /// Sitz-Schlüssel (`rat`, `umpire`, …).
    pub seat_key: &'a str,
    /// Agentenrolle (`matrix-player` …).
    pub role: &'static str,
    /// System-Prompt (nur beim ersten Aufruf des Sitzes wirksam).
    pub system: &'a str,
    /// Turn-Input.
    pub prompt: String,
}

/// Zukunft einer Sitz-Antwort.
pub type SeatFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// Liefert Rohantworten der Sitze. Produktiv: [`SpawnerDriver`]; in Tests
/// ein geskripteter Treiber.
pub trait SeatDriver: Send {
    /// Fragt einen Sitz; `Err` ist eine deutsche Fehlerbeschreibung (der
    /// Sitz passt dann).
    fn ask<'a>(&'a mut self, request: SeatRequest<'a>) -> SeatFuture<'a>;

    /// Gibt alle gehaltenen Sitze frei (Spielende).
    fn release_all(&mut self) {}

    /// Bricht den laufenden Aufruf eines Sitzes ab und vergisst ihn
    /// (Zeitlimit oder Abbruch des Laufs, Runde 7 Teil M); der nächste
    /// Aufruf startet ein frisches Kind.
    fn abandon(&mut self, _seat_key: &str) {}

    /// Hinweise seit dem letzten Aufruf (z. B. Unterlagen nicht einsehbar);
    /// der Runner journalisiert sie für den Beobachter.
    fn take_warnings(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// Runde 9, E7: `true`, wenn der nächste Aufruf dieses Sitzes an ein
    /// Kind geht, das seine früheren Züge noch kennt. Bei `false` schickt der
    /// Runner ein volles Lagebild statt nur des Deltas. Der Treiber darf hier
    /// gehaltene Kinder erneuern (Lease) oder gezielt ersetzen (Restbudget).
    fn keeps_context(&mut self, _seat_key: &str) -> bool {
        true
    }
}

/// Treiber ohne Agenten: jeder Aufruf scheitert (Sitz passt). Dient `end`
/// ohne Spawner, damit das AAR trotzdem entsteht.
#[derive(Debug, Default)]
pub struct NullDriver;

impl SeatDriver for NullDriver {
    fn ask<'a>(&'a mut self, request: SeatRequest<'a>) -> SeatFuture<'a> {
        let seat = request.seat_key.to_owned();
        Box::pin(async move { Err(format!("kein Agent-Spawner für Sitz `{seat}` verfügbar")) })
    }
}

/// Produktiver Treiber: ein multi-turn Kind je Sitz über den
/// [`ManagedAgentSpawner`].
pub struct SpawnerDriver {
    spawner: Arc<ManagedAgentSpawner>,
    store: Arc<dyn StateStore>,
    /// Sandbox der aufrufenden Session (Obergrenze jedes Sitzes).
    parent_sandbox: SandboxSpec,
    /// Sandbox ohne jede Berechtigung für Läufe ohne Unterlagen-Kopien.
    sandbox: SandboxSpec,
    parent: SessionId,
    cancel: CancelToken,
    budget: AgentBudget,
    children: BTreeMap<String, SessionId>,
    /// Runde 9, E7: letzter Aufruf je gehaltenem Sitz (für die Verdrängung,
    /// wenn der Fan-out-Deckel weniger gleichzeitige Kinder erlaubt, als das
    /// Szenario Sitze hat).
    last_used: BTreeMap<String, u64>,
    tick: u64,
    /// `<run_dir>/materials`, falls Unterlagen-Kopien existieren.
    materials_root: Option<PathBuf>,
    /// Präfix der Workspace-IDs der Sitz-Sandboxen.
    run_tag: String,
    warnings: Vec<String>,
}

impl SpawnerDriver {
    /// Baut den Treiber aus dem Op-Kontext. Jeder Sitz bekommt (mit
    /// [`Self::with_materials`]) seine Unterlagen-Kopie als Workspace mit
    /// `{ReadWorkspace}` ∩ Parent-Rechten und ohne Netz; ohne Kopie die
    /// Parent-Sandbox ohne jede Berechtigung.
    ///
    /// # Errors
    /// [`OpError::NotAvailable`] ohne Spawner oder StateStore.
    pub fn from_ctx(ctx: &OpContext) -> Result<Self, OpError> {
        let spawner = ctx.managed_spawner().ok_or_else(|| {
            OpError::NotAvailable(
                "kein Agent-Spawner in diesem Kontext — Matrix-Games laufen über den Game Master (matrix-game-master)"
                    .to_owned(),
            )
        })?;
        let store = ctx.state_store().ok_or_else(|| {
            OpError::NotAvailable("kein StateStore in diesem Kontext konfiguriert".to_owned())
        })?;
        Ok(Self {
            spawner,
            store,
            parent_sandbox: ctx.sandbox().clone(),
            sandbox: ctx.sandbox().restrict(&PermissionRequest::empty()),
            parent: ctx.session_id().clone(),
            cancel: ctx.cancel_token().cloned().unwrap_or_else(CancelToken::new),
            budget: parse_budget_hint(SEAT_TURN_BUDGET)?,
            children: BTreeMap::new(),
            last_used: BTreeMap::new(),
            tick: 0,
            materials_root: None,
            run_tag: String::new(),
            warnings: Vec::new(),
        })
    }

    /// Bindet die Sitze an ihre Unterlagen-Kopien unter `root`
    /// (`<root>/<seat_id>/`).
    #[must_use]
    pub fn with_materials(mut self, root: Option<PathBuf>, run_tag: &str) -> Self {
        self.materials_root = root;
        self.run_tag = run_tag.to_owned();
        self
    }

    /// Sandbox eines Sitzes: seine Unterlagen-Kopie als Harness-Lesesicht
    /// (nur lesend, ohne Netz); ohne Kopien die berechtigungslose Sandbox.
    ///
    /// # Errors
    /// Deutsche Fehlerbeschreibung, wenn die Lesesicht nicht gebaut werden
    /// kann — der Sitz verwirkt dann den Zug; es gibt bewusst keinen
    /// Rückfall auf eine andere Sandbox.
    fn seat_sandbox_for(&self, key: &str) -> Result<SandboxSpec, String> {
        let Some(root) = &self.materials_root else {
            return Ok(self.sandbox.clone());
        };
        let workspace = format!("matrix-{}-{key}", self.run_tag);
        seat_sandbox(&self.parent_sandbox, &root.join(key), &workspace).map_err(|error| {
            format!("Unterlagen-Sandbox für Sitz `{key}` nicht erzeugbar: {error}")
        })
    }

    /// Übernimmt die bereits zugelassenen Sitze eines Laufs.
    #[must_use]
    pub fn with_children(mut self, children: BTreeMap<String, SessionId>) -> Self {
        self.children = children;
        self
    }

    /// Gibt die Sitz-Zuordnung an den Lauf zurück.
    #[must_use]
    pub fn into_children(self) -> BTreeMap<String, SessionId> {
        self.children
    }

    /// Vergisst einen Sitz und gibt sein Kind frei (nach einem Fehler; der
    /// nächste Aufruf startet ein frisches Kind).
    fn forget(&mut self, seat_key: &str) {
        self.last_used.remove(seat_key);
        if let Some(child) = self.children.remove(seat_key) {
            if let Err(error) = self.spawner.release_child(&child) {
                tracing::warn!(child = %child, error = %error, "matrix.seat_release_failed");
            }
        }
    }

    /// Runde 9, E7: verlängert die Lease aller gehaltenen Sitze. Ein Sitz
    /// wartet zwischen seinen Zügen oft länger als die Lease (15 min), weil
    /// die anderen Sitze nacheinander ziehen; ohne Verlängerung räumte der
    /// Reaper ihn ab und sein nächster Zug scheiterte an einem fehlenden
    /// Admission-Record.
    fn renew_held_leases(&self) {
        let now = Timestamp::now();
        for child in self.children.values() {
            if let Err(error) = self.spawner.renew_lease(child, now) {
                tracing::debug!(child = %child, error = %error, "matrix.seat_lease_renew_failed");
            }
        }
    }

    /// Runde 9, E7: macht vor dem Start eines neuen Sitzes Platz, wenn der
    /// Fan-out-Deckel des Spawners (`max_active_children_per_parent`, aus dem
    /// Modellprofil, oft 3–4) schon von gehaltenen Sitzen belegt ist: der am
    /// längsten nicht gefragte Sitz wird freigegeben und beim nächsten Zug
    /// frisch gestartet (mit vollem Lagebild).
    ///
    /// # Errors
    /// Deutsche Beschreibung, wenn der Deckel 0 ist (Modell ohne Delegation).
    fn make_room_for(&mut self, key: &str) -> Result<(), String> {
        let limit = self.spawner.limits().max_active_children_per_parent;
        if limit == 0 {
            return Err(
                "Spawn fehlgeschlagen: das Modellprofil erlaubt keine Kind-Agenten \
                 (max_child_fanout = 0) — Sitze können nicht starten"
                    .to_owned(),
            );
        }
        while self.children.len() >= limit {
            let Some(victim) = self
                .children
                .keys()
                .filter(|seat| seat.as_str() != key)
                .min_by_key(|seat| self.last_used.get(seat.as_str()).copied().unwrap_or(0))
                .cloned()
            else {
                break;
            };
            tracing::info!(seat = %victim, limit, "matrix.seat_evicted_for_capacity");
            self.forget(&victim);
        }
        Ok(())
    }
}

impl SeatDriver for SpawnerDriver {
    fn ask<'a>(&'a mut self, request: SeatRequest<'a>) -> SeatFuture<'a> {
        Box::pin(async move {
            let key = request.seat_key;
            let child = match self.children.get(key) {
                Some(child) => child.clone(),
                None => {
                    // Kein Rückfall: ist die Lesesicht nicht baubar oder
                    // weist die Admission sie ab, verwirkt der Sitz den Zug
                    // (fail-closed).
                    let bound = self.seat_sandbox_for(key)?;
                    self.make_room_for(key)?;
                    let input = harw_extension_api::SpawnInput {
                        parent_session_id: self.parent.clone(),
                        handoff_call_id: ToolCallId::new(),
                        instructions: Some(request.system.to_owned()),
                        context: json!({ "matrix_seat": key }),
                        ceiling: None,
                    };
                    let child = self
                        .spawner
                        .spawn_child_or_wait(
                            request.role,
                            input,
                            bound,
                            None,
                            SEAT_SLOT_WAIT,
                            &self.cancel,
                        )
                        .await
                        .map(harw_core::child_controller::ChildGuard::keep)
                        .map_err(|error| format!("Spawn fehlgeschlagen: {error}"))?;
                    // Der Sitz bleibt über alle Phasen zugelassen; die
                    // Freigabe übernimmt `release_all` bzw. `forget`.
                    self.children.insert(key.to_owned(), child.clone());
                    child
                }
            };
            self.tick += 1;
            self.last_used.insert(key.to_owned(), self.tick);
            let Some(declared) = self.spawner.child_budget(&child) else {
                self.forget(key);
                return Err(format!(
                    "Admission-Record von `{child}` fehlt (Sitz-Agent wurde freigegeben, z. B. Lease abgelaufen)"
                ));
            };
            let budget = tighten_budget(self.budget, declared);
            let run = match self
                .spawner
                .run_child_with_budget(
                    &child,
                    self.store.as_ref(),
                    None,
                    TurnInput::user(request.prompt),
                    budget,
                )
                .await
            {
                Ok(run) => run,
                Err(error) => {
                    self.forget(key);
                    return Err(format!("Lauf fehlgeschlagen: {error}"));
                }
            };
            if !matches!(run.outcome, TurnOutcome::Completed) {
                let outcome = format!("{:?}", run.outcome);
                self.forget(key);
                return Err(format!("Sitz-Agent schloss nicht ab ({outcome})"));
            }
            match run.full_text {
                Some(text) => Ok(text),
                None => self
                    .spawner
                    .child_final_assistant_text_full(&child)
                    .map_err(|error| format!("keine Antwort verfügbar: {error}")),
            }
        })
    }

    fn release_all(&mut self) {
        let keys: Vec<String> = self.children.keys().cloned().collect();
        for key in keys {
            self.forget(&key);
        }
    }

    fn abandon(&mut self, seat_key: &str) {
        if let Some(child) = self.children.get(seat_key) {
            let requested = self.spawner.request_cancellation(child);
            tracing::info!(child = %child, requested, "matrix.seat_abandoned");
        }
        self.forget(seat_key);
    }

    fn keeps_context(&mut self, seat_key: &str) -> bool {
        self.renew_held_leases();
        let Some(child) = self.children.get(seat_key).cloned() else {
            return false;
        };
        // Vom Reaper oder anderswo freigegeben: frisch starten statt am
        // fehlenden Admission-Record zu scheitern.
        let Some(remaining) = self.spawner.remaining_budget(&child) else {
            self.children.remove(seat_key);
            self.last_used.remove(seat_key);
            return false;
        };
        // Fast aufgebrauchtes Sitzungsbudget: rechtzeitig ersetzen.
        if remaining
            .max_tokens
            .is_some_and(|left| left < SEAT_RECYCLE_BELOW_TOKENS)
        {
            tracing::info!(seat = seat_key, child = %child, "matrix.seat_recycled_for_budget");
            self.forget(seat_key);
            return false;
        }
        true
    }

    fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }
}

/// Ausgang eines bewachten Sitz-Aufrufs.
#[derive(Debug)]
enum Guarded<T> {
    /// Antwort liegt vor.
    Done(T),
    /// Zeitlimit überschritten.
    TimedOut,
    /// Lauf abgebrochen.
    Cancelled,
}

/// Wartet auf `future`, höchstens `limit` lang und nur bis `cancel`
/// ausgelöst wird (Runde 7, Teil M). Ohne Token zählt nur das Zeitlimit.
async fn guarded<F, T>(future: F, limit: Duration, cancel: Option<&CancelToken>) -> Guarded<T>
where
    F: Future<Output = T>,
{
    let mut future = std::pin::pin!(future);
    let mut sleep = std::pin::pin!(tokio::time::sleep(limit));
    let mut cancelled = std::pin::pin!(async move {
        match cancel {
            Some(token) => token.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    });
    std::future::poll_fn(|cx| {
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Guarded::Done(value));
        }
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Guarded::Cancelled);
        }
        if sleep.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Guarded::TimedOut);
        }
        Poll::Pending
    })
    .await
}

/// Live-Event-Senke eines Laufs (nur gesetzt, wenn der Kontext einen Hub trägt).
#[derive(Debug, Clone)]
pub struct EventSink {
    /// Hub.
    pub hub: Arc<AgentEventHub>,
    /// Treibende Session.
    pub agent: SessionId,
}

impl EventSink {
    /// Aus dem Op-Kontext (`Arc<AgentEventHub>` im ServiceMap).
    #[must_use]
    pub fn from_ctx(ctx: &OpContext) -> Option<Self> {
        ctx.service::<Arc<AgentEventHub>>().map(|hub| Self {
            hub: Arc::clone(hub),
            agent: ctx.session_id().clone(),
        })
    }
}

// ── Lauf ─────────────────────────────────────────────────────────────────────

/// Ein laufendes Matrix-Spiel.
pub struct MatrixRun {
    run_id: String,
    loaded: LoadedScenario,
    source: String,
    log: GameLog,
    cfg: PhaseConfig,
    cursor: RoundCursor,
    /// Nächste Position abweichend von der FSM (Facilitator „Ende“).
    jump: Option<RoundCursor>,
    /// Sitz-Schlüssel → zugelassene Kind-Session.
    children: BTreeMap<String, SessionId>,
    /// Sitz-Schlüssel → lokale Nummer des zuletzt gesehenen Projektionseintrags.
    seen: BTreeMap<String, usize>,
    /// Offengelegte Argumente der laufenden Runde.
    args: Vec<RoundArgument>,
    pending_injects: Vec<ScheduledInject>,
    pending_overrides: BTreeMap<String, Value>,
    pending_vetoes: BTreeMap<String, String>,
    inject_counter: u32,
    end_reason: String,
    run_dir: Option<PathBuf>,
    /// Pfad der Szenario-Datei (Basis relativer `[materials]`-Ordner).
    scenario_path: Option<PathBuf>,
    /// `<run_dir>/materials`, sobald die Sitz-Kopien angelegt sind.
    materials_root: Option<PathBuf>,
    /// Kopierberichte je Sitz (für das Journal nach dem Anlegen).
    materials_reports: Vec<(String, MaterialsReport)>,
    flushed: usize,
    status: RunStatus,
    timestamps: bool,
    sink: Option<EventSink>,
    aar: Option<String>,
    /// Hartes Zeitlimit je Sitz-Aufruf (Runde 7, Teil M).
    seat_timeout: Duration,
    /// Abbruch des Laufs (Game-Master-Turn bzw. Aufrufer).
    cancel: Option<CancelToken>,
    /// Umpire-Synthese aus der AAR-Phase (für `report.md`).
    synthesis: Option<UmpireSynthesis>,
    /// Spieler-Debriefs aus der AAR-Phase (für `report.md`).
    debriefs: BTreeMap<PlayerId, PlayerDebrief>,
    /// Pfad von `report.md`, sobald geschrieben.
    report_path: Option<PathBuf>,
}

impl std::fmt::Debug for MatrixRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixRun")
            .field("run_id", &self.run_id)
            .field("scenario", &self.loaded.scenario.id())
            .field("cursor", &self.cursor)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Bildet einen Kernfehler auf einen Operationsfehler ab.
pub(crate) fn matrix_err(error: MatrixError) -> OpError {
    OpError::Execution(format!("Matrix-Spiel: {error}"))
}

fn io_err(what: &str, path: &Path, error: &std::io::Error) -> OpError {
    OpError::Execution(format!("{what} `{}`: {error}", path.display()))
}

/// Master-Seed eines Laufs: `--seed` > Szenario-Seed > SHA-256 des
/// Szenario-Quelltexts (deterministisch).
#[must_use]
pub fn master_seed_for(loaded: &LoadedScenario, explicit: Option<u64>) -> [u8; 32] {
    let fallback = || sha256_parts(&[DOMAIN, b"/runner-seed", loaded.source_hash.as_bytes()]);
    if let Some(n) = explicit {
        return SeedSpec::Number(n).master_seed().unwrap_or_else(fallback);
    }
    loaded
        .scenario
        .seed()
        .and_then(SeedSpec::master_seed)
        .unwrap_or_else(fallback)
}

/// Sitz-Schlüssel (`rat`, `umpire`).
#[must_use]
pub fn seat_key(seat: &Seat) -> String {
    match seat {
        Seat::Player(p) => p.as_str().to_owned(),
        Seat::Umpire => UMPIRE_KEY.to_owned(),
        Seat::RedCell => RED_CELL_KEY.to_owned(),
    }
}

/// Agentenrolle eines Sitzes (Single Source of Truth: `role_names`).
fn role_name(role: SeatRole) -> &'static str {
    match role {
        SeatRole::Player => role_names::MATRIX_PLAYER,
        SeatRole::Umpire => role_names::MATRIX_UMPIRE,
        SeatRole::Market => role_names::MATRIX_MARKET,
        SeatRole::RedCell => role_names::MATRIX_REDCELL,
    }
}

fn role_label(role: SeatRole) -> &'static str {
    match role {
        SeatRole::Player => "player",
        SeatRole::Umpire => "umpire",
        SeatRole::Market => "market",
        SeatRole::RedCell => "red-cell",
    }
}

impl MatrixRun {
    /// Eröffnet ein Spiel (Setup: `GameCreated`, Deklarationen, Briefings).
    /// Mit `run_dir` werden Verzeichnis, Szenario-Kopie, Journal und die
    /// Unterlagen-Kopien je Sitz (`materials/<sitz>/`) angelegt.
    /// `scenario_path` ist der Pfad der Szenario-Datei (für relative
    /// `[materials]`-Ordner; `None` bei gebündelten Szenarien).
    /// `package` wählt ein Inject-Paket der Szenario-Bibliothek; ohne Angabe
    /// wählt der Seed eines (nur wenn das Szenario Pakete hat).
    ///
    /// # Errors
    /// [`OpError::Execution`] bei Kern- oder Dateifehlern,
    /// [`OpError::InvalidArguments`], wenn der Unterlagen-Ordner fehlt.
    // Jeder Parameter ist ein eigenständiger Laufwert (Szenario, Quelle, Pfad,
    // Seed, Kennung, Verzeichnis, Zeitstempel, Paket); ein Hilfs-Struct brächte
    // hier nur Umverpackung.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        loaded: LoadedScenario,
        source: String,
        scenario_path: Option<PathBuf>,
        master_seed: [u8; 32],
        run_id: String,
        run_dir: Option<PathBuf>,
        timestamps: bool,
        package: Option<&str>,
    ) -> Result<Self, OpError> {
        let at = timestamps.then(Timestamp::now);
        let mut log = open_game(&loaded, &master_seed, at).map_err(matrix_err)?;
        library::record_inject_package(&mut log, &loaded.scenario, package, at)
            .map_err(matrix_err)?;
        let cfg = PhaseConfig::from_scenario(&loaded.scenario);
        let mut run = Self::from_log(
            loaded,
            source,
            scenario_path,
            log,
            run_id,
            run_dir,
            timestamps,
            cfg,
        )?;
        run.note_materials()?;
        Ok(run)
    }

    #[allow(clippy::too_many_arguments)]
    fn from_log(
        loaded: LoadedScenario,
        source: String,
        scenario_path: Option<PathBuf>,
        log: GameLog,
        run_id: String,
        run_dir: Option<PathBuf>,
        timestamps: bool,
        cfg: PhaseConfig,
    ) -> Result<Self, OpError> {
        let mut materials_root = None;
        let mut materials_reports = Vec::new();
        if let Some(dir) = &run_dir {
            std::fs::create_dir_all(dir)
                .map_err(|e| io_err("Laufverzeichnis nicht anlegbar", dir, &e))?;
            let copy = dir.join(SCENARIO_FILE);
            std::fs::write(&copy, &source)
                .map_err(|e| io_err("Szenario-Kopie nicht schreibbar", &copy, &e))?;
            let source_dir = loaded.materials_dir(scenario_path.as_deref());
            if let Some(src) = &source_dir {
                if !src.is_dir() {
                    return Err(OpError::InvalidArguments(format!(
                        "Unterlagen-Ordner `{}` existiert nicht",
                        src.display()
                    )));
                }
            }
            let root = dir.join(MATERIALS_DIR);
            let mut seats: Vec<Seat> = log
                .state
                .players
                .iter()
                .cloned()
                .map(Seat::Player)
                .collect();
            seats.push(Seat::Umpire);
            if loaded.scenario.red_cell().is_some() {
                seats.push(Seat::RedCell);
            }
            for seat in &seats {
                let key = seat_key(seat);
                if !safe_component(&key) {
                    return Err(OpError::InvalidArguments(format!(
                        "Sitz-ID `{key}` taugt nicht als Ordnername"
                    )));
                }
                let report = build_seat_materials(
                    source_dir.as_deref(),
                    &log.state.players,
                    seat,
                    &root.join(&key),
                )
                .map_err(OpError::Execution)?;
                materials_reports.push((key, report));
            }
            materials_root = Some(root);
        }
        let cursor = RoundCursor {
            round: log.state.round,
            phase: log.state.phase,
        };
        Ok(Self {
            run_id,
            loaded,
            source,
            log,
            cfg,
            cursor,
            jump: None,
            children: BTreeMap::new(),
            seen: BTreeMap::new(),
            args: Vec::new(),
            pending_injects: Vec::new(),
            pending_overrides: BTreeMap::new(),
            pending_vetoes: BTreeMap::new(),
            inject_counter: 0,
            end_reason: "Reguläres Spielende".to_owned(),
            run_dir,
            scenario_path,
            materials_root,
            materials_reports,
            flushed: 0,
            status: RunStatus::Running,
            timestamps,
            sink: None,
            aar: None,
            seat_timeout: SEAT_TURN_TIMEOUT,
            cancel: None,
            synthesis: None,
            debriefs: BTreeMap::new(),
            report_path: None,
        })
    }

    // ── Zugriffe ────────────────────────────────────────────────────────────

    /// Lauf-ID.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Geladenes Szenario.
    #[must_use]
    pub fn loaded(&self) -> &LoadedScenario {
        &self.loaded
    }

    /// Szenario-Quelltext.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Pfad der Szenario-Datei (`None` bei gebündelten Szenarien).
    #[must_use]
    pub fn scenario_path(&self) -> Option<&Path> {
        self.scenario_path.as_deref()
    }

    /// Zustand und Journal.
    #[must_use]
    pub fn log(&self) -> &GameLog {
        &self.log
    }

    /// FSM-Position.
    #[must_use]
    pub fn cursor(&self) -> RoundCursor {
        self.cursor
    }

    /// Status.
    #[must_use]
    pub fn status(&self) -> RunStatus {
        self.status
    }

    /// Setzt den Status (Pause/Fortsetzen; `Ended` nur über das AAR).
    pub fn set_status(&mut self, status: RunStatus) {
        if self.status != RunStatus::Ended {
            self.status = status;
        }
    }

    /// Laufverzeichnis.
    #[must_use]
    pub fn run_dir(&self) -> Option<&Path> {
        self.run_dir.as_deref()
    }

    /// AAR-Markdown nach Spielende.
    #[must_use]
    pub fn aar(&self) -> Option<&str> {
        self.aar.as_deref()
    }

    /// Setzt (oder löscht) die Live-Event-Senke.
    pub fn set_sink(&mut self, sink: Option<EventSink>) {
        self.sink = sink;
    }

    /// Setzt das harte Zeitlimit je Sitz-Aufruf (Vorgabe
    /// [`SEAT_TURN_TIMEOUT`]).
    pub fn set_seat_timeout(&mut self, limit: Duration) {
        self.seat_timeout = limit;
    }

    /// Setzt (oder löscht) den Abbruch-Token des Laufs: ist er ausgelöst,
    /// passt jeder weitere Sitz-Aufruf sofort, ein laufender wird abgebrochen.
    pub fn set_cancel(&mut self, cancel: Option<CancelToken>) {
        self.cancel = cancel;
    }

    /// `true`, wenn der Abbruch-Token des Laufs ausgelöst ist.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(CancelToken::is_cancelled)
    }

    /// Pfad des paper-tauglichen Berichts (`report.md`), sobald geschrieben.
    #[must_use]
    pub fn report_path(&self) -> Option<&Path> {
        self.report_path.as_deref()
    }

    /// Umpire-Synthese der AAR-Phase.
    #[must_use]
    pub fn synthesis(&self) -> Option<&UmpireSynthesis> {
        self.synthesis.as_ref()
    }

    /// Spieler-Debriefs der AAR-Phase.
    #[must_use]
    pub fn debriefs(&self) -> &BTreeMap<PlayerId, PlayerDebrief> {
        &self.debriefs
    }

    fn rules(&self) -> &Rules {
        self.loaded.scenario.rules()
    }

    fn at(&self) -> Option<Timestamp> {
        self.timestamps.then(Timestamp::now)
    }

    fn vis(&self) -> VisibilityCfg {
        VisibilityCfg::from_settings(self.loaded.scenario.visibility())
    }

    fn all_seats(&self) -> Vec<Seat> {
        let mut seats: Vec<Seat> = self
            .log
            .state
            .players
            .iter()
            .cloned()
            .map(Seat::Player)
            .collect();
        seats.push(Seat::Umpire);
        if self.loaded.scenario.red_cell().is_some() {
            seats.push(Seat::RedCell);
        }
        seats
    }

    // ── Journal ─────────────────────────────────────────────────────────────

    fn record(&mut self, entry: GameEntry) -> Result<(), OpError> {
        let at = self.at();
        self.log.record(entry, at).map(|_| ()).map_err(matrix_err)
    }

    /// Journalisiert einen Facilitator- bzw. Runner-Vermerk (nur Beobachter).
    ///
    /// # Errors
    /// [`OpError::Execution`] bei Zustandsfehlern.
    pub fn note(&mut self, command: &str, detail: impl Into<String>) -> Result<(), OpError> {
        let round = self.log.state.round;
        self.record(GameEntry::new(
            round,
            Audience::ObserverOnly,
            EntryKind::FacilitatorNote {
                command: command.to_owned(),
                detail: detail.into(),
            },
        ))
    }

    /// Journalisiert die Kopierberichte der Unterlagen (nur Beobachter).
    fn note_materials(&mut self) -> Result<(), OpError> {
        for (key, report) in std::mem::take(&mut self.materials_reports) {
            let mut detail = format!(
                "Sitz `{key}`: {} Datei(en), {} Byte",
                report.copied, report.bytes
            );
            if !report.skipped.is_empty() {
                detail.push_str(&format!(" · übersprungen: {}", report.skipped.join("; ")));
            }
            self.note("materials", detail)?;
        }
        Ok(())
    }

    /// Hängt alle noch nicht geschriebenen Journalzeilen an `journal.jsonl`
    /// an und veröffentlicht sie als Live-Events.
    ///
    /// # Errors
    /// [`OpError::Execution`] bei Schreibfehlern.
    pub fn flush(&mut self) -> Result<(), OpError> {
        let records = self.log.journal.records();
        let total = records.len();
        let fresh = records.get(self.flushed..).unwrap_or(&[]);
        if fresh.is_empty() {
            return Ok(());
        }
        if let Some(dir) = &self.run_dir {
            let path = dir.join(JOURNAL_FILE);
            for record in fresh {
                Journal::append_record_to_file(&path, record).map_err(matrix_err)?;
            }
        }
        if let Some(sink) = &self.sink {
            let revealed = revealed_secrets(&self.log.journal);
            let vis = self.vis();
            let seats = self.all_seats();
            for record in fresh {
                let entry = &record.entry;
                let mut event = entry_json(
                    entry.round,
                    &entry.audience,
                    &entry.kind,
                    &self.loaded.scenario,
                );
                let visible: Vec<String> = seats
                    .iter()
                    .filter(|seat| visible_to(entry, seat, &vis, &revealed))
                    .map(seat_key)
                    .collect();
                let events: Vec<Value> = events_for_entry(entry)
                    .iter()
                    .filter_map(|e| serde_json::to_value(e).ok())
                    .collect();
                if let Value::Object(map) = &mut event {
                    map.insert("seats".to_owned(), json!(visible));
                    map.insert("events".to_owned(), Value::Array(events));
                }
                sink.hub
                    .publish_matrix(sink.agent.clone(), self.run_id.clone(), event);
            }
        }
        self.flushed = total;
        Ok(())
    }

    // ── Schritt ─────────────────────────────────────────────────────────────

    /// Führt genau eine Phase mit echten Kind-Agenten aus.
    ///
    /// # Errors
    /// [`OpError::NotAvailable`] ohne Spawner, sonst wie [`Self::step_with`].
    pub async fn step(&mut self, ctx: &OpContext) -> Result<StepReport, OpError> {
        let driver =
            SpawnerDriver::from_ctx(ctx)?.with_materials(self.materials_root.clone(), &self.run_id);
        let mut driver = driver.with_children(std::mem::take(&mut self.children));
        let result = self.step_with(&mut driver).await;
        self.children = driver.into_children();
        result
    }

    /// Führt genau eine Phase mit einem beliebigen [`SeatDriver`] aus.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] nach Spielende; [`OpError::Execution`]
    /// bei Kern- oder Dateifehlern.
    pub async fn step_with<D: SeatDriver>(
        &mut self,
        driver: &mut D,
    ) -> Result<StepReport, OpError> {
        if self.status == RunStatus::Ended {
            return Err(OpError::InvalidArguments(format!(
                "Lauf `{}` ist beendet",
                self.run_id
            )));
        }
        let next = match self.jump.take() {
            Some(cursor) => cursor,
            None => self.cursor.next(&self.cfg).ok_or_else(|| {
                OpError::InvalidArguments(format!("Lauf `{}` hat keine weitere Phase", self.run_id))
            })?,
        };
        self.cursor = next;
        let at = self.at();
        enter_phase(&mut self.log, next, at).map_err(matrix_err)?;
        let mut report = StepReport::new(next);
        if next.phase == Phase::Briefing {
            self.args.clear();
            let mut injects = self.loaded.scenario.injects_for_round(next.round);
            injects.extend(library::package_injects_for_round(
                &self.loaded.scenario,
                &self.log.journal,
                next.round,
            ));
            for inject in injects {
                apply_inject(&mut self.log, self.loaded.scenario.rules(), &inject, at)
                    .map_err(matrix_err)?;
            }
        }
        self.apply_pending_injects()?;
        self.flush()?;
        match next.phase {
            Phase::Setup | Phase::Veroeffentlichung => {}
            Phase::Briefing => self.run_briefing(driver, &mut report).await?,
            Phase::Verhandlung => self.run_negotiation(driver, &mut report).await?,
            Phase::Argumente => self.run_arguments(driver, &mut report).await?,
            Phase::Gegenargumente => self.run_counters(driver, &mut report).await?,
            Phase::Adjudikation => self.run_adjudication(driver, &mut report).await?,
            Phase::Rundenende => {
                // Runde 7, Teil M5: im Business-Modus zuerst das Marktmodell.
                let at = self.at();
                close_round_with_market(&mut self.log, &self.loaded.scenario, at)
                    .map_err(matrix_err)?;
            }
            Phase::Schlussargumente => self.run_final_arguments(driver, &mut report).await?,
            Phase::Aar => self.run_aar(driver, &mut report).await?,
        }
        // Runde 9, E7: scheiterte jeder Sitz-Aufruf der Phase technisch
        // (Spawn, Admission, Lauf, Zeitlimit), ist das kein Spielergebnis —
        // der Lauf pausiert mit der Ursache, statt weitere Runden leerer
        // Pässe zu spielen.
        if self.status != RunStatus::Ended
            && let Some(causes) = report.all_calls_failed_technically()
        {
            self.status = RunStatus::Paused;
            self.note(
                TECHNICAL_STOP_NOTE,
                format!(
                    "Runde {} · Phase {}: alle {} Sitz-Aufrufe scheiterten technisch — {causes}",
                    self.cursor.round,
                    self.cursor.phase.label(),
                    report.calls
                ),
            )?;
            report.technical_stop = Some(causes);
        }
        self.flush()?;
        report.round = self.cursor.round;
        report.ended = self.status == RunStatus::Ended;
        Ok(report)
    }

    fn apply_pending_injects(&mut self) -> Result<(), OpError> {
        let pending = std::mem::take(&mut self.pending_injects);
        let at = self.at();
        for mut inject in pending {
            inject.round = self.log.state.round;
            apply_inject(&mut self.log, self.loaded.scenario.rules(), &inject, at)
                .map_err(matrix_err)?;
        }
        Ok(())
    }

    fn calls_for(&self, phase: Phase, step: PhaseStep) -> Vec<ExpectedCall> {
        let round = self.log.state.round;
        let members: BTreeSet<PlayerId> = self
            .log
            .state
            .channels
            .values()
            .filter(|c| c.round == round)
            .flat_map(|c| c.members.iter().cloned())
            .collect();
        expected_calls(
            phase,
            step,
            &self.log.state.players,
            &members,
            self.loaded.scenario.rules(),
            self.loaded.scenario.red_cell().is_some(),
        )
    }

    /// Ein Sitz-Aufruf mit Parse, Validierung und genau einem Reparatur-Turn.
    /// Rückgabe: Ausgabe plus Versuchsnummer (0/1), oder `None` = Forfeit.
    async fn call_seat<D: SeatDriver>(
        &mut self,
        driver: &mut D,
        call: &ExpectedCall,
        vis: &VisibilityCfg,
        report: &mut StepReport,
    ) -> Result<Option<(PhaseOutput, u32)>, OpError> {
        let key = seat_key(&call.seat);
        let role = SeatRole::for_seat(&self.loaded, &call.seat);
        let system = prompts::system_prompt(role, &self.loaded, Some(&call.seat));
        let view = project(&self.log.journal, &call.seat, vis);
        // Runde 9, E7: ein frisch (wieder) gestarteter Sitz kennt nichts —
        // dann volles Lagebild statt Delta.
        if !driver.keeps_context(&key) {
            self.seen.remove(&key);
        }
        let since = self.seen.get(&key).copied().unwrap_or(0);
        let situation = since == 0
            || self.cursor.phase == Phase::Briefing
            || (call.seat == Seat::Umpire && self.cursor.phase == Phase::Adjudikation);
        let mut prompt = prompts::turn_prompt(&view, since, call, situation);
        let last_seen = view
            .entries()
            .last()
            .map_or(0, |e| usize::try_from(e.n).unwrap_or(usize::MAX));
        report.calls += 1;
        let mut last_error = String::new();
        for attempt in 0..=REPAIR_ATTEMPTS {
            let mut kind = ForfeitKind::Technical;
            let answer = if self.is_cancelled() {
                kind = ForfeitKind::Cancelled;
                Err("Lauf abgebrochen".to_owned())
            } else {
                let outcome = guarded(
                    driver.ask(SeatRequest {
                        seat_key: &key,
                        role: role_name(role),
                        system: &system,
                        prompt: std::mem::take(&mut prompt),
                    }),
                    self.seat_timeout,
                    self.cancel.as_ref(),
                )
                .await;
                match outcome {
                    Guarded::Done(answer) => answer,
                    Guarded::TimedOut => {
                        driver.abandon(&key);
                        Err(format!(
                            "Zeitlimit von {} s für den Zug überschritten",
                            self.seat_timeout.as_secs()
                        ))
                    }
                    Guarded::Cancelled => {
                        driver.abandon(&key);
                        kind = ForfeitKind::Cancelled;
                        Err("Lauf abgebrochen".to_owned())
                    }
                }
            };
            for warning in driver.take_warnings() {
                self.note("materials", warning)?;
            }
            let raw = match answer {
                Ok(raw) => raw,
                Err(error) => {
                    // Ein neues Kind kennt nichts — beim nächsten Mal volles Lagebild.
                    self.seen.remove(&key);
                    self.note(
                        FORFEIT_NOTE,
                        format!(
                            "Sitz `{key}` ({:?}): {FAILED_CALL} — {error}",
                            call.contract
                        ),
                    )?;
                    report.push_forfeit(key, error, kind);
                    return Ok(None);
                }
            };
            self.seen.insert(key.clone(), last_seen);
            let error = match PhaseOutput::parse(call.contract, &raw) {
                Ok(output) => match self.check_output(call, &output, attempt) {
                    Ok(()) => return Ok(Some((output, attempt))),
                    Err(error) => error,
                },
                Err(error) => error.to_string(),
            };
            prompt = prompts::repair_prompt(&error, call.contract);
            last_error = error;
        }
        self.note(
            FORFEIT_NOTE,
            format!(
                "Sitz `{key}` ({:?}): {INVALID_ANSWER} — {last_error}",
                call.contract
            ),
        )?;
        report.push_forfeit(
            key,
            INVALID_ANSWER_CAUSE.to_owned(),
            ForfeitKind::InvalidAnswer,
        );
        Ok(None)
    }

    /// Prüft eine geparste Antwort gegen Contract, Zustand und (im ersten
    /// Versuch, Modus `strict`) den Leak-Guard.
    fn check_output(
        &self,
        call: &ExpectedCall,
        output: &PhaseOutput,
        attempt: u32,
    ) -> Result<(), String> {
        let state = &self.log.state;
        let rules = self.rules();
        let player = match &call.seat {
            Seat::Player(p) => Some(p),
            Seat::Umpire | Seat::RedCell => None,
        };
        let result = match (output, player) {
            (PhaseOutput::BriefingAck(_), _) | (PhaseOutput::PlayerDebrief(_), Some(_)) => Ok(()),
            (PhaseOutput::NegotiationRequest(r), Some(p)) => {
                validate_negotiation_request(r, p, &state.players, rules)
            }
            (PhaseOutput::NegotiationMessages(m), Some(p)) => {
                validate_negotiation_messages(m, p, state, rules)
            }
            (PhaseOutput::PlayerArgument(a), Some(p)) => validate_player_argument(
                a,
                p,
                state,
                rules,
                call.contract == ContractKind::FinalArgument,
            ),
            (PhaseOutput::CounterArgument(c), Some(p)) => {
                validate_counter_argument(c, p, &self.args)
            }
            (PhaseOutput::UmpireAdjudication(a), None) => {
                validate_umpire_adjudication(a, &self.args, state, rules)
            }
            (PhaseOutput::UmpireNarration(n), None) => {
                validate_umpire_narration(n, &self.args, &state.players)
            }
            (PhaseOutput::UmpireSynthesis(s), None) => {
                let errors = validate_goal_ratings(&s.goal_ratings, &self.loaded.scenario);
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(MatrixError::Contract(errors))
                }
            }
            (PhaseOutput::RedCellObjection(o), None) if call.seat == Seat::RedCell => {
                match self.loaded.scenario.red_cell() {
                    Some(settings) => validate_red_cell_objection(o, &self.args, settings),
                    None => Err(MatrixError::Contract(vec![
                        "Red Cell ist in diesem Szenario nicht aktiv".to_owned(),
                    ])),
                }
            }
            _ => Err(MatrixError::Contract(vec![format!(
                "Antwort passt nicht zum Sitz `{}`",
                call.seat
            )])),
        };
        result.map_err(|e| e.to_string())?;
        if attempt == 0 {
            let mode = self.loaded.scenario.visibility().leak_guard;
            for (source, text) in public_texts(output, &self.args) {
                if let GuardDecision::Reprompt(findings) =
                    guard_public_text(&text, &self.log.journal, mode, 0)
                {
                    return Err(format!(
                        "Leak-Verdacht in {source}: {}. Formuliere den öffentlichen Text ohne geschützte Inhalte (private Kanäle, geheime Ziele, verdeckte Werte) neu.",
                        describe_findings(&findings)
                    ));
                }
            }
        }
        Ok(())
    }

    /// Wendet den Leak-Guard auf einen akzeptierten öffentlichen Text an.
    /// Rückgabe `true` = Text veröffentlichen.
    fn guard_text(
        &mut self,
        source: &str,
        text: &str,
        attempt: u32,
        report: &mut StepReport,
    ) -> Result<bool, OpError> {
        let mode = self.loaded.scenario.visibility().leak_guard;
        let round = self.log.state.round;
        match guard_public_text(text, &self.log.journal, mode, attempt) {
            GuardDecision::Clean | GuardDecision::Reprompt(_) => Ok(true),
            GuardDecision::Withhold(findings) => {
                report.leaks += 1;
                self.record(leak_suspect_entry(round, source, text, &findings))?;
                Ok(false)
            }
            GuardDecision::Flag(findings) => {
                report.leaks += 1;
                self.record(leak_suspect_entry(round, source, text, &findings))?;
                Ok(true)
            }
        }
    }

    // ── Phasen ──────────────────────────────────────────────────────────────

    async fn run_briefing<D: SeatDriver>(
        &mut self,
        driver: &mut D,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        let vis = self.vis();
        let calls = self.calls_for(Phase::Briefing, PhaseStep::Main);
        let mut answers = Vec::new();
        for call in &calls {
            let ack = match self.call_seat(driver, call, &vis, report).await? {
                Some((PhaseOutput::BriefingAck(ack), _)) => Some(ack),
                _ => None,
            };
            answers.push((call.seat.clone(), ack));
        }
        let at = self.at();
        for (seat, ack) in answers {
            match (ack, &seat) {
                (Some(ack), _) => {
                    record_briefing(&mut self.log, &seat, &ack, at).map_err(matrix_err)?;
                }
                (None, Seat::Player(p)) => self.record_forfeit(p, Phase::Briefing)?,
                (None, Seat::Umpire | Seat::RedCell) => {}
            }
        }
        Ok(())
    }

    fn record_forfeit(&mut self, player: &PlayerId, phase: Phase) -> Result<(), OpError> {
        let name = self.loaded.scenario.display_name(player);
        let round = self.log.state.round;
        self.record(GameEntry::new(
            round,
            Audience::Public,
            EntryKind::Forfeit {
                seat: player.clone(),
                phase,
                text: format!("{name} passt in der Phase {}.", phase.label()),
            },
        ))
    }

    async fn run_negotiation<D: SeatDriver>(
        &mut self,
        driver: &mut D,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        let vis = self.vis();
        let calls = self.calls_for(Phase::Verhandlung, PhaseStep::Main);
        let mut requests: BTreeMap<PlayerId, NegotiationRequest> = BTreeMap::new();
        for call in &calls {
            let Seat::Player(player) = &call.seat else {
                continue;
            };
            if let Some((PhaseOutput::NegotiationRequest(request), _)) =
                self.call_seat(driver, call, &vis, report).await?
            {
                requests.insert(player.clone(), request);
            }
        }
        let at = self.at();
        open_channels(&mut self.log, &requests, at).map_err(matrix_err)?;
        self.flush()?;
        for exchange in 1..=self.rules().negotiation.max_exchanges {
            let calls = self.calls_for(Phase::Verhandlung, PhaseStep::Exchange(exchange));
            if calls.is_empty() {
                break;
            }
            let mut replies = Vec::new();
            for call in &calls {
                let Seat::Player(player) = &call.seat else {
                    continue;
                };
                if let Some((PhaseOutput::NegotiationMessages(messages), _)) =
                    self.call_seat(driver, call, &vis, report).await?
                {
                    replies.push((player.clone(), messages));
                }
            }
            // Erst nach allen Aufrufen posten: jeder Sitz antwortet auf
            // denselben Stand (kanonische Sitzreihenfolge).
            let at = self.at();
            for (player, messages) in replies {
                post_messages(&mut self.log, &player, &messages, at).map_err(matrix_err)?;
            }
            self.flush()?;
        }
        let at = self.at();
        close_negotiation(&mut self.log, at).map_err(matrix_err)
    }

    async fn run_arguments<D: SeatDriver>(
        &mut self,
        driver: &mut D,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        let vis = self.vis();
        let round = self.cursor.round;
        let calls = self.calls_for(Phase::Argumente, PhaseStep::Main);
        let mut sealed = ArgumentBox::new(round);
        for call in &calls {
            let Seat::Player(player) = &call.seat else {
                continue;
            };
            match self.call_seat(driver, call, &vis, report).await? {
                Some((PhaseOutput::PlayerArgument(argument), _)) => {
                    sealed.seal(player.clone(), argument).map_err(matrix_err)?;
                }
                _ => sealed.forfeit(player.clone()).map_err(matrix_err)?,
            }
        }
        let at = self.at();
        self.args =
            reveal_round(&mut self.log, &self.loaded.scenario, sealed, at).map_err(matrix_err)?;
        Ok(())
    }

    async fn run_counters<D: SeatDriver>(
        &mut self,
        driver: &mut D,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        let vis = self.vis();
        let calls = self.calls_for(Phase::Gegenargumente, PhaseStep::Main);
        let mut counters = Vec::new();
        for call in &calls {
            let Seat::Player(player) = &call.seat else {
                continue;
            };
            let counter = match self.call_seat(driver, call, &vis, report).await? {
                Some((PhaseOutput::CounterArgument(counter), _)) => counter,
                _ => CounterArgument::default(),
            };
            counters.push((player.clone(), counter));
        }
        let at = self.at();
        for (player, counter) in counters {
            submit_counters(&mut self.log, &mut self.args, &player, &counter, at)
                .map_err(matrix_err)?;
        }
        // Die Red Cell kommt nach allen Spielern und sieht deren Contras schon.
        // Fehlt ihre Antwort oder ist sie ungültig, gilt sie als kein Einwand.
        for call in calls.iter().filter(|c| c.seat == Seat::RedCell) {
            let objection = match self.call_seat(driver, call, &vis, report).await? {
                Some((PhaseOutput::RedCellObjection(objection), _)) => objection,
                _ => RedCellObjection {
                    no_objection: true,
                    ..RedCellObjection::default()
                },
            };
            let at = self.at();
            submit_red_cell(
                &mut self.log,
                &self.loaded.scenario,
                &mut self.args,
                &objection,
                at,
            )
            .map_err(matrix_err)?;
        }
        Ok(())
    }

    async fn run_adjudication<D: SeatDriver>(
        &mut self,
        driver: &mut D,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        let vis = self
            .vis()
            .with_cited(cited_channels(&self.log.state, &self.args));
        // Aufruf A: Urteil.
        let mut adjudication = None;
        for call in &self.calls_for(Phase::Adjudikation, PhaseStep::Main) {
            if let Some((PhaseOutput::UmpireAdjudication(mut a), attempt)) =
                self.call_seat(driver, call, &vis, report).await?
            {
                self.guard_adjudication(&mut a, attempt, report)?;
                adjudication = Some(a);
            }
        }
        let mut adjudication = match adjudication {
            Some(a) => a,
            None => {
                self.note(
                    "runner",
                    "Umpire-Urteil ungültig — neutrales Ersatzurteil (alle Gründe Gewicht 1, keine Effekte)",
                )?;
                fallback_adjudication(&self.args, self.rules())
            }
        };
        self.apply_facilitator_rulings(&mut adjudication)?;
        let at = self.at();
        record_standing(&mut self.log, &adjudication.standing, at).map_err(matrix_err)?;
        // Runde 7, Teil M5: Konfliktpaare über `dice::resolve_conflict`,
        // Geschäftsregeln als Veto — beides im Kern (`resolve_adjudication`).
        let rules = self.rules().clone();
        let fallback = |arg: &RoundArgument| neutral_ruling(arg, &rules);
        let at = self.at();
        let resolved = resolve_adjudication(
            &mut self.log,
            &self.loaded.scenario,
            &self.args,
            &adjudication,
            &fallback,
            at,
        )
        .map_err(matrix_err)?;
        let conflicts = resolved
            .iter()
            .filter(|r| r.conflict_with.is_some())
            .count();
        if conflicts > 0 {
            self.note(
                "conflict",
                format!("{conflicts} Argument(e) über Konfliktwürfe entschieden"),
            )?;
        }
        self.flush()?;
        // Aufruf B: Erzählung.
        for call in &self.calls_for(Phase::Adjudikation, PhaseStep::Umpire) {
            if let Some((PhaseOutput::UmpireNarration(mut n), attempt)) =
                self.call_seat(driver, call, &vis, report).await?
            {
                self.guard_narration(&mut n, attempt, report)?;
                let at = self.at();
                record_narration(&mut self.log, &self.args, &n, at).map_err(matrix_err)?;
            }
        }
        Ok(())
    }

    fn guard_adjudication(
        &mut self,
        adjudication: &mut UmpireAdjudication,
        attempt: u32,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        for index in 0..adjudication.rulings.len() {
            let Some(ruling) = adjudication.rulings.get(index) else {
                continue;
            };
            let public = self
                .args
                .iter()
                .any(|a| a.id == ruling.argument_id && !a.is_secret());
            let (Some(text), true) = (ruling.public_rationale.clone(), public) else {
                continue;
            };
            let source = format!("umpire:{}:public_rationale", ruling.argument_id);
            if !self.guard_text(&source, &text, attempt, report)? {
                if let Some(ruling) = adjudication.rulings.get_mut(index) {
                    // Ein öffentliches Veto braucht eine Begründung.
                    ruling.public_rationale = (ruling.verdict == Verdict::Veto)
                        .then(|| "Begründung vom Leak-Guard zurückgehalten.".to_owned());
                }
            }
        }
        Ok(())
    }

    fn guard_narration(
        &mut self,
        narration: &mut UmpireNarration,
        attempt: u32,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        let mut keep = Vec::with_capacity(narration.narrations.len());
        for n in std::mem::take(&mut narration.narrations) {
            if n.audience.0.is_public() {
                let source = format!("umpire:{}:narration", n.argument_id);
                if !self.guard_text(&source, &n.text, attempt, report)? {
                    continue;
                }
            }
            keep.push(n);
        }
        narration.narrations = keep;
        if let Some(summary) = narration.round_summary.clone() {
            if !self.guard_text("umpire:round_summary", &summary, attempt, report)? {
                narration.round_summary = None;
            }
        }
        Ok(())
    }

    /// Wendet offene Facilitator-Overrides (vor dem Wurf) und Vetos auf das
    /// Urteil an. Ist das Ergebnis ungültig, gilt das Umpire-Urteil unverändert.
    fn apply_facilitator_rulings(
        &mut self,
        adjudication: &mut UmpireAdjudication,
    ) -> Result<(), OpError> {
        if self.pending_overrides.is_empty() && self.pending_vetoes.is_empty() {
            return Ok(());
        }
        let overrides = std::mem::take(&mut self.pending_overrides);
        let vetoes = std::mem::take(&mut self.pending_vetoes);
        let original = adjudication.clone();
        let mut notes = Vec::new();
        for ruling in &mut adjudication.rulings {
            if let Some(patch) = overrides.get(&ruling.argument_id) {
                match patch_ruling(ruling, patch) {
                    Ok(patched) => {
                        *ruling = patched;
                        notes.push(format!("Override für `{}` angewendet", ruling.argument_id));
                    }
                    Err(error) => notes.push(format!(
                        "Override für `{}` verworfen: {error}",
                        ruling.argument_id
                    )),
                }
            }
            if let Some(reason) = vetoes.get(&ruling.argument_id) {
                let secret = self
                    .args
                    .iter()
                    .any(|a| a.id == ruling.argument_id && a.is_secret());
                let text = format!("Veto durch den Facilitator: {reason}");
                ruling.verdict = Verdict::Veto;
                if secret {
                    ruling.private_notes = Some(text);
                } else {
                    ruling.public_rationale = Some(text);
                }
                notes.push(format!("Veto für `{}` angewendet", ruling.argument_id));
            }
        }
        if let Err(error) =
            validate_umpire_adjudication(adjudication, &self.args, &self.log.state, self.rules())
        {
            *adjudication = original;
            notes.push(format!(
                "Facilitator-Eingriffe ungültig, Umpire-Urteil gilt unverändert: {error}"
            ));
        }
        for note in notes {
            self.note("override", note)?;
        }
        Ok(())
    }

    async fn run_final_arguments<D: SeatDriver>(
        &mut self,
        driver: &mut D,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        let vis = self.vis();
        let calls = self.calls_for(Phase::Schlussargumente, PhaseStep::Main);
        let mut finals = Vec::new();
        for call in &calls {
            let Seat::Player(player) = &call.seat else {
                continue;
            };
            let argument = match self.call_seat(driver, call, &vis, report).await? {
                Some((PhaseOutput::PlayerArgument(argument), _)) => Some(argument),
                _ => None,
            };
            finals.push((player.clone(), argument));
        }
        let round = self.log.state.round;
        for (player, argument) in finals {
            let Some(argument) = argument else {
                self.record_forfeit(&player, Phase::Schlussargumente)?;
                continue;
            };
            let name = self.loaded.scenario.display_name(&player);
            self.record(GameEntry::new(
                round,
                Audience::Public,
                EntryKind::Narrated {
                    argument_id: Some(format!("final-{player}")),
                    text: format!(
                        "Schlussargument {name}: {} — Gründe: {}",
                        argument.action,
                        argument.pros.join(" | ")
                    ),
                },
            ))?;
        }
        Ok(())
    }

    async fn run_aar<D: SeatDriver>(
        &mut self,
        driver: &mut D,
        report: &mut StepReport,
    ) -> Result<(), OpError> {
        if !self.log.state.ended {
            let at = self.at();
            let reason = self.end_reason.clone();
            end_game(&mut self.log, &reason, at).map_err(matrix_err)?;
            self.flush()?;
        }
        let vis = self.vis();
        let mut synthesis: Option<UmpireSynthesis> = None;
        let mut debriefs: BTreeMap<PlayerId, PlayerDebrief> = BTreeMap::new();
        for call in &self.calls_for(Phase::Aar, PhaseStep::Main) {
            match (
                self.call_seat(driver, call, &vis, report).await?,
                &call.seat,
            ) {
                (Some((PhaseOutput::UmpireSynthesis(s), _)), _) => synthesis = Some(s),
                (Some((PhaseOutput::PlayerDebrief(d), _)), Seat::Player(p)) => {
                    debriefs.insert(p.clone(), d);
                }
                _ => {}
            }
        }
        let markdown = build_aar(&AarInput {
            loaded: &self.loaded,
            journal: &self.log.journal,
            state: &self.log.state,
            synthesis: synthesis.as_ref(),
            debriefs: &debriefs,
            models: None,
        })
        .map_err(matrix_err)?;
        // Runde 9, E7: jeder Verzicht mit seiner konkreten Ursache.
        let markdown = with_forfeit_causes(markdown, &self.log.journal);
        if let Some(dir) = &self.run_dir {
            let path = dir.join(AAR_FILE);
            std::fs::write(&path, &markdown)
                .map_err(|e| io_err("AAR nicht schreibbar", &path, &e))?;
        }
        self.aar = Some(markdown);
        self.synthesis = synthesis;
        self.debriefs = debriefs;
        self.status = RunStatus::Ended;
        driver.release_all();
        self.children.clear();
        // Runde 7, Teil M4: paper-tauglicher Bericht neben dem AAR.
        if let Some(dir) = self.run_dir.clone() {
            let path = dir.join(REPORT_FILE);
            let report = super::report::build_report(self);
            std::fs::write(&path, report)
                .map_err(|e| io_err("Bericht nicht schreibbar", &path, &e))?;
            self.report_path = Some(path);
        }
        Ok(())
    }

    // ── Recherche-Fakten (Plan R9) ─────────────────────────────────────────

    /// Journalisiert einen recherchierten Fakt mit Belegen als öffentliche
    /// Lage („Recherche-Inject“ bzw. Grundierung vor Runde 1).
    ///
    /// # Beschreibung
    /// Der Game Master lässt eine eng gefasste Frage vom Web-Rechercheur
    /// (`intel-web-researcher`) oder aus Repo-/Git-Belegen
    /// (`evidence-collector`) beantworten und trägt das Ergebnis hier ein. Der
    /// Eintrag ist ein [`EntryKind::FactAdded`] mit `sources`
    /// (Audience öffentlich): alle Sitze sehen ihn als „Lage: … (Quelle: …)“
    /// — die Sitze selbst bleiben offline. Vor Runde 1 (Runde 0) erscheint er
    /// in der Ausgangslage jeder Sitz-Projektion. Ein Beobachter-Vermerk
    /// `research` hält fest, dass der Fakt von außen kam.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] bei leerem Text, ohne Beleg, bei einem
    /// Beleg ohne Fundstelle oder Abrufdatum, bei zu langen Texten und nach
    /// Spielende; [`OpError::Execution`] bei Zustands- oder Schreibfehlern.
    pub fn add_research_fact(
        &mut self,
        text: &str,
        sources: Vec<FactSource>,
    ) -> Result<String, OpError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(OpError::InvalidArguments(
                "Recherche-Fakt braucht einen Text".to_owned(),
            ));
        }
        if text.chars().count() > MAX_RESEARCH_FACT_CHARS {
            return Err(OpError::InvalidArguments(format!(
                "Recherche-Fakt ist länger als {MAX_RESEARCH_FACT_CHARS} Zeichen — kürzer fassen \
                 oder in mehrere Fakten teilen"
            )));
        }
        if sources.is_empty() {
            return Err(OpError::InvalidArguments(
                "Recherche-Fakt braucht mindestens eine Quelle (url, retrieved) — Annahmen \
                 gehören ins Szenario, nicht in die Lage"
                    .to_owned(),
            ));
        }
        if sources.len() > MAX_RESEARCH_FACT_SOURCES {
            return Err(OpError::InvalidArguments(format!(
                "höchstens {MAX_RESEARCH_FACT_SOURCES} Quellen je Fakt"
            )));
        }
        let mut cleaned = Vec::with_capacity(sources.len());
        for source in sources {
            let url = source.url.trim().to_owned();
            let retrieved = source.retrieved.trim().to_owned();
            if url.is_empty() || retrieved.is_empty() {
                return Err(OpError::InvalidArguments(
                    "jede Quelle braucht `url` (URL oder Workspace-Fundstelle) und `retrieved` \
                     (Abrufdatum)"
                        .to_owned(),
                ));
            }
            if url.chars().count() > 500 || retrieved.chars().count() > 40 {
                return Err(OpError::InvalidArguments(
                    "Quelle zu lang (url höchstens 500, retrieved höchstens 40 Zeichen)".to_owned(),
                ));
            }
            cleaned.push(FactSource {
                url,
                retrieved,
                rating: source
                    .rating
                    .map(|rating| rating.trim().to_owned())
                    .filter(|rating| !rating.is_empty() && rating.chars().count() <= 20),
            });
        }
        if self.status == RunStatus::Ended {
            return Err(OpError::InvalidArguments(
                "Lauf ist beendet — keine neuen Fakten".to_owned(),
            ));
        }
        let round = self.log.state.round;
        let line = harw_matrix_game::state::sourced_fact_text(text, &cleaned);
        self.record(GameEntry::new(
            round,
            Audience::Public,
            EntryKind::FactAdded {
                text: text.to_owned(),
                sources: cleaned,
            },
        ))?;
        self.note("research", format!("Runde {round}: {line}"))?;
        self.flush()?;
        Ok(line)
    }

    // ── Facilitator ─────────────────────────────────────────────────────────

    /// `inject`: Ereignis für die nächste Phasengrenze vormerken.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] bei leerem Text oder unbekannter Audience.
    pub fn queue_inject(
        &mut self,
        text: &str,
        audience: &Audience,
        attributed: bool,
        effects: Vec<EffectOp>,
    ) -> Result<String, OpError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(OpError::InvalidArguments(
                "Inject braucht einen Text: /matrix inject <text>".to_owned(),
            ));
        }
        for p in audience.players() {
            if !self.log.state.players.contains(p) {
                return Err(OpError::InvalidArguments(format!(
                    "Audience nennt unbekannten Sitz `{p}`"
                )));
            }
        }
        self.inject_counter += 1;
        let id = format!("facilitator-{}", self.inject_counter);
        self.pending_injects.push(ScheduledInject {
            id: id.clone(),
            round: self.log.state.round,
            audiences: vec![audience.clone()],
            text: text.to_owned(),
            effects,
            attributed,
        });
        self.note("inject", format!("{id} ({audience}): {text}"))?;
        self.flush()?;
        Ok(id)
    }

    /// `override <json>`: vor dem Wurf Urteilsfelder ersetzen (wirkt in der
    /// nächsten Adjudikation), nach dem Wurf Effekte als markierte Korrektur
    /// anwenden (`{"argument_id","effects":[…],"text"?}`).
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] bei ungültigem JSON oder Argument.
    pub fn apply_override(&mut self, raw: &str) -> Result<String, OpError> {
        let value: Value = serde_json::from_str(raw).map_err(|e| {
            OpError::InvalidArguments(format!("Override ist kein gültiges JSON: {e}"))
        })?;
        let argument_id = value
            .get("argument_id")
            .and_then(Value::as_str)
            .ok_or_else(|| OpError::InvalidArguments("Override braucht `argument_id`".to_owned()))?
            .to_owned();
        if self.log.state.outcomes.contains_key(&argument_id) {
            let effects: Vec<EffectOp> = value
                .get("effects")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| OpError::InvalidArguments(format!("`effects` ungültig: {e}")))?
                .unwrap_or_default();
            if effects.is_empty() {
                return Err(OpError::InvalidArguments(format!(
                    "`{argument_id}` ist bereits gewürfelt — nach dem Wurf sind nur `effects` erlaubt"
                )));
            }
            let detail = value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("Effekte ersetzt");
            let inject = ScheduledInject {
                id: format!("override-{argument_id}"),
                round: self.log.state.round,
                audiences: vec![Audience::Public],
                text: format!("Facilitator-Korrektur zu {argument_id}: {detail}"),
                effects,
                attributed: true,
            };
            self.note("override", format!("nach dem Wurf: {raw}"))?;
            let at = self.at();
            apply_inject(&mut self.log, self.loaded.scenario.rules(), &inject, at)
                .map_err(matrix_err)?;
            self.flush()?;
            return Ok(format!(
                "Korrektur zu `{argument_id}` angewendet (nach dem Wurf, deutlich markiert)."
            ));
        }
        if !self.args.iter().any(|a| a.id == argument_id) {
            return Err(OpError::InvalidArguments(format!(
                "Argument `{argument_id}` ist in dieser Runde nicht offengelegt"
            )));
        }
        if !value.is_object() {
            return Err(OpError::InvalidArguments(
                "Override muss ein JSON-Objekt sein".to_owned(),
            ));
        }
        self.pending_overrides.insert(argument_id.clone(), value);
        self.note("override", format!("vor dem Wurf vorgemerkt: {raw}"))?;
        self.flush()?;
        Ok(format!(
            "Override für `{argument_id}` vorgemerkt — wirkt in der Adjudikation vor dem Wurf."
        ))
    }

    /// `veto <argument> [grund]`: Argument in der nächsten Adjudikation verwerfen.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`], wenn das Argument nicht offen ist.
    pub fn queue_veto(&mut self, argument_id: &str, reason: &str) -> Result<String, OpError> {
        if !self.args.iter().any(|a| a.id == argument_id)
            || self.log.state.outcomes.contains_key(argument_id)
        {
            return Err(OpError::InvalidArguments(format!(
                "Argument `{argument_id}` ist nicht offen (nur offengelegte, noch nicht gewürfelte Argumente)"
            )));
        }
        let reason = if reason.trim().is_empty() {
            "ohne Angabe von Gründen".to_owned()
        } else {
            reason.trim().to_owned()
        };
        self.pending_vetoes
            .insert(argument_id.to_owned(), reason.clone());
        self.note("veto", format!("{argument_id}: {reason}"))?;
        self.flush()?;
        Ok(format!(
            "Veto für `{argument_id}` vorgemerkt — wirkt in der Adjudikation."
        ))
    }

    /// `reveal <geheimnis|argument>`: geheimes Argument sofort offenlegen.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] bei unbekanntem Geheimnis.
    pub fn reveal(&mut self, target: &str) -> Result<String, OpError> {
        let target = target.trim().trim_start_matches('#');
        let secret_id = self
            .log
            .state
            .secrets
            .values()
            .find(|s| s.secret_id == target || s.argument_id == target)
            .map(|s| (s.secret_id.clone(), s.revealed))
            .ok_or_else(|| {
                OpError::InvalidArguments(format!("kein geheimes Argument `{target}`"))
            })?;
        if secret_id.1 {
            return Ok(format!("Geheimnis #{} ist bereits offen.", secret_id.0));
        }
        self.note("reveal", format!("#{}", secret_id.0))?;
        let at = self.at();
        reveal_secret(&mut self.log, &secret_id.0, RevealedBy::Facilitator, at)
            .map_err(matrix_err)?;
        self.flush()?;
        Ok(format!("Geheimnis #{} offengelegt.", secret_id.0))
    }

    /// `end`: direkt zu Schlussargumenten bzw. AAR springen (die folgenden
    /// Schritte führt der Aufrufer aus).
    ///
    /// # Errors
    /// [`OpError::Execution`] bei Journalfehlern.
    pub fn request_end(&mut self) -> Result<(), OpError> {
        self.note(
            "end",
            format!(
                "Runde {}, Phase {}",
                self.cursor.round,
                self.cursor.phase.label()
            ),
        )?;
        self.end_reason = "Facilitator: Ende".to_owned();
        if !matches!(self.cursor.phase, Phase::Schlussargumente | Phase::Aar) {
            self.jump = self.cursor.end_early(&self.cfg);
        }
        self.flush()
    }

    /// `fork <runde>`: neuer Lauf ab dem Rundenende `round` desselben Journals.
    /// Die Sitze des neuen Laufs starten frisch.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`], wenn die Runde nie geschlossen wurde;
    /// [`OpError::Execution`] bei Replay- oder Dateifehlern.
    pub fn fork(
        &mut self,
        round: u32,
        run_id: String,
        run_dir: Option<PathBuf>,
    ) -> Result<Self, OpError> {
        let journal = self
            .log
            .journal
            .fork_at_round_end(round)
            .map_err(|e| OpError::InvalidArguments(e.to_string()))?;
        let state = replay(&self.loaded.scenario, &journal).map_err(matrix_err)?;
        let log = GameLog { state, journal };
        let mut forked = Self::from_log(
            self.loaded.clone(),
            self.source.clone(),
            self.scenario_path.clone(),
            log,
            run_id,
            run_dir,
            self.timestamps,
            self.cfg,
        )?;
        forked.note(
            "fork",
            format!("von `{}` ab Rundenende {round}", self.run_id),
        )?;
        forked.note_materials()?;
        forked.flush()?;
        self.note(
            "fork",
            format!("→ `{}` ab Rundenende {round}", forked.run_id),
        )?;
        self.flush()?;
        Ok(forked)
    }

    /// Prüft das Journal per Replay (ohne Modellaufrufe).
    ///
    /// # Errors
    /// [`OpError::Execution`] bei `ReplayDivergence`.
    pub fn verify_replay(&self) -> Result<String, OpError> {
        let state = replay(&self.loaded.scenario, &self.log.journal).map_err(matrix_err)?;
        let replayed = state.state_hash().map_err(matrix_err)?;
        let live = self.log.state.state_hash().map_err(matrix_err)?;
        if replayed != live {
            return Err(OpError::Execution(format!(
                "ReplayDivergence: Zustand {replayed} statt {live}"
            )));
        }
        Ok(format!(
            "Replay ok: {} Journalzeilen, state_hash {}",
            self.log.journal.len(),
            replayed.get(..16).unwrap_or(&replayed)
        ))
    }

    // ── Anzeige ─────────────────────────────────────────────────────────────

    /// Daten für `/matrix show` (Form siehe Moduldoku von `super`).
    #[must_use]
    pub fn show_json(&self) -> Value {
        let scenario = &self.loaded.scenario;
        let journal = &self.log.journal;
        let seats: Vec<Value> = self
            .all_seats()
            .iter()
            .map(|seat| {
                let name = match seat {
                    Seat::Player(p) => scenario.display_name(p),
                    Seat::Umpire => "Umpire".to_owned(),
                    Seat::RedCell => "Red Cell".to_owned(),
                };
                json!({
                    "id": seat_key(seat),
                    "name": name,
                    "role": role_label(SeatRole::for_seat(&self.loaded, seat)),
                })
            })
            .collect();
        let observer: Vec<Value> = project_observer(journal)
            .entries()
            .iter()
            .map(|e| entry_json(e.round, &e.audience, &e.kind, scenario))
            .collect();
        let vis = self.vis();
        let mut views = serde_json::Map::new();
        for seat in self.all_seats() {
            let entries: Vec<Value> = project(journal, &seat, &vis)
                .entries()
                .iter()
                .map(|e| entry_json(e.round, &e.audience, &e.kind, scenario))
                .collect();
            views.insert(seat_key(&seat), Value::Array(entries));
        }
        let channels: Vec<Value> = self
            .log
            .state
            .channels
            .values()
            .map(|c| {
                let [a, b] = &c.members;
                json!({ "id": c.id, "members": [a.as_str(), b.as_str()] })
            })
            .collect();
        json!({
            "run_id": self.run_id,
            "scenario": scenario.id(),
            "round": self.cursor.round,
            "phase": self.cursor.phase.label(),
            "status": self.status.as_str(),
            "seats": seats,
            "observer": observer,
            "views": Value::Object(views),
            "channels": channels,
        })
    }

    /// Kurzbeschreibung für Textausgaben.
    #[must_use]
    pub fn headline(&self) -> String {
        format!(
            "Matrix `{}` ({}) · Runde {}/{} · Phase {} · {}",
            self.run_id,
            self.loaded.scenario.title(),
            self.cursor.round,
            self.cfg.rounds,
            self.cursor.phase.label(),
            match self.status {
                RunStatus::Running => "läuft",
                RunStatus::Paused => "pausiert",
                RunStatus::Ended => "beendet",
            }
        )
    }

    /// Master-Seed (Hex-Präfix) für Anzeigen.
    #[must_use]
    pub fn seed_prefix(&self) -> String {
        self.log
            .state
            .master_seed()
            .map(|seed| to_hex(&seed).chars().take(8).collect())
            .unwrap_or_default()
    }
}

// ── Reine Helfer ─────────────────────────────────────────────────────────────

/// Öffentliche Umpire-Texte einer Ausgabe: `(Quelle, Text)`.
fn public_texts(output: &PhaseOutput, args: &[RoundArgument]) -> Vec<(String, String)> {
    match output {
        PhaseOutput::UmpireAdjudication(a) => a
            .rulings
            .iter()
            .filter(|r| {
                args.iter()
                    .any(|arg| arg.id == r.argument_id && !arg.is_secret())
            })
            .flat_map(|r| {
                let rationale = r.public_rationale.as_ref().map(|text| {
                    (
                        format!("umpire:{}:public_rationale", r.argument_id),
                        text.clone(),
                    )
                });
                let precedent = r.precedent.as_ref().map(|flag| {
                    (
                        format!("umpire:{}:precedent", r.argument_id),
                        flag.principle.clone(),
                    )
                });
                rationale.into_iter().chain(precedent)
            })
            .collect(),
        PhaseOutput::UmpireNarration(n) => {
            let mut out: Vec<(String, String)> = n
                .narrations
                .iter()
                .filter(|x| x.audience.0.is_public())
                .map(|x| {
                    (
                        format!("umpire:{}:narration", x.argument_id),
                        x.text.clone(),
                    )
                })
                .collect();
            if let Some(summary) = &n.round_summary {
                out.push(("umpire:round_summary".to_owned(), summary.clone()));
            }
            out
        }
        _ => Vec::new(),
    }
}

fn describe_findings(findings: &[LeakFinding]) -> String {
    findings
        .iter()
        .map(LeakFinding::describe)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Ersetzt Felder eines Urteils durch die eines JSON-Patches (`argument_id`
/// bleibt).
fn patch_ruling(ruling: &UmpireRuling, patch: &Value) -> Result<UmpireRuling, String> {
    let mut base = serde_json::to_value(ruling).map_err(|e| e.to_string())?;
    let (Value::Object(target), Value::Object(fields)) = (&mut base, patch) else {
        return Err("Override muss ein JSON-Objekt sein".to_owned());
    };
    for (key, value) in fields {
        if key != "argument_id" {
            target.insert(key.clone(), value.clone());
        }
    }
    serde_json::from_value(base).map_err(|e| e.to_string())
}

/// Neutrales, immer gültiges Urteil (alle Gründe Gewicht 1, keine Effekte).
fn neutral_ruling(arg: &RoundArgument, rules: &Rules) -> UmpireRuling {
    let con_weights = if arg.is_secret() {
        BTreeMap::new()
    } else {
        arg.counters
            .iter()
            .filter(|(_, cons)| !cons.is_empty())
            .map(|(player, cons)| (player.as_str().to_owned(), vec![1u8; cons.len()]))
            .collect()
    };
    UmpireRuling {
        argument_id: arg.id.clone(),
        verdict: Verdict::Roll,
        pro_weights: vec![1u8; arg.body.pros.len()],
        con_weights,
        umpire_cons: Vec::new(),
        context_modifier: 0,
        context_reason: None,
        probability: matches!(rules.adjudication, AdjudicationSystem::EstimativeD100).then_some(50),
        inconsistent_with: None,
        public_rationale: None,
        private_notes: Some("Ersatzurteil des Runners (Umpire-Antwort ungültig).".to_owned()),
        on_success: Vec::new(),
        on_failure: Vec::new(),
        triggers_secret: None,
        precedent: None,
    }
}

fn fallback_adjudication(args: &[RoundArgument], rules: &Rules) -> UmpireAdjudication {
    UmpireAdjudication {
        rulings: args.iter().map(|a| neutral_ruling(a, rules)).collect(),
        conflicts: Vec::new(),
        standing: Vec::new(),
    }
}

/// Audience-Kategorie für die Anzeige.
#[must_use]
pub fn audience_label(audience: &Audience) -> &'static str {
    match audience {
        Audience::Public => "public",
        Audience::UmpireOnly => "umpire",
        Audience::Seat(_) => "seat",
        Audience::SeatAndUmpire(_) => "seat_and_umpire",
        Audience::Pair(..) => "pair",
        Audience::ObserverOnly => "observer",
    }
}

fn kind_tag(kind: &EntryKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default()
}

fn entry_from(kind: &EntryKind) -> Option<String> {
    match kind {
        EntryKind::Briefed { seat, .. } => Some(seat_key(seat)),
        EntryKind::ChannelOpened { initiator, .. } => Some(initiator.to_string()),
        EntryKind::NegotiationPosted { from, .. } => Some(from.to_string()),
        EntryKind::ArgumentSealed { seat, .. }
        | EntryKind::ArgumentRevealed { seat, .. }
        | EntryKind::SecretArgumentAnnounced { seat, .. }
        | EntryKind::PrivateNote { seat, .. }
        | EntryKind::CountersSubmitted { seat, .. }
        | EntryKind::Forfeit { seat, .. }
        | EntryKind::ArgumentResolved { seat, .. }
        | EntryKind::SecretRevealed { seat, .. } => Some(seat.to_string()),
        EntryKind::Adjudicated { .. }
        | EntryKind::RulingPublished { .. }
        | EntryKind::Narrated { .. }
        | EntryKind::StandingSet { .. } => Some(UMPIRE_KEY.to_owned()),
        EntryKind::FacilitatorNote { .. } => Some("facilitator".to_owned()),
        EntryKind::InjectApplied { attributed, .. } => attributed.then(|| "facilitator".to_owned()),
        EntryKind::RedCellObjection { .. } => Some(RED_CELL_KEY.to_owned()),
        EntryKind::PrecedentSet { .. } => Some(UMPIRE_KEY.to_owned()),
        _ => None,
    }
}

fn target_text(target: Option<u8>, probability_pct: u8) -> String {
    match target {
        Some(t) => format!("Ziel {t}+ ({probability_pct} %)"),
        None => format!("{probability_pct} %"),
    }
}

/// Deutsche Kurzform eines Journal-Eintrags.
#[must_use]
pub fn entry_text(kind: &EntryKind, scenario: &Scenario) -> String {
    match kind {
        EntryKind::GameCreated {
            scenario_id,
            master_seed_hex,
            ..
        } => format!(
            "Spiel angelegt: `{scenario_id}` (Seed {}…)",
            master_seed_hex.get(..8).unwrap_or(master_seed_hex)
        ),
        EntryKind::VarDeclared { var } => format!("{}: {}", var.label, var.value.display()),
        EntryKind::FactionBriefing { name, briefing, .. } => format!("{name}: {briefing}"),
        EntryKind::SecretBriefing {
            secret_goals,
            private_brief,
            ..
        } => {
            let mut text = format!("Geheime Ziele: {}", secret_goals.join("; "));
            if let Some(brief) = private_brief {
                text.push_str(&format!(" · Privat: {brief}"));
            }
            text
        }
        EntryKind::PhaseEntered { phase } => format!("Phase: {}", phase.label()),
        EntryKind::Briefed { intent, .. } => intent
            .clone()
            .unwrap_or_else(|| "Briefing bestätigt.".to_owned()),
        EntryKind::ChannelOpened { opening, .. } => opening.clone(),
        EntryKind::NegotiationPosted {
            text,
            proposal,
            accept,
            decline,
            ..
        } => {
            let mut out = text.clone();
            if let Some(p) = proposal {
                out.push_str(&format!(" [Vorschlag: {p}]"));
            }
            if let Some(a) = accept {
                out.push_str(&format!(" [Angenommen: {a}]"));
            }
            if *decline {
                out.push_str(" [Abgelehnt]");
            }
            out
        }
        EntryKind::NegotiationClosed => "Verhandlungsphase beendet.".to_owned(),
        EntryKind::ArgumentSealed {
            argument_id,
            commitment,
            ..
        } => format!("{argument_id} versiegelt [{}]", commitment.short()),
        EntryKind::ArgumentRevealed {
            argument_id,
            argument,
            ..
        } => format!(
            "{argument_id}: {} — Gründe: {}",
            argument.action,
            argument.pros.join(" | ")
        ),
        EntryKind::SecretArgumentAnnounced { text, .. }
        | EntryKind::PrivateNote { text, .. }
        | EntryKind::Forfeit { text, .. }
        | EntryKind::Narrated { text, .. }
        | EntryKind::InjectApplied { text, .. }
        | EntryKind::SuspicionRaised { text, .. } => text.clone(),
        // Plan R9: ein recherchierter Fakt nennt seine Belege
        // („Fakt (Quelle: URL, abgerufen …)“ im Bericht).
        EntryKind::FactAdded { text, sources } => {
            harw_matrix_game::state::sourced_fact_text(text, sources)
        }
        EntryKind::BehaviorBriefing { faction, profile } => format!(
            "Verhaltensprofil {}: {} Regel(n), Risiko {:.1}, {} rote Linie(n)",
            scenario.display_name(faction),
            profile.rules.len(),
            profile.risk,
            profile.red_lines.len()
        ),
        EntryKind::RedCellObjection {
            target,
            assumption,
            cons,
        } => match target {
            Some(target) => format!(
                "Red Cell gegen {target}: Annahme „{}“ — {}",
                assumption.as_deref().unwrap_or("?"),
                cons.join(" | ")
            ),
            None => "Red Cell: kein Einwand.".to_owned(),
        },
        EntryKind::PrecedentSet { precedent } => format!(
            "Präzedenzfall {} ({}): {}",
            precedent.id, precedent.argument_id, precedent.principle
        ),
        EntryKind::InjectPackageSelected { package_id, plan } => {
            let parts: Vec<String> = plan
                .iter()
                .map(|p| format!("{} (Runde {})", p.inject_id, p.round))
                .collect();
            format!("Inject-Paket `{package_id}` gewählt: {}", parts.join(", "))
        }
        EntryKind::CountersSubmitted { counters, .. } => {
            let parts: Vec<String> = counters
                .iter()
                .filter(|c| !c.cons.is_empty())
                .map(|c| format!("{}: {}", c.argument_id, c.cons.join(" | ")))
                .collect();
            if parts.is_empty() {
                "Keine Gegenargumente.".to_owned()
            } else {
                format!("Gegenargumente — {}", parts.join("; "))
            }
        }
        EntryKind::Adjudicated {
            argument_id,
            ruling,
            net,
            target,
            probability_pct,
        } => {
            let mut text = format!(
                "{argument_id}: {:?}, Netto {net:+}, {}",
                ruling.verdict,
                target_text(*target, *probability_pct)
            );
            if let Some(notes) = &ruling.private_notes {
                text.push_str(&format!(" — {notes}"));
            }
            text
        }
        EntryKind::RulingPublished {
            argument_id,
            net,
            target,
            probability_pct,
            rationale,
            ..
        } => {
            let mut text = format!(
                "{argument_id}: Netto {net:+} → {}",
                target_text(*target, *probability_pct)
            );
            if let Some(r) = rationale {
                text.push_str(&format!(" — {r}"));
            }
            text
        }
        EntryKind::DiceRolled { roll } => {
            let dice: Vec<String> = roll.dice.iter().map(u8::to_string).collect();
            let label = if roll.kind == harw_matrix_game::dice::RollKind::Conflict {
                "Konfliktwurf"
            } else {
                "Wurf"
            };
            format!(
                "{label} {} (Versuch {}): {} = {} gegen {} → {}",
                roll.argument_id,
                roll.attempt,
                dice.join("+"),
                roll.total,
                roll.target,
                if roll.success { "Erfolg" } else { "Misserfolg" }
            )
        }
        EntryKind::ArgumentResolved {
            argument_id,
            outcome,
            ..
        } => format!("{argument_id}: {outcome:?}"),
        EntryKind::WorldDelta {
            var,
            from,
            to,
            cause,
        } => format!("{var}: {} → {} ({cause})", from.display(), to.display()),
        EntryKind::OngoingStarted { ongoing } => format!("Fortwirkend: {}", ongoing.text),
        EntryKind::OngoingStopped { id } => format!("Fortwirkender Effekt `{id}` beendet."),
        EntryKind::EffectRejected {
            argument_id,
            op_index,
            reason,
        } => format!("Effekt verworfen ({argument_id}[{op_index}]): {reason}"),
        EntryKind::SecretRevealed {
            secret_id,
            argument_id,
            seat,
            content,
            ..
        } => format!(
            "Geheimnis #{secret_id} ({argument_id}, {}) offengelegt: {}",
            scenario.display_name(seat),
            content.action
        ),
        EntryKind::LeakSuspect {
            source, findings, ..
        } => format!("Leak-Verdacht ({source}): {}", findings.join("; ")),
        EntryKind::StandingSet { order } => {
            let names: Vec<String> = order.iter().map(PlayerId::to_string).collect();
            format!("Einschätzung: {}", names.join(" > "))
        }
        EntryKind::RoundClosed { state_hash } => format!(
            "Runde geschlossen (state_hash {}…)",
            state_hash.get(..12).unwrap_or(state_hash)
        ),
        EntryKind::FacilitatorNote { command, detail } => format!("{command}: {detail}"),
        EntryKind::GameEnded { reason } => format!("Spielende: {reason}"),
    }
}

/// Anzeige-Eintrag `{"round","kind","audience","from","text"}`.
#[must_use]
pub fn entry_json(round: u32, audience: &Audience, kind: &EntryKind, scenario: &Scenario) -> Value {
    json!({
        "round": round,
        "kind": kind_tag(kind),
        "audience": audience_label(audience),
        "from": entry_from(kind),
        "text": entry_text(kind, scenario),
    })
}

// ── Unterlagen ───────────────────────────────────────────────────────────────

/// Ergebnis des Kopierens der Unterlagen eines Sitzes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterialsReport {
    /// Kopierte Dateien.
    pub copied: usize,
    /// Kopierte Bytes.
    pub bytes: u64,
    /// Übersprungene Einträge mit Grund.
    pub skipped: Vec<String>,
}

/// Ein einzelner, harmloser Pfadbestandteil (kein Trenner, kein `.`/`..`).
fn safe_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// Gültige Paarordner `paare/<a>+<b>/` (keine Symlinks), sortiert.
fn pair_folders(src_root: &Path) -> Vec<(String, String, String)> {
    let Ok(entries) = std::fs::read_dir(src_root.join("paare")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some((a, b)) = pair_folder_members(&name) {
            out.push((name, a, b));
        }
    }
    out.sort();
    out
}

/// Kopierplan eines Sitzes: `(Quelle relativ zu dir, Ziel relativ zur Kopie)`
/// gemäß [`materials_for_seat`].
#[must_use]
pub fn materials_plan(
    src_root: &Path,
    players: &[PlayerId],
    seat: &Seat,
) -> Vec<(PathBuf, PathBuf)> {
    let selection = materials_for_seat(seat);
    let pairs = pair_folders(src_root);
    let mut plan = Vec::new();
    if selection.shared {
        plan.push((PathBuf::from("geteilt"), PathBuf::from("geteilt")));
    }
    if let Some(own) = selection.own.as_deref().filter(|own| safe_component(own)) {
        plan.push((PathBuf::from(own), PathBuf::from("eigene")));
    }
    if selection.pairs_with {
        if let Seat::Player(me) = seat {
            for (name, a, b) in &pairs {
                let partner = if a == me.as_str() {
                    b
                } else if b == me.as_str() {
                    a
                } else {
                    continue;
                };
                plan.push((
                    Path::new("paare").join(name),
                    PathBuf::from(format!("mit-{partner}")),
                ));
            }
        }
    }
    if selection.all_seats {
        for player in players.iter().filter(|p| safe_component(p.as_str())) {
            plan.push((
                PathBuf::from(player.as_str()),
                Path::new("sitze").join(player.as_str()),
            ));
        }
    }
    if selection.all_pairs {
        for (name, _, _) in &pairs {
            plan.push((Path::new("paare").join(name), Path::new("paare").join(name)));
        }
    }
    if selection.umpire {
        plan.push((PathBuf::from("umpire"), PathBuf::from("schiedsrichter")));
    }
    plan
}

/// Legt die Unterlagen-Kopie eines Sitzes unter `dest` an. Ohne Quelle
/// entsteht ein leerer Ordner. Kopiert werden nur reguläre Dateien
/// (≤ [`MAX_MATERIAL_FILE_BYTES`], zusammen ≤ [`MAX_MATERIAL_TOTAL_BYTES`]);
/// symbolische Links, Sonderdateien und alles, was kanonisiert außerhalb
/// der Quelle liegt, wird übersprungen.
///
/// # Errors
/// Deutsche Fehlerbeschreibung bei Ein-/Ausgabefehlern.
pub fn build_seat_materials(
    source: Option<&Path>,
    players: &[PlayerId],
    seat: &Seat,
    dest: &Path,
) -> Result<MaterialsReport, String> {
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("Unterlagen-Kopie `{}` nicht anlegbar: {e}", dest.display()))?;
    let mut report = MaterialsReport::default();
    let Some(source) = source else {
        return Ok(report);
    };
    let root = source
        .canonicalize()
        .map_err(|e| format!("Unterlagen-Ordner `{}` nicht lesbar: {e}", source.display()))?;
    for (src_rel, dest_rel) in materials_plan(&root, players, seat) {
        let src = root.join(&src_rel);
        let Ok(meta) = std::fs::symlink_metadata(&src) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            report
                .skipped
                .push(format!("{}: symbolischer Link", src_rel.display()));
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        let canonical = src
            .canonicalize()
            .map_err(|e| format!("`{}` nicht lesbar: {e}", src.display()))?;
        if !canonical.starts_with(&root) {
            report.skipped.push(format!(
                "{}: außerhalb des Unterlagen-Ordners",
                src_rel.display()
            ));
            continue;
        }
        let target = dest.join(&dest_rel);
        std::fs::create_dir_all(&target)
            .map_err(|e| format!("`{}` nicht anlegbar: {e}", target.display()))?;
        copy_tree(&root, &canonical, &target, &src_rel, 0, &mut report)?;
    }
    Ok(report)
}

fn copy_tree(
    root: &Path,
    dir: &Path,
    target: &Path,
    label: &Path,
    depth: usize,
    report: &mut MaterialsReport,
) -> Result<(), String> {
    if depth >= MAX_MATERIAL_DEPTH {
        report
            .skipped
            .push(format!("{}: zu tief verschachtelt", label.display()));
        return Ok(());
    }
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)
        .map_err(|e| format!("`{}` nicht lesbar: {e}", dir.display()))?
        .flatten()
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let path = entry.path();
        let shown = label.join(&name);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let kind = meta.file_type();
        if kind.is_symlink() {
            report
                .skipped
                .push(format!("{}: symbolischer Link", shown.display()));
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(root) {
            report.skipped.push(format!(
                "{}: außerhalb des Unterlagen-Ordners",
                shown.display()
            ));
            continue;
        }
        let out = target.join(&name);
        if kind.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| format!("`{}` nicht anlegbar: {e}", out.display()))?;
            copy_tree(root, &canonical, &out, &shown, depth + 1, report)?;
        } else if kind.is_file() {
            copy_limited(&canonical, &out, meta.len(), &shown, report)?;
        } else {
            report
                .skipped
                .push(format!("{}: keine reguläre Datei", shown.display()));
        }
    }
    Ok(())
}

fn copy_limited(
    src: &Path,
    out: &Path,
    len: u64,
    shown: &Path,
    report: &mut MaterialsReport,
) -> Result<(), String> {
    if len > MAX_MATERIAL_FILE_BYTES {
        report
            .skipped
            .push(format!("{}: größer als 5 MiB", shown.display()));
        return Ok(());
    }
    if report.bytes.saturating_add(len) > MAX_MATERIAL_TOTAL_BYTES {
        report
            .skipped
            .push(format!("{}: Gesamtgrenze 50 MiB erreicht", shown.display()));
        return Ok(());
    }
    let file =
        std::fs::File::open(src).map_err(|e| format!("`{}` nicht lesbar: {e}", src.display()))?;
    let mut limited = std::io::Read::take(file, MAX_MATERIAL_FILE_BYTES + 1);
    let mut sink = std::fs::File::create(out)
        .map_err(|e| format!("`{}` nicht schreibbar: {e}", out.display()))?;
    let copied = std::io::copy(&mut limited, &mut sink)
        .map_err(|e| format!("`{}` nicht kopierbar: {e}", src.display()))?;
    drop(sink);
    // Die Datei ist zwischen Prüfung und Kopie gewachsen: verwerfen.
    if copied > MAX_MATERIAL_FILE_BYTES
        || report.bytes.saturating_add(copied) > MAX_MATERIAL_TOTAL_BYTES
    {
        let _ = std::fs::remove_file(out);
        report.skipped.push(format!(
            "{}: Grenze beim Kopieren überschritten",
            shown.display()
        ));
        return Ok(());
    }
    report.copied += 1;
    report.bytes += copied;
    Ok(())
}

/// Sandbox eines Sitzes: Workspace = seine Unterlagen-Kopie, Rechte =
/// `{ReadWorkspace}` ∩ Parent-Rechte, kein Netz, Ursprung
/// `AuthorityOrigin::HarnessReadView` (über
/// [`SandboxSpec::harness_read_view`]). Nur dieser Ursprung besteht die
/// Admission (`ensure_child_of`) trotz abweichendem Workspace.
///
/// Die Lesesicht darf nur auf eine vom Runner selbst angelegte
/// Unterlagen-Kopie zeigen: `seat_dir` muss ein echtes Verzeichnis (kein
/// Symlink) direkt unter einem Ordner [`MATERIALS_DIR`] sein, und sein Name
/// muss ein sicherer Pfadbestandteil sein. So kann ein fehlerhafter Aufrufer
/// keine beliebigen Verzeichnisse als harness-eigene Sicht freigeben.
///
/// # Errors
/// Deutsche Fehlerbeschreibung, wenn die Kopie nicht bindbar ist, nicht wie
/// eine Unterlagen-Kopie aussieht oder der Parent kein `ReadWorkspace` hält.
pub fn seat_sandbox(
    parent: &SandboxSpec,
    seat_dir: &Path,
    workspace: &str,
) -> Result<SandboxSpec, String> {
    let is_copy = seat_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(safe_component)
        && seat_dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == MATERIALS_DIR)
        && std::fs::symlink_metadata(seat_dir).is_ok_and(|meta| meta.file_type().is_dir());
    if !is_copy {
        return Err(format!(
            "`{}` ist keine Unterlagen-Kopie des Runners (<lauf>/{MATERIALS_DIR}/<sitz>/)",
            seat_dir.display()
        ));
    }
    let tenant = parent.workspace().tenant().clone();
    let workspace = WorkspaceId::from_str(workspace);
    let registry = WorkspaceRegistry::build(
        seat_dir,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: seat_dir.to_path_buf(),
        }],
    )
    .map_err(|e| e.to_string())?;
    let binding = registry
        .resolve(&tenant, &workspace)
        .map_err(|e| e.to_string())?;
    SandboxSpec::harness_read_view(parent, binding)
        .map_err(|e| format!("Lesesicht auf die Unterlagen abgewiesen: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_matrix_game::scenario::load_scenario;

    // Runde 9, E7: echter `SpawnerDriver`-Pfad in der Kette UIA → Game
    // Master (Kind) → Sitze (`runner/tests/spawner_chain.rs`).
    mod spawner_chain;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const KARST: &str = include_str!("../../../harw-matrix-game/scenarios/karst-islands.toml");

    /// Geskripteter Treiber: minimal gültige Antworten je Contract, ohne Kinder.
    struct ScriptedDriver {
        asked: Vec<String>,
        fail_seat: Option<String>,
    }

    impl ScriptedDriver {
        fn new() -> Self {
            Self {
                asked: Vec::new(),
                fail_seat: None,
            }
        }
    }

    fn scripted_answer(prompt: &str, seat: &str) -> String {
        // Turn- und Reparatur-Prompt enden mit „… nach Vertrag `<name>`.“
        let contract = prompt
            .rsplit_once("nach Vertrag `")
            .and_then(|(_, rest)| rest.split('`').next())
            .unwrap_or_default();
        match contract {
            "briefing_ack" => r#"{"ack":true,"intent":"Wir halten Kurs."}"#.to_owned(),
            "negotiation_request" => r#"{"requests":[]}"#.to_owned(),
            "negotiation_message" => r#"{"messages":[]}"#.to_owned(),
            "player_argument" => format!(
                r#"{{"action":"Die Fraktion {seat} verstärkt ihre Präsenz am Hafen.","pros":["Sie hat Leute vor Ort.","Die Lage verlangt Handeln."]}}"#
            ),
            "counter_argument" => r#"{"counters":[]}"#.to_owned(),
            "umpire_narration" => {
                r#"{"narrations":[],"round_summary":"Eine ruhige Runde."}"#.to_owned()
            }
            // Adjudikation: absichtlich ungültig → Ersatzurteil greift.
            _ => "kein JSON".to_owned(),
        }
    }

    impl SeatDriver for ScriptedDriver {
        fn ask<'a>(&'a mut self, request: SeatRequest<'a>) -> SeatFuture<'a> {
            self.asked.push(request.seat_key.to_owned());
            let fail = self.fail_seat.as_deref() == Some(request.seat_key);
            let answer = scripted_answer(&request.prompt, request.seat_key);
            Box::pin(async move {
                if fail {
                    Err("absichtlicher Fehler".to_owned())
                } else {
                    Ok(answer)
                }
            })
        }
    }

    fn run() -> Result<MatrixRun, Box<dyn std::error::Error>> {
        let loaded = load_scenario(KARST)?;
        let seed = master_seed_for(&loaded, Some(7));
        Ok(MatrixRun::start(
            loaded,
            KARST.to_owned(),
            None,
            seed,
            "test-run".to_owned(),
            None,
            false,
            None,
        )?)
    }

    #[test]
    fn master_seed_is_deterministic() -> TestResult {
        let loaded = load_scenario(KARST)?;
        assert_eq!(
            master_seed_for(&loaded, Some(7)),
            master_seed_for(&loaded, Some(7))
        );
        assert_ne!(
            master_seed_for(&loaded, Some(7)),
            master_seed_for(&loaded, Some(8))
        );
        // Karst hat keinen Seed: Rückfall auf den Quelltext-Hash, stabil.
        assert_eq!(
            master_seed_for(&loaded, None),
            master_seed_for(&loaded, None)
        );
        Ok(())
    }

    #[test]
    fn show_json_has_contract_shape() -> TestResult {
        let run = run()?;
        let data = run.show_json();
        for key in [
            "run_id", "scenario", "round", "phase", "status", "seats", "observer", "views",
            "channels",
        ] {
            assert!(data.get(key).is_some(), "Feld `{key}` fehlt");
        }
        let seats = data["seats"].as_array().ok_or("seats")?;
        assert_eq!(seats.len(), 5, "4 Spieler + Umpire");
        assert!(
            seats
                .iter()
                .any(|s| s["id"] == "umpire" && s["role"] == "umpire")
        );
        let views = data["views"].as_object().ok_or("views")?;
        assert!(views.contains_key("rat") && views.contains_key("umpire"));
        let observer = data["observer"].as_array().ok_or("observer")?;
        let first = observer.first().ok_or("observer leer")?;
        for key in ["round", "kind", "audience", "from", "text"] {
            assert!(first.get(key).is_some(), "E-Feld `{key}` fehlt");
        }
        assert_eq!(first["kind"], "game_created");
        assert_eq!(first["audience"], "observer");
        // Der Beobachter sieht mehr als jeder Sitz (Seed, geheime Briefings).
        let rat = views["rat"].as_array().ok_or("rat")?;
        assert!(observer.len() > rat.len());
        assert!(
            !rat.iter().any(|e| e["audience"] == "observer"),
            "Sitz-Sicht ohne Beobachter-Einträge"
        );
        Ok(())
    }

    #[tokio::test]
    async fn scripted_round_runs_with_fallback_adjudication() -> TestResult {
        let mut run = run()?;
        let mut driver = ScriptedDriver::new();
        // Runde 1 bis einschließlich Rundenende.
        let mut phases = Vec::new();
        loop {
            let report = run.step_with(&mut driver).await?;
            phases.push(report.phase);
            if report.phase == Phase::Rundenende {
                break;
            }
            assert!(phases.len() < 20, "FSM läuft nicht weiter");
        }
        assert_eq!(phases.first(), Some(&Phase::Briefing));
        assert!(phases.contains(&Phase::Adjudikation));
        // Jedes Argument ist entschieden (Ersatzurteil nach ungültigem Umpire).
        assert_eq!(run.log().state.outcomes.len(), 4);
        let notes = run
            .log()
            .journal
            .entries()
            .filter(|e| matches!(&e.kind, EntryKind::FacilitatorNote { command, .. } if command == "runner"))
            .count();
        assert_eq!(notes, 1, "genau ein Ersatzurteil");
        // Replay reproduziert den Zustand.
        run.verify_replay()?;
        Ok(())
    }

    /// Treiber, dessen Sitze nie antworten (z. B. eine nie beantwortete
    /// Freigabe im Kind) — merkt sich abgebrochene Sitze.
    #[derive(Default)]
    struct HangingDriver {
        asked: usize,
        abandoned: Vec<String>,
    }

    impl SeatDriver for HangingDriver {
        fn ask<'a>(&'a mut self, _request: SeatRequest<'a>) -> SeatFuture<'a> {
            self.asked += 1;
            Box::pin(std::future::pending())
        }

        fn abandon(&mut self, seat_key: &str) {
            self.abandoned.push(seat_key.to_owned());
        }
    }

    /// Runde 7, Teil M: ein hängender Sitz blockiert den Lauf nicht — nach
    /// dem Zeitlimit passt er, sein Kind wird abgebrochen, die Phase endet.
    #[tokio::test]
    async fn hanging_seat_times_out_and_forfeits() -> TestResult {
        let mut run = run()?;
        run.set_seat_timeout(Duration::from_millis(20));
        let mut driver = HangingDriver::default();
        let started = std::time::Instant::now();
        let briefing = run.step_with(&mut driver).await?;
        assert_eq!(briefing.phase, Phase::Briefing);
        assert_eq!(briefing.forfeits.len(), driver.asked, "{briefing:?}");
        assert_eq!(driver.abandoned.len(), driver.asked);
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(run.log().journal.entries().any(|e| matches!(
            &e.kind,
            EntryKind::FacilitatorNote { detail, .. } if detail.contains("Zeitlimit")
        )));
        Ok(())
    }

    /// Runde 7, Teil M: ein abgebrochener Lauf fragt keinen Sitz mehr und
    /// bricht einen laufenden Aufruf sofort ab.
    #[tokio::test]
    async fn cancelled_run_asks_no_seat() -> TestResult {
        let mut run = run()?;
        let cancel = CancelToken::new();
        cancel.cancel(harw_types::cancel::CancelReason::User);
        run.set_cancel(Some(cancel));
        assert!(run.is_cancelled());
        let mut driver = HangingDriver::default();
        let briefing = run.step_with(&mut driver).await?;
        assert_eq!(driver.asked, 0);
        assert!(!briefing.forfeits.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn failing_seat_forfeits_and_game_continues() -> TestResult {
        let mut run = run()?;
        let mut driver = ScriptedDriver::new();
        driver.fail_seat = Some("gilde".to_owned());
        let briefing = run.step_with(&mut driver).await?;
        assert_eq!(briefing.phase, Phase::Briefing);
        assert_eq!(briefing.forfeits, vec!["gilde".to_owned()]);
        Ok(())
    }

    /// Treiber, dessen Sitze alle an der Admission scheitern — wie im
    /// Live-Lauf mit dem Game Master als Kind der UIA.
    struct SpawnFailingDriver {
        asked: usize,
    }

    /// Fehlertext aus dem Live-Lauf (Runde 9, E7).
    const SPAWN_ERROR: &str = "Spawn fehlgeschlagen: unknown child parent: 891e693a";

    impl SeatDriver for SpawnFailingDriver {
        fn ask<'a>(&'a mut self, _request: SeatRequest<'a>) -> SeatFuture<'a> {
            self.asked += 1;
            Box::pin(async { Err(SPAWN_ERROR.to_owned()) })
        }
    }

    /// Runde 9, E7: scheitert jeder Sitz einer Phase technisch, pausiert der
    /// Lauf nach genau dieser Phase; Zusammenfassung, AAR und Bericht nennen
    /// die konkrete Ursache (entdoppelt), nicht nur „gepasst“.
    #[tokio::test]
    async fn all_technical_forfeits_pause_the_run_and_name_the_cause() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let loaded = load_scenario(KARST)?;
        let seed = master_seed_for(&loaded, Some(7));
        let mut run = MatrixRun::start(
            loaded,
            KARST.to_owned(),
            None,
            seed,
            "technical-stop".to_owned(),
            Some(tmp.path().join("lauf")),
            false,
            None,
        )?;
        let mut driver = SpawnFailingDriver { asked: 0 };
        let briefing = run.step_with(&mut driver).await?;
        assert_eq!(briefing.phase, Phase::Briefing);
        assert_eq!(driver.asked, 5, "4 Spieler + Umpire");
        assert_eq!(run.status(), RunStatus::Paused, "Lauf pausiert sofort");
        let causes = briefing
            .technical_stop
            .as_deref()
            .ok_or("technischer Abbruch fehlt")?;
        assert_eq!(causes, SPAWN_ERROR, "Ursache genau einmal");
        let summary = briefing.summary();
        assert!(summary.contains(&format!("({SPAWN_ERROR})")), "{summary}");
        for seat in ["rat", "gilde", "nord", "mission", "umpire"] {
            assert!(summary.contains(seat), "{seat} fehlt: {summary}");
        }
        assert_eq!(
            summary.matches(SPAWN_ERROR).count(),
            1,
            "Ursache entdoppelt: {summary}"
        );
        assert!(summary.contains("technischer Abbruch"), "{summary}");
        assert!(run.log().journal.entries().any(|entry| matches!(
            &entry.kind,
            EntryKind::FacilitatorNote { command, detail }
                if command == TECHNICAL_STOP_NOTE && detail.contains(SPAWN_ERROR)
        )));

        // Ein einzelner technischer Fehler neben gültigen Antworten hält den
        // Lauf nicht an (siehe `failing_seat_forfeits_and_game_continues`);
        // hier endet der Lauf regulär über das AAR.
        run.request_end()?;
        for _ in 0..3 {
            if run.status() == RunStatus::Ended {
                break;
            }
            run.step_with(&mut driver).await?;
        }
        assert_eq!(run.status(), RunStatus::Ended);
        let aar = run.aar().ok_or("AAR fehlt")?;
        assert!(aar.contains("## Verzichte und ihre Ursachen"), "{aar}");
        assert!(aar.contains(SPAWN_ERROR), "{aar}");
        let report_path = run.report_path().ok_or("report.md fehlt")?;
        let report = std::fs::read_to_string(report_path)?;
        assert!(report.contains("Technischer Abbruch im Lauf"), "{report}");
        assert!(report.contains(SPAWN_ERROR), "{report}");
        assert!(report.contains("technischer Fehler"), "{report}");
        Ok(())
    }

    /// Runde 9, E7: ungültige Antworten sind Spielzüge, kein technischer
    /// Fehler — auch wenn alle Sitze so passen, läuft das Spiel weiter.
    #[tokio::test]
    async fn invalid_answers_do_not_trigger_a_technical_stop() -> TestResult {
        struct GarbageDriver;
        impl SeatDriver for GarbageDriver {
            fn ask<'a>(&'a mut self, _request: SeatRequest<'a>) -> SeatFuture<'a> {
                Box::pin(async { Ok("kein JSON".to_owned()) })
            }
        }
        let mut run = run()?;
        let briefing = run.step_with(&mut GarbageDriver).await?;
        assert_eq!(briefing.forfeits.len(), briefing.calls);
        assert!(briefing.technical_stop.is_none());
        assert_eq!(run.status(), RunStatus::Running);
        assert!(
            briefing.summary().contains(INVALID_ANSWER_CAUSE),
            "{}",
            briefing.summary()
        );
        Ok(())
    }

    #[tokio::test]
    async fn end_writes_aar_without_agents() -> TestResult {
        let mut run = run()?;
        run.request_end()?;
        let mut driver = NullDriver;
        let report = run.step_with(&mut driver).await?;
        assert_eq!(report.phase, Phase::Aar);
        assert!(report.ended);
        assert_eq!(run.status(), RunStatus::Ended);
        assert!(
            run.aar()
                .is_some_and(|md| md.contains("After-Action-Review"))
        );
        assert!(run.step_with(&mut driver).await.is_err());
        Ok(())
    }

    #[test]
    fn inject_is_queued_and_noted() -> TestResult {
        let mut run = run()?;
        let id = run.queue_inject("Sturmflut im Hafen", &Audience::Public, false, Vec::new())?;
        assert_eq!(id, "facilitator-1");
        assert!(
            run.queue_inject("  ", &Audience::Public, false, Vec::new())
                .is_err()
        );
        assert!(
            run.queue_inject(
                "x",
                &Audience::Seat(PlayerId::new("niemand")),
                false,
                Vec::new()
            )
            .is_err()
        );
        let last = run.log().journal.entries().last().ok_or("leer")?;
        assert!(
            matches!(&last.kind, EntryKind::FacilitatorNote { command, .. } if command == "inject")
        );
        assert_eq!(last.audience, Audience::ObserverOnly);
        Ok(())
    }

    #[test]
    fn override_and_veto_need_open_arguments() -> TestResult {
        let mut run = run()?;
        assert!(run.apply_override(r#"{"argument_id":"r1-a1"}"#).is_err());
        assert!(run.apply_override("kein json").is_err());
        assert!(run.queue_veto("r1-a1", "zu vage").is_err());
        assert!(run.reveal("s1").is_err());
        Ok(())
    }

    #[test]
    fn patch_ruling_replaces_fields_but_keeps_id() -> TestResult {
        let loaded = load_scenario(KARST)?;
        let arg = RoundArgument {
            id: "r1-a1".to_owned(),
            round: 1,
            seat: PlayerId::new("rat"),
            body: harw_matrix_game::phases::ArgumentBody {
                action: "a".to_owned(),
                pros: vec!["p".to_owned()],
                cites_negotiation: Vec::new(),
                conflict_target: None,
                project: None,
                use_fail_chit_if_failed: false,
            },
            secret_id: None,
            counters: BTreeMap::new(),
        };
        let ruling = neutral_ruling(&arg, loaded.scenario.rules());
        let patched = patch_ruling(
            &ruling,
            &json!({"argument_id": "anders", "context_modifier": 2, "context_reason": "Präzedenz"}),
        )?;
        assert_eq!(patched.argument_id, "r1-a1");
        assert_eq!(patched.context_modifier, 2);
        assert!(patch_ruling(&ruling, &json!({"unbekannt": 1})).is_err());
        Ok(())
    }

    // ── Unterlagen ──────────────────────────────────────────────────────────

    fn write(path: &Path, bytes: &[u8]) -> TestResult {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    fn players() -> Vec<PlayerId> {
        ["a", "b", "c"].into_iter().map(PlayerId::new).collect()
    }

    /// Quelle: geteilt/, a/, b/, c/, paare/a+b/, umpire/.
    fn materials_source(root: &Path) -> TestResult {
        write(&root.join("geteilt/lage.txt"), b"fuer alle")?;
        write(&root.join("a/plan.txt"), b"nur a")?;
        write(&root.join("b/plan.txt"), b"nur b")?;
        write(&root.join("c/plan.txt"), b"nur c")?;
        write(&root.join("paare/a+b/deal.txt"), b"a und b")?;
        write(&root.join("umpire/notiz.txt"), b"nur umpire")?;
        Ok(())
    }

    fn files_under(root: &Path) -> Vec<String> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else if let Ok(rel) = path.strip_prefix(base) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    #[test]
    fn materials_layout_per_seat_and_privacy() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let src = tmp.path().join("src");
        materials_source(&src)?;
        let dest = tmp.path().join("dest");
        let players = players();
        for seat in [
            Seat::player("a"),
            Seat::player("b"),
            Seat::player("c"),
            Seat::Umpire,
        ] {
            build_seat_materials(Some(&src), &players, &seat, &dest.join(seat_key(&seat)))?;
        }
        assert_eq!(
            files_under(&dest.join("a")),
            vec!["eigene/plan.txt", "geteilt/lage.txt", "mit-b/deal.txt"]
        );
        assert_eq!(
            files_under(&dest.join("b")),
            vec!["eigene/plan.txt", "geteilt/lage.txt", "mit-a/deal.txt"]
        );
        // C ist nicht im Paar a+b und sieht keine fremden Sitz-Ordner.
        assert_eq!(
            files_under(&dest.join("c")),
            vec!["eigene/plan.txt", "geteilt/lage.txt"]
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("a/eigene/plan.txt"))?,
            "nur a",
            "A bekommt nie B's Datei"
        );
        assert_eq!(
            files_under(&dest.join("umpire")),
            vec![
                "geteilt/lage.txt",
                "paare/a+b/deal.txt",
                "schiedsrichter/notiz.txt",
                "sitze/a/plan.txt",
                "sitze/b/plan.txt",
                "sitze/c/plan.txt",
            ]
        );
        Ok(())
    }

    fn parent_sandbox(root: &Path) -> Result<SandboxSpec, Box<dyn std::error::Error>> {
        let tenant = harw_types::TenantId::from_str("t");
        let workspace = WorkspaceId::from_str("ws");
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: root.to_path_buf(),
            }],
        )?;
        Ok(SandboxSpec::from_resolved(
            registry.resolve(&tenant, &workspace)?,
            harw_authority::PermissionSet::from_policy([harw_authority::Permission::ReadWorkspace]),
        ))
    }

    #[test]
    fn seat_sandbox_only_accepts_runner_materials_copies() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let parent = parent_sandbox(tmp.path())?;
        let copy = tmp.path().join(MATERIALS_DIR).join("a");
        std::fs::create_dir_all(&copy)?;
        let view = seat_sandbox(&parent, &copy, "matrix-test-a")?;
        assert!(
            view.permissions()
                .contains(harw_authority::Permission::ReadWorkspace)
        );
        assert!(view.network_scope().is_empty());

        let elsewhere = tmp.path().join("beliebig");
        std::fs::create_dir_all(&elsewhere)?;
        assert!(seat_sandbox(&parent, &elsewhere, "matrix-test-x").is_err());
        let missing = tmp.path().join(MATERIALS_DIR).join("fehlt");
        assert!(seat_sandbox(&parent, &missing, "matrix-test-y").is_err());
        Ok(())
    }

    #[test]
    fn materials_without_source_is_an_empty_dir() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let dest = tmp.path().join("leer");
        let report = build_seat_materials(None, &players(), &Seat::player("a"), &dest)?;
        assert_eq!(report, MaterialsReport::default());
        assert!(dest.is_dir());
        assert!(files_under(&dest).is_empty());
        Ok(())
    }

    #[test]
    fn materials_skip_oversize_files() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let src = tmp.path().join("src");
        let big = usize::try_from(MAX_MATERIAL_FILE_BYTES + 1)?;
        write(&src.join("geteilt/gross.bin"), &vec![0u8; big])?;
        write(&src.join("geteilt/klein.txt"), b"ok")?;
        let dest = tmp.path().join("dest");
        let report = build_seat_materials(Some(&src), &players(), &Seat::player("a"), &dest)?;
        assert_eq!(files_under(&dest), vec!["geteilt/klein.txt"]);
        assert!(report.skipped.iter().any(|s| s.contains("gross.bin")));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn materials_skip_symlinks_and_refuse_traversal() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let src = tmp.path().join("src");
        let outside = tmp.path().join("geheim");
        write(&outside.join("passwort.txt"), b"streng geheim")?;
        write(&src.join("geteilt/lage.txt"), b"ok")?;
        std::os::unix::fs::symlink(outside.join("passwort.txt"), src.join("geteilt/link.txt"))?;
        std::os::unix::fs::symlink(&outside, src.join("geteilt/ordnerlink"))?;
        // Der eigene Ordner von `a` ist selbst ein Link nach draußen.
        std::os::unix::fs::symlink(&outside, src.join("a"))?;
        let dest = tmp.path().join("dest");
        let report = build_seat_materials(Some(&src), &players(), &Seat::player("a"), &dest)?;
        assert_eq!(files_under(&dest), vec!["geteilt/lage.txt"]);
        assert!(report.skipped.len() >= 3, "{:?}", report.skipped);
        // Traversal über die Sitz-ID wird gar nicht erst geplant.
        let plan = materials_plan(&src, &players(), &Seat::player(".."));
        assert!(plan.iter().all(|(from, _)| from != Path::new("..")));
        let plan = materials_plan(&src, &players(), &Seat::player("../geheim"));
        assert!(plan.iter().all(|(_, to)| to != Path::new("eigene")));
        Ok(())
    }

    #[test]
    fn audience_labels_cover_contract() {
        let a = PlayerId::new("a");
        let b = PlayerId::new("b");
        assert_eq!(audience_label(&Audience::Public), "public");
        assert_eq!(audience_label(&Audience::UmpireOnly), "umpire");
        assert_eq!(audience_label(&Audience::Seat(a.clone())), "seat");
        assert_eq!(
            audience_label(&Audience::SeatAndUmpire(a.clone())),
            "seat_and_umpire"
        );
        assert_eq!(audience_label(&Audience::pair(a, b)), "pair");
        assert_eq!(audience_label(&Audience::ObserverOnly), "observer");
    }
}
