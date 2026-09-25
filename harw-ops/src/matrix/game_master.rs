//! Modell-Werkzeuge des Game Masters (`matrix-game-master`, Runde 7 Teil M).
//!
//! # Verantwortungsbereich
//! Der Game Master ist ein spezialisierter Orchestrator: die UIA startet ihn
//! per `transfer_to_matrix-game-master` als Hintergrund-Kind, er entwirft aus
//! dem Freitext der Nutzerin ein Szenario, lässt es freigeben, spielt es mit
//! dem vorhandenen Runner durch (Sitz-Agenten als seine Kinder) und liefert
//! AAR plus `report.md`. Seine Werkzeugfläche sind genau diese fünf
//! Operationen:
//!
//! | Werkzeug | Wirkung | Freigabe |
//! |---|---|---|
//! | [`MATRIX_DRAFT_SCENARIO`] | validiert ein Szenario-TOML und speichert es unter `<profil>/knowledge/matrix/scenarios/<slug>.toml` | keine (nur Matrix-Speicher) |
//! | [`MATRIX_STATUS`] | Stand eines Laufs, Liste, oder Quelltext eines Beispiel-/Entwurfsszenarios | keine (lesend) |
//! | [`MATRIX_START`] | eröffnet einen Lauf (Setup, Laufverzeichnis) | immer |
//! | [`MATRIX_RUN`] | spielt Runden mit Sitz-Agenten (Kosten, Budget) | immer |
//! | [`MATRIX_FINISH`] | Schlussargumente, AAR, `report.md`, Kopie in den Workspace | immer |
//! | [`MATRIX_ADD_FACT`] | recherchierter Fakt mit Belegen als öffentliche Lage (Plan R9) | keine (nur Journal des eigenen Laufs) |
//!
//! # Recherche (Plan R9)
//! Der Game Master grundiert ein Szenario **vor** dem Entwurf mit einer
//! Recherche-Welle (Web über `intel-web-researcher`, Repo/Git über
//! `evidence-collector`, Prüfung über `evidence-critic`) und kann **zwischen**
//! Runden eine eng gefasste Frage recherchieren lassen („Recherche-Inject“).
//! Das Ergebnis trägt [`MATRIX_ADD_FACT`] als `FactAdded` mit `sources` ins
//! Journal ein; Sitz-Projektionen, Live-Events und `report.md` zeigen es als
//! „… (Quelle: URL, abgerufen …)“. Die Sitze selbst bleiben offline.
//!
//! # Warum Operationen mit `model_tool`, aber nicht in `register_all`
//! Die Operationen tragen ihre Freigabe ehrlich im `#[operation]`-Attribut.
//! Sie stehen bewusst **nicht** in [`crate::register_all`]: sonst bekäme die
//! UIA-Wurzel sie über ihren allgemeinen `ModelToolProvider` ebenfalls. Die
//! Kind-Registry-Fabrik (`harw-runtime/src/children.rs`) hängt sie über einen
//! rollengebundenen `ModelToolProvider` ausschließlich an die Rolle
//! `matrix-game-master` — dasselbe Muster wie `delegate_wave`.
//!
//! # Sicherheit
//! - Kein Werkzeug liest beliebige Dateien: Szenarien kommen nur aus dem
//!   Bündel oder dem Matrix-Speicher des Profils (Slug-Grammatik, keine
//!   Pfade).
//! - Geschrieben wird nur in den Matrix-Speicher des Profils und — von
//!   [`MATRIX_FINISH`], freigabepflichtig — genau eine neue Datei
//!   `<workspace>/matrix/<szenario>-<lauf>.md` (nie überschreibend, kein
//!   Symlink-Ziel). Das ist eine enge, dokumentierte Ausnahme wie `plan.write`;
//!   die Sandbox des Game Masters selbst bleibt nur lesend und ohne Netz.
//! - Würfel wirft ausschließlich die Engine (`harw-matrix-game`), nie das
//!   Modell.
//!
//! # Nicht blockierend
//! Der Game Master läuft als Hintergrund-Kind der UIA; ein langer
//! `matrix.run` hält nur seinen eigenen Turn auf. Jeder Sitz-Aufruf hat ein
//! hartes Zeitlimit ([`super::runner::SEAT_TURN_TIMEOUT`]), ein Abbruch des
//! Game-Master-Turns (`agent.cancel`) bricht laufende Sitz-Aufrufe ab, und
//! ein `matrix.run`-Aufruf endet spätestens nach [`RUN_CALL_BUDGET`] an der
//! nächsten Phasengrenze.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use harw_macros::operation;
use harw_matrix_game::scenario::{LoadedScenario, Scenario, load_scenario};
use harw_operations::operation::Operation;
use harw_operations::{OpContext, OpError, OpOutput};
use serde_json::json;

use super::runner::{AAR_FILE, RunStatus};
use super::{
    BUNDLED, MAX_AUTO_ROUNDS, SCENARIOS_DIR, StopReason, advance, end_loop, invalid, matrix_root,
    output, put_run, resolve_known_scenario, sanitize, saved_scenarios, session_run_id,
    show_output, start_run, take_run, with_run,
};

/// Werkzeugname: Szenario entwerfen/validieren/speichern.
pub const MATRIX_DRAFT_SCENARIO: &str = "matrix.draft_scenario";
/// Werkzeugname: Lauf eröffnen.
pub const MATRIX_START: &str = "matrix.start";
/// Werkzeugname: Runden spielen.
pub const MATRIX_RUN: &str = "matrix.run";
/// Werkzeugname: Stand, Liste oder Beispielszenario.
pub const MATRIX_STATUS: &str = "matrix.status";
/// Werkzeugname: Spiel abschließen (AAR, Bericht).
pub const MATRIX_FINISH: &str = "matrix.finish";
/// Werkzeugname: recherchierten Fakt mit Belegen eintragen (Plan R9).
pub const MATRIX_ADD_FACT: &str = "matrix.add_fact";

/// Zeitbudget eines `matrix.run`-Aufrufs: danach endet er an der nächsten
/// Phasengrenze und meldet „fortsetzen mit matrix.run“.
pub const RUN_CALL_BUDGET: Duration = Duration::from_secs(25 * 60);

/// Obergrenze eines gespeicherten Szenario-Quelltexts.
const MAX_SCENARIO_BYTES: usize = 256 * 1024;

/// Unterordner der Workspace-Kopie des Berichts.
pub const WORKSPACE_REPORT_DIR: &str = "matrix";

/// Die fünf Game-Master-Operationen in Werkzeugreihenfolge — Eingabe des
/// rollengebundenen `ModelToolProvider` in `harw-runtime/src/children.rs`.
#[must_use]
pub fn game_master_operations() -> Vec<Arc<dyn Operation>> {
    let draft: Arc<dyn Operation> = Arc::new(MatrixDraftScenarioOperation);
    let status: Arc<dyn Operation> = Arc::new(MatrixStatusOperation);
    let start: Arc<dyn Operation> = Arc::new(MatrixStartOperation);
    let run: Arc<dyn Operation> = Arc::new(MatrixRunOperation);
    let finish: Arc<dyn Operation> = Arc::new(MatrixFinishOperation);
    let add_fact: Arc<dyn Operation> = Arc::new(MatrixAddFactOperation);
    vec![draft, status, start, run, finish, add_fact]
}

fn model_only(name: &str) -> OpError {
    OpError::InvalidArguments(format!(
        "`{name}` ist nur als Modell-Werkzeug des Game Masters verfügbar"
    ))
}

// ── Argumente ────────────────────────────────────────────────────────────────

/// Argumente von `matrix.draft_scenario`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct DraftScenarioArgs {
    /// Vollständiges Szenario im Format `harwness.matrix-scenario/v1` (TOML).
    #[serde(default)]
    pub toml: String,
    /// Kurzname für die Ablage (a-z, 0-9, `-`, `_`); Vorgabe: Szenario-ID.
    #[serde(default)]
    pub slug: Option<String>,
    /// Einen eigenen, bereits gespeicherten Entwurf gleichen Namens ersetzen.
    #[serde(default)]
    pub overwrite: Option<bool>,
}

/// Argumente von `matrix.status`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct StatusArgs {
    /// Lauf-ID; Vorgabe: der zuletzt von dieser Sitzung gestartete Lauf.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Statt eines Laufs den TOML-Quelltext eines Beispiels
    /// (`karst-islands`, `cloud-sme-2027`) oder eines gespeicherten Entwurfs.
    #[serde(default)]
    pub example: Option<String>,
}

/// Argumente von `matrix.start`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct StartArgs {
    /// Slug eines gespeicherten Entwurfs oder eines gebündelten Szenarios.
    #[serde(default)]
    pub scenario: String,
    /// Optionaler Seed (reproduzierbare Würfel); Vorgabe: Szenario-Seed.
    #[serde(default)]
    pub seed: Option<u64>,
}

/// Argumente von `matrix.run`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct RunArgs {
    /// Lauf-ID; Vorgabe: der zuletzt von dieser Sitzung gestartete Lauf.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Zu spielende Runden (1–20, Vorgabe 1); endet früher bei Spielende.
    #[serde(default)]
    pub rounds: Option<u32>,
}

/// Argumente von `matrix.finish`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct FinishArgs {
    /// Lauf-ID; Vorgabe: der zuletzt von dieser Sitzung gestartete Lauf.
    #[serde(default)]
    pub run_id: Option<String>,
}

/// Argumente von `matrix.add_fact` (Plan R9).
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct AddFactArgs {
    /// Lauf-ID; Vorgabe: der zuletzt von dieser Sitzung gestartete Lauf.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Der Fakt in einem bis drei Sätzen, nur so weit die Belege tragen.
    #[serde(default)]
    pub text: String,
    /// 1–5 Belege, je `"<URL oder pfad:zeile> | <Abrufdatum JJJJ-MM-TT> | <Einstufung, optional, z. B. B2>"`.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// Zerlegt die Belege aus [`AddFactArgs::sources`]
/// (`"<fundstelle> | <abrufdatum> | <einstufung?>"`).
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn ein Beleg Fundstelle oder
/// Abrufdatum vermissen lässt.
fn parse_fact_sources(raw: &[String]) -> Result<Vec<harw_matrix_game::state::FactSource>, OpError> {
    raw.iter()
        .map(|entry| {
            let mut parts = entry.split('|').map(str::trim);
            let url = parts.next().unwrap_or_default();
            let retrieved = parts.next().unwrap_or_default();
            let rating = parts.next().filter(|rating| !rating.is_empty());
            if url.is_empty() || retrieved.is_empty() {
                return Err(invalid(format!(
                    "Beleg `{entry}` unvollständig — Form: \"<URL oder pfad:zeile> | <Abrufdatum> | <Einstufung>\""
                )));
            }
            Ok(harw_matrix_game::state::FactSource {
                url: url.to_owned(),
                retrieved: retrieved.to_owned(),
                rating: rating.map(str::to_owned),
            })
        })
        .collect()
}

macro_rules! model_only_raw_args {
    ($ty:ty, $name:expr) => {
        impl harw_operations::FromRawArgs for $ty {
            fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
                Err(model_only($name))
            }
        }
    };
}

model_only_raw_args!(DraftScenarioArgs, MATRIX_DRAFT_SCENARIO);
model_only_raw_args!(StatusArgs, MATRIX_STATUS);
model_only_raw_args!(StartArgs, MATRIX_START);
model_only_raw_args!(RunArgs, MATRIX_RUN);
model_only_raw_args!(FinishArgs, MATRIX_FINISH);
model_only_raw_args!(AddFactArgs, MATRIX_ADD_FACT);

// ── Szenario-Entwurf ─────────────────────────────────────────────────────────

/// Validiert `source` und speichert es als `<root>/scenarios/<slug>.toml`.
///
/// # Rückgabe
/// Pfad und geladenes Szenario.
///
/// # Errors
/// [`OpError::InvalidArguments`] mit den Validator-Befunden (der Game Master
/// korrigiert und ruft erneut auf), bei ungültigem Slug, bei Namensgleichheit
/// mit einem gebündelten Szenario oder einem vorhandenen Entwurf ohne
/// `overwrite`; [`OpError::Execution`] bei Dateifehlern.
fn save_draft(
    root: &Path,
    source: &str,
    slug: Option<&str>,
    overwrite: bool,
) -> Result<(PathBuf, LoadedScenario), OpError> {
    let source = source.trim();
    if source.is_empty() {
        return Err(invalid(
            "`toml` fehlt — vollständiges Szenario im Format harwness.matrix-scenario/v1 übergeben \
             (Beispiele: matrix.status {\"example\": \"karst-islands\"})",
        ));
    }
    if source.len() > MAX_SCENARIO_BYTES {
        return Err(invalid(format!(
            "Szenario ist größer als {} KiB",
            MAX_SCENARIO_BYTES / 1024
        )));
    }
    let loaded = load_scenario(source).map_err(|error| {
        invalid(format!(
            "Szenario ungültig — bitte korrigieren und matrix.draft_scenario erneut aufrufen:\n{error}"
        ))
    })?;
    let wanted = slug
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| loaded.scenario.id())
        .to_owned();
    let slug = sanitize(&wanted).to_lowercase();
    if slug != wanted.to_lowercase() {
        return Err(invalid(format!(
            "Slug `{wanted}` enthält unzulässige Zeichen (erlaubt: a-z, 0-9, `-`, `_`; Vorschlag: `{slug}`)"
        )));
    }
    if BUNDLED.iter().any(|(file, _)| *file == slug) {
        return Err(invalid(format!(
            "`{slug}` ist ein gebündeltes Szenario — anderen Slug wählen"
        )));
    }
    let dir = root.join(SCENARIOS_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| OpError::Execution(format!("`{}` nicht anlegbar: {e}", dir.display())))?;
    let path = dir.join(format!("{slug}.toml"));
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(OpError::Execution(format!(
                "`{}` ist ein Symlink — wird nicht beschrieben",
                path.display()
            )));
        }
        Ok(_) if !overwrite => {
            return Err(invalid(format!(
                "Entwurf `{slug}` existiert bereits — `overwrite: true` zum Ersetzen oder anderen Slug"
            )));
        }
        _ => {}
    }
    let mut text = source.to_owned();
    text.push('\n');
    std::fs::write(&path, text)
        .map_err(|e| OpError::Execution(format!("`{}` nicht schreibbar: {e}", path.display())))?;
    Ok((path, loaded))
}

/// Lesbare Zusammenfassung eines Szenarios zur Freigabe: Akteure, Ziele,
/// Ressourcen, Regeln, Runden.
#[must_use]
pub fn scenario_summary(loaded: &LoadedScenario) -> String {
    let scenario = &loaded.scenario;
    let rules = scenario.rules();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Szenario „{}“ (`{}`, Modus {:?}, {} Runde(n))",
        scenario.title(),
        scenario.id(),
        scenario.mode(),
        scenario.rounds()
    );
    let _ = writeln!(out, "Zweck: {}", scenario.purpose().trim());
    let situation = scenario.public_situation();
    if !situation.trim().is_empty() {
        let _ = writeln!(
            out,
            "Ausgangslage: {}",
            situation.trim().replace('\n', " / ")
        );
    }
    out.push_str("Akteure:\n");
    for faction in scenario.factions() {
        let _ = writeln!(out, "- {} (`{}`)", faction.name, faction.id);
        if !faction.public_goals.is_empty() {
            let _ = writeln!(out, "  Ziele: {}", faction.public_goals.join("; "));
        }
        if !faction.secret_goals.is_empty() {
            let _ = writeln!(out, "  Geheime Ziele: {}", faction.secret_goals.join("; "));
        }
        if !faction.assets.is_empty() {
            let _ = writeln!(out, "  Ressourcen: {}", faction.assets.join(", "));
        }
        if let Some(behavior) = &faction.behavior {
            let _ = writeln!(
                out,
                "  Verhalten: {} Regel(n), Risiko {}, {} rote Linie(n)",
                behavior.rules.len(),
                behavior.risk_label(),
                behavior.red_lines.len()
            );
        }
    }
    let _ = writeln!(
        out,
        "Regeln: Adjudikation {:?}, max. Track-Schritt {}, Fail-Chits {}, höchstens {} geheime(s) Argument(e) je Sitz",
        rules.adjudication,
        rules.max_track_step,
        if rules.fail_chits { "an" } else { "aus" },
        rules.max_secret_arguments_per_seat
    );
    if let Scenario::Business(b) = scenario {
        if let Some(model) = &b.business.market_model {
            let _ = writeln!(out, "Marktmodell: {} (β = {})", model.kind, model.beta);
        }
        for rule in &b.business.rules {
            let _ = writeln!(
                out,
                "Geschäftsregel `{}`: wenn {} → {}",
                rule.id, rule.when, rule.effect
            );
        }
    }
    if scenario.red_cell().is_some() {
        out.push_str("Red Cell: aktiv\n");
    }
    for warning in &loaded.warnings {
        let _ = writeln!(out, "Hinweis: {warning}");
    }
    out
}

// ── Operationen ──────────────────────────────────────────────────────────────

/// `matrix.draft_scenario`: Szenario validieren und im Matrix-Speicher ablegen.
///
/// # Errors
/// Siehe [`save_draft`].
#[operation(
    name = "matrix.draft_scenario",
    summary = "Validiert ein Matrix-Game-Szenario (TOML, harwness.matrix-scenario/v1) und speichert es als Entwurf; liefert die Zusammenfassung zur Freigabe. Bei Befunden korrigieren und erneut aufrufen.",
    domain = "knowledge",
    permission = "operator",
    model_tool(approval = "none")
)]
async fn matrix_draft_scenario(
    _ctx: &OpContext,
    args: DraftScenarioArgs,
) -> Result<OpOutput, OpError> {
    let root = matrix_root()?;
    let (path, loaded) = save_draft(
        &root,
        &args.toml,
        args.slug.as_deref(),
        args.overwrite.unwrap_or(false),
    )?;
    let slug = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned();
    let text = format!(
        "{}\nGespeichert als Entwurf `{slug}` ({}).\nNächster Schritt: diese Zusammenfassung der Nutzerin zur Freigabe vorlegen; erst danach matrix.start {{\"scenario\": \"{slug}\"}}.",
        scenario_summary(&loaded),
        path.display()
    );
    Ok(OpOutput {
        text,
        data: Some(json!({ "slug": slug, "path": path.display().to_string() })),
    })
}

/// `matrix.status`: Stand eines Laufs, Liste der Szenarien oder Quelltext
/// eines Beispiels.
///
/// # Errors
/// [`OpError::InvalidArguments`] bei unbekanntem Beispiel oder Lauf.
#[operation(
    name = "matrix.status",
    summary = "Zeigt den Stand eines Matrix-Game-Laufs, die bekannten Szenarien oder (mit `example`) den TOML-Quelltext eines Beispiel- oder Entwurfsszenarios.",
    domain = "knowledge",
    permission = "operator",
    model_tool(readonly, approval = "none")
)]
async fn matrix_status(ctx: &OpContext, args: StatusArgs) -> Result<OpOutput, OpError> {
    if let Some(example) = args.example.as_deref().filter(|e| !e.trim().is_empty()) {
        let root = matrix_root().ok();
        let resolved = resolve_known_scenario(example, root.as_deref())?;
        return Ok(OpOutput {
            text: resolved.source,
            data: None,
        });
    }
    match session_run_id(args.run_id, ctx.session_id().as_str()) {
        Ok(id) => show_output(&id),
        Err(_) => {
            let saved = matrix_root()
                .map(|root| saved_scenarios(&root))
                .unwrap_or_default();
            let bundled: Vec<&str> = BUNDLED.iter().map(|(file, _)| *file).collect();
            Ok(OpOutput {
                text: format!(
                    "Noch kein Lauf in dieser Sitzung.\nBeispiele: {}\nEntwürfe: {}",
                    bundled.join(", "),
                    if saved.is_empty() {
                        "—".to_owned()
                    } else {
                        saved.join(", ")
                    }
                ),
                data: Some(json!({ "examples": bundled, "saved": saved })),
            })
        }
    }
}

/// `matrix.start`: eröffnet einen Lauf aus einem freigegebenen Entwurf.
///
/// # Errors
/// [`OpError::InvalidArguments`] bei unbekanntem Szenario (Freitext →
/// Hinweis auf `matrix.draft_scenario`).
#[operation(
    name = "matrix.start",
    summary = "Eröffnet einen Matrix-Game-Lauf aus einem gespeicherten Entwurf (Slug) oder gebündelten Szenario — erst nach Freigabe der Szenario-Zusammenfassung durch die Nutzerin.",
    domain = "knowledge",
    permission = "operator",
    model_tool(approval = "always")
)]
async fn matrix_start(ctx: &OpContext, args: StartArgs) -> Result<OpOutput, OpError> {
    let root = matrix_root()?;
    let resolved = resolve_known_scenario(&args.scenario, Some(&root))?;
    let summary = scenario_summary(&resolved.loaded);
    let (mut out, run_id) = start_run(ctx, resolved, args.seed)?;
    out.text = format!(
        "{}\n\n{summary}\nLauf-ID `{run_id}`. Weiter mit matrix.run {{\"rounds\": 1}} (Zwischenstand per parent.message melden).",
        out.text
    );
    Ok(out)
}

/// `matrix.run`: spielt Runden mit den Sitz-Agenten.
///
/// # Errors
/// [`OpError::InvalidArguments`] bei ungültiger Rundenzahl oder unbekanntem
/// Lauf, [`OpError::NotAvailable`] ohne Agent-Spawner.
#[operation(
    name = "matrix.run",
    summary = "Spielt 1–20 Runden eines Matrix-Game-Laufs mit den Sitz-Agenten (Würfel nur aus der Engine) und meldet je Phase eine Zeile; endet früher bei Spielende, Leak-Verdacht, Abbruch oder nach 25 Minuten.",
    domain = "knowledge",
    permission = "operator",
    model_tool(approval = "always")
)]
async fn matrix_run(ctx: &OpContext, args: RunArgs) -> Result<OpOutput, OpError> {
    let rounds = args.rounds.unwrap_or(1);
    if !(1..=MAX_AUTO_ROUNDS).contains(&rounds) {
        return Err(invalid(format!(
            "rounds muss zwischen 1 und {MAX_AUTO_ROUNDS} liegen"
        )));
    }
    let id = session_run_id(args.run_id, ctx.session_id().as_str())?;
    let mut run = take_run(&id)?;
    if run.status() == RunStatus::Ended {
        put_run(run)?;
        return Err(invalid(format!(
            "Lauf `{id}` ist beendet — Bericht über matrix.finish"
        )));
    }
    let deadline = Instant::now() + RUN_CALL_BUDGET;
    let result = advance(ctx, &mut run, rounds, Some(deadline)).await;
    run.set_cancel(None);
    let out = result.map(|(lines, reason)| {
        let next = match reason {
            StopReason::Ended => "Spiel beendet — weiter mit matrix.finish.".to_owned(),
            StopReason::Rounds => {
                "Runde(n) gespielt — Zwischenstand melden, dann matrix.run oder matrix.finish."
                    .to_owned()
            }
            StopReason::Leak => {
                "Angehalten: Leak-Verdacht — Beobachter-Protokoll prüfen, dann matrix.run."
                    .to_owned()
            }
            StopReason::Cancelled => "Abgebrochen.".to_owned(),
            StopReason::Deadline => {
                "Zeitbudget des Aufrufs erschöpft — fortsetzen mit matrix.run.".to_owned()
            }
            StopReason::StepCap => {
                "Sicherheitsnetz der Phasenzahl erreicht — fortsetzen mit matrix.run.".to_owned()
            }
            // Runde 9, E7: klare Zeile für Game Master und UIA.
            StopReason::Technical(causes) => technical_stop_text(&causes),
        };
        output(
            &run,
            format!("{}\n{}\n{next}", run.headline(), lines.join("\n")),
        )
    });
    put_run(run)?;
    out
}

/// Runde 9, E7: Werkzeugtext nach einem technischen Abbruch — für den Game
/// Master und die UIA unmissverständlich kein Spielergebnis.
fn technical_stop_text(causes: &str) -> String {
    format!(
        "TECHNISCHER FEHLER — KEIN SPIELERGEBNIS: Alle Sitz-Aufrufe der letzten Phase \
         scheiterten technisch, kein Sitz-Modell hat geantwortet. Ursache: {causes}. \
         Der Lauf ist pausiert. Melde der UIA genau diese Ursache als technischen Fehler \
         (nicht als Pässe der Akteure) und starte matrix.run nicht blind erneut — erst \
         wenn die Ursache behoben ist."
    )
}

/// Legt eine Kopie des Berichts als neue Datei unter
/// `<workspace>/matrix/<szenario>-<lauf>.md` an (nie überschreibend, kein
/// Symlink-Ordner).
///
/// # Errors
/// Deutsche Beschreibung bei Symlink, Dateifehlern oder erschöpften Namen.
fn copy_report_to_workspace(
    workspace_root: &Path,
    report: &Path,
    scenario_id: &str,
    run_id: &str,
) -> Result<PathBuf, String> {
    let bytes = std::fs::read(report).map_err(|e| format!("Bericht nicht lesbar: {e}"))?;
    let dir = workspace_root.join(WORKSPACE_REPORT_DIR);
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("`{}` ist ein Symlink", dir.display()));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("`{}` nicht anlegbar: {e}", dir.display()))?;
    let base = format!("{}-{}", sanitize(scenario_id), sanitize(run_id));
    for n in 0..100u32 {
        let name = if n == 0 {
            format!("{base}.md")
        } else {
            format!("{base}-{n}.md")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(&bytes)
                    .map_err(|e| format!("`{}` nicht schreibbar: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("`{}` nicht anlegbar: {e}", path.display())),
        }
    }
    Err("kein freier Dateiname für die Kopie".to_owned())
}

/// `matrix.finish`: Schlussargumente, AAR, `report.md`, Kopie in den
/// Workspace.
///
/// # Errors
/// [`OpError::InvalidArguments`] bei unbekanntem Lauf.
#[operation(
    name = "matrix.finish",
    summary = "Schließt einen Matrix-Game-Lauf ab (Schlussargumente, AAR, paper-tauglicher report.md) und legt eine Kopie des Berichts unter matrix/<szenario>-<lauf>.md im Workspace an.",
    domain = "knowledge",
    permission = "operator",
    model_tool(approval = "always")
)]
async fn matrix_finish(ctx: &OpContext, args: FinishArgs) -> Result<OpOutput, OpError> {
    let id = session_run_id(args.run_id, ctx.session_id().as_str())?;
    let mut run = take_run(&id)?;
    let result = end_loop(ctx, &mut run).await;
    run.set_cancel(None);
    let lines = match result {
        Ok(lines) => lines,
        Err(error) => {
            put_run(run)?;
            return Err(error);
        }
    };
    let mut text = format!("{}\n{}", run.headline(), lines.join("\n"));
    let mut data = json!({ "run_id": id });
    if let Some(dir) = run.run_dir() {
        let _ = write!(text, "\nAAR: {}", dir.join(AAR_FILE).display());
        data["aar"] = json!(dir.join(AAR_FILE).display().to_string());
    }
    match run.report_path().map(Path::to_path_buf) {
        Some(report) => {
            let _ = write!(text, "\nBericht: {}", report.display());
            data["report"] = json!(report.display().to_string());
            let workspace = ctx.sandbox().workspace().canonical_root().to_path_buf();
            match copy_report_to_workspace(
                &workspace,
                &report,
                run.loaded().scenario.id(),
                run.run_id(),
            ) {
                Ok(copy) => {
                    let _ = write!(text, "\nKopie im Workspace: {}", copy.display());
                    data["workspace_report"] = json!(copy.display().to_string());
                }
                Err(error) => {
                    let _ = write!(
                        text,
                        "\nKeine Kopie im Workspace ({error}) — die UIA kann den Bericht über einen Schreib-Helfer übernehmen."
                    );
                }
            }
            text.push_str(
                "\nFür die UIA: Ist LaTeX verfügbar (latex.check), uia-latex-writer mit der Vorlage \
                 `business-paper` beauftragen — .tex und .pdf im selben Ordner wie die Workspace-Kopie. \
                 Ohne LaTeX bleibt es bei der .md (mit Hinweis an die Nutzerin).",
            );
        }
        None => text.push_str("\nKein report.md (Lauf ohne Verzeichnis)."),
    }
    let out = OpOutput {
        text,
        data: Some(data),
    };
    put_run(run)?;
    Ok(out)
}

/// `matrix.add_fact`: recherchierten Fakt mit Belegen als öffentliche Lage
/// eintragen (Plan R9, Grundierung vor Runde 1 bzw. „Recherche-Inject“
/// zwischen Runden).
///
/// # Errors
/// [`OpError::InvalidArguments`] ohne Text, ohne oder mit unvollständigem
/// Beleg, bei unbekanntem oder beendetem Lauf.
#[operation(
    name = "matrix.add_fact",
    summary = "Trägt einen recherchierten Fakt mit Quellen (URL bzw. Fundstelle, Abrufdatum, Einstufung) als öffentliche Lage in den eigenen Lauf ein; alle Sitze sehen ihn, der Bericht nennt die Quellen. Nur belegte Fakten, keine Annahmen.",
    domain = "knowledge",
    permission = "operator",
    model_tool(approval = "none")
)]
async fn matrix_add_fact(ctx: &OpContext, args: AddFactArgs) -> Result<OpOutput, OpError> {
    let sources = parse_fact_sources(&args.sources)?;
    let id = session_run_id(args.run_id, ctx.session_id().as_str())?;
    with_run(&id, |run| {
        let line = run.add_research_fact(&args.text, sources)?;
        let round = run.log().state.round;
        let when = if round == 0 {
            "Ausgangslage vor Runde 1".to_owned()
        } else {
            format!("Lage ab Runde {round}")
        };
        Ok(output(
            run,
            format!("Fakt eingetragen ({when}, für alle Sitze sichtbar): {line}"),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_operations::{ApprovalPolicy, FromRawArgs, Surface};
    use harw_registry_defaults::profile::MATRIX_GAME_MASTER_TOOLS;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const KARST: &str = include_str!("../../../harw-matrix-game/scenarios/karst-islands.toml");

    /// Die Werkzeugfläche des Game Masters ist genau die registrierte
    /// Rollenliste; nur `status`/`draft_scenario` laufen ohne Freigabe, kein
    /// Werkzeug hat eine Slash-Fläche, und menschliche Eingriffe (Veto,
    /// Override, Inject) gibt es für das Modell nicht.
    #[test]
    fn game_master_tools_match_the_role_and_declare_their_approval() -> TestResult {
        let ops = game_master_operations();
        let names: Vec<&str> = ops.iter().map(|op| op.meta().name).collect();
        assert_eq!(names, MATRIX_GAME_MASTER_TOOLS);
        for op in &ops {
            let meta = op.meta();
            assert_eq!(meta.surfaces.len(), 1, "{}", meta.name);
            let Some(Surface::ModelTool { readonly, approval }) = meta.surfaces.first() else {
                return Err(
                    format!("{} muss eine Modell-Werkzeug-Fläche tragen", meta.name).into(),
                );
            };
            let free = matches!(
                meta.name,
                MATRIX_STATUS | MATRIX_DRAFT_SCENARIO | MATRIX_ADD_FACT
            );
            assert_eq!(*approval == ApprovalPolicy::None, free, "{}", meta.name);
            assert_eq!(*readonly, meta.name == MATRIX_STATUS, "{}", meta.name);
            assert!(
                meta.args_schema.is_some(),
                "{} braucht ein Schema",
                meta.name
            );
            for human in ["veto", "override", "inject", "reveal", "fork"] {
                assert!(!meta.name.contains(human), "{}", meta.name);
            }
        }
        Ok(())
    }

    /// Plan R9: Belege brauchen Fundstelle und Abrufdatum; die Einstufung
    /// ist optional.
    #[test]
    fn fact_sources_parse_and_reject_incomplete_entries() -> TestResult {
        let parsed = parse_fact_sources(&[
            "https://example.org/LICENSE | 2026-09-24 | A1".to_owned(),
            "SECURITY.md:1 | 2026-09-24".to_owned(),
        ])?;
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].url, "https://example.org/LICENSE");
        assert_eq!(parsed[0].retrieved, "2026-09-24");
        assert_eq!(parsed[0].rating.as_deref(), Some("A1"));
        assert_eq!(parsed[1].rating, None);
        assert!(parse_fact_sources(&["https://example.org".to_owned()]).is_err());
        assert!(parse_fact_sources(&[" | 2026-09-24".to_owned()]).is_err());
        assert!(AddFactArgs::from_raw_args(&[]).is_err());
        Ok(())
    }

    #[test]
    fn slash_tokens_are_refused() {
        assert!(RunArgs::from_raw_args(&["3".to_owned()]).is_err());
        assert!(StartArgs::from_raw_args(&[]).is_err());
    }

    /// Runde 7, Teil M2: ein gültiges Szenario wird gespeichert und ist über
    /// seinen Slug auflösbar; ein ungültiges liefert die Validator-Befunde.
    #[test]
    fn draft_validates_saves_and_resolves() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let source = KARST.replace("id = \"karst-wasserkrise\"", "id = \"karst-entwurf\"");
        let (path, loaded) = save_draft(tmp.path(), &source, None, false)?;
        assert_eq!(
            path,
            tmp.path().join(SCENARIOS_DIR).join("karst-entwurf.toml")
        );
        let summary = scenario_summary(&loaded);
        assert!(summary.contains("Akteure:"), "{summary}");
        assert!(summary.contains("Regeln:"), "{summary}");
        let resolved = resolve_known_scenario("karst-entwurf", Some(tmp.path()))?;
        assert_eq!(resolved.loaded.scenario.id(), "karst-entwurf");
        // Kein stilles Überschreiben, gebündelte Namen sind reserviert.
        assert!(save_draft(tmp.path(), &source, None, false).is_err());
        assert!(save_draft(tmp.path(), &source, None, true).is_ok());
        assert!(save_draft(tmp.path(), &source, Some("karst-islands"), true).is_err());
        assert!(save_draft(tmp.path(), &source, Some("../raus"), true).is_err());
        // Ungültig: Validator-Befunde gehen an den Game Master zurück.
        let broken = source.replace("schema = ", "schema_kaputt = ");
        let error = save_draft(tmp.path(), &broken, Some("kaputt"), false)
            .err()
            .ok_or("ungültiges Szenario muss abgewiesen werden")?;
        assert!(error.to_string().contains("korrigieren"), "{error}");
        assert!(save_draft(tmp.path(), "   ", None, false).is_err());
        Ok(())
    }

    #[test]
    fn workspace_copy_never_overwrites() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let report = tmp.path().join("report.md");
        std::fs::write(&report, "# Bericht")?;
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace)?;
        let first = copy_report_to_workspace(&workspace, &report, "szenario", "lauf-1")?;
        let second = copy_report_to_workspace(&workspace, &report, "szenario", "lauf-1")?;
        assert_eq!(
            first,
            workspace
                .join(WORKSPACE_REPORT_DIR)
                .join("szenario-lauf-1.md")
        );
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&second)?, "# Bericht");
        Ok(())
    }
}
