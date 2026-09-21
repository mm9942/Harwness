//! `harw lens` — den Wissensindex des Retrieval-Subsystems (`harw-lens`)
//! bauen, aktualisieren oder seinen Status anzeigen.
//!
//! # Die Produktionslücke, die dieses Modul schließt
//! `harw-tool-lens` bewirbt `lens.ask` als Agenten-Werkzeug, und `harw-lens`
//! (die Fassade) bietet [`harw_lens::build`] seit Knoten AW5-10 an — aber
//! bis zu diesem Knoten rief **niemand außerhalb der Lens-Crates selbst**
//! `build`/[`harw_lens::collect_design_docs`]/
//! [`harw_lens::collect_palace_documents`] in Produktion auf.
//! `harw-lens-source`s eigene Moduldokumentation sagt es ausdrücklich: „Der
//! Build ist ein Job auf der vorhandenen Job-Bridge, kein eigener
//! Scheduler" — dieser Job existierte bislang nicht. Ohne ihn fragt
//! `lens.ask` ausschließlich leere Indizes ab. Dieses Modul **ist** dieser
//! Einstiegspunkt: ein Betreiberbefehl, kein automatischer Hintergrundlauf
//! (siehe unten, „Warum kein automatischer Bau").
//!
//! # Warum genau diese beiden Quellen, genau diese Sichtbarkeiten
//! `harw-tool-lens`s [`harw_tool_lens::scope::KNOWN_SELECTORS`] listet
//! **exakt** die `(index_name, visibility)`-Paare, die `lens.ask`
//! tatsächlich befragt:
//! `docs.design`@`workspace` und `knowledge.palace`@{`workspace`,
//! `operator-only`}. Dieses Modul baut **genau diese** Paare — kein
//! `knowledge.diary`, kein `code.rust`, obwohl `harw-lens-source` beide
//! Erfassungen längst anbietet: `lens.ask` fragt sie (Stand dieses Knotens)
//! nicht ab, ein hier gebauter Index bliebe toter Speicherplatz. Sobald ein
//! künftiger Knoten `KNOWN_SELECTORS` erweitert, muss dieses Modul
//! entsprechend mitwachsen — nicht vorher raten.
//!
//! # Warum derselbe Embedder wie `lens.ask`
//! [`harw_lens_types::IndexManifest::compatible_with`] lehnt jede Abfrage
//! ab, deren `model`/`chunker_version` vom Manifest des durchsuchten Index
//! abweicht (K43, fail-closed, nie stillschweigend). Baute dieser Befehl mit
//! einem anderen Modellnamen als [`harw_tool_lens::provenance::ask_embedder`]
//! tatsächlich verwendet, wäre der gebaute Index für `lens.ask`
//! **grundsätzlich unerreichbar** — nicht leer, sondern permanent
//! inkompatibel. [`cmd_build`] ruft deshalb dieselben drei Funktionen auf
//! ([`harw_tool_lens::provenance::ask_embedder`]/`ask_descriptor`/
//! `ask_provenance`), statt eine zweite, unabhängige Auswahl zu treffen, die
//! auseinanderlaufen könnte. Ohne konfigurierten entfernten Endpunkt
//! (`HARW_LENS_ASK_REMOTE_EMBED_BASE_URL`/`_API_KEY`, siehe
//! `harw_tool_lens::provenance`s Moduldoku) bleibt das der deterministische
//! Platzhalter — ein echter Bau ohne semantischen Gehalt, aber End-to-End
//! funktionsfähig: `lens.ask` findet Treffer, auch wenn ihre Rangfolge ohne
//! ein echtes Embedding-Modell nicht aussagekräftig ist.
//!
//! # Warum kein automatischer Bau beim Sitzungsstart
//! Eine Indizierung über `docs/` und den vollständigen Memory Palace kann
//! bei einem gewachsenen Bestand mehrere Minuten dauern (Chunking plus
//! Embedding jedes neuen Chunks). Ein automatischer Bau beim
//! Sitzungsstart überraschte den Nutzer mit einer unerwarteten Wartezeit,
//! die mit seiner eigentlichen Anfrage nichts zu tun hat. Dieser Befehl ist
//! deshalb der einzige Bau-Einstiegspunkt — bewusst manuell, wie `harw
//! catalog --refresh` oder `harw update`.
//!
//! # `--force`
//! Ein gebauter Index ist inkrementell: ein zweiter Bau über unverändertes
//! Material ruft den Embedder kein einziges Mal erneut auf (siehe
//! `harw_lens_source::build_index`s Moduldoku). `--force` verwirft trotzdem
//! den bereits gespeicherten Index samt seines Embedding-Caches vor dem
//! Bau — ein garantiert vollständiger statt eines inkrementellen Neuaufbaus,
//! z. B. zur Fehlersuche. `harw-lens-store` bietet dafür bewusst **keine**
//! Lösch-Methode an (L1: „Lens löscht nie etwas", siehe dessen
//! Moduldokumentation); [`force_clear`] wischt deshalb direkt über die in
//! `harw-lens-store`s Kratedokumentation öffentlich dokumentierte
//! Ablagekonvention (`# Ablageform`: `<store>/lens_store/index/<name>/`),
//! nicht über eine private API.
//!
//! # Wo Indizes liegen
//! Exakt dort, wo `lens.ask` sie sucht: unter
//! [`harw_home::paths::visibility_index_dir`] relativ zum **Root-Space**
//! (`--home`/`HARW_HOME`, ohne Profilunterordner) — [`harw_tool_lens::ask_tool`]
//! löst `home` selbst über `harw_home::paths::home_dir()` auf, nicht über ein
//! Profilverzeichnis. Dieses Modul folgt exakt derselben Auflösung (über
//! `crate::home::resolve_home`, das ohne `--home` auf denselben Wert wie
//! `harw_home::paths::home_dir()` fällt), damit ein Bau ohne `--home` und
//! eine Abfrage ohne `--home` garantiert denselben Ort treffen. Nur die
//! **Quelle** von `knowledge.palace` (der Memory Palace, aus dem
//! [`harw_lens::collect_palace_documents`] liest) liegt unter dem aktiven
//! Profil (`<home>/profiles/<profil>/knowledge`, wie
//! `crate::gateway::run`s Knowledge-Store-Montage) — das ist keine neue
//! Konvention, sondern dieselbe, die der Gateway-Prozess bereits für denselben
//! Zweck verwendet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use harw_lens::{Embedder, Metric};
use harw_knowledge::{KnowledgeIndex, KnowledgeStore};

use crate::cli::LensAction;

/// Schwelle, ab der [`cmd_status`] einen Index als möglicherweise veraltet
/// kennzeichnet — ein grobes Signal (siehe `harw_lens_source::index_status`s
/// Begründung, warum es keine bessere Zeitquelle als die Manifest-`mtime`
/// gibt), kein Beweis, dass sich Quellinhalte tatsächlich geändert haben.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Führt `harw lens [action]` aus.
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter `--home`-Wert; siehe
///   [`crate::home::resolve_home`].
/// - `action` (`Option<LensAction>`): Unterbefehl, oder `None` für den
///   Status (wie bei `crate::sandbox_cmd::run`).
///
/// # Errors
/// Ein `String` mit Kontext, wenn die Home-Auflösung, das Einlesen der
/// Quellen oder der Bau/die Statusabfrage selbst scheitert.
pub fn run(home_override: Option<PathBuf>, action: Option<LensAction>) -> Result<(), String> {
    match action.unwrap_or(LensAction::Status) {
        LensAction::Status => cmd_status(home_override),
        LensAction::Build { source, force } => cmd_build(home_override, source, force),
    }
}

/// Welche Quellmenge ein `--source`-Wert benennt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `docs.design`, aus `collect_design_docs`.
    Docs,
    /// `knowledge.palace`, aus `collect_palace_documents`.
    Knowledge,
}

/// Prüft und normalisiert einen `--source`-Wert.
///
/// # Returns
/// `Ok(None)`, wenn keiner angegeben wurde (beide Quellen); sonst
/// `Ok(Some(Source))`.
///
/// # Errors
/// Ein `String`, wenn `raw` weder `"docs"` noch `"knowledge"` ist.
fn parse_source(raw: Option<&str>) -> Result<Option<Source>, String> {
    match raw {
        None => Ok(None),
        Some("docs") => Ok(Some(Source::Docs)),
        Some("knowledge") => Ok(Some(Source::Knowledge)),
        Some(other) => Err(format!(
            "unbekannte Quellmenge '{other}': gültig sind 'docs' (docs.design) und 'knowledge' \
             (knowledge.palace)"
        )),
    }
}

/// Führt `harw lens build`/`harw lens build --source <name>` aus.
fn cmd_build(home_override: Option<PathBuf>, source: Option<String>, force: bool) -> Result<(), String> {
    let home = crate::home::resolve_home(home_override)?;
    crate::home::ensure_home(&home).map_err(|error| error.to_string())?;
    let requested = parse_source(source.as_deref())?;

    // Dieselbe Auswahl wie `lens.ask` -- siehe den `//!`-Block dieses
    // Moduls, Abschnitt „Warum derselbe Embedder wie `lens.ask`".
    let embedder = harw_tool_lens::provenance::ask_embedder();
    let descriptor = harw_tool_lens::provenance::ask_descriptor();
    let provenance = harw_tool_lens::provenance::ask_provenance();
    let locality = embedder.locality();

    println!(
        "Baue mit Modell '{}' (Lokalität {:?}, Chunker-Fassung {}).",
        provenance.model, locality, provenance.chunker_version
    );

    let mut built_any = false;

    if requested.is_none() || requested == Some(Source::Docs) {
        built_any |= build_docs_source(
            &home,
            embedder.as_ref(),
            &descriptor,
            &provenance.model,
            locality,
            force,
        )?;
    }

    if requested.is_none() || requested == Some(Source::Knowledge) {
        built_any |= build_knowledge_source(
            &home,
            embedder.as_ref(),
            &descriptor,
            &provenance.model,
            locality,
            force,
        )?;
    }

    if !built_any {
        println!("Kein Index gebaut -- keine Quelldokumente gefunden.");
    }
    Ok(())
}

/// Baut `docs.design` aus `docs/` im aktuellen Arbeitsverzeichnis.
///
/// # Returns
/// `true`, wenn mindestens ein Dokument gefunden (und gebaut) wurde.
fn build_docs_source(
    home: &Path,
    embedder: &dyn Embedder,
    descriptor: &harw_lens::EmbeddingDescriptor,
    model: &str,
    locality: harw_lens::Locality,
    force: bool,
) -> Result<bool, String> {
    let cwd = std::env::current_dir().map_err(|error| format!("Arbeitsverzeichnis nicht lesbar: {error}"))?;
    let docs_root = cwd.join("docs");
    let documents = harw_lens::collect_design_docs(&docs_root)
        .map_err(|error| format!("{}: Design-Dokumente einlesen: {error}", harw_lens::DOCS_DESIGN_INDEX))?;

    if documents.is_empty() {
        println!(
            "{}: keine Markdown-Dokumente unter {} gefunden -- kein Bau.",
            harw_lens::DOCS_DESIGN_INDEX,
            docs_root.display()
        );
        return Ok(false);
    }

    if force {
        force_clear(home, harw_lens::DOCS_DESIGN_INDEX, harw_lens::DEFAULT_VISIBILITY)?;
    }

    let reports = harw_lens::build(
        home,
        harw_lens::DOCS_DESIGN_INDEX,
        &documents,
        model,
        locality,
        Metric::Cosine,
        embedder,
        descriptor,
    )
    .map_err(|error| format!("{}: Bau fehlgeschlagen: {error}", harw_lens::DOCS_DESIGN_INDEX))?;
    print_reports(&reports);
    Ok(true)
}

/// Baut `knowledge.palace` aus dem Memory Palace des aktiven Profils.
///
/// # Returns
/// `true`, wenn mindestens ein Palace-Artefakt gefunden (und gebaut) wurde.
fn build_knowledge_source(
    home: &Path,
    embedder: &dyn Embedder,
    descriptor: &harw_lens::EmbeddingDescriptor,
    model: &str,
    locality: harw_lens::Locality,
    force: bool,
) -> Result<bool, String> {
    let index = load_knowledge_index(home)?;
    let documents = harw_lens::collect_palace_documents(&index);

    if documents.is_empty() {
        println!(
            "{}: keine Palace-Artefakte im aktiven Profil gefunden -- kein Bau.",
            harw_lens::KNOWLEDGE_PALACE_INDEX
        );
        return Ok(false);
    }

    if force {
        force_clear(home, harw_lens::KNOWLEDGE_PALACE_INDEX, harw_lens::DEFAULT_VISIBILITY)?;
        force_clear(
            home,
            harw_lens::KNOWLEDGE_PALACE_INDEX,
            harw_lens::OPERATOR_ONLY_VISIBILITY,
        )?;
    }

    let reports = harw_lens::build(
        home,
        harw_lens::KNOWLEDGE_PALACE_INDEX,
        &documents,
        model,
        locality,
        Metric::Cosine,
        embedder,
        descriptor,
    )
    .map_err(|error| format!("{}: Bau fehlgeschlagen: {error}", harw_lens::KNOWLEDGE_PALACE_INDEX))?;
    print_reports(&reports);
    Ok(true)
}

/// Lädt den `KnowledgeIndex` des aktiven Profils -- dieselbe Montage wie
/// `crate::gateway::run` (`<home>/profiles/<profil>/knowledge`).
fn load_knowledge_index(home: &Path) -> Result<KnowledgeIndex, String> {
    let profile_name = harw_home::active_profile_name(home);
    let profile = harw_home::profile_dir(home, &profile_name).map_err(|error| error.to_string())?;
    let knowledge_root = profile.join("knowledge");
    let store = KnowledgeStore::new(&knowledge_root);
    KnowledgeIndex::rebuild(&store).map_err(|error| format!("Memory Palace einlesen: {error}"))
}

/// Druckt einen [`harw_lens::IndexBuildReport`] je Sichtbarkeits-Bucket.
fn print_reports(reports: &[harw_lens::IndexBuildReport]) {
    for report in reports {
        println!(
            "  {} @ {}: {} Chunks ({} neu eingebettet, {} aus dem Cache übernommen)",
            report.index_name, report.visibility, report.chunk_count, report.embedded_count, report.reused_count
        );
    }
}

/// Löscht einen bereits gebauten Index samt seines Embedding-Caches vom
/// Datenträger -- siehe den `//!`-Block dieses Moduls, Abschnitt
/// „`--force`", für die vollständige Begründung.
fn force_clear(home: &Path, index_name: &str, visibility: &str) -> Result<(), String> {
    let store_root =
        harw_home::paths::visibility_index_dir(home, visibility).map_err(|error| error.to_string())?;
    let lens_store_root = harw_home::paths::lens_store_dir(&store_root);
    for name in [index_name.to_owned(), format!("{index_name}.embedding-cache")] {
        let dir = lens_store_root.join("index").join(&name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|error| format!("Index-Verzeichnis {} entfernen: {error}", dir.display()))?;
        }
    }
    Ok(())
}

/// Führt `harw lens status` aus.
fn cmd_status(home_override: Option<PathBuf>) -> Result<(), String> {
    let home = crate::home::resolve_home(home_override)?;
    println!("Lens-Indizes (Root-Space {}):", home.display());
    println!();

    let mut any_missing = false;
    for &(index_name, visibility) in harw_tool_lens::scope::KNOWN_SELECTORS {
        match harw_lens::index_status(&home, index_name, visibility)
            .map_err(|error| format!("{index_name}@{visibility}: {error}"))?
        {
            Some(status) => print_status_line(&status),
            None => {
                any_missing = true;
                println!(
                    "  {index_name} @ {visibility}: nicht gebaut -- `harw lens build` ausführen."
                );
            }
        }
    }

    println!();
    if any_missing {
        println!(
            "Hinweis: `lens.ask` liefert für einen fehlenden Index keine leere Antwort, \
             sondern überspringt ihn (`FederatedOutcome::skipped`). Fehlende Treffer bedeuten \
             hier also: nie gebaut, nicht \"nichts gefunden\"."
        );
    } else {
        println!("Alle von `lens.ask` befragten Indizes sind gebaut.");
    }
    Ok(())
}

/// Druckt eine Statuszeile für einen gefundenen Index, inklusive eines
/// Veraltungs-Hinweises ab [`STALE_AFTER`].
fn print_status_line(status: &harw_lens::IndexStatus) {
    let age = status.modified.and_then(|modified| modified.elapsed().ok());
    let age_text = age.map_or_else(|| "unbekannt".to_owned(), format_age);
    let dimension_text = status
        .dimension
        .map_or_else(|| "-".to_owned(), |dimension| dimension.to_string());

    println!(
        "  {} @ {}: Modell '{}', Dimension {}, Lokalität {:?}, Chunker v{}, {} Chunks, Alter {}{}",
        status.index_name,
        status.visibility,
        status.model,
        dimension_text,
        status.locality,
        status.chunker_version,
        status.chunk_count,
        age_text,
        stale_suffix(age),
    );
}

/// Liefert den Hinweistext, wenn `age` [`STALE_AFTER`] überschreitet.
fn stale_suffix(age: Option<Duration>) -> &'static str {
    match age {
        Some(age) if age >= STALE_AFTER => {
            " -- könnte veraltet sein, `harw lens build` erneut ausführen"
        }
        _ => "",
    }
}

/// Formatiert eine Dauer grob als Alter (`s`/`min`/`h`/`d`).
fn format_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 60 * 60 {
        format!("{}min", secs / 60)
    } else if secs < 24 * 60 * 60 {
        format!("{}h", secs / (60 * 60))
    } else {
        format!("{}d", secs / (24 * 60 * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_source_accepts_known_names() {
        assert_eq!(parse_source(None).unwrap(), None);
        assert_eq!(parse_source(Some("docs")).unwrap(), Some(Source::Docs));
        assert_eq!(
            parse_source(Some("knowledge")).unwrap(),
            Some(Source::Knowledge)
        );
    }

    #[test]
    fn test_parse_source_rejects_unknown_name() {
        let result = parse_source(Some("nonsense"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nonsense"));
    }

    #[test]
    fn test_format_age_uses_the_coarsest_fitting_unit() {
        assert_eq!(format_age(Duration::from_secs(30)), "30s");
        assert_eq!(format_age(Duration::from_secs(120)), "2min");
        assert_eq!(format_age(Duration::from_secs(2 * 60 * 60)), "2h");
        assert_eq!(format_age(Duration::from_secs(3 * 24 * 60 * 60)), "3d");
    }

    #[test]
    fn test_stale_suffix_empty_below_threshold_and_set_at_or_above() {
        assert_eq!(stale_suffix(None), "");
        assert_eq!(stale_suffix(Some(Duration::from_secs(60))), "");
        assert_eq!(stale_suffix(Some(STALE_AFTER - Duration::from_secs(1))), "");
        assert_ne!(stale_suffix(Some(STALE_AFTER)), "");
        assert_ne!(stale_suffix(Some(STALE_AFTER + Duration::from_secs(1))), "");
    }

    #[test]
    fn test_cmd_build_and_status_round_trip_with_deterministic_embedder() {
        // Kein Netzwerk, kein `harw_tool_lens::provenance`-Aufruf (der würde
        // die reale Prozessumgebung lesen): dieser Test übt stattdessen
        // direkt `harw_lens::build`/`index_status` an denselben Pfaden, die
        // `cmd_build`/`cmd_status` auflösen, um die Pfadkonvention selbst zu
        // belegen (siehe den `//!`-Block dieses Moduls, Abschnitt „Wo
        // Indizes liegen").
        let home = tempfile::tempdir().expect("tempdir");
        let docs_root = home.path().join("docs-src");
        std::fs::create_dir_all(&docs_root).expect("docs-src anlegen");
        std::fs::write(docs_root.join("intro.md"), "# Intro\n\nEin Testabsatz.\n")
            .expect("intro.md schreiben");

        let documents = harw_lens::collect_design_docs(&docs_root).expect("liest Dokumente");
        assert_eq!(documents.len(), 1);

        let embedder = harw_lens::DeterministicEmbedder::new(8);
        let descriptor = harw_lens::EmbeddingDescriptor {
            document_prefix: "passage: ".to_owned(),
            query_prefix: "query: ".to_owned(),
            normalize: false,
        };

        let before = harw_lens::index_status(
            home.path(),
            harw_lens::DOCS_DESIGN_INDEX,
            harw_lens::DEFAULT_VISIBILITY,
        )
        .expect("kein Fehler vor dem Bau");
        assert_eq!(before, None);

        harw_lens::build(
            home.path(),
            harw_lens::DOCS_DESIGN_INDEX,
            &documents,
            "test-model",
            harw_lens::Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor,
        )
        .expect("baut");

        let after = harw_lens::index_status(
            home.path(),
            harw_lens::DOCS_DESIGN_INDEX,
            harw_lens::DEFAULT_VISIBILITY,
        )
        .expect("kein Fehler nach dem Bau")
        .expect("Index existiert nach dem Bau");
        assert_eq!(after.chunk_count, 1);
        assert_eq!(after.model, "test-model");
    }

    #[test]
    fn test_force_clear_removes_index_directory_and_is_idempotent_when_absent() {
        let home = tempfile::tempdir().expect("tempdir");

        // Ohne vorherigen Bau ist dies ein No-Op, kein Fehler.
        force_clear(home.path(), "docs.design", harw_lens::DEFAULT_VISIBILITY)
            .expect("Löschen eines nie gebauten Index darf nicht fehlschlagen");

        let documents = vec![harw_lens::RawDocument {
            source: harw_lens::SourceRef::File {
                path: "intro.md".to_owned(),
            },
            text: "# Intro\n\nText.\n".to_owned(),
            visibility: harw_lens::DEFAULT_VISIBILITY.to_owned(),
        }];
        let embedder = harw_lens::DeterministicEmbedder::new(8);
        let descriptor = harw_lens::EmbeddingDescriptor {
            document_prefix: "passage: ".to_owned(),
            query_prefix: "query: ".to_owned(),
            normalize: false,
        };
        harw_lens::build(
            home.path(),
            harw_lens::DOCS_DESIGN_INDEX,
            &documents,
            "test-model",
            harw_lens::Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor,
        )
        .expect("baut");
        assert!(harw_lens::index_status(
            home.path(),
            harw_lens::DOCS_DESIGN_INDEX,
            harw_lens::DEFAULT_VISIBILITY
        )
        .expect("liest")
        .is_some());

        force_clear(home.path(), "docs.design", harw_lens::DEFAULT_VISIBILITY)
            .expect("Löschen eines vorhandenen Index muss gelingen");

        assert_eq!(
            harw_lens::index_status(
                home.path(),
                harw_lens::DOCS_DESIGN_INDEX,
                harw_lens::DEFAULT_VISIBILITY
            )
            .expect("liest"),
            None
        );
    }
}
