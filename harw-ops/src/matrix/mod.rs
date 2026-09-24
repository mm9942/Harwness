//! `/matrix` — Ansicht vergangener und laufender Matrix-Game-Läufe
//! (`docs/design/matrix-game.md` §4, §7) und gemeinsame Laufverwaltung für
//! den Game Master.
//!
//! # Runde 7, Teil M: Start und Steuerung über den Game Master
//! Matrix-Games startet und steuert die UIA über den spezialisierten
//! Orchestrator `matrix-game-master` (Hintergrund-Kind der UIA, eigene
//! Modell-Werkzeuge in [`game_master`]). Der Slash-Befehl startet und steuert
//! **nicht** mehr: `/matrix start|step|auto|…` blockierte die
//! TUI-Ereignisschleife, solange die Sitz-Agenten liefen (der Befehl wird im
//! Event-Loop abgewartet, `harw-tui/src/app.rs`, Dispatch der
//! `/command`-Zeile) — Freigabedialoge der Sitze konnten nicht erscheinen,
//! Esc/Ctrl+C griffen nicht. Diese Unterbefehle antworten jetzt mit einem
//! Hinweis auf den Game Master.
//!
//! # Subcommands (nur lesend)
//! - `show` (auch bare `/matrix`) — Panel-Daten des aktuellen bzw. per
//!   `--run=<id>` gewählten Laufs.
//! - `list` — Läufe dieses Prozesses, gebündelte und gespeicherte Szenarien.
//! - `replay` — prüft das Journal eines Laufs per Replay (ohne Neustart).
//! - `compare <lauf> <lauf> …` — Vergleich mehrerer Läufe (Design-Lehren).
//!
//! # `show`-Daten
//! `{"run_id","scenario","round","phase","status","seats":[{"id","name",
//! "role"}],"observer":[E],"views":{"<seat_id>":[E]},"channels":[{"id",
//! "members":["a","b"]}]}` mit `E = {"round","kind","audience","from",
//! "text"}`; `audience` ∈ `public|umpire|seat|seat_and_umpire|pair|observer`.
//! Die Views entstehen aus `project(journal, seat)` — genau das, was der
//! Sitz sieht.
//!
//! # Live-Events
//! Liegt ein `Arc<AgentEventHub>` im Kontext, geht jede neue Journalzeile als
//! `AgentEventKind::Matrix { run_id, event }` hinaus; `event` ist `E` plus
//! `"seats"` (Sitz-Schlüssel, deren Projektion den Eintrag enthält) und
//! `"events"` (serialisierte `MatrixGameEvent`s aus `events_for_entry`).
//!
//! # Szenarien
//! `resolve_known_scenario` kennt gebündelte Szenarien (Dateiname oder ID)
//! und gespeicherte Entwürfe unter
//! `<profil>/knowledge/matrix/scenarios/<slug>.toml` (vom Game Master über
//! `matrix.draft_scenario` angelegt). Pfade zu beliebigen Dateien löst es
//! bewusst nicht auf: der Aufruf kommt vom Modell und darf nichts außerhalb
//! des Matrix-Speichers lesen. Freitext mit Leerzeichen verweist auf den
//! Entwurf.
//!
//! # Laufzustand
//! Läufe leben prozessweit in `RUNS`. Für die Dauer eines Schritts wird der
//! Lauf aus der Map genommen (kein `std::sync::Mutex` über ein `await`);
//! `show` liefert dann den zuletzt gespeicherten Stand mit `status = "busy"`.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`] — Grammatik, unbekanntes Szenario/Lauf,
//!   Steuerbefehl über den Slash-Pfad.
//! - [`OpError::NotAvailable`] — kein Agent-Spawner (Game-Master-Werkzeuge).
//! - [`OpError::Execution`] — Kern-, Datei- oder Registry-Fehler.

pub mod game_master;
pub mod report;
pub mod runner;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use harw_macros::operation;
use harw_matrix_game::phases::Phase;
use harw_matrix_game::scenario::{LoadedScenario, load_scenario};
use harw_operations::{OpContext, OpError, OpOutput};
use serde_json::{Value, json};

use runner::{EventSink, MatrixRun, NullDriver, RunStatus, master_seed_for};

/// Obergrenze für die Rundenzahl eines `matrix.run`-Aufrufs.
pub const MAX_AUTO_ROUNDS: u32 = 20;

/// Sicherheitsnetz: höchstens so viele Phasen je Durchlauf.
const MAX_AUTO_STEPS: usize = 400;

/// Unterordner gespeicherter Szenario-Entwürfe unter `<profil>/knowledge/matrix`.
pub const SCENARIOS_DIR: &str = "scenarios";

/// Gebündelte Szenarien (Dateiname ohne `.toml`, Quelltext).
const BUNDLED: [(&str, &str); 2] = [
    (
        "karst-islands",
        include_str!("../../../harw-matrix-game/scenarios/karst-islands.toml"),
    ),
    (
        "cloud-sme-2027",
        include_str!("../../../harw-matrix-game/scenarios/cloud-sme-2027.toml"),
    ),
];

/// Hinweis für Steuerbefehle über den Slash-Pfad (Runde 7, Teil M).
pub const GAME_MASTER_HINT: &str = "Matrix-Games startet und steuert die UIA über den Game Master \
(`matrix-game-master`, läuft im Hintergrund und blockiert den Chat nicht). Sag einfach z. B. \
„spiel ein Matrix-Game zu …“. `/matrix` zeigt nur noch an: show, list, replay, compare.";

// ── Argumente ────────────────────────────────────────────────────────────────

/// Argument-Container für `/matrix`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct MatrixArgs {
    /// Alle Tokens nach `/matrix`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl harw_operations::FromRawArgs for MatrixArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Ein geparster `/matrix`-Befehl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatrixCommand {
    /// `show`.
    Show,
    /// `list`.
    List,
    /// `replay` (nur Prüfung).
    Replay,
    /// `compare <lauf> <lauf> …`: Vergleich mehrerer Läufe (Design-Lehren).
    Compare(Vec<String>),
    /// Früherer Steuerbefehl (`start`, `step`, `auto`, …) — wird mit
    /// [`GAME_MASTER_HINT`] abgewiesen.
    Steering(String),
}

/// Befehl plus optional gewählter Lauf (`--run=<id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Gewählter Lauf.
    pub run: Option<String>,
    /// Befehl.
    pub command: MatrixCommand,
}

const USAGE: &str = "show, list, replay, compare <lauf> <lauf> …";

/// Frühere Steuerbefehle des Slash-Pfads (Runde 7, Teil M: nur noch über den
/// Game Master).
const STEERING: &[&str] = &[
    "start", "step", "auto", "pause", "inject", "override", "veto", "reveal", "fork", "end",
    "draft", "run",
];

fn invalid(message: impl Into<String>) -> OpError {
    OpError::InvalidArguments(message.into())
}

/// Entfernt `--<name>=<wert>` bzw. `--<name> <wert>` aus `tokens`.
fn take_value_flag(tokens: &mut Vec<String>, name: &str) -> Result<Option<String>, OpError> {
    let long = format!("--{name}");
    let prefix = format!("--{name}=");
    let Some(index) = tokens
        .iter()
        .position(|t| *t == long || t.starts_with(&prefix))
    else {
        return Ok(None);
    };
    let token = tokens.remove(index);
    if let Some(value) = token.strip_prefix(&prefix) {
        return Ok(Some(value.to_owned()));
    }
    if index < tokens.len() {
        return Ok(Some(tokens.remove(index)));
    }
    Err(invalid(format!("`{long}` braucht einen Wert")))
}

/// Parst die Tokens eines `/matrix`-Aufrufs.
///
/// # Errors
/// [`OpError::InvalidArguments`] mit deutscher Meldung.
pub fn parse_command(tokens: &[String]) -> Result<ParsedCommand, OpError> {
    let mut tokens: Vec<String> = tokens.to_vec();
    let run = take_value_flag(&mut tokens, "run")?;
    let Some(sub) = tokens.first().cloned() else {
        return Ok(ParsedCommand {
            run,
            command: MatrixCommand::Show,
        });
    };
    let rest: Vec<String> = tokens.get(1..).unwrap_or_default().to_vec();
    let no_extra = |rest: &[String], sub: &str| -> Result<(), OpError> {
        if rest.is_empty() {
            Ok(())
        } else {
            Err(invalid(format!(
                "`/matrix {sub}` erwartet keine weiteren Argumente: {}",
                rest.join(" ")
            )))
        }
    };
    let command = match sub.as_str() {
        "show" => {
            no_extra(&rest, "show")?;
            MatrixCommand::Show
        }
        "list" => {
            no_extra(&rest, "list")?;
            MatrixCommand::List
        }
        "replay" => {
            no_extra(&rest, "replay")?;
            MatrixCommand::Replay
        }
        "compare" => {
            if rest.len() < 2 {
                return Err(invalid(
                    "mindestens zwei Läufe erwartet: /matrix compare <lauf> <lauf> …",
                ));
            }
            MatrixCommand::Compare(rest)
        }
        steering if STEERING.contains(&steering) => MatrixCommand::Steering(sub),
        other => {
            return Err(invalid(format!(
                "unbekannter /matrix-Subcommand: {other} ({USAGE})"
            )));
        }
    };
    Ok(ParsedCommand { run, command })
}

// ── Laufregistry ─────────────────────────────────────────────────────────────

/// Alle Läufe dieses Prozesses (Lauf-ID → Lauf).
static RUNS: OnceLock<Mutex<HashMap<String, MatrixRun>>> = OnceLock::new();
/// Aktueller Lauf (zuletzt gestartet, gleich welche Sitzung).
static CURRENT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
/// Zuletzt gestarteter Lauf je Sitzung (Game Master: Session-ID → Lauf-ID).
static SESSION_RUNS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
/// Läufe, die gerade einen Schritt ausführen (Lauf-ID → letzter `show`-Stand).
static BUSY: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
/// Zähler für eindeutige Lauf-IDs.
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

fn lock<T: Default>(cell: &'static OnceLock<Mutex<T>>) -> Result<MutexGuard<'static, T>, OpError> {
    cell.get_or_init(|| Mutex::new(T::default()))
        .lock()
        .map_err(|_| OpError::Execution("Matrix-Registry ist vergiftet".to_owned()))
}

fn current_id(explicit: Option<String>) -> Result<String, OpError> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    lock(&CURRENT)?.clone().ok_or_else(|| {
        invalid(
            "kein aktueller Matrix-Lauf — Matrix-Games startet der Game Master (oder --run=<id>)",
        )
    })
}

fn set_current(id: &str, session: &str) -> Result<(), OpError> {
    *lock(&CURRENT)? = Some(id.to_owned());
    lock(&SESSION_RUNS)?.insert(session.to_owned(), id.to_owned());
    Ok(())
}

/// Lauf einer Sitzung: explizit gewählt oder der zuletzt von ihr gestartete.
fn session_run_id(explicit: Option<String>, session: &str) -> Result<String, OpError> {
    if let Some(id) = explicit.filter(|id| !id.trim().is_empty()) {
        return Ok(id);
    }
    lock(&SESSION_RUNS)?.get(session).cloned().ok_or_else(|| {
        invalid("kein Matrix-Lauf in dieser Sitzung — erst matrix.start (oder run_id angeben)")
    })
}

fn is_busy(id: &str) -> Result<bool, OpError> {
    Ok(lock(&BUSY)?.contains_key(id))
}

/// Nimmt einen Lauf für einen Schritt aus der Registry (kein Lock über `await`).
fn take_run(id: &str) -> Result<MatrixRun, OpError> {
    let taken = lock(&RUNS)?.remove(id);
    match taken {
        Some(run) => {
            let mut snapshot = run.show_json();
            if let Value::Object(map) = &mut snapshot {
                map.insert("status".to_owned(), json!("busy"));
            }
            lock(&BUSY)?.insert(id.to_owned(), snapshot);
            Ok(run)
        }
        None if is_busy(id)? => Err(invalid(format!(
            "Lauf `{id}` führt gerade einen Schritt aus — bitte warten"
        ))),
        None => Err(invalid(format!("unbekannter Matrix-Lauf `{id}`"))),
    }
}

/// Legt einen Lauf (zurück) in die Registry.
fn put_run(run: MatrixRun) -> Result<(), OpError> {
    let id = run.run_id().to_owned();
    lock(&BUSY)?.remove(&id);
    lock(&RUNS)?.insert(id, run);
    Ok(())
}

/// Führt eine synchrone Aktion auf einem ruhenden Lauf aus.
fn with_run<T>(
    id: &str,
    action: impl FnOnce(&mut MatrixRun) -> Result<T, OpError>,
) -> Result<T, OpError> {
    {
        let mut runs = lock(&RUNS)?;
        if let Some(run) = runs.get_mut(id) {
            return action(run);
        }
    }
    if is_busy(id)? {
        Err(invalid(format!(
            "Lauf `{id}` führt gerade einen Schritt aus — bitte warten"
        )))
    } else {
        Err(invalid(format!("unbekannter Matrix-Lauf `{id}`")))
    }
}

// ── Szenarien und Pfade ──────────────────────────────────────────────────────

/// Ein aufgelöstes Szenario: Quelltext, geladen, Pfad (bei Dateien).
struct ResolvedScenario {
    source: String,
    loaded: LoadedScenario,
    path: Option<PathBuf>,
}

fn load(source: String, path: Option<PathBuf>) -> Result<ResolvedScenario, OpError> {
    let loaded =
        load_scenario(&source).map_err(|e| invalid(format!("Szenario nicht ladbar: {e}")))?;
    Ok(ResolvedScenario {
        source,
        loaded,
        path,
    })
}

/// Gebündeltes Szenario (Dateiname oder ID) oder gespeicherter Entwurf
/// (`<root>/scenarios/<slug>.toml`, Runde 7 Teil M2). Freitext mit
/// Leerzeichen ist nie ein Slug — die Meldung verweist dann auf den Entwurf.
fn resolve_known_scenario(name: &str, root: Option<&Path>) -> Result<ResolvedScenario, OpError> {
    let name = name.trim();
    let stem = name.strip_suffix(".toml").unwrap_or(name);
    if let Some((_, source)) = BUNDLED.iter().find(|(file, _)| *file == stem) {
        return load((*source).to_owned(), None);
    }
    for (_, source) in BUNDLED {
        if let Ok(loaded) = load_scenario(source) {
            if loaded.scenario.id() == name {
                return Ok(ResolvedScenario {
                    source: source.to_owned(),
                    loaded,
                    path: None,
                });
            }
        }
    }
    if let Some(root) = root {
        let slug = sanitize(stem);
        if slug == stem {
            let path = root.join(SCENARIOS_DIR).join(format!("{slug}.toml"));
            if path.is_file() {
                let source = std::fs::read_to_string(&path).map_err(|e| {
                    OpError::Execution(format!(
                        "gespeichertes Szenario `{}` nicht lesbar: {e}",
                        path.display()
                    ))
                })?;
                return load(source, Some(path));
            }
        }
    }
    let mut known: Vec<String> = BUNDLED.iter().map(|(file, _)| (*file).to_owned()).collect();
    if let Some(root) = root {
        known.extend(saved_scenarios(root));
    }
    if name.contains(char::is_whitespace) {
        return Err(invalid(format!(
            "`{name}` ist Freitext, kein Szenario — erst ein Szenario entwerfen \
             (matrix.draft_scenario), dann mit dessen Slug starten (bekannt: {})",
            known.join(", ")
        )));
    }
    Err(invalid(format!(
        "unbekanntes Szenario `{name}` (bekannt: {})",
        known.join(", ")
    )))
}

/// Slugs der gespeicherten Entwürfe unter `<root>/scenarios/`, sortiert.
fn saved_scenarios(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join(SCENARIOS_DIR)) else {
        return Vec::new();
    };
    let mut slugs: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|e| e.to_str()) == Some("toml"))
                .then(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
                .flatten()
        })
        .collect();
    slugs.sort();
    slugs
}

/// `<profil>/knowledge/matrix`.
fn matrix_root() -> Result<PathBuf, OpError> {
    let home = harw_home::home_dir()
        .map_err(|e| OpError::Execution(format!("HARW_HOME nicht auflösbar: {e}")))?;
    let profile = harw_home::active_profile_name(&home);
    let dir = harw_home::profile_dir(&home, &profile)
        .map_err(|e| OpError::Execution(format!("Profil '{profile}' nicht auflösbar: {e}")))?;
    Ok(harw_home::matrix_dir(&dir))
}

/// Dateisystemtauglicher Name (ASCII-Alphanumerik, `-`, `_`).
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.trim_matches('-').is_empty() {
        "szenario".to_owned()
    } else {
        cleaned
    }
}

/// Neue Lauf-ID und ihr Verzeichnis unter `<root>/<szenario>/`.
fn new_run_location(root: &Path, scenario_id: &str) -> (String, PathBuf) {
    let stamp = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%S").to_string();
    let base = root.join(sanitize(scenario_id));
    loop {
        let n = RUN_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("{stamp}-{n}");
        let dir = base.join(&id);
        if !dir.exists() {
            return (id, dir);
        }
    }
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Führt `/matrix` aus (nur lesende Ansichten, Runde 7 Teil M).
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "matrix",
    summary = "Matrix Game ansehen: show, list, replay, compare (Start und Steuerung über den Game Master).",
    domain = "knowledge",
    permission = "operator",
    command(
        path = "/matrix",
        visibility = "channel_reduced",
        busy_subcommands = "show=immediate, list=immediate"
    )
)]
async fn matrix(_ctx: &OpContext, args: MatrixArgs) -> Result<OpOutput, OpError> {
    let parsed = parse_command(&args.tokens)?;
    run_command(parsed)
}

fn run_command(parsed: ParsedCommand) -> Result<OpOutput, OpError> {
    let ParsedCommand { run, command } = parsed;
    match command {
        MatrixCommand::List => list_output(),
        MatrixCommand::Compare(ids) => compare_output(&ids),
        MatrixCommand::Show => show_output(&current_id(run)?),
        MatrixCommand::Replay => {
            let id = current_id(run)?;
            with_run(&id, |r| {
                let text = r.verify_replay()?;
                Ok(output(r, format!("{}\n{text}", r.headline())))
            })
        }
        MatrixCommand::Steering(sub) => {
            Err(invalid(format!("`/matrix {sub}`: {GAME_MASTER_HINT}")))
        }
    }
}

fn output(run: &MatrixRun, text: String) -> OpOutput {
    OpOutput {
        text,
        data: Some(run.show_json()),
    }
}

/// Startet einen Lauf aus einem aufgelösten Szenario und macht ihn zum
/// aktuellen Lauf der Sitzung.
fn start_run(
    ctx: &OpContext,
    resolved: ResolvedScenario,
    seed: Option<u64>,
) -> Result<(OpOutput, String), OpError> {
    let ResolvedScenario {
        source,
        loaded,
        path,
    } = resolved;
    let master = master_seed_for(&loaded, seed);
    let (run_id, dir) = new_run_location(&matrix_root()?, loaded.scenario.id());
    let warnings = loaded.warnings.clone();
    let mut run = MatrixRun::start(
        loaded,
        source,
        path,
        master,
        run_id.clone(),
        Some(dir.clone()),
        true,
        None,
    )?;
    run.set_sink(EventSink::from_ctx(ctx));
    run.flush()?;
    let mut text = format!(
        "{}\nSeed {}… · Verzeichnis {}",
        run.headline(),
        run.seed_prefix(),
        dir.display()
    );
    for warning in warnings {
        text.push_str(&format!("\nHinweis: {warning}"));
    }
    let out = output(&run, text);
    put_run(run)?;
    set_current(&run_id, ctx.session_id().as_str())?;
    Ok((out, run_id))
}

async fn step_once(
    ctx: &OpContext,
    run: &mut MatrixRun,
    label: &str,
) -> Result<runner::StepReport, OpError> {
    run.set_sink(EventSink::from_ctx(ctx));
    run.set_cancel(ctx.cancel_token().cloned());
    run.set_status(RunStatus::Running);
    let cursor = run.cursor();
    run.note(
        label,
        format!("ab Runde {}, Phase {}", cursor.round, cursor.phase.label()),
    )?;
    run.step(ctx).await
}

/// Grund, aus dem ein Durchlauf endete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopReason {
    /// Gewünschte Rundenzahl erreicht.
    Rounds,
    /// Spiel beendet.
    Ended,
    /// Leak-Verdacht (Beobachter-Protokoll prüfen).
    Leak,
    /// Abbruch des aufrufenden Turns.
    Cancelled,
    /// Zeitbudget des Werkzeugaufrufs erschöpft.
    Deadline,
    /// Sicherheitsnetz der Phasenzahl.
    StepCap,
}

/// Spielt bis zu `rounds` Runden (bis zum Rundenende) bzw. bis Spielende,
/// Leak-Verdacht, Abbruch oder `deadline`.
async fn advance(
    ctx: &OpContext,
    run: &mut MatrixRun,
    rounds: u32,
    deadline: Option<std::time::Instant>,
) -> Result<(Vec<String>, StopReason), OpError> {
    run.note("run", format!("{rounds} Runde(n)"))?;
    let mut lines = Vec::new();
    let mut closed = 0;
    for _ in 0..MAX_AUTO_STEPS {
        if ctx.cancel_token().is_some_and(|c| c.is_cancelled()) {
            run.set_status(RunStatus::Paused);
            run.note("pause", "Durchlauf abgebrochen")?;
            run.flush()?;
            return Ok((lines, StopReason::Cancelled));
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            run.set_status(RunStatus::Paused);
            run.note("pause", "Zeitbudget des Aufrufs erschöpft")?;
            run.flush()?;
            return Ok((lines, StopReason::Deadline));
        }
        let report = step_once(ctx, run, "run-step").await?;
        lines.push(report.summary());
        if report.ended {
            return Ok((lines, StopReason::Ended));
        }
        if report.leaks > 0 {
            run.set_status(RunStatus::Paused);
            run.note("pause", "Durchlauf wegen Leak-Verdacht angehalten")?;
            run.flush()?;
            return Ok((lines, StopReason::Leak));
        }
        if report.phase == Phase::Rundenende {
            closed += 1;
            if closed >= rounds {
                return Ok((lines, StopReason::Rounds));
            }
        }
    }
    Ok((lines, StopReason::StepCap))
}

/// Direkt zu Schlussargumenten und AAR (schreibt `aar.md` und `report.md`).
async fn end_loop(ctx: &OpContext, run: &mut MatrixRun) -> Result<Vec<String>, OpError> {
    let mut lines = Vec::new();
    if run.status() == RunStatus::Ended {
        return Ok(lines);
    }
    run.set_sink(EventSink::from_ctx(ctx));
    run.set_cancel(ctx.cancel_token().cloned());
    run.request_end()?;
    // Höchstens Schlussargumente und AAR.
    for _ in 0..3 {
        if run.status() == RunStatus::Ended {
            break;
        }
        let report = match run.step(ctx).await {
            // Ohne Spawner entsteht das AAR ohne Umpire-Synthese.
            Err(OpError::NotAvailable(_)) => run.step_with(&mut NullDriver).await?,
            other => other?,
        };
        lines.push(report.summary());
    }
    Ok(lines)
}

/// Vergleicht mehrere Läufe dieser Sitzung (Kennzahlen je Lauf plus
/// Design-Lehren) und schreibt den Bericht neben den ersten Lauf.
fn compare_output(ids: &[String]) -> Result<OpOutput, OpError> {
    let mut summaries = Vec::with_capacity(ids.len());
    let mut first_dir = None;
    for id in ids {
        let (summary, dir) = with_run(id, |run| {
            Ok((
                harw_matrix_game::aar::summarize_run(
                    id,
                    run.loaded(),
                    &run.log().journal,
                    &run.log().state,
                ),
                run.run_dir().map(Path::to_path_buf),
            ))
        })?;
        if first_dir.is_none() {
            first_dir = dir;
        }
        summaries.push(summary);
    }
    let mut text = harw_matrix_game::aar::compare_runs(&summaries);
    if let Some(parent) = first_dir.as_deref().and_then(Path::parent) {
        let path = parent.join(format!("compare-{}.md", ids.join("+")));
        match std::fs::write(&path, &text) {
            Ok(()) => text.push_str(&format!("\nGespeichert: {}\n", path.display())),
            Err(e) => text.push_str(&format!("\nNicht gespeichert ({}): {e}\n", path.display())),
        }
    }
    Ok(OpOutput { text, data: None })
}

fn show_output(id: &str) -> Result<OpOutput, OpError> {
    if let Some(snapshot) = lock(&BUSY)?.get(id).cloned() {
        return Ok(OpOutput {
            text: format!("Lauf `{id}` führt gerade einen Schritt aus."),
            data: Some(snapshot),
        });
    }
    with_run(id, |run| {
        let state = &run.log().state;
        let mut text = run.headline();
        text.push_str(&format!(
            "\nSitze: {} + Umpire · Journal: {} Einträge · Kanäle: {}",
            state
                .players
                .iter()
                .map(|p| run.loaded().scenario.display_name(p))
                .collect::<Vec<_>>()
                .join(", "),
            run.log().journal.len(),
            state.channels.len()
        ));
        Ok(output(run, text))
    })
}

fn list_output() -> Result<OpOutput, OpError> {
    let mut runs: Vec<Value> = lock(&RUNS)?
        .values()
        .map(|run| {
            let cursor = run.cursor();
            json!({
                "run_id": run.run_id(),
                "scenario": run.loaded().scenario.id(),
                "round": cursor.round,
                "phase": cursor.phase.label(),
                "status": run.status().as_str(),
            })
        })
        .collect();
    for (id, snapshot) in lock(&BUSY)?.iter() {
        runs.push(json!({
            "run_id": id,
            "scenario": snapshot["scenario"],
            "round": snapshot["round"],
            "phase": snapshot["phase"],
            "status": "busy",
        }));
    }
    runs.sort_by(|a, b| a["run_id"].as_str().cmp(&b["run_id"].as_str()));
    let current = lock(&CURRENT)?.clone();
    let scenarios: Vec<&str> = BUNDLED.iter().map(|(file, _)| *file).collect();
    let saved = matrix_root()
        .map(|root| saved_scenarios(&root))
        .unwrap_or_default();
    let mut text = format!("Gebündelte Szenarien: {}", scenarios.join(", "));
    if !saved.is_empty() {
        text.push_str(&format!("\nGespeicherte Entwürfe: {}", saved.join(", ")));
    }
    if runs.is_empty() {
        text.push_str("\nKeine Läufe in diesem Prozess.");
    }
    for run in &runs {
        let id = run["run_id"].as_str().unwrap_or_default();
        let marker = if current.as_deref() == Some(id) {
            "*"
        } else {
            " "
        };
        text.push_str(&format!(
            "\n{marker} {id} · {} · Runde {} · {} · {}",
            run["scenario"].as_str().unwrap_or_default(),
            run["round"],
            run["phase"].as_str().unwrap_or_default(),
            run["status"].as_str().unwrap_or_default()
        ));
    }
    Ok(OpOutput {
        text,
        data: Some(
            json!({ "runs": runs, "current": current, "scenarios": scenarios, "saved": saved }),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::toks;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn parse(tokens: &[&str]) -> Result<MatrixCommand, OpError> {
        parse_command(&toks(tokens)).map(|p| p.command)
    }

    #[test]
    fn bare_and_show_are_show() -> TestResult {
        assert_eq!(parse(&[])?, MatrixCommand::Show);
        assert_eq!(parse(&["show"])?, MatrixCommand::Show);
        assert_eq!(parse(&["list"])?, MatrixCommand::List);
        assert_eq!(parse(&["replay"])?, MatrixCommand::Replay);
        assert_eq!(
            parse(&["compare", "a", "b"])?,
            MatrixCommand::Compare(vec!["a".to_owned(), "b".to_owned()])
        );
        assert!(parse(&["compare", "a"]).is_err());
        assert!(parse(&["show", "extra"]).is_err());
        assert!(parse(&["replay", "--seed", "3"]).is_err());
        assert!(parse(&["tanzen"]).is_err());
        Ok(())
    }

    /// Runde 7, Teil M: Start und Steuerung laufen über den Game Master —
    /// der Slash-Pfad weist sie mit einem Hinweis ab, statt die TUI zu
    /// blockieren.
    #[test]
    fn steering_commands_are_rejected_with_the_game_master_hint() -> TestResult {
        let cases: [&[&str]; 6] = [
            &["start", "karst-islands"],
            &["start", "veröffentlichung", "von", "harwness"],
            &["auto", "10"],
            &["step"],
            &["veto", "r1-a1"],
            &["end"],
        ];
        for sub in cases {
            let parsed = parse_command(&toks(sub))?;
            assert!(
                matches!(parsed.command, MatrixCommand::Steering(_)),
                "{sub:?}"
            );
            let error = run_command(parsed)
                .err()
                .ok_or("Steuerbefehl muss abgewiesen werden")?;
            assert!(error.to_string().contains("Game Master"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn run_flag_is_extracted_anywhere() -> TestResult {
        let parsed = parse_command(&toks(&["show", "--run=abc"]))?;
        assert_eq!(parsed.run.as_deref(), Some("abc"));
        assert_eq!(parsed.command, MatrixCommand::Show);
        let parsed = parse_command(&toks(&["--run", "xyz"]))?;
        assert_eq!(parsed.run.as_deref(), Some("xyz"));
        assert_eq!(parsed.command, MatrixCommand::Show);
        Ok(())
    }

    #[test]
    fn bundled_scenarios_resolve_by_file_and_id() -> TestResult {
        let by_file = resolve_known_scenario("karst-islands", None)?;
        assert_eq!(by_file.loaded.scenario.id(), "karst-wasserkrise");
        assert!(by_file.path.is_none());
        let by_id = resolve_known_scenario("karst-wasserkrise", None)?;
        assert_eq!(by_id.source, by_file.source);
        assert!(resolve_known_scenario("cloud-sme-2027.toml", None).is_ok());
        assert!(resolve_known_scenario("gibt-es-nicht", None).is_err());
        Ok(())
    }

    /// Runde 7, Teil M2: gespeicherte Entwürfe werden über ihren Slug
    /// gefunden; Freitext verweist auf den Entwurf; Pfade werden nie gelesen.
    #[test]
    fn saved_slug_is_resolved_and_freetext_points_to_draft() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join(SCENARIOS_DIR);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("mein-spiel.toml"), BUNDLED[0].1)?;
        let resolved = resolve_known_scenario("mein-spiel", Some(tmp.path()))?;
        assert_eq!(resolved.path, Some(dir.join("mein-spiel.toml")));
        assert_eq!(saved_scenarios(tmp.path()), vec!["mein-spiel".to_owned()]);
        let freetext = resolve_known_scenario("veröffentlichung von harwness", Some(tmp.path()))
            .err()
            .ok_or("Freitext darf kein Szenario sein")?;
        assert!(
            freetext.to_string().contains("matrix.draft_scenario"),
            "{freetext}"
        );
        let outside = tmp.path().join("fremd.toml");
        std::fs::write(&outside, BUNDLED[0].1)?;
        assert!(
            resolve_known_scenario(&outside.display().to_string(), Some(tmp.path())).is_err(),
            "Pfade außerhalb des Matrix-Speichers werden nicht gelesen"
        );
        assert!(resolve_known_scenario("../mein-spiel", Some(tmp.path())).is_err());
        Ok(())
    }

    #[test]
    fn sanitize_keeps_ids_filesystem_safe() {
        assert_eq!(sanitize("karst-wasserkrise"), "karst-wasserkrise");
        assert_eq!(sanitize("../böse"), "---b-se");
        assert_eq!(sanitize("///"), "szenario");
    }

    #[test]
    fn registry_show_busy_and_list_roundtrip() -> TestResult {
        let resolved = resolve_known_scenario("karst-islands", None)?;
        let seed = master_seed_for(&resolved.loaded, Some(1));
        let id = format!("test-registry-{}", std::process::id());
        let run = MatrixRun::start(
            resolved.loaded,
            resolved.source,
            None,
            seed,
            id.clone(),
            None,
            false,
            None,
        )?;
        put_run(run)?;

        let shown = show_output(&id)?;
        let data = shown.data.ok_or("show ohne Daten")?;
        assert_eq!(data["run_id"], json!(id));
        assert_eq!(data["scenario"], json!("karst-wasserkrise"));
        assert_eq!(data["status"], json!("running"));

        // Während eines Schritts: show liefert den Schnappschuss, ein zweiter
        // Zugriff wird abgewiesen.
        let taken = take_run(&id)?;
        let busy = show_output(&id)?.data.ok_or("busy ohne Daten")?;
        assert_eq!(busy["status"], json!("busy"));
        assert!(take_run(&id).is_err());
        assert!(with_run(&id, |_| Ok(())).is_err());
        put_run(taken)?;

        let listed = list_output()?.data.ok_or("list ohne Daten")?;
        let runs = listed["runs"].as_array().ok_or("runs")?;
        assert!(runs.iter().any(|r| r["run_id"] == json!(id)));
        lock(&RUNS)?.remove(&id);
        assert!(show_output(&id).is_err());
        Ok(())
    }
}
