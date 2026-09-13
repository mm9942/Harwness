//! Golden-Test für die Programmbibliothek der Kontextprogramme (AW6-05).
//!
//! Spezifikationsquelle: `harw-registry-defaults/agents/context-programs/TESTS.txt`
//! (Punkt 6) und die neun `golden/*.golden.txt`-Dateien selbst, die laut ihrer
//! eigenen `STATUS`-Zeile *von Hand aus der Renderlogik abgeleitet, nicht
//! durch einen tatsächlichen Lauf erzeugt* wurden. Dieser Test schließt genau
//! diese Lücke: er löst jedes der neun Programme über
//! [`harw_agent_dsl::context_program::resolve_context_program`] auf, rendert
//! das Ergebnis über [`harw_agent_dsl::context_program::render_context_program`]
//! und vergleicht es gegen den Abschnitt „Gerenderter Turn-Kontext
//! (Platzhalter-Inhalt)" der zugehörigen Golden-Datei.
//!
//! # Warum nur dieser eine Abschnitt verglichen wird
//! Jede Golden-Datei trägt vor diesem Abschnitt eine von Hand geschriebene
//! Kopfzeile, eine Methodik-Erklärung und eine Sektionstabelle — reine
//! Dokumentation für Menschen, die diese Fixture lesen, kein Feld, das
//! [`render_context_program`] produziert (der Renderer erhält nur ein
//! [`ResolvedContextProgramDefinition`], keine Angabe über seine eigene
//! Entstehungsgeschichte). Ein Vergleich der gesamten Datei würde diese
//! Dokumentation fälschlich als Renderer-Ausgabe behandeln.
//!
//! # Herkunft der neun `.toml`-Definitionen
//! Wird bewusst nicht über `harw_registry_defaults::embedded_agents` geladen
//! (das ist der eingebettete Loader für **Rollen**-Definitionen, AW6-00,
//! `harw-registry-defaults/src/**` — außerhalb des Schreibbereichs dieses
//! Knotens). Stattdessen liest dieser Test die zehn `.toml`-Dateien unter
//! `agents/context-programs/` direkt vom Dateisystem — ein reiner
//! Test-Fixture-Zugriff, keine Verletzung der Reinheitszusage des Renderers
//! selbst (§ Moduldoku von `context_program.rs`).
//!
//! [`ResolvedContextProgramDefinition`]: harw_agent_dsl::context_program::ResolvedContextProgramDefinition

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use harw_agent_dsl::context_program::{
    render_context_program, resolve_context_program, RawContextProgramDefinition,
    ResolvedContextProgramDefinition,
};
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::layers::DefinitionLayer;

/// Marker-Zeile, ab der eine Golden-Datei den tatsächlich zu vergleichenden
/// Renderer-Output trägt (§ Moduldoku „Warum nur dieser eine Abschnitt").
const RENDER_MARKER: &str = "--- Gerenderter Turn-Kontext (Platzhalter-Inhalt) ---";

/// Die neun Rollenprogramme dieser Bibliothek (AW6-05), ohne `base` selbst —
/// `base` ist die gemeinsame Basis, kein eigenständig golden-getestetes
/// Programm (§ TESTS.txt Punkt 6: „Ein Golden Render je Programm (neun...)").
const NINE_PROGRAMS: [&str; 9] = [
    "curate",
    "explore",
    "implement",
    "orchestrate",
    "plan",
    "research-deps",
    "research-web",
    "review",
    "triage",
];

/// Verzeichnis mit den zehn `.toml`-Quelldefinitionen dieses Knotens.
fn context_programs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("agents/context-programs")
}

/// Lädt alle `.toml`-Dateien direkt unter `agents/context-programs/` (nicht
/// rekursiv — `golden/` und `TESTS.txt` liegen daneben, nicht darunter) und
/// parst sie zu `(DefinitionLayer::BuiltIn, RawContextProgramDefinition)`.
///
/// Alle zehn Layer-Einträge tragen denselben [`DefinitionLayer::BuiltIn`]:
/// dieser Test prüft `extends` zwischen unterschiedlichen `id`s, keine
/// Overlay-Patches über Layer-Prioritäten hinweg — dafür ist die relative
/// Layer-Reihenfolge irrelevant (siehe `resolve_context_program`, das nach
/// `target_id` sucht, nicht nach Layer, sobald keine zwei Einträge dieselbe
/// `id` teilen).
fn load_layers() -> Vec<(DefinitionLayer, RawContextProgramDefinition)> {
    let dir = context_programs_dir();
    let entries = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("agents/context-programs/ nicht lesbar ({dir:?}): {e}"));

    let mut layers = Vec::new();
    for entry in entries {
        let entry = entry.expect("Verzeichniseintrag sollte lesbar sein");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let src = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{path:?} nicht lesbar: {e}"));
        let raw: RawContextProgramDefinition = toml::from_str(&src)
            .unwrap_or_else(|e| panic!("{path:?} parst nicht als Kontextprogramm: {e}"));
        layers.push((DefinitionLayer::BuiltIn, raw));
    }
    assert_eq!(
        layers.len(),
        10,
        "erwartet base.toml + neun Rollenprogramme unter {dir:?}, gefunden: {}",
        layers.len()
    );
    layers
}

/// Löst `name` (z. B. `"curate"`) über die geladenen Layer zu
/// `harwness.context.<name>@1` auf.
fn resolve(
    layers: &[(DefinitionLayer, RawContextProgramDefinition)],
    name: &str,
) -> ResolvedContextProgramDefinition {
    let id = DefinitionId::parse(&format!("harwness.context.{name}@1"))
        .unwrap_or_else(|e| panic!("ungültige ID für Programm '{name}': {e}"));
    resolve_context_program(&id, layers, time::OffsetDateTime::now_utc())
        .unwrap_or_else(|e| panic!("Auflösung von '{name}' schlägt fehl: {e}"))
}

/// Extrahiert den erwarteten Renderer-Output aus einer Golden-Datei: alles
/// nach [`RENDER_MARKER`], von der führenden Leerzeile befreit, ohne
/// nachgestellte Leerzeichen/Zeilenumbrüche.
fn expected_render(golden_path: &Path) -> String {
    let content = fs::read_to_string(golden_path)
        .unwrap_or_else(|e| panic!("Golden-Datei {golden_path:?} nicht lesbar: {e}"));
    let idx = content.find(RENDER_MARKER).unwrap_or_else(|| {
        panic!("Golden-Datei {golden_path:?} enthält nicht die Marker-Zeile '{RENDER_MARKER}'")
    });
    let after_marker = &content[idx + RENDER_MARKER.len()..];
    after_marker.trim_start_matches('\n').trim_end().to_owned()
}

/// Vergleicht `actual` gegen `expected` und meldet bei Abweichung **welches
/// Programm** und **welche Zeile** zuerst voneinander abweicht (§ Auftrag:
/// „die Fehlermeldung muss zeigen, welches Programm und welche Zeile
/// abweicht, nicht nur 'nicht gleich'").
fn assert_render_matches(program: &str, actual: &str, expected: &str) {
    if actual == expected {
        return;
    }
    let actual_lines: Vec<&str> = actual.split('\n').collect();
    let expected_lines: Vec<&str> = expected.split('\n').collect();
    let max_len = actual_lines.len().max(expected_lines.len());
    for i in 0..max_len {
        let a = actual_lines.get(i).copied();
        let e = expected_lines.get(i).copied();
        if a != e {
            panic!(
                "Golden-Abweichung bei Programm '{program}', Zeile {line}:\n  \
                 erwartet (golden): {expected:?}\n  \
                 tatsächlich (renderer): {actual:?}",
                line = i + 1,
                expected = e.unwrap_or("<Datei endet hier>"),
                actual = a.unwrap_or("<Renderer-Ausgabe endet hier>"),
            );
        }
    }
    // Sollte nie erreicht werden, wenn `actual != expected`: Fallback, falls
    // beide Zeilenlisten identisch sind, sich aber z. B. nur in einem
    // führenden/nachgestellten Zeichen außerhalb der Zeilenaufteilung
    // unterscheiden.
    panic!("Golden-Abweichung bei Programm '{program}', aber keine einzelne Zeile identifizierbar");
}

/// Der wichtigste Test dieses Knotens (§ TESTS.txt Punkt 6): alle neun
/// Programme werden aufgelöst, gerendert und gegen ihre Golden-Datei
/// verglichen.
#[test]
fn all_nine_programs_render_match_golden() {
    let layers = load_layers();
    let golden_dir = context_programs_dir().join("golden");

    for name in NINE_PROGRAMS {
        let resolved = resolve(&layers, name);
        let actual = render_context_program(&resolved);
        let golden_path = golden_dir.join(format!("{name}.golden.txt"));
        let expected = expected_render(&golden_path);
        assert_render_matches(name, &actual, &expected);
    }
}

/// Zwei Läufe über dasselbe aufgelöste Programm liefern byte-genau dieselbe
/// Ausgabe (§ Auftrag „Determinismus"). Ohne das wäre der Golden-Vergleich
/// oben wertlos, da ein flackernder Renderer jeden Lauf zufällig grün oder
/// rot färben könnte.
#[test]
fn render_is_byte_identical_across_repeated_calls() {
    let layers = load_layers();
    for name in NINE_PROGRAMS {
        let resolved = resolve(&layers, name);
        let first = render_context_program(&resolved);
        let second = render_context_program(&resolved);
        assert_eq!(
            first, second,
            "render_context_program('{name}') ist nicht deterministisch"
        );
    }
}

/// Ein Programm, das `base` über `extends` erweitert, zeigt im gerenderten
/// Ergebnis das **aufgelöste** Programm — inklusive der von `base` geerbten
/// Sektionen `task.objective`/`history.tail` —, nicht nur seine eigene, rohe
/// `[[sections]]`-Deklaration (§ Auftrag: „Prüfe, dass der Renderer das
/// aufgelöste Ergebnis zeigt, nicht die rohe Deklaration").
///
/// `curate.toml` deklariert selbst nur zwei Sektionen
/// (`knowledge.candidates`, `memory.palace_index`); würde der Test hier nur
/// die rohe Deklaration statt der Auflösung sehen, schlüge diese Prüfung
/// fehl.
#[test]
fn extends_renders_resolved_program_not_raw_declaration() {
    let layers = load_layers();

    let raw_curate = layers
        .iter()
        .map(|(_, def)| def)
        .find(|def| def.id.name == "curate")
        .expect("curate.toml sollte geladen sein");
    assert_eq!(
        raw_curate.sections.len(),
        2,
        "rohe curate.toml sollte nur ihre zwei eigenen Sektionen deklarieren"
    );

    let resolved = resolve(&layers, "curate");
    assert_eq!(
        resolved.sections.len(),
        4,
        "aufgelöstes curate-Programm sollte die zwei geerbten base-Sektionen \
         plus seine zwei eigenen tragen"
    );

    let section_names: Vec<&str> = resolved
        .sections
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        section_names.contains(&"task.objective"),
        "aufgelöstes curate-Programm sollte die von base geerbte Sektion \
         'task.objective' tragen: {section_names:?}"
    );
    assert!(
        section_names.contains(&"history.tail"),
        "aufgelöstes curate-Programm sollte die von base geerbte Sektion \
         'history.tail' tragen: {section_names:?}"
    );

    let rendered = render_context_program(&resolved);
    assert!(
        rendered.contains("## task.objective"),
        "gerendertes curate-Programm sollte den Header der geerbten Sektion \
         'task.objective' enthalten:\n{rendered}"
    );
    assert!(
        rendered.contains("## history.tail"),
        "gerendertes curate-Programm sollte den Header der geerbten Sektion \
         'history.tail' enthalten:\n{rendered}"
    );
}

/// Jede der neun `extends`-Auflösungen trägt tatsächlich einen `"base"`-
/// Auflösungsschritt, der exakt auf `harwness.context.base@1` zeigt (§
/// TESTS.txt Punkt 3) — nicht nur irgendeinen `"base"`-Schritt.
#[test]
fn every_program_traces_a_base_step_pointing_at_base_definition() {
    let layers = load_layers();
    for name in NINE_PROGRAMS {
        let resolved = resolve(&layers, name);
        let has_base_step = resolved
            .trace
            .steps
            .iter()
            .any(|step| step.kind == "base" && step.source == "harwness.context.base@1");
        assert!(
            has_base_step,
            "Programm '{name}' sollte einen 'base'-Schritt tragen, der exakt \
             auf 'harwness.context.base@1' zeigt; trace={:?}",
            resolved.trace.steps
        );
    }
}

/// Regressionstest für die Diff-Fehlermeldung selbst (§ Auftrag: „die
/// Fehlermeldung muss zeigen, welches Programm und welche Zeile abweicht").
/// Erzeugt absichtlich zwei unterschiedliche Texte und prüft, dass die
/// `panic`-Nachricht von [`assert_render_matches`] den Programmnamen und die
/// abweichende Zeilennummer nennt.
#[test]
fn assert_render_matches_names_program_and_line_on_mismatch() {
    let actual = "## a [instruction]\n<eins>\n\n## b [evidence]\n<zwei>";
    let expected = "## a [instruction]\n<eins>\n\n## b [evidence]\n<ANDERS>";

    let result = std::panic::catch_unwind(|| {
        assert_render_matches("beispiel-programm", actual, expected);
    });
    let err = result.expect_err("assert_render_matches sollte bei Abweichung panicken");
    let message = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .expect("panic-Payload sollte ein String sein");

    assert!(
        message.contains("beispiel-programm"),
        "Fehlermeldung sollte den Programmnamen nennen: {message}"
    );
    assert!(
        message.contains("Zeile 5"),
        "Fehlermeldung sollte die abweichende Zeilennummer (5) nennen: {message}"
    );
}

/// Keine Namenskollision zwischen den zehn Kontextprogramm-IDs dieses
/// Knotens (informativ; die vollständige Disjunktheits-Prüfung gegen
/// Rollennamen aus AW6-00 gehört laut TESTS.txt Punkt 2 einem zentralen
/// Verzeichnis-Loader, der außerhalb dieses Schreibbereichs liegt).
#[test]
fn ten_context_program_ids_are_pairwise_distinct() {
    let layers = load_layers();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for (_, def) in &layers {
        let key = def.id.as_string();
        assert!(
            seen.insert(key.clone(), ()).is_none(),
            "doppelte Kontextprogramm-ID: {key}"
        );
    }
    assert_eq!(seen.len(), 10);
}
