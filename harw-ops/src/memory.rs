//! `/memory` — Long-Term-Memory-Operation.
//!
//! # Verantwortungsbereich
//! Exponiert zwei Gedächtnisschichten als `/memory`-Command (channel_parity):
//! die bestehende Signal-basierte HOT/WARM/COLD-Schicht (v2,
//! `harw-memory/src/store.rs`, Subcommands `list`/`stats`/`maintain`) und
//! den adressierbaren Fakt-Speicher (v3, `harw_memory::facts::FactStore`,
//! Subcommands `record`/`recall`/`forget`) nach
//! `docs/design/memory-v3-ltm.md` §6. Die Operation wird derzeit nicht als
//! Model-Tool exponiert, weil ihr invocation-neutraler Callback auch
//! mutierende Subcommands ausführt.
//!
//! # Subcommands
//! - `list` (Default) — gibt den HOT-Tier komplett zurück (≤100 Zeilen).
//! - `stats` — Zähler-Statistik über HOT/WARM/COLD und offene Signale (v2).
//! - `recall <stichwort…>` — durchsucht **beide** Fakt-Wurzeln (Projekt vor
//!   Global, Design §4) und zeigt je Treffer die Herkunft.
//! - `record <text…> [--project|--global]` (Default `--project`, Contract
//!   §4/Design §5.1) — schreibt `<text>` als [`harw_memory::Fact`] in den
//!   gewählten Scope; der Fakt-Name wird aus `<text>` per
//!   [`harw_memory::slugify`] abgeleitet, mit Kollisionsschutz (siehe
//!   [`unique_slug`]).
//! - `forget <name>` (Design §6) — löscht den Fakt `<name>` aus Projekt
//!   **und** Global, wo immer er existiert (bestes Bemühen je Wurzel).
//! - `maintain` — führt den idempotenten v2-Konsolidierungslauf aus.
//! - `consolidate [--project|--global]` (Default `--project`, Design
//!   §5.3/§6) — stößt Phase 2 (Konsolidierung) sofort an, siehe
//!   [`consolidate_dispatch`]. Führt nur den **deterministischen** Teil aus
//!   (Lock, `plan_consolidation`/`apply_plan`, Baseline-Digest aktualisieren)
//!   — der `memory-steward`-Agentenlauf selbst (Widersprüche auflösen,
//!   Confidence-Verfall/Löschung) kann von dieser Operation nicht gestartet
//!   werden, siehe dort.
//!
//! # Scopes und Wurzeln (Contract §2, Design §2)
//! - `Project`: `<projekt-root>/.harw/memories/` — bleibt im Repo,
//!   versionierbar, gewährt keine Rechte (siehe [`project_memories_root`]).
//! - `Global`: `~/.harw/profiles/<profil>/memories/` (siehe
//!   [`global_memories_root`]).
//!
//! # Nebenläufigkeit
//! Die Op selbst ist zustandslos; das v2-Backend
//! (`Arc<dyn harw_memory::Memory>`) wird aus [`OpContext::service`] aufgelöst,
//! der v3-[`FactStore`] wird pro Aufruf frisch geöffnet (er hält keinen
//! Zustand über die Dateien hinaus).
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein v2-Memory-Backend im Kontext
//!   registriert (nur für `list`/`stats`/`maintain`).
//! - [`OpError::InvalidArguments`] — Argumentgrammatik verletzt, oder
//!   `forget` fand keinen passenden Fakt.
//! - [`OpError::Execution`] — Backend-/Dateisystemfehler (I/O, Serde,
//!   Tier-Overflow, Lock, Projekt-Erkennung).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_macros::operation;
use harw_memory::{Fact, FactScope, FactStore, FactType, Memory, slugify};
use harw_operations::{OpContext, OpError, OpOutput};

/// Obergrenze der Treffer je Wurzel bei `recall` (wie zuvor bei der
/// WARM-Suche des v2-Backends).
const RECALL_LIMIT: usize = 8;

/// Obergrenze der `description`-Länge (Unicode-Zeichen) bei `record`, bevor
/// sie mit `…` gekürzt wird.
const DESCRIPTION_MAX_CHARS: usize = 80;

/// Argument-Container für `/memory`.
///
/// # Beschreibung
/// Positional: `sub` ist das Subcommand, `tail` sind die restlichen Tokens.
/// Die Command-Fläche parst über [`FromRawArgs`]; jedes Subcommand parst
/// seinen `tail` selbst (siehe Moduldoku).
#[derive(Default, serde::Deserialize)]
pub struct MemoryArgs {
    /// Subcommand: `list`, `stats`, `recall`, `record`, `forget`,
    /// `maintain`, `consolidate`.
    #[serde(default)]
    pub sub: Option<String>,
    /// Weitere Tokens nach dem Subcommand (Keywords, Text, Flags, …).
    #[serde(default)]
    pub tail: Vec<String>,
}

impl harw_operations::FromRawArgs for MemoryArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        Ok(Self {
            sub: tokens.first().cloned(),
            tail: tokens.iter().skip(1).cloned().collect(),
        })
    }
}

/// Führt den `/memory`-Subcommand aus.
///
/// # Argumente
/// - `ctx` — Ausführungskontext; `list`/`stats`/`maintain` benötigen
///   `Arc<dyn Memory>` als Service, `recall`/`record`/`forget` lösen ihre
///   Fakt-Wurzeln selbst über `harw_home` auf.
/// - `args` — bereits geparst (Subcommand + Tail-Tokens).
///
/// # Rückgabe
/// `Ok(OpOutput::from(text))` mit menschlich lesbarem Bericht (Markdown-Snippets).
///
/// # Fehler
/// Siehe Modul-Doku.
#[operation(
    name = "memory",
    summary = "Long-Term-Memory: list, recall, record, forget, stats, maintain.",
    domain = "session",
    permission = "operator",
    command(path = "/memory", visibility = "channel_parity")
)]
async fn memory(ctx: &OpContext, args: MemoryArgs) -> Result<OpOutput, OpError> {
    let sub = args.sub.as_deref().unwrap_or("list");
    match sub {
        "list" => render_hot(&*memory_store(ctx)?),
        "stats" => render_stats(&*memory_store(ctx)?),
        "recall" => render_fact_recall(ctx, &args.tail),
        "record" => record_dispatch(ctx, &args.tail),
        "forget" => forget_fact(ctx, &args.tail),
        "maintain" => run_maintain(&*memory_store(ctx)?),
        "consolidate" => consolidate_dispatch(ctx, &args.tail),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter /memory-Subcommand: {other} (list, stats, recall, record, forget, maintain, consolidate)"
        ))),
    }
}

/// Löst das v2-`Arc<dyn Memory>`-Backend aus der `ServiceMap` auf.
fn memory_store(ctx: &OpContext) -> Result<Arc<dyn Memory>, OpError> {
    ctx.service::<Arc<dyn Memory>>()
        .cloned()
        .ok_or_else(|| OpError::NotAvailable("kein Memory-Backend im Kontext".to_owned()))
}

fn render_hot(store: &dyn Memory) -> Result<OpOutput, OpError> {
    let hot = store
        .hot()
        .map_err(|e| OpError::Execution(format!("memory.hot failed: {e}")))?;
    let text = if hot.is_empty() {
        "HOT-Tier ist leer.".to_owned()
    } else {
        format!("HOT-Tier ({} Zeilen):\n{hot}", hot.lines().count())
    };
    Ok(OpOutput::from(text))
}

fn render_stats(store: &dyn Memory) -> Result<OpOutput, OpError> {
    let s = store
        .stats()
        .map_err(|e| OpError::Execution(format!("memory.stats failed: {e}")))?;
    let text = format!(
        "Memory-Statistik\n\
         · HOT: {} Zeilen\n\
         · WARM: {} Namespaces / {} Zeilen\n\
         · COLD: {} Namespaces\n\
         · Offene Signale: {}",
        s.hot_lines, s.warm_namespaces, s.warm_total_lines, s.cold_namespaces, s.pending_signals,
    );
    Ok(OpOutput::from(text))
}

fn run_maintain(store: &dyn Memory) -> Result<OpOutput, OpError> {
    let report = store
        .maintain()
        .map_err(|e| OpError::Execution(format!("memory.maintain failed: {e}")))?;
    Ok(OpOutput::from(format!(
        "Maintenance abgeschlossen: {} Signale, {} promoted→HOT, {} demoted→COLD, {} neue WARM.",
        report.signals_processed,
        report.promoted_to_hot,
        report.demoted_to_cold,
        report.warm_created,
    )))
}

// ── Fakt-Speicher (v3): Wurzeln, Scope-Flags ────────────────────────────────

/// Löst `<projekt-root>/.harw/memories` auf und stellt sicher, dass sie
/// existiert (Contract §2/§3, Design §2).
///
/// # Errors
/// [`OpError::Execution`], wenn Home-Auflösung, Projekt-Erkennung oder das
/// Anlegen des Projekt-Homes fehlschlägt.
fn project_memories_root(ctx: &OpContext) -> Result<PathBuf, OpError> {
    let markers = crate::config_util::load_default_config("")
        .ok()
        .and_then(|config| config.harness.project_root_markers)
        .unwrap_or_default();
    let cwd = ctx.sandbox().workspace().canonical_root();
    let project = harw_home::discover_project(cwd, &markers).map_err(|error| {
        OpError::Execution(format!("Projekt-Erkennung fehlgeschlagen: {error}"))
    })?;
    let project_home = harw_home::ProjectHome::at(&project);
    project_home.ensure().map_err(|error| {
        OpError::Execution(format!("Projekt-Home anlegen fehlgeschlagen: {error}"))
    })?;
    Ok(project_home.memories_dir())
}

/// Löst `~/.harw/profiles/<profil>/memories` auf (Contract §2, Design §2).
///
/// # Errors
/// [`OpError::Execution`], wenn Home- oder Profil-Auflösung fehlschlägt.
fn global_memories_root() -> Result<PathBuf, OpError> {
    let home = harw_home::home_dir()
        .map_err(|error| OpError::Execution(format!("Home nicht auflösbar: {error}")))?;
    let profile = harw_home::active_profile_name(&home);
    let dir = harw_home::profile_dir(&home, &profile)
        .map_err(|error| OpError::Execution(format!("Profil-Pfad fehlgeschlagen: {error}")))?;
    Ok(dir.join("memories"))
}

/// Trennt `--project`/`--global` aus `tokens`, liefert die verbleibenden
/// positionalen Tokens zusammen mit dem gewählten [`FactScope`].
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn beide Flags gleichzeitig angegeben
/// werden (widersprüchliche Flags).
fn parse_memory_scope_flags(
    tokens: &[String],
    default_scope: FactScope,
) -> Result<(Vec<String>, FactScope), OpError> {
    let mut positional = Vec::new();
    let mut scope: Option<FactScope> = None;
    for token in tokens {
        match token.as_str() {
            "--project" => set_memory_scope_flag(&mut scope, FactScope::Project)?,
            "--global" => set_memory_scope_flag(&mut scope, FactScope::Global)?,
            other => positional.push(other.to_owned()),
        }
    }
    Ok((positional, scope.unwrap_or(default_scope)))
}

fn set_memory_scope_flag(slot: &mut Option<FactScope>, value: FactScope) -> Result<(), OpError> {
    match slot {
        Some(existing) if *existing != value => Err(OpError::InvalidArguments(
            "widersprüchliche Scope-Flags: nur eines von --project/--global ist erlaubt".to_owned(),
        )),
        _ => {
            *slot = Some(value);
            Ok(())
        }
    }
}

/// Kürzt `text` auf höchstens [`DESCRIPTION_MAX_CHARS`] Unicode-Zeichen für
/// `Fact::description`, mit angehängtem `…` bei Kürzung.
fn truncate_description(text: &str) -> String {
    if text.chars().count() <= DESCRIPTION_MAX_CHARS {
        return text.to_owned();
    }
    let cut = text
        .char_indices()
        .nth(DESCRIPTION_MAX_CHARS)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    format!("{}…", &text[..cut])
}

/// Findet einen freien oder inhaltlich identischen Fakt-Namen ausgehend von
/// `base` (bereits per [`slugify`] gebildet).
///
/// # Beschreibung
/// Existiert unter `base` noch kein Fakt, oder existiert einer mit
/// identischem `body`, wird `base` selbst zurückgegeben (Idempotenz: das
/// erneute Aufzeichnen desselben Texts aktualisiert denselben Fakt). Existiert
/// ein **anderer** Fakt unter diesem Namen, wird `base-2`, `base-3`, …
/// versucht.
///
/// # Errors
/// [`OpError::Execution`], wenn binnen 50 Versuchen kein passender Name
/// gefunden wurde, oder das Lesen eines Kandidaten fehlschlägt.
fn unique_slug(store: &FactStore, base: &str, body: &str) -> Result<String, OpError> {
    let mut candidate = base.to_owned();
    for suffix in 2u32..=50 {
        match store
            .read(&candidate)
            .map_err(|error| OpError::Execution(format!("Fakt lesen fehlgeschlagen: {error}")))?
        {
            None => return Ok(candidate),
            // `FactStore::write` (harw-memory/src/facts.rs::to_markdown) hängt
            // an jeden nicht-leeren Body genau einen abschließenden
            // Zeilenumbruch an, bevor er auf die Platte geschrieben wird — ein
            // zurückgelesener `body` trägt diesen also immer, während der hier
            // übergebene `body` (frisch getrimmter Eingabetext aus
            // `record_fact`) ihn nie trägt. Ohne den Vergleich davon zu lösen,
            // würde derselbe erneut aufgezeichnete Text nie als „identisch"
            // erkannt und bekäme bei jedem Aufruf einen neuen Suffix statt
            // denselben Fakt zu aktualisieren.
            Some(existing)
                if existing.body.trim_end_matches('\n') == body.trim_end_matches('\n') =>
            {
                return Ok(candidate);
            }
            Some(_) => candidate = format!("{base}-{suffix}"),
        }
    }
    Err(OpError::Execution(format!(
        "kein freier Fakt-Name für '{base}' gefunden (50 Kollisionen)"
    )))
}

/// `/memory record <text…> [--project|--global]`. Default `--project`.
fn record_dispatch(ctx: &OpContext, tail: &[String]) -> Result<OpOutput, OpError> {
    let (positional, scope) = parse_memory_scope_flags(tail, FactScope::Project)?;
    if positional.is_empty() {
        return Err(OpError::InvalidArguments(
            "/memory record <text…> [--project|--global] braucht Text".to_owned(),
        ));
    }
    let text = positional.join(" ");
    record_fact(ctx, scope, &text)
}

/// Schreibt `text` als [`Fact`] in den `scope`-Wurzel-Speicher.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: `text` ist (nach Trimmen) leer.
/// - [`OpError::Execution`]: Wurzel-Auflösung, Öffnen oder Schreiben des
///   [`FactStore`] schlug fehl.
fn record_fact(ctx: &OpContext, scope: FactScope, text: &str) -> Result<OpOutput, OpError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(OpError::InvalidArguments(
            "/memory record <text…> [--project|--global] braucht Text".to_owned(),
        ));
    }
    let root = match scope {
        FactScope::Project => project_memories_root(ctx)?,
        FactScope::Global => global_memories_root()?,
    };
    let store = FactStore::open(&root, scope).map_err(|error| {
        OpError::Execution(format!("Fakt-Speicher öffnen fehlgeschlagen: {error}"))
    })?;
    let base = slugify(trimmed);
    let name = unique_slug(&store, &base, trimmed)?;
    let now = time::OffsetDateTime::now_utc();
    let fact = Fact {
        name: name.clone(),
        description: truncate_description(trimmed),
        fact_type: FactType::Fact,
        scope,
        created: now,
        updated: now,
        confidence: 1.0,
        sources: Vec::new(),
        tags: Vec::new(),
        body: trimmed.to_owned(),
    };
    store
        .write(&fact)
        .map_err(|error| OpError::Execution(format!("Fakt schreiben fehlgeschlagen: {error}")))?;
    Ok(OpOutput::from(format!(
        "Fakt '{name}' aufgezeichnet ({scope}, {}).",
        root.display()
    )))
}

/// `/memory recall <stichwort…>` — durchsucht Projekt- und Global-Wurzel,
/// Projekt zuerst (Design §4), und zeigt je Treffer die Herkunft. Eine
/// Wurzel, die nicht geöffnet werden kann (z. B. kein Projekt erkennbar),
/// wird stillschweigend übersprungen statt die gesamte Suche fehlschlagen zu
/// lassen — die jeweils andere Wurzel bleibt durchsuchbar.
///
/// # Errors
/// [`OpError::InvalidArguments`]: keine Suchbegriffe angegeben.
fn render_fact_recall(ctx: &OpContext, tail: &[String]) -> Result<OpOutput, OpError> {
    if tail.is_empty() {
        return Err(OpError::InvalidArguments(
            "/memory recall <stichwort…> braucht mindestens einen Suchbegriff".to_owned(),
        ));
    }
    let keywords: Vec<&str> = tail.iter().map(String::as_str).collect();
    let mut hits: Vec<(FactScope, Fact)> = Vec::new();

    if let Ok(root) = project_memories_root(ctx) {
        if let Ok(store) = FactStore::open(&root, FactScope::Project) {
            if let Ok(found) = store.search(&keywords, RECALL_LIMIT) {
                hits.extend(found.into_iter().map(|fact| (FactScope::Project, fact)));
            }
        }
    }
    if let Ok(root) = global_memories_root() {
        if let Ok(store) = FactStore::open(&root, FactScope::Global) {
            if let Ok(found) = store.search(&keywords, RECALL_LIMIT) {
                hits.extend(found.into_iter().map(|fact| (FactScope::Global, fact)));
            }
        }
    }

    if hits.is_empty() {
        return Ok(OpOutput::from(format!(
            "Keine Treffer für: {}",
            tail.join(" ")
        )));
    }
    let mut buf = format!("{} Treffer:\n", hits.len());
    for (scope, fact) in &hits {
        buf.push_str(&format!(
            "· [{scope}] {} — {}\n  {}\n",
            fact.name,
            fact.description,
            fact.body.trim()
        ));
    }
    Ok(OpOutput::from(buf))
}

/// `/memory forget <name>` — löscht `<name>` aus Projekt und Global, wo
/// immer er existiert (Design §6, §7 „Löschen ist immer möglich und
/// vollständig").
///
/// # Errors
/// [`OpError::InvalidArguments`]: kein Name angegeben, oder in keiner Wurzel
/// gefunden.
/// [`OpError::Execution`]: Löschen in einer erreichbaren Wurzel schlug fehl
/// (Wurzeln, die gar nicht auflösbar sind, werden wie bei `recall`
/// übersprungen).
fn forget_fact(ctx: &OpContext, tail: &[String]) -> Result<OpOutput, OpError> {
    let Some(name) = tail.first() else {
        return Err(OpError::InvalidArguments(
            "/memory forget <name>".to_owned(),
        ));
    };

    let mut deleted_from = Vec::new();
    if let Ok(root) = project_memories_root(ctx) {
        if let Ok(store) = FactStore::open(&root, FactScope::Project) {
            if store.delete(name).map_err(|error| {
                OpError::Execution(format!("Fakt löschen fehlgeschlagen (Projekt): {error}"))
            })? {
                deleted_from.push("Projekt");
            }
        }
    }
    if let Ok(root) = global_memories_root() {
        if let Ok(store) = FactStore::open(&root, FactScope::Global) {
            if store.delete(name).map_err(|error| {
                OpError::Execution(format!("Fakt löschen fehlgeschlagen (Global): {error}"))
            })? {
                deleted_from.push("Global");
            }
        }
    }

    if deleted_from.is_empty() {
        Err(OpError::InvalidArguments(format!(
            "/memory forget: kein Fakt '{name}' gefunden"
        )))
    } else {
        Ok(OpOutput::from(format!(
            "Fakt '{name}' gelöscht ({}).",
            deleted_from.join(", ")
        )))
    }
}

// ── Konsolidierung (Phase 2, Design §5.3/§6) ────────────────────────────────

/// `/memory consolidate [--project|--global]` (Default `--project`).
///
/// # Errors
/// Siehe [`run_consolidate`].
fn consolidate_dispatch(ctx: &OpContext, tail: &[String]) -> Result<OpOutput, OpError> {
    let (_positional, scope) = parse_memory_scope_flags(tail, FactScope::Project)?;
    let root = match scope {
        FactScope::Project => project_memories_root(ctx)?,
        FactScope::Global => global_memories_root()?,
    };
    run_consolidate(&root, scope)
}

/// Stößt Phase 2 der Konsolidierung (Design §5.3) an der Fakt-Wurzel `root`
/// an und meldet, was dabei ausgeführt wurde — und was **nicht**.
///
/// # Beschreibung
/// Erwirbt [`harw_memory::consolidation::ConsolidationLock`] an `root`; bei
/// Kontention (eine andere Konsolidierung läuft bereits) wird das ohne
/// Fehler gemeldet. Andernfalls:
/// 1. Liest die Baseline ([`harw_memory::consolidation::ConsolidationBaseline::read`])
///    und den aktuellen Fakten-Bestand, bildet den Diff seit der letzten
///    Baseline.
/// 2. Nimmt alle Kandidaten aus `facts/_incoming/` (`IncomingStore::take_all`).
///    Sind keine vorhanden, endet der Lauf hier — nur der Baseline-Diff wird
///    gemeldet, nichts wird geschrieben.
/// 3. Plant ([`harw_memory::consolidation::plan_consolidation`]) und wendet
///    den **deterministischen** Teil an (`apply_plan`: Duplikate
///    verschmelzen, Kandidaten übernehmen, `MEMORY.md` neu schreiben).
///    `plan.deletions` ist dabei laut Vertrag immer leer — Löschentscheidungen
///    sind Sache des Stewards, nicht dieser Operation.
/// 4. Schreibt die Baseline auf den neuen Fakten-Bestand zurück (§5.3:
///    „Baseline zurücksetzen" — hier als Digest-Manifest statt als
///    git-Commit, siehe `consolidation`-Moduldoku).
/// 5. Baut den `memory-steward`-Auftragstext
///    ([`harw_memory::consolidation::steward_prompt`]) aus Index, Kandidaten,
///    betroffenen Fakten und Baseline-Diff — **ruft den Agenten aber nicht
///    auf**: [`OpContext`] löst ausschließlich Services (`ctx.service::<T>()`)
///    und Werkzeuge auf, es hat keinen Agenten-Runner. Der Bericht macht das
///    ausdrücklich sichtbar, statt die fehlende Ermessens-Entscheidung
///    (Widersprüche auflösen, veraltete Fakten senken/löschen) still zu
///    überspringen.
///
/// # Errors
/// [`OpError::Execution`] bei Lock-/I/O-/Serde-Fehlern der beteiligten
/// [`FactStore`]/`IncomingStore`/`ConsolidationBaseline`-Aufrufe.
fn run_consolidate(root: &Path, scope: FactScope) -> Result<OpOutput, OpError> {
    use harw_memory::consolidation::{
        ConsolidationBaseline, ConsolidationLock, apply_plan, plan_consolidation, steward_prompt,
    };
    use harw_memory::{IncomingStore, MemoryError};

    let lock = match ConsolidationLock::try_acquire(root) {
        Ok(lock) => lock,
        Err(MemoryError::LockContention { .. }) => {
            return Ok(OpOutput::from(format!(
                "Konsolidierung ({scope}) übersprungen: Lock bereits belegt (läuft schon, oder ein Absturz liegt < 30 Minuten zurück)."
            )));
        }
        Err(error) => {
            return Err(OpError::Execution(format!(
                "Konsolidierungs-Lock fehlgeschlagen: {error}"
            )));
        }
    };

    let outcome = (|| -> Result<String, OpError> {
        let incoming_store = IncomingStore::open(root).map_err(|error| {
            OpError::Execution(format!("Incoming-Speicher öffnen fehlgeschlagen: {error}"))
        })?;
        let fact_store = FactStore::open(root, scope).map_err(|error| {
            OpError::Execution(format!("Fakt-Speicher öffnen fehlgeschlagen: {error}"))
        })?;

        let baseline = ConsolidationBaseline::read(root).map_err(|error| {
            OpError::Execution(format!("Baseline lesen fehlgeschlagen: {error}"))
        })?;
        let existing_before = fact_store.list().map_err(|error| {
            OpError::Execution(format!("Bestehende Fakten lesen fehlgeschlagen: {error}"))
        })?;
        let changed_since_baseline = baseline.diff(&existing_before);

        let taken = incoming_store.take_all().map_err(|error| {
            OpError::Execution(format!("Kandidaten übernehmen fehlgeschlagen: {error}"))
        })?;
        if taken.is_empty() {
            return Ok(format!(
                "Konsolidierung ({scope}): keine Kandidaten in facts/_incoming/ — nichts zu tun. \
                 {} Fakt(en) seit letzter Baseline geändert.",
                changed_since_baseline.len()
            ));
        }
        let candidate_count = taken.len();

        let plan = plan_consolidation(&existing_before, &taken);
        let affected_names: std::collections::HashSet<&str> = plan
            .merges
            .iter()
            .map(|merge| merge.target.as_str())
            .collect();
        let affected: Vec<Fact> = existing_before
            .into_iter()
            .filter(|fact| affected_names.contains(fact.name.as_str()))
            .collect();
        let merges = plan.merges.len();
        let conflicts = plan.conflicts.len();

        let report = apply_plan(&fact_store, &plan).map_err(|error| {
            OpError::Execution(format!("Plan anwenden fehlgeschlagen: {error}"))
        })?;

        let existing_after = fact_store.list().map_err(|error| {
            OpError::Execution(format!(
                "Fakten nach Anwendung lesen fehlgeschlagen: {error}"
            ))
        })?;
        ConsolidationBaseline::from_facts(&existing_after)
            .write(root)
            .map_err(|error| {
                OpError::Execution(format!("Baseline schreiben fehlgeschlagen: {error}"))
            })?;

        let index = fs::read_to_string(root.join("MEMORY.md")).unwrap_or_default();
        let prompt = steward_prompt(&index, &taken, &affected, &changed_since_baseline);
        let affected_names_display = if affected.is_empty() {
            "keine".to_owned()
        } else {
            affected
                .iter()
                .map(|fact| fact.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };

        Ok(format!(
            "Konsolidierung ({scope}) — deterministischer Teil abgeschlossen:\n\
             · Kandidaten: {candidate_count}\n\
             · Merges: {merges} (betroffene Fakten: {affected_names_display})\n\
             · geschrieben: {} · gelöscht: {}\n\
             · Ungelöste Widersprüche: {conflicts}\n\
             · Fakten seit letzter Baseline geändert: {}\n\n\
             FEHLT: der `memory-steward`-Agentenlauf (Widersprüche auflösen, Confidence-Verfall/Löschung \
             veralteter Fakten, Design §5.3). Dieser Op-Kontext kann keine Subagenten starten — `OpContext` \
             löst ausschließlich Services/Werkzeuge auf, es hat keinen Agenten-Runner. Der fertige \
             Auftragstext für den Steward wurde gebaut ({} Zeichen, enthält Index, Kandidaten, betroffene \
             Fakten und den Baseline-Diff), muss aber von außerhalb dieser Operation an den \
             `memory-steward`-Agenten übergeben werden.",
            report.written,
            report.deleted,
            changed_since_baseline.len(),
            prompt.len(),
        ))
    })();

    let _ = lock.release();
    outcome.map(OpOutput::from)
}

#[cfg(test)]
mod tests {
    use super::{
        MemoryArgs, MemoryOperation, parse_memory_scope_flags, truncate_description, unique_slug,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_memory::{Fact, FactScope, FactStore, FactType};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpError, Operation};

    #[test]
    fn from_raw_args_no_tokens_uses_default_sub() -> TestResult {
        let a = MemoryArgs::from_raw_args(&toks(&[])).map_err(ctx("from_raw_args"))?;
        assert!(a.sub.is_none());
        assert!(a.tail.is_empty());
        Ok(())
    }

    #[test]
    fn from_raw_args_captures_sub_and_tail() -> TestResult {
        let a = MemoryArgs::from_raw_args(&toks(&["recall", "popup", "enter"]))
            .map_err(ctx("from_raw_args"))?;
        assert_eq!(a.sub.as_deref(), Some("recall"));
        assert_eq!(a.tail, vec!["popup".to_owned(), "enter".to_owned()]);
        Ok(())
    }

    #[test]
    fn from_raw_args_maintain_has_no_tail() -> TestResult {
        let a = MemoryArgs::from_raw_args(&toks(&["maintain"])).map_err(ctx("from_raw_args"))?;
        assert_eq!(a.sub.as_deref(), Some("maintain"));
        assert!(a.tail.is_empty());
        Ok(())
    }

    #[test]
    fn memory_operation_is_command_only() {
        let surfaces = &MemoryOperation.meta().surfaces;

        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(surfaces.iter().any(|surface| {
            matches!(
                surface,
                Surface::Command {
                    path: "/memory",
                    visibility: CommandVisibility::ChannelParity,
                }
            )
        }));
    }

    #[test]
    fn test_parse_memory_scope_flags_defaults_to_project() -> TestResult {
        let (positional, scope) =
            parse_memory_scope_flags(&toks(&["hallo", "welt"]), FactScope::Project)
                .map_err(ctx("parse_memory_scope_flags"))?;
        assert_eq!(positional, vec!["hallo".to_owned(), "welt".to_owned()]);
        assert_eq!(scope, FactScope::Project);
        Ok(())
    }

    #[test]
    fn test_parse_memory_scope_flags_reads_global() -> TestResult {
        let (positional, scope) =
            parse_memory_scope_flags(&toks(&["hallo", "--global"]), FactScope::Project)
                .map_err(ctx("parse_memory_scope_flags"))?;
        assert_eq!(positional, vec!["hallo".to_owned()]);
        assert_eq!(scope, FactScope::Global);
        Ok(())
    }

    #[test]
    fn test_parse_memory_scope_flags_rejects_conflicting_flags() -> TestResult {
        let result =
            parse_memory_scope_flags(&toks(&["--project", "--global"]), FactScope::Project);
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("widersprüchliche"))
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_parse_memory_scope_flags_repeating_same_flag_is_not_a_conflict() -> TestResult {
        let (_positional, scope) =
            parse_memory_scope_flags(&toks(&["--project", "--project"]), FactScope::Global)
                .map_err(ctx("parse_memory_scope_flags"))?;
        assert_eq!(scope, FactScope::Project);
        Ok(())
    }

    #[test]
    fn test_truncate_description_keeps_short_text_unchanged() {
        assert_eq!(truncate_description("kurz"), "kurz");
    }

    #[test]
    fn test_truncate_description_truncates_long_text_with_ellipsis() {
        let text = "a".repeat(100);
        let truncated = truncate_description(&text);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), 81);
    }

    fn temp_fact_store(label: &str) -> TestResult<(tempfile::TempDir, FactStore)> {
        let dir = tempfile::Builder::new()
            .prefix(&format!("harw-memory-facts-{label}-"))
            .tempdir()
            .map_err(ctx("tempdir"))?;
        let store = FactStore::open(dir.path(), FactScope::Project).map_err(ctx("open store"))?;
        Ok((dir, store))
    }

    fn sample_fact(name: &str, body: &str) -> Fact {
        let now = time::OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: body.to_owned(),
            fact_type: FactType::Fact,
            scope: FactScope::Project,
            created: now,
            updated: now,
            confidence: 1.0,
            sources: Vec::new(),
            tags: Vec::new(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn test_unique_slug_returns_base_when_free() -> TestResult {
        let (_dir, store) = temp_fact_store("free")?;
        let name = unique_slug(&store, "mein-fakt", "Text A").map_err(ctx("resolve slug"))?;
        assert_eq!(name, "mein-fakt");
        Ok(())
    }

    #[test]
    fn test_unique_slug_returns_base_when_identical_body_already_exists() -> TestResult {
        let (_dir, store) = temp_fact_store("identical")?;
        store
            .write(&sample_fact("mein-fakt", "Text A"))
            .map_err(ctx("seed fact"))?;
        let name = unique_slug(&store, "mein-fakt", "Text A").map_err(ctx("resolve slug"))?;
        assert_eq!(name, "mein-fakt", "identical content re-uses the same slug");
        Ok(())
    }

    #[test]
    fn test_unique_slug_appends_suffix_on_content_collision() -> TestResult {
        let (_dir, store) = temp_fact_store("collision")?;
        store
            .write(&sample_fact("mein-fakt", "Text A"))
            .map_err(ctx("seed fact"))?;
        let name = unique_slug(&store, "mein-fakt", "Text B, komplett anders")
            .map_err(ctx("resolve slug"))?;
        assert_eq!(name, "mein-fakt-2");
        Ok(())
    }

    #[test]
    fn test_record_fact_and_forget_round_trip_via_fact_store_directly() -> TestResult {
        // Deckt den Fakt-Store-Teil des Rundlaufs ab (Schreiben, Lesen,
        // Löschen), ohne `harw_home`/Projekt-Erkennung zu berühren — das
        // übernehmen `project_memories_root`/`global_memories_root`, die
        // bewusst nicht gegen einen echten `$HOME` getestet werden (siehe
        // `permissions.rs`-Tests für die Begründung).
        let (_dir, store) = temp_fact_store("round-trip")?;
        let fact = sample_fact("mein-fakt", "Ein Beispieltext für den Fakt-Speicher.");
        store.write(&fact).map_err(ctx("write fact"))?;

        let read_back = store
            .read("mein-fakt")
            .map_err(ctx("read"))?
            .ok_or(TestError::Missing("fact must exist"))?;
        // `FactStore::write` normalisiert jeden nicht-leeren Body auf genau
        // einen abschließenden Zeilenumbruch (harw-memory/src/facts.rs::
        // to_markdown) — dieselbe Konvention, die harw-memory's eigener
        // Rundlauf-Test `write_then_read_round_trips_umlauts_and_multiline_body`
        // voraussetzt (dessen Body-Fixture endet bewusst mit `\n`). Der Store
        // ist hier korrekt; diese Zeile erwartete zuvor fälschlich
        // Bytegleichheit ohne diese Normalisierung.
        assert_eq!(read_back.body, format!("{}\n", fact.body));

        let deleted = store.delete("mein-fakt").map_err(ctx("delete"))?;
        assert!(deleted);
        assert!(
            store
                .read("mein-fakt")
                .map_err(ctx("read after delete"))?
                .is_none()
        );
        Ok(())
    }

    // -- run_consolidate ---------------------------------------------------
    //
    // `run_consolidate` nimmt bewusst `&Path`/`FactScope` statt `&OpContext`
    // entgegen (siehe Funktionsdoku) — genau deshalb ist es hier direkt
    // testbar, ohne `harw_home`/Projekt-Erkennung zu berühren (vgl. Kommentar
    // bei `test_record_fact_and_forget_round_trip_via_fact_store_directly`).

    fn tmp_consolidate_root(tag: &str) -> TestResult<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix(&format!("harw-memory-consolidate-{tag}-"))
            .tempdir()
            .map_err(ctx("tempdir"))
    }

    #[test]
    fn run_consolidate_reports_nothing_to_do_without_candidates() -> TestResult {
        let dir = tmp_consolidate_root("empty")?;
        let out = super::run_consolidate(dir.path(), FactScope::Project)
            .map_err(ctx("run_consolidate"))?;
        assert!(out.text.contains("nichts zu tun"), "text was: {}", out.text);
        assert!(out.text.contains("Fakt(en) seit letzter Baseline geändert"));
        Ok(())
    }

    #[test]
    fn run_consolidate_merges_candidate_into_existing_fact_and_reports_missing_steward()
    -> TestResult {
        let dir = tmp_consolidate_root("merge")?;
        let store =
            FactStore::open(dir.path(), FactScope::Project).map_err(ctx("open fact store"))?;
        let mut existing = sample_fact("tui-approval-arming", "alte Beschreibung");
        existing.sources = vec!["session:old".to_owned()];
        store.write(&existing).map_err(ctx("seed existing fact"))?;

        let incoming = harw_memory::extraction::IncomingStore::open(dir.path())
            .map_err(ctx("open incoming store"))?;
        let mut candidate = sample_fact("tui-approval-arming", "neue Beschreibung");
        candidate.sources = vec!["session:new".to_owned()];
        // Explizit später als `existing.updated`, damit `merge_facts` deterministisch
        // die neuere Beschreibung wählt (siehe `consolidation.rs`s eigene Tests).
        candidate.updated = existing.updated + time::Duration::seconds(1);
        incoming
            .write_candidates(&[candidate])
            .map_err(ctx("write candidate"))?;

        let out = super::run_consolidate(dir.path(), FactScope::Project)
            .map_err(ctx("run_consolidate"))?;

        assert!(out.text.contains("Merges: 1"), "text was: {}", out.text);
        assert!(
            out.text.contains("memory-steward"),
            "muss auf den fehlenden Agentenlauf hinweisen"
        );
        assert!(out.text.contains("FEHLT"));

        // Kandidat wurde konsumiert, Fakt wurde verschmolzen (neuere Beschreibung gewinnt).
        assert!(incoming.list().map_err(ctx("list incoming"))?.is_empty());
        let merged = store
            .read("tui-approval-arming")
            .map_err(ctx("read merged"))?
            .ok_or(TestError::Missing("must exist"))?;
        assert_eq!(merged.description, "neue Beschreibung");

        // Baseline wurde geschrieben und deckt den frisch verschmolzenen Fakt ab.
        let baseline = harw_memory::consolidation::ConsolidationBaseline::read(dir.path())
            .map_err(ctx("read baseline"))?;
        assert!(baseline.digests.contains_key("tui-approval-arming"));

        // Zweiter Lauf ohne neue Kandidaten: nichts zu tun, keine weiteren Änderungen seit Baseline.
        let second = super::run_consolidate(dir.path(), FactScope::Project)
            .map_err(ctx("second run_consolidate"))?;
        assert!(second.text.contains("nichts zu tun"));
        assert!(
            second
                .text
                .contains("0 Fakt(en) seit letzter Baseline geändert")
        );
        Ok(())
    }
}
