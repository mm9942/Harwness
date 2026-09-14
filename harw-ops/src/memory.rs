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

use std::path::PathBuf;
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
    /// Subcommand: `list`, `stats`, `recall`, `record`, `forget`, `maintain`.
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
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter /memory-Subcommand: {other} (list, stats, recall, record, forget, maintain)"
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
    let project = harw_home::discover_project(cwd, &markers)
        .map_err(|error| OpError::Execution(format!("Projekt-Erkennung fehlgeschlagen: {error}")))?;
    let project_home = harw_home::ProjectHome::at(&project);
    project_home
        .ensure()
        .map_err(|error| OpError::Execution(format!("Projekt-Home anlegen fehlgeschlagen: {error}")))?;
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
            Some(existing) if existing.body == body => return Ok(candidate),
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
    let store = FactStore::open(&root, scope)
        .map_err(|error| OpError::Execution(format!("Fakt-Speicher öffnen fehlgeschlagen: {error}")))?;
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
        return Ok(OpOutput::from(format!("Keine Treffer für: {}", tail.join(" "))));
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
        return Err(OpError::InvalidArguments("/memory forget <name>".to_owned()));
    };

    let mut deleted_from = Vec::new();
    if let Ok(root) = project_memories_root(ctx) {
        if let Ok(store) = FactStore::open(&root, FactScope::Project) {
            if store
                .delete(name)
                .map_err(|error| OpError::Execution(format!("Fakt löschen fehlgeschlagen (Projekt): {error}")))?
            {
                deleted_from.push("Projekt");
            }
        }
    }
    if let Ok(root) = global_memories_root() {
        if let Ok(store) = FactStore::open(&root, FactScope::Global) {
            if store
                .delete(name)
                .map_err(|error| OpError::Execution(format!("Fakt löschen fehlgeschlagen (Global): {error}")))?
            {
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

#[cfg(test)]
mod tests {
    use super::{
        MemoryArgs, MemoryOperation, parse_memory_scope_flags, truncate_description, unique_slug,
    };
    use crate::testutil::toks;
    use harw_memory::{Fact, FactScope, FactStore, FactType};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpError, Operation};

    #[test]
    fn from_raw_args_no_tokens_uses_default_sub() {
        let a = MemoryArgs::from_raw_args(&toks(&[])).unwrap();
        assert!(a.sub.is_none());
        assert!(a.tail.is_empty());
    }

    #[test]
    fn from_raw_args_captures_sub_and_tail() {
        let a = MemoryArgs::from_raw_args(&toks(&["recall", "popup", "enter"])).unwrap();
        assert_eq!(a.sub.as_deref(), Some("recall"));
        assert_eq!(a.tail, vec!["popup".to_owned(), "enter".to_owned()]);
    }

    #[test]
    fn from_raw_args_maintain_has_no_tail() {
        let a = MemoryArgs::from_raw_args(&toks(&["maintain"])).unwrap();
        assert_eq!(a.sub.as_deref(), Some("maintain"));
        assert!(a.tail.is_empty());
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
    fn test_parse_memory_scope_flags_defaults_to_project() {
        let (positional, scope) =
            parse_memory_scope_flags(&toks(&["hallo", "welt"]), FactScope::Project).unwrap();
        assert_eq!(positional, vec!["hallo".to_owned(), "welt".to_owned()]);
        assert_eq!(scope, FactScope::Project);
    }

    #[test]
    fn test_parse_memory_scope_flags_reads_global() {
        let (positional, scope) =
            parse_memory_scope_flags(&toks(&["hallo", "--global"]), FactScope::Project).unwrap();
        assert_eq!(positional, vec!["hallo".to_owned()]);
        assert_eq!(scope, FactScope::Global);
    }

    #[test]
    fn test_parse_memory_scope_flags_rejects_conflicting_flags() {
        let result =
            parse_memory_scope_flags(&toks(&["--project", "--global"]), FactScope::Project);
        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("widersprüchliche")),
            other => panic!("expected invalid arguments, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_memory_scope_flags_repeating_same_flag_is_not_a_conflict() {
        let (_positional, scope) =
            parse_memory_scope_flags(&toks(&["--project", "--project"]), FactScope::Global).unwrap();
        assert_eq!(scope, FactScope::Project);
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

    fn temp_fact_store(label: &str) -> (tempfile::TempDir, FactStore) {
        let dir = tempfile::Builder::new()
            .prefix(&format!("harw-memory-facts-{label}-"))
            .tempdir()
            .expect("tempdir");
        let store = FactStore::open(dir.path(), FactScope::Project).expect("open store");
        (dir, store)
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
    fn test_unique_slug_returns_base_when_free() {
        let (_dir, store) = temp_fact_store("free");
        let name = unique_slug(&store, "mein-fakt", "Text A").expect("resolve slug");
        assert_eq!(name, "mein-fakt");
    }

    #[test]
    fn test_unique_slug_returns_base_when_identical_body_already_exists() {
        let (_dir, store) = temp_fact_store("identical");
        store.write(&sample_fact("mein-fakt", "Text A")).expect("seed fact");
        let name = unique_slug(&store, "mein-fakt", "Text A").expect("resolve slug");
        assert_eq!(name, "mein-fakt", "identical content re-uses the same slug");
    }

    #[test]
    fn test_unique_slug_appends_suffix_on_content_collision() {
        let (_dir, store) = temp_fact_store("collision");
        store.write(&sample_fact("mein-fakt", "Text A")).expect("seed fact");
        let name = unique_slug(&store, "mein-fakt", "Text B, komplett anders").expect("resolve slug");
        assert_eq!(name, "mein-fakt-2");
    }

    #[test]
    fn test_record_fact_and_forget_round_trip_via_fact_store_directly() {
        // Deckt den Fakt-Store-Teil des Rundlaufs ab (Schreiben, Lesen,
        // Löschen), ohne `harw_home`/Projekt-Erkennung zu berühren — das
        // übernehmen `project_memories_root`/`global_memories_root`, die
        // bewusst nicht gegen einen echten `$HOME` getestet werden (siehe
        // `permissions.rs`-Tests für die Begründung).
        let (_dir, store) = temp_fact_store("round-trip");
        let fact = sample_fact("mein-fakt", "Ein Beispieltext für den Fakt-Speicher.");
        store.write(&fact).expect("write fact");

        let read_back = store.read("mein-fakt").expect("read").expect("fact must exist");
        assert_eq!(read_back.body, fact.body);

        let deleted = store.delete("mein-fakt").expect("delete");
        assert!(deleted);
        assert!(store.read("mein-fakt").expect("read after delete").is_none());
    }
}
