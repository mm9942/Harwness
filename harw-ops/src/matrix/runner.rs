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
use std::time::Duration;

use harw_authority::{
    Permission, PermissionRequest, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
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
use harw_matrix_game::phases::{
    ArgumentBox, ContractKind, CounterArgument, EffectOp, ExpectedCall, NegotiationRequest, Phase,
    PhaseConfig, PhaseOutput, PhaseStep, PlayerDebrief, RoundArgument, RoundCursor,
    UmpireAdjudication, UmpireNarration, UmpireRuling, UmpireSynthesis, Verdict, apply_inject,
    close_negotiation, close_round, end_game, enter_phase, expected_calls, open_channels,
    open_game, post_messages, record_briefing, record_narration, record_standing,
    resolve_argument, reveal_round, reveal_secret, submit_counters, validate_counter_argument,
    validate_negotiation_messages, validate_negotiation_request, validate_player_argument,
    validate_umpire_adjudication, validate_umpire_narration,
};
use harw_matrix_game::prompts::{self, SeatRole};
use harw_matrix_game::scenario::{
    AdjudicationSystem, LoadedScenario, Rules, Scenario, ScheduledInject, SeedSpec,
    materials_for_seat, pair_folder_members,
};
use harw_matrix_game::state::{
    Audience, EntryKind, GameEntry, GameLog, Journal, PlayerId, RevealedBy, Seat, replay,
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

/// Sitz-Schlüssel des Umpires in `views`/`seats`.
pub const UMPIRE_KEY: &str = "umpire";

/// Unterordner des Laufverzeichnisses mit den Unterlagen-Kopien je Sitz.
pub const MATERIALS_DIR: &str = "materials";
/// Höchstgröße einer kopierten Unterlagen-Datei.
pub const MAX_MATERIAL_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Höchstgröße aller Unterlagen einer Sitz-Kopie.
pub const MAX_MATERIAL_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
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
    /// Anzahl der Leak-Befunde (`LeakSuspect`-Einträge).
    pub leaks: usize,
    /// Spiel nach diesem Schritt beendet.
    pub ended: bool,
}

impl StepReport {
    fn new(cursor: RoundCursor) -> Self {
        Self {
            round: cursor.round,
            phase: cursor.phase,
            calls: 0,
            forfeits: Vec::new(),
            leaks: 0,
            ended: false,
        }
    }

    /// Einzeilige deutsche Zusammenfassung.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut line = format!(
            "Runde {} · Phase {} · {} Aufruf(e)",
            self.round,
            self.phase.label(),
            self.calls
        );
        if !self.forfeits.is_empty() {
            line.push_str(&format!(" · gepasst: {}", self.forfeits.join(", ")));
        }
        if self.leaks > 0 {
            line.push_str(&format!(" · Leak-Verdacht: {}", self.leaks));
        }
        if self.ended {
            line.push_str(" · Spiel beendet");
        }
        line
    }
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

    /// Hinweise seit dem letzten Aufruf (z. B. Unterlagen nicht einsehbar);
    /// der Runner journalisiert sie für den Beobachter.
    fn take_warnings(&mut self) -> Vec<String> {
        Vec::new()
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
    /// Rückfall ohne jede Berechtigung (Unterlagen nicht bindbar).
    sandbox: SandboxSpec,
    parent: SessionId,
    cancel: CancelToken,
    budget: AgentBudget,
    children: BTreeMap<String, SessionId>,
    /// `<run_dir>/materials`, falls Unterlagen-Kopien existieren.
    materials_root: Option<PathBuf>,
    /// Präfix der Workspace-IDs der Sitz-Sandboxen.
    run_tag: String,
    warnings: Vec<String>,
}

impl SpawnerDriver {
    /// Baut den Treiber aus dem Op-Kontext. Die Kind-Sandbox ist die
    /// Parent-Sandbox ohne jede Berechtigung und ohne Netz — die Sitz-Rollen
    /// sind werkzeuglos.
    ///
    /// # Errors
    /// [`OpError::NotAvailable`] ohne Spawner oder StateStore.
    pub fn from_ctx(ctx: &OpContext) -> Result<Self, OpError> {
        let spawner = ctx.managed_spawner().ok_or_else(|| {
            OpError::NotAvailable(
                "kein Agent-Spawner in diesem Kontext — /matrix step braucht die Slash-Oberfläche einer Sitzung"
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

    /// Sandbox eines Sitzes: seine Unterlagen-Kopie, nur lesend, ohne Netz;
    /// ohne Kopie die berechtigungslose Rückfall-Sandbox.
    fn seat_sandbox_for(&mut self, key: &str) -> SandboxSpec {
        let Some(root) = &self.materials_root else {
            return self.sandbox.clone();
        };
        let workspace = format!("matrix-{}-{key}", self.run_tag);
        match seat_sandbox(&self.parent_sandbox, &root.join(key), &workspace) {
            Ok(sandbox) => sandbox,
            Err(error) => {
                self.warnings.push(format!(
                    "Sitz `{key}`: Unterlagen nicht bindbar ({error}) — Sitz liest ohne Unterlagen"
                ));
                self.sandbox.clone()
            }
        }
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
        if let Some(child) = self.children.remove(seat_key) {
            if let Err(error) = self.spawner.release_child(&child) {
                tracing::warn!(child = %child, error = %error, "matrix.seat_release_failed");
            }
        }
    }
}

impl SeatDriver for SpawnerDriver {
    fn ask<'a>(&'a mut self, request: SeatRequest<'a>) -> SeatFuture<'a> {
        Box::pin(async move {
            let key = request.seat_key;
            let child = match self.children.get(key) {
                Some(child) => child.clone(),
                None => {
                    let bound = self.seat_sandbox_for(key);
                    let parent = self.parent.clone();
                    let system = request.system;
                    let input = || harw_extension_api::SpawnInput {
                        parent_session_id: parent.clone(),
                        handoff_call_id: ToolCallId::new(),
                        instructions: Some(system.to_owned()),
                        context: json!({ "matrix_seat": key }),
                        ceiling: None,
                    };
                    let first = self
                        .spawner
                        .spawn_child_or_wait(
                            request.role,
                            input(),
                            bound,
                            None,
                            SEAT_SLOT_WAIT,
                            &self.cancel,
                        )
                        .await
                        .map(harw_core::child_controller::ChildGuard::keep);
                    let child = match first {
                        Ok(child) => child,
                        // Die Admission verlangt dieselbe Workspace-Bindung
                        // wie beim Parent: dann ohne Unterlagen, aber nie
                        // mit mehr Rechten (fail-closed).
                        Err(error)
                            if self.materials_root.is_some()
                                && error.to_string().contains("escalation") =>
                        {
                            self.warnings.push(format!(
                                "Sitz `{key}`: Unterlagen-Sandbox abgewiesen ({error}) — Sitz liest ohne Unterlagen"
                            ));
                            self.spawner
                                .spawn_child_or_wait(
                                    request.role,
                                    input(),
                                    self.sandbox.clone(),
                                    None,
                                    SEAT_SLOT_WAIT,
                                    &self.cancel,
                                )
                                .await
                                .map(harw_core::child_controller::ChildGuard::keep)
                                .map_err(|error| format!("Spawn fehlgeschlagen: {error}"))?
                        }
                        Err(error) => return Err(format!("Spawn fehlgeschlagen: {error}")),
                    };
                    // Der Sitz bleibt über alle Phasen zugelassen; die
                    // Freigabe übernimmt `release_all` bzw. `forget`.
                    self.children.insert(key.to_owned(), child.clone());
                    child
                }
            };
            let Some(declared) = self.spawner.child_budget(&child) else {
                self.forget(key);
                return Err(format!("Admission-Record von `{child}` fehlt"));
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

    fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }
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
    flushed: usize,
    status: RunStatus,
    timestamps: bool,
    sink: Option<EventSink>,
    aar: Option<String>,
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
    }
}

/// Agentenrolle eines Sitzes (Single Source of Truth: `role_names`).
fn role_name(role: SeatRole) -> &'static str {
    match role {
        SeatRole::Player => role_names::MATRIX_PLAYER,
        SeatRole::Umpire => role_names::MATRIX_UMPIRE,
        SeatRole::Market => role_names::MATRIX_MARKET,
    }
}

fn role_label(role: SeatRole) -> &'static str {
    match role {
        SeatRole::Player => "player",
        SeatRole::Umpire => "umpire",
        SeatRole::Market => "market",
    }
}

impl MatrixRun {
    /// Eröffnet ein Spiel (Setup: `GameCreated`, Deklarationen, Briefings).
    /// Mit `run_dir` werden Verzeichnis, Szenario-Kopie und Journal angelegt.
    ///
    /// # Errors
    /// [`OpError::Execution`] bei Kern- oder Dateifehlern.
    pub fn start(
        loaded: LoadedScenario,
        source: String,
        master_seed: [u8; 32],
        run_id: String,
        run_dir: Option<PathBuf>,
        timestamps: bool,
    ) -> Result<Self, OpError> {
        let at = timestamps.then(Timestamp::now);
        let log = open_game(&loaded, &master_seed, at).map_err(matrix_err)?;
        let cfg = PhaseConfig::from_scenario(&loaded.scenario);
        let run = Self::from_log(loaded, source, log, run_id, run_dir, timestamps, cfg)?;
        Ok(run)
    }

    fn from_log(
        loaded: LoadedScenario,
        source: String,
        log: GameLog,
        run_id: String,
        run_dir: Option<PathBuf>,
        timestamps: bool,
        cfg: PhaseConfig,
    ) -> Result<Self, OpError> {
        if let Some(dir) = &run_dir {
            std::fs::create_dir_all(dir)
                .map_err(|e| io_err("Laufverzeichnis nicht anlegbar", dir, &e))?;
            let copy = dir.join(SCENARIO_FILE);
            std::fs::write(&copy, &source)
                .map_err(|e| io_err("Szenario-Kopie nicht schreibbar", &copy, &e))?;
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
            flushed: 0,
            status: RunStatus::Running,
            timestamps,
            sink: None,
            aar: None,
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
        let driver = SpawnerDriver::from_ctx(ctx)?;
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
                OpError::InvalidArguments(format!(
                    "Lauf `{}` hat keine weitere Phase",
                    self.run_id
                ))
            })?,
        };
        self.cursor = next;
        let at = self.at();
        enter_phase(&mut self.log, next, at).map_err(matrix_err)?;
        let mut report = StepReport::new(next);
        if next.phase == Phase::Briefing {
            self.args.clear();
            for inject in self.loaded.scenario.injects_for_round(next.round) {
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
                let at = self.at();
                close_round(&mut self.log, self.loaded.scenario.rules(), at)
                    .map_err(matrix_err)?;
            }
            Phase::Schlussargumente => self.run_final_arguments(driver, &mut report).await?,
            Phase::Aar => self.run_aar(driver, &mut report).await?,
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
            let answer = driver
                .ask(SeatRequest {
                    seat_key: &key,
                    role: role_name(role),
                    system: &system,
                    prompt: std::mem::take(&mut prompt),
                })
                .await;
            let raw = match answer {
                Ok(raw) => raw,
                Err(error) => {
                    // Ein neues Kind kennt nichts — beim nächsten Mal volles Lagebild.
                    self.seen.remove(&key);
                    self.note(
                        "forfeit",
                        format!(
                            "Sitz `{key}` ({:?}): Aufruf fehlgeschlagen — {error}",
                            call.contract
                        ),
                    )?;
                    report.forfeits.push(key);
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
            "forfeit",
            format!(
                "Sitz `{key}` ({:?}): Antwort auch nach Reparaturversuch ungültig — {last_error}",
                call.contract
            ),
        )?;
        report.forfeits.push(key);
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
            Seat::Umpire => None,
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
                (None, Seat::Umpire) => {}
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
                    sealed
                        .seal(player.clone(), argument)
                        .map_err(matrix_err)?;
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
        let args = self.args.clone();
        for arg in &args {
            let ruling = adjudication
                .rulings
                .iter()
                .find(|r| r.argument_id == arg.id)
                .cloned()
                .unwrap_or_else(|| neutral_ruling(arg, self.rules()));
            let at = self.at();
            resolve_argument(&mut self.log, &self.loaded.scenario, arg, &ruling, at)
                .map_err(matrix_err)?;
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
            match (self.call_seat(driver, call, &vis, report).await?, &call.seat) {
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
        if let Some(dir) = &self.run_dir {
            let path = dir.join(AAR_FILE);
            std::fs::write(&path, &markdown)
                .map_err(|e| io_err("AAR nicht schreibbar", &path, &e))?;
        }
        self.aar = Some(markdown);
        self.status = RunStatus::Ended;
        driver.release_all();
        self.children.clear();
        Ok(())
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
            .ok_or_else(|| {
                OpError::InvalidArguments("Override braucht `argument_id`".to_owned())
            })?
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
            log,
            run_id,
            run_dir,
            self.timestamps,
            self.cfg,
        )?;
        forked.note("fork", format!("von `{}` ab Rundenende {round}", self.run_id))?;
        forked.flush()?;
        self.note("fork", format!("→ `{}` ab Rundenende {round}", forked.run_id))?;
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

    /// Gibt die Sitz-Kinder zurück (für die Freigabe außerhalb eines Schritts).
    pub fn take_children(&mut self) -> BTreeMap<String, SessionId> {
        std::mem::take(&mut self.children)
    }

    /// Übernimmt Sitz-Kinder zurück.
    pub fn restore_children(&mut self, children: BTreeMap<String, SessionId>) {
        self.children = children;
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
            .filter_map(|r| {
                r.public_rationale.as_ref().map(|text| {
                    (
                        format!("umpire:{}:public_rationale", r.argument_id),
                        text.clone(),
                    )
                })
            })
            .collect(),
        PhaseOutput::UmpireNarration(n) => {
            let mut out: Vec<(String, String)> = n
                .narrations
                .iter()
                .filter(|x| x.audience.0.is_public())
                .map(|x| (format!("umpire:{}:narration", x.argument_id), x.text.clone()))
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
        probability: matches!(rules.adjudication, AdjudicationSystem::EstimativeD100)
            .then_some(50),
        inconsistent_with: None,
        public_rationale: None,
        private_notes: Some("Ersatzurteil des Runners (Umpire-Antwort ungültig).".to_owned()),
        on_success: Vec::new(),
        on_failure: Vec::new(),
        triggers_secret: None,
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
        EntryKind::InjectApplied { attributed, .. } => {
            attributed.then(|| "facilitator".to_owned())
        }
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
        | EntryKind::FactAdded { text }
        | EntryKind::InjectApplied { text, .. } => text.clone(),
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
            format!(
                "Wurf {} (Versuch {}): {} = {} gegen {} → {}",
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
        } => format!(
            "{var}: {} → {} ({cause})",
            from.display(),
            to.display()
        ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use harw_matrix_game::scenario::load_scenario;

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
        // Der Turn-Prompt endet mit dem Contract-Namen.
        if prompt.contains("`briefing_ack`") {
            r#"{"ack":true,"intent":"Wir halten Kurs."}"#.to_owned()
        } else if prompt.contains("`negotiation_request`") {
            r#"{"requests":[]}"#.to_owned()
        } else if prompt.contains("`negotiation_message`") {
            r#"{"messages":[]}"#.to_owned()
        } else if prompt.contains("`player_argument`") {
            format!(
                r#"{{"action":"Die Fraktion {seat} verstärkt ihre Präsenz am Hafen.","pros":["Sie hat Leute vor Ort.","Die Lage verlangt Handeln."]}}"#
            )
        } else if prompt.contains("`counter_argument`") {
            r#"{"counters":[]}"#.to_owned()
        } else if prompt.contains("`umpire_narration`") {
            r#"{"narrations":[],"round_summary":"Eine ruhige Runde."}"#.to_owned()
        } else {
            // Adjudikation: absichtlich ungültig → Ersatzurteil greift.
            "kein JSON".to_owned()
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
            seed,
            "test-run".to_owned(),
            None,
            false,
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
        assert!(seats.iter().any(|s| s["id"] == "umpire" && s["role"] == "umpire"));
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

    #[tokio::test]
    async fn end_writes_aar_without_agents() -> TestResult {
        let mut run = run()?;
        run.request_end()?;
        let mut driver = NullDriver;
        let report = run.step_with(&mut driver).await?;
        assert_eq!(report.phase, Phase::Aar);
        assert!(report.ended);
        assert_eq!(run.status(), RunStatus::Ended);
        assert!(run.aar().is_some_and(|md| md.contains("After-Action-Review")));
        assert!(run.step_with(&mut driver).await.is_err());
        Ok(())
    }

    #[test]
    fn inject_is_queued_and_noted() -> TestResult {
        let mut run = run()?;
        let id = run.queue_inject("Sturmflut im Hafen", &Audience::Public, false, Vec::new())?;
        assert_eq!(id, "facilitator-1");
        assert!(run.queue_inject("  ", &Audience::Public, false, Vec::new()).is_err());
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
        assert!(matches!(&last.kind, EntryKind::FacilitatorNote { command, .. } if command == "inject"));
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
