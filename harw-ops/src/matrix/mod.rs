//! `/matrix` — Matrix Game live (`docs/design/matrix-game.md` §4, §7).
//!
//! # Subcommands
//! Alle Subcommands außer `start` und `list` wirken auf den aktuellen Lauf
//! oder den per `--run=<id>` gewählten.
//! - `start <szenario-id|pfad> [--seed N]` — lädt ein gebündeltes Szenario
//!   (`karst-islands`, `cloud-sme-2027`, auch über die Szenario-ID) oder eine
//!   TOML-Datei, eröffnet das Spiel (Setup) und legt
//!   `<profil>/knowledge/matrix/<szenario>/<lauf>/` mit `scenario.toml`,
//!   `journal.jsonl` und den Unterlagen-Kopien je Sitz (`materials/<sitz>/`)
//!   an. Der Lauf wird zum aktuellen Lauf.
//! - `step` — genau eine Phase (Sitz-Aufrufe über Kind-Agenten).
//! - `auto N` — bis zu `N` (1–20) Runden ohne Halt; bricht bei Spielende,
//!   Leak-Verdacht, Fehler oder `pause` ab.
//! - `pause` — hält den Lauf an (während `auto`: nach dem laufenden Schritt).
//! - `inject [--audience=<a>] [--attributed] [--effects=<json>] <text…>` —
//!   Ereignis zur nächsten Phasengrenze (`a`: `public`, `umpire`, `seat:x`,
//!   `seat+umpire:x`, `pair:a,b`; `--effects` als JSON-Liste ohne Leerzeichen).
//! - `override <json>` — vor dem Wurf: Urteilsfelder eines offenen Arguments
//!   ersetzen (`{"argument_id":"r2-a1","context_modifier":1,…}`); nach dem
//!   Wurf: `{"argument_id","effects":[…],"text"}` als markierte Korrektur.
//! - `veto <argument-id> [grund…]` — Argument in der Adjudikation verwerfen.
//! - `reveal <geheimnis|argument-id>` — geheimes Argument offenlegen.
//! - `fork <runde>` — neuer Lauf ab dem Rundenende `runde` (wird aktuell).
//! - `end` — direkt zu Schlussargumenten/AAR; schreibt `aar.md`.
//! - `replay` — prüft das Journal per Replay; `replay --seed N` startet
//!   dasselbe Szenario neu mit Seed `N`.
//! - `show` (auch bare `/matrix`) — Panel-Daten.
//! - `list` — Läufe dieses Prozesses und gebündelte Szenarien.
//!
//! Jeder Facilitator-Eingriff steht als `FacilitatorNote` (nur Beobachter)
//! im Journal.
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
//! # Laufzustand
//! Läufe leben prozessweit in `RUNS`. Für die Dauer eines Schritts wird der
//! Lauf aus der Map genommen (kein `std::sync::Mutex` über ein `await`);
//! `show` liefert dann den zuletzt gespeicherten Stand mit `status = "busy"`.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`] — Grammatik, unbekanntes Szenario/Lauf,
//!   unzulässiger Eingriff.
//! - [`OpError::NotAvailable`] — kein Agent-Spawner (nur `step`/`auto`).
//! - [`OpError::Execution`] — Kern-, Datei- oder Registry-Fehler.

pub mod runner;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use harw_macros::operation;
use harw_matrix_game::phases::{EffectOp, Phase};
use harw_matrix_game::scenario::{LoadedScenario, load_scenario};
use harw_matrix_game::state::Audience;
use harw_operations::{OpContext, OpError, OpOutput};
use serde_json::{Value, json};

use runner::{AAR_FILE, EventSink, MatrixRun, NullDriver, RunStatus, master_seed_for};

/// Obergrenze für `auto N` (Runden).
pub const MAX_AUTO_ROUNDS: u32 = 20;

/// Sicherheitsnetz: höchstens so viele Phasen je `auto`.
const MAX_AUTO_STEPS: usize = 400;

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
    /// `start <szenario> [--seed N]`.
    Start {
        /// Szenario-ID, Dateiname oder Pfad.
        scenario: String,
        /// Expliziter Seed.
        seed: Option<u64>,
        /// Inject-Paket der Szenario-Bibliothek (`--package <id>`).
        package: Option<String>,
    },
    /// `step`.
    Step,
    /// `auto N` (Runden).
    Auto(u32),
    /// `pause`.
    Pause,
    /// `inject …`.
    Inject {
        /// Text.
        text: String,
        /// Audience in Textform.
        audience: Option<String>,
        /// Mit Urheber.
        attributed: bool,
        /// Effekt-Ops als JSON-Liste.
        effects: Option<String>,
    },
    /// `override <json>`.
    Override(String),
    /// `veto <argument> [grund]`.
    Veto {
        /// Argument-ID.
        argument: String,
        /// Begründung.
        reason: String,
    },
    /// `reveal <ziel>`.
    Reveal(String),
    /// `fork <runde>`.
    Fork(u32),
    /// `end`.
    End,
    /// `replay [--seed N]`.
    Replay {
        /// Neustart mit diesem Seed.
        seed: Option<u64>,
    },
    /// `show`.
    Show,
    /// `list`.
    List,
    /// `compare <lauf> <lauf> …`: Vergleich mehrerer Läufe (Design-Lehren).
    Compare(Vec<String>),
}

/// Befehl plus optional gewählter Lauf (`--run=<id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Gewählter Lauf.
    pub run: Option<String>,
    /// Befehl.
    pub command: MatrixCommand,
}

const USAGE: &str = "start <szenario> [--seed N], step, auto N, pause, inject <text>, override <json>, veto <arg> [grund], reveal <arg>, fork <runde>, end, replay [--seed N], show, list, compare <lauf> <lauf> …";

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

fn take_switch(tokens: &mut Vec<String>, name: &str) -> bool {
    let long = format!("--{name}");
    match tokens.iter().position(|t| *t == long) {
        Some(index) => {
            tokens.remove(index);
            true
        }
        None => false,
    }
}

fn parse_seed(raw: Option<String>) -> Result<Option<u64>, OpError> {
    raw.map(|value| {
        value
            .trim()
            .parse::<u64>()
            .map_err(|_| invalid(format!("Seed `{value}` ist keine nicht-negative Ganzzahl")))
    })
    .transpose()
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
    let mut rest: Vec<String> = tokens.get(1..).unwrap_or_default().to_vec();
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
        "compare" => {
            if rest.len() < 2 {
                return Err(invalid(
                    "mindestens zwei Läufe erwartet: /matrix compare <lauf> <lauf> …",
                ));
            }
            MatrixCommand::Compare(rest)
        }
        "list" => {
            no_extra(&rest, "list")?;
            MatrixCommand::List
        }
        "start" => {
            let seed = parse_seed(take_value_flag(&mut rest, "seed")?)?;
            let package = take_value_flag(&mut rest, "package")?;
            let scenario = match rest.as_slice() {
                [one] => one.clone(),
                [] => {
                    return Err(invalid(
                        "Szenario fehlt: /matrix start <szenario-id|pfad> [--seed N] [--package ID]",
                    ));
                }
                more => {
                    return Err(invalid(format!(
                        "genau ein Szenario erwartet, erhalten: {}",
                        more.join(" ")
                    )));
                }
            };
            MatrixCommand::Start {
                scenario,
                seed,
                package,
            }
        }
        "step" => {
            no_extra(&rest, "step")?;
            MatrixCommand::Step
        }
        "auto" => {
            let n = match rest.as_slice() {
                [n] => n
                    .parse::<u32>()
                    .map_err(|_| invalid(format!("`{n}` ist keine Rundenzahl")))?,
                _ => return Err(invalid("Aufruf: /matrix auto N (1–20 Runden)")),
            };
            if !(1..=MAX_AUTO_ROUNDS).contains(&n) {
                return Err(invalid(format!(
                    "auto N: N muss zwischen 1 und {MAX_AUTO_ROUNDS} liegen"
                )));
            }
            MatrixCommand::Auto(n)
        }
        "pause" => {
            no_extra(&rest, "pause")?;
            MatrixCommand::Pause
        }
        "inject" => {
            let audience = take_value_flag(&mut rest, "audience")?;
            let effects = take_value_flag(&mut rest, "effects")?;
            let attributed = take_switch(&mut rest, "attributed");
            let text = rest.join(" ");
            if text.trim().is_empty() {
                return Err(invalid(
                    "Inject-Text fehlt: /matrix inject [--audience=<a>] [--attributed] <text>",
                ));
            }
            MatrixCommand::Inject {
                text,
                audience,
                attributed,
                effects,
            }
        }
        "override" => {
            let json = rest.join(" ");
            if json.trim().is_empty() {
                return Err(invalid(
                    "Override fehlt: /matrix override {\"argument_id\":\"r1-a1\",…}",
                ));
            }
            MatrixCommand::Override(json)
        }
        "veto" => {
            let Some(argument) = rest.first().cloned() else {
                return Err(invalid("Aufruf: /matrix veto <argument-id> [grund]"));
            };
            MatrixCommand::Veto {
                argument,
                reason: rest.get(1..).unwrap_or_default().join(" "),
            }
        }
        "reveal" => match rest.as_slice() {
            [target] => MatrixCommand::Reveal(target.clone()),
            _ => return Err(invalid("Aufruf: /matrix reveal <geheimnis|argument-id>")),
        },
        "fork" => match rest.as_slice() {
            [round] => MatrixCommand::Fork(
                round
                    .parse::<u32>()
                    .map_err(|_| invalid(format!("`{round}` ist keine Runde")))?,
            ),
            _ => return Err(invalid("Aufruf: /matrix fork <runde>")),
        },
        "end" => {
            no_extra(&rest, "end")?;
            MatrixCommand::End
        }
        "replay" => {
            let seed = parse_seed(take_value_flag(&mut rest, "seed")?)?;
            no_extra(&rest, "replay")?;
            MatrixCommand::Replay { seed }
        }
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
/// Aktueller Lauf.
static CURRENT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
/// Läufe, die gerade einen Schritt ausführen (Lauf-ID → letzter `show`-Stand).
static BUSY: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
/// Vorgemerkte Pausen laufender `auto`-Schleifen.
static PAUSE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
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
        invalid("kein aktueller Matrix-Lauf — erst /matrix start <szenario> (oder --run=<id>)")
    })
}

fn set_current(id: &str) -> Result<(), OpError> {
    *lock(&CURRENT)? = Some(id.to_owned());
    Ok(())
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
            "Lauf `{id}` führt gerade einen Schritt aus — bitte warten oder /matrix pause"
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

fn pause_requested(id: &str) -> Result<bool, OpError> {
    Ok(lock(&PAUSE)?.remove(id))
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

/// Gebündeltes Szenario (Dateiname oder ID) oder TOML-Datei.
fn resolve_scenario(name: &str) -> Result<ResolvedScenario, OpError> {
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
    let path = Path::new(name);
    if path.is_file() {
        let source = std::fs::read_to_string(path)
            .map_err(|e| invalid(format!("Szenario `{name}` nicht lesbar: {e}")))?;
        let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        return load(source, Some(absolute));
    }
    let bundled: Vec<&str> = BUNDLED.iter().map(|(file, _)| *file).collect();
    Err(invalid(format!(
        "unbekanntes Szenario `{name}` (gebündelt: {}; oder Pfad zu einer TOML-Datei)",
        bundled.join(", ")
    )))
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

/// Dateisystemtauglicher Name (ASCII-Alphanumerik, `-`, `_`, `.`).
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

/// Führt `/matrix` aus.
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "matrix",
    summary = "Matrix Game: start, step, auto, pause, inject, override, veto, reveal, fork, end, replay, show, list, compare.",
    domain = "knowledge",
    permission = "operator",
    command(
        path = "/matrix",
        visibility = "channel_reduced",
        busy_subcommands = "show=immediate, list=immediate"
    )
)]
async fn matrix(ctx: &OpContext, args: MatrixArgs) -> Result<OpOutput, OpError> {
    let parsed = parse_command(&args.tokens)?;
    run_command(ctx, parsed).await
}

async fn run_command(ctx: &OpContext, parsed: ParsedCommand) -> Result<OpOutput, OpError> {
    let ParsedCommand { run, command } = parsed;
    match command {
        MatrixCommand::List => list_output(),
        MatrixCommand::Compare(ids) => compare_output(&ids),
        MatrixCommand::Show => show_output(&current_id(run)?),
        MatrixCommand::Start {
            scenario,
            seed,
            package,
        } => {
            let resolved = resolve_scenario(&scenario)?;
            start_run(ctx, resolved, seed, package.as_deref(), None)
        }
        MatrixCommand::Step => step_command(ctx, &current_id(run)?).await,
        MatrixCommand::Auto(rounds) => auto_command(ctx, &current_id(run)?, rounds).await,
        MatrixCommand::Pause => pause_command(&current_id(run)?),
        MatrixCommand::Inject {
            text,
            audience,
            attributed,
            effects,
        } => {
            let audience = match audience {
                Some(raw) => Audience::parse(&raw).map_err(invalid)?,
                None => Audience::Public,
            };
            let effects: Vec<EffectOp> = match effects {
                Some(raw) => serde_json::from_str(&raw).map_err(|e| {
                    invalid(format!("`--effects` ist keine gültige Effektliste: {e}"))
                })?,
                None => Vec::new(),
            };
            let id = current_id(run)?;
            facilitator(&id, |r| {
                let inject = r.queue_inject(&text, &audience, attributed, effects)?;
                Ok(format!(
                    "Inject `{inject}` vorgemerkt — wirkt an der nächsten Phasengrenze."
                ))
            })
        }
        MatrixCommand::Override(raw) => facilitator(&current_id(run)?, |r| r.apply_override(&raw)),
        MatrixCommand::Veto { argument, reason } => {
            facilitator(&current_id(run)?, |r| r.queue_veto(&argument, &reason))
        }
        MatrixCommand::Reveal(target) => facilitator(&current_id(run)?, |r| r.reveal(&target)),
        MatrixCommand::Fork(round) => fork_command(ctx, &current_id(run)?, round),
        MatrixCommand::End => end_command(ctx, &current_id(run)?).await,
        MatrixCommand::Replay { seed } => replay_command(ctx, &current_id(run)?, seed),
    }
}

fn output(run: &MatrixRun, text: String) -> OpOutput {
    OpOutput {
        text,
        data: Some(run.show_json()),
    }
}

/// Synchroner Facilitator-Eingriff mit Panel-Daten.
fn facilitator(
    id: &str,
    action: impl FnOnce(&mut MatrixRun) -> Result<String, OpError>,
) -> Result<OpOutput, OpError> {
    with_run(id, |run| {
        let text = action(run)?;
        Ok(output(run, format!("{}\n{text}", run.headline())))
    })
}

fn start_run(
    ctx: &OpContext,
    resolved: ResolvedScenario,
    seed: Option<u64>,
    package: Option<&str>,
    note: Option<String>,
) -> Result<OpOutput, OpError> {
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
        package,
    )?;
    if let Some(note) = note {
        run.note("replay", note)?;
    }
    run.set_sink(EventSink::from_ctx(ctx));
    run.flush()?;
    let mut text = format!(
        "{}\nSeed {}… · Verzeichnis {}\nWeiter mit /matrix step (eine Phase) oder /matrix auto N.",
        run.headline(),
        run.seed_prefix(),
        dir.display()
    );
    for warning in warnings {
        text.push_str(&format!("\nHinweis: {warning}"));
    }
    let out = output(&run, text);
    put_run(run)?;
    set_current(&run_id)?;
    Ok(out)
}

async fn step_command(ctx: &OpContext, id: &str) -> Result<OpOutput, OpError> {
    let mut run = take_run(id)?;
    let result = step_once(ctx, &mut run, "step").await;
    let out =
        result.map(|report| output(&run, format!("{}\n{}", run.headline(), report.summary())));
    put_run(run)?;
    out
}

async fn step_once(
    ctx: &OpContext,
    run: &mut MatrixRun,
    label: &str,
) -> Result<runner::StepReport, OpError> {
    run.set_sink(EventSink::from_ctx(ctx));
    run.set_status(RunStatus::Running);
    let cursor = run.cursor();
    run.note(
        label,
        format!("ab Runde {}, Phase {}", cursor.round, cursor.phase.label()),
    )?;
    run.step(ctx).await
}

async fn auto_command(ctx: &OpContext, id: &str, rounds: u32) -> Result<OpOutput, OpError> {
    let mut run = take_run(id)?;
    // Eine alte, nie abgeholte Pause gilt nicht für diesen Lauf.
    let _ = pause_requested(id);
    let result = auto_loop(ctx, &mut run, rounds).await;
    let out = result.map(|lines| output(&run, format!("{}\n{}", run.headline(), lines.join("\n"))));
    put_run(run)?;
    out
}

async fn auto_loop(
    ctx: &OpContext,
    run: &mut MatrixRun,
    rounds: u32,
) -> Result<Vec<String>, OpError> {
    run.note("auto", format!("{rounds} Runde(n)"))?;
    let mut lines = Vec::new();
    let mut closed = 0;
    for _ in 0..MAX_AUTO_STEPS {
        if pause_requested(run.run_id())? {
            run.set_status(RunStatus::Paused);
            run.note("pause", "Auto angehalten")?;
            run.flush()?;
            lines.push("Pause: Auto angehalten.".to_owned());
            break;
        }
        let report = step_once(ctx, run, "auto-step").await?;
        lines.push(report.summary());
        if report.ended {
            break;
        }
        if report.leaks > 0 {
            run.set_status(RunStatus::Paused);
            run.note("pause", "Auto wegen Leak-Verdacht angehalten")?;
            run.flush()?;
            lines.push(
                "Auto angehalten: Leak-Verdacht — bitte Beobachter-Protokoll prüfen.".to_owned(),
            );
            break;
        }
        if report.phase == Phase::Rundenende {
            closed += 1;
            if closed >= rounds {
                break;
            }
        }
    }
    Ok(lines)
}

fn pause_command(id: &str) -> Result<OpOutput, OpError> {
    if is_busy(id)? {
        lock(&PAUSE)?.insert(id.to_owned());
        let snapshot = lock(&BUSY)?.get(id).cloned();
        return Ok(OpOutput {
            text: format!(
                "Pause für `{id}` vorgemerkt — der Lauf hält nach dem laufenden Schritt an."
            ),
            data: snapshot,
        });
    }
    facilitator(id, |run| {
        run.set_status(RunStatus::Paused);
        run.note("pause", "Facilitator")?;
        run.flush()?;
        Ok("Lauf angehalten. /matrix step oder /matrix auto N setzt fort.".to_owned())
    })
}

async fn end_command(ctx: &OpContext, id: &str) -> Result<OpOutput, OpError> {
    let mut run = take_run(id)?;
    let result = end_loop(ctx, &mut run).await;
    let out = result.map(|lines| {
        let mut text = format!("{}\n{}", run.headline(), lines.join("\n"));
        if let Some(dir) = run.run_dir() {
            text.push_str(&format!("\nAAR: {}", dir.join(AAR_FILE).display()));
        }
        output(&run, text)
    });
    put_run(run)?;
    out
}

async fn end_loop(ctx: &OpContext, run: &mut MatrixRun) -> Result<Vec<String>, OpError> {
    if run.status() == RunStatus::Ended {
        return Err(invalid(format!(
            "Lauf `{}` ist bereits beendet",
            run.run_id()
        )));
    }
    run.set_sink(EventSink::from_ctx(ctx));
    run.request_end()?;
    let mut lines = Vec::new();
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

fn fork_command(ctx: &OpContext, id: &str, round: u32) -> Result<OpOutput, OpError> {
    let root = matrix_root()?;
    let forked = with_run(id, |run| {
        let (new_id, dir) = new_run_location(&root, run.loaded().scenario.id());
        run.fork(round, new_id, Some(dir))
    })?;
    let mut forked = forked;
    forked.set_sink(EventSink::from_ctx(ctx));
    forked.flush()?;
    let new_id = forked.run_id().to_owned();
    let out = output(
        &forked,
        format!(
            "{}\nFork von `{id}` ab Rundenende {round}; der neue Lauf ist jetzt aktuell.",
            forked.headline()
        ),
    );
    put_run(forked)?;
    set_current(&new_id)?;
    Ok(out)
}

fn replay_command(ctx: &OpContext, id: &str, seed: Option<u64>) -> Result<OpOutput, OpError> {
    let Some(seed) = seed else {
        return facilitator(id, |run| run.verify_replay());
    };
    let (source, path) = with_run(id, |run| {
        Ok((
            run.source().to_owned(),
            run.scenario_path().map(Path::to_path_buf),
        ))
    })?;
    let resolved = load(source, path)?;
    start_run(
        ctx,
        resolved,
        Some(seed),
        None,
        Some(format!("Neustart von `{id}` mit Seed {seed}")),
    )
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
    let mut text = format!("Gebündelte Szenarien: {}", scenarios.join(", "));
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
        data: Some(json!({ "runs": runs, "current": current, "scenarios": scenarios })),
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
        Ok(())
    }

    #[test]
    fn start_with_seed_forms() -> TestResult {
        assert_eq!(
            parse(&["start", "karst-islands"])?,
            MatrixCommand::Start {
                scenario: "karst-islands".to_owned(),
                seed: None,
                package: None
            }
        );
        let expected = MatrixCommand::Start {
            scenario: "karst-islands".to_owned(),
            seed: Some(42),
            package: None,
        };
        assert_eq!(
            parse(&["start", "karst-islands", "--seed", "42"])?,
            expected
        );
        assert_eq!(parse(&["start", "--seed=42", "karst-islands"])?, expected);
        assert_eq!(
            parse(&["start", "karst-islands", "--package", "sturm"])?,
            MatrixCommand::Start {
                scenario: "karst-islands".to_owned(),
                seed: None,
                package: Some("sturm".to_owned()),
            }
        );
        assert_eq!(
            parse(&["compare", "a", "b"])?,
            MatrixCommand::Compare(vec!["a".to_owned(), "b".to_owned()])
        );
        assert!(parse(&["compare", "a"]).is_err());
        assert!(parse(&["start"]).is_err());
        assert!(parse(&["start", "a", "b"]).is_err());
        assert!(parse(&["start", "a", "--seed", "x"]).is_err());
        assert!(parse(&["start", "a", "--seed"]).is_err());
        Ok(())
    }

    #[test]
    fn auto_is_bounded() -> TestResult {
        assert_eq!(parse(&["auto", "3"])?, MatrixCommand::Auto(3));
        assert_eq!(parse(&["auto", "20"])?, MatrixCommand::Auto(20));
        assert!(parse(&["auto", "0"]).is_err());
        assert!(parse(&["auto", "21"]).is_err());
        assert!(parse(&["auto"]).is_err());
        assert!(parse(&["auto", "viel"]).is_err());
        Ok(())
    }

    #[test]
    fn facilitator_commands_parse() -> TestResult {
        assert_eq!(parse(&["step"])?, MatrixCommand::Step);
        assert_eq!(parse(&["pause"])?, MatrixCommand::Pause);
        assert_eq!(parse(&["end"])?, MatrixCommand::End);
        assert_eq!(
            parse(&[
                "inject",
                "--audience=seat:rat",
                "--attributed",
                "Sturm",
                "zieht",
                "auf"
            ])?,
            MatrixCommand::Inject {
                text: "Sturm zieht auf".to_owned(),
                audience: Some("seat:rat".to_owned()),
                attributed: true,
                effects: None,
            }
        );
        assert!(parse(&["inject"]).is_err());
        assert_eq!(
            parse(&["override", "{\"argument_id\":", "\"r1-a1\"}"])?,
            MatrixCommand::Override("{\"argument_id\": \"r1-a1\"}".to_owned())
        );
        assert!(parse(&["override"]).is_err());
        assert_eq!(
            parse(&["veto", "r1-a2", "zu", "vage"])?,
            MatrixCommand::Veto {
                argument: "r1-a2".to_owned(),
                reason: "zu vage".to_owned()
            }
        );
        assert!(parse(&["veto"]).is_err());
        assert_eq!(
            parse(&["reveal", "s1"])?,
            MatrixCommand::Reveal("s1".to_owned())
        );
        assert!(parse(&["reveal"]).is_err());
        assert_eq!(parse(&["fork", "2"])?, MatrixCommand::Fork(2));
        assert!(parse(&["fork", "zwei"]).is_err());
        assert_eq!(parse(&["replay"])?, MatrixCommand::Replay { seed: None });
        assert_eq!(
            parse(&["replay", "--seed", "9"])?,
            MatrixCommand::Replay { seed: Some(9) }
        );
        assert!(parse(&["step", "extra"]).is_err());
        assert!(parse(&["tanzen"]).is_err());
        Ok(())
    }

    #[test]
    fn run_flag_is_extracted_anywhere() -> TestResult {
        let parsed = parse_command(&toks(&["step", "--run=abc"]))?;
        assert_eq!(parsed.run.as_deref(), Some("abc"));
        assert_eq!(parsed.command, MatrixCommand::Step);
        let parsed = parse_command(&toks(&["--run", "xyz"]))?;
        assert_eq!(parsed.run.as_deref(), Some("xyz"));
        assert_eq!(parsed.command, MatrixCommand::Show);
        Ok(())
    }

    #[test]
    fn bundled_scenarios_resolve_by_file_and_id() -> TestResult {
        let by_file = resolve_scenario("karst-islands")?;
        assert_eq!(by_file.loaded.scenario.id(), "karst-wasserkrise");
        assert!(by_file.path.is_none());
        let by_id = resolve_scenario("karst-wasserkrise")?;
        assert_eq!(by_id.source, by_file.source);
        assert!(resolve_scenario("cloud-sme-2027.toml").is_ok());
        assert!(resolve_scenario("gibt-es-nicht").is_err());
        Ok(())
    }

    #[test]
    fn sanitize_keeps_ids_filesystem_safe() {
        assert_eq!(sanitize("karst-wasserkrise"), "karst-wasserkrise");
        assert_eq!(sanitize("../böse"), "---b-se");
        assert_eq!(sanitize("///"), "szenario");
    }

    #[test]
    fn registry_show_busy_and_facilitator_roundtrip() -> TestResult {
        let resolved = resolve_scenario("karst-islands")?;
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

        let inject = facilitator(&id, |r| {
            r.queue_inject("Sturm", &Audience::Public, false, Vec::new())
        })?;
        assert!(inject.text.contains("facilitator-1"));

        // Während eines Schritts: show liefert den Schnappschuss, Eingriffe
        // werden abgewiesen, pause wird vorgemerkt.
        let taken = take_run(&id)?;
        let busy = show_output(&id)?.data.ok_or("busy ohne Daten")?;
        assert_eq!(busy["status"], json!("busy"));
        assert!(facilitator(&id, |r| r.reveal("s1")).is_err());
        assert!(take_run(&id).is_err());
        pause_command(&id)?;
        assert!(pause_requested(&id)?);
        put_run(taken)?;

        let listed = list_output()?.data.ok_or("list ohne Daten")?;
        let runs = listed["runs"].as_array().ok_or("runs")?;
        assert!(runs.iter().any(|r| r["run_id"] == json!(id)));
        lock(&RUNS)?.remove(&id);
        assert!(show_output(&id).is_err());
        Ok(())
    }
}
