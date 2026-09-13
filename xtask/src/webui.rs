//! Typerzeugung und Frische-Prüfung für die Control-Plane-TypeScript-Schicht.
//!
//! # Verantwortungsbereich
//! Dieses Modul erzeugt `webui/lib/generated/operations.ts` — die
//! TypeScript-Sicht auf jede `Surface::Web`-Deklaration im Workspace — und
//! bietet einen Prüfmodus, der eine veraltete erzeugte Datei sichtbar macht,
//! statt sie still driften zu lassen.
//!
//! # Woher die Daten kommen — und warum nicht durch Linken
//! Der naheliegende Weg wäre, `xtask` gegen `harw-operations` (oder gegen
//! `harw-ops::register_all`, das die tatsächlich befüllte Registry liefert)
//! zu linken und die Registry zur Laufzeit abzufragen. Das wurde geprüft und
//! bewusst **nicht** getan:
//!
//! - `harw-operations` selbst trägt keine einzige Operation — die Literale
//!   leben in `harw-ops` und Geschwister-Crates. Um die *tatsächliche*
//!   Registry zu bekommen, müsste `xtask` gegen `harw-ops` linken.
//! - `harw-ops` hängt (Stand dieses Knotens) von nahezu der gesamten
//!   Business-Logik-Ebene des Workspace ab (`harw-core`, `harw-provider`,
//!   `harw-session-store`, `harw-plan`, `harw-knowledge`, …). `xtask` ist
//!   heute eine schlanke Kante mit einer einzigen internen Abhängigkeit
//!   (`harw-code-graph`, siehe `xtask/Cargo.toml`) — genau damit ein
//!   CI-Werkzeug nicht selbst zum größten Kompilat des Workspace wird
//!   (dasselbe Prinzip, aus dem `gate_edges.rs` `cargo metadata` als
//!   Subprozess vermeidet).
//! - Dieser Knoten (UI-01) besitzt weder `xtask/Cargo.toml` noch irgendeine
//!   andere Crate — eine neue Abhängigkeit hätte ohnehin nicht gesetzt werden
//!   können, ohne den Schreibbereich zu verlassen.
//!
//! Die Auflage lautet ausdrücklich: **melden statt erzwingen**. Deshalb liest
//! dieses Modul die `OperationMeta`/`Surface::Web`-Literale **textuell** aus
//! dem Rust-Quelltext aller Workspace-Crates (Crate-Liste und -Verzeichnisse
//! kommen aus [`harw_code_graph::WorkspaceGraph`], bereits eine bestehende
//! `xtask`-Abhängigkeit — keine neue). Das ist kein vollständiger Parser,
//! sondern ein klammer- und string-bewusster Scanner, der genau das Muster
//! erkennt, in dem dieser Workspace `OperationMeta { .. }` und
//! `Surface::Web { .. }` überall schreibt (siehe `harw-web`- und
//! `harw-operations`-Moduldoku für Belegstellen). Er ist **fragiler** als ein
//! echter Parser gegenüber Umformatierung — das ist der Preis für „kein
//! Linken, keine neue Abhängigkeit". Ein sauberer Nachfolgeschritt (für einen
//! späteren Knoten, außerhalb dieses Schreibbereichs) wäre entweder (a)
//! `xtask/Cargo.toml` gezielt um eine reine Syntax-Bibliothek wie `syn` zu
//! erweitern, oder (b) `harw-ops` selbst einen kleinen Dump-Binary an die
//! Hand zu geben, der die Registry als JSON exportiert, das `xtask` dann nur
//! noch liest (kein Linken, kein Parsen).
//!
//! # Ausschluss von Testfixturen
//! Unit-Test-Operationen (`mod tests { .. }`-Blöcke, wie sie
//! `harw-web::router`, `harw-operations::adapter::web` u. a. für ihre
//! Tier-/Surface-Tests anlegen) deklarieren häufig eigene, frei erfundene
//! `Surface::Web`-Pfade (`/api/observer`, `/api/dup`, …). Diese dürfen nicht
//! in der erzeugten Datei erscheinen — sie sind keine echten Routen. Der
//! Scanner ermittelt deshalb zuerst die Byte-Spannen aller `mod tests { .. }`
//! Blöcke einer Datei und verwirft jeden `OperationMeta`-Fund, dessen Anfang
//! in einer dieser Spannen liegt.
//!
//! # Erzeugen vs. Prüfen — und was im CI läuft
//! - `cargo xtask webui types` **erzeugt** `webui/lib/generated/operations.ts`
//!   neu und überschreibt die bestehende Datei.
//! - `cargo xtask webui types --check` **prüft nur**: es erzeugt den Inhalt
//!   im Speicher und vergleicht ihn byte-genau mit der vorhandenen Datei,
//!   ohne zu schreiben. Weicht sie ab oder fehlt sie, liefert der Aufruf
//!   `Err(..)` mit einer für Menschen lesbaren Anweisung.
//! - **Im CI läuft der Prüfmodus** (`--check`). Der Erzeugen-Modus ist ein
//!   Entwickler-Werkzeug, das nach einer `Surface::Web`-Änderung von Hand
//!   aufgerufen wird — ohne das CI-Gate würde die generierte Datei sonst
//!   still veralten, genau die Auflage, die dieser Knoten adressiert.
//!
//! # Was eine veraltete Datei bedeutet
//! Eine veraltete `operations.ts` ist keine kaputte Datei — sie kompiliert
//! weiterhin. Sie beschreibt aber eine Route, ein Tier oder eine
//! Approval-Politik, die der Server nicht mehr (oder noch nicht) kennt. Der
//! Fehler erscheint dann nicht beim Bauen, sondern als kaputte Seite zur
//! Laufzeit — exakt das Risiko, das der Prüfmodus in einen CI-Fehler
//! vorzieht.
//!
//! # Stand
//! Gerüst aus Knoten AW0-00; der Inhalt entsteht in Knoten **UI-01**.

use std::fs;
use std::path::{Path, PathBuf};

use harw_code_graph::WorkspaceGraph;

/// Pfad der erzeugten Datei, relativ zur Workspace-Wurzel.
const GENERATED_RELATIVE_PATH: &str = "webui/lib/generated/operations.ts";

/// Kopfzeilen der erzeugten Datei — nennt den Erzeuger und den
/// Regenerierungsbefehl, damit niemand versehentlich von Hand editiert.
const HEADER: &str = "\
// ACHTUNG: automatisch erzeugte Datei — nicht von Hand bearbeiten.
//
// Erzeugt von `cargo xtask webui types` aus den `Surface::Web`-Deklarationen
// im Rust-Quelltext (siehe xtask/src/webui.rs für die Extraktionsregeln).
// Bei Abweichung schlägt `cargo xtask webui types --check` fehl — das ist
// das CI-Gate für diese Datei.
//
// Neu erzeugen: `cargo xtask webui types`
";

/// Eine aus einer `Surface::Web`-Deklaration extrahierte HTTP-Route.
///
/// # Description
/// Fläche-neutrale Zwischendarstellung zwischen dem Rust-Quelltext-Scanner
/// und dem TypeScript-Renderer. Trägt bewusst nur, was
/// `harw_operations::adapter::WebAdapter` selbst kennt — siehe dessen
/// Moduldoku (`path`, `readonly`, `approval`, `permission`,
/// `operation_name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebRouteDescriptor {
    /// Maschinenlesbarer Name der Operation (`OperationMeta::name`).
    pub operation: String,
    /// `OperationMeta::summary`, falls im Quelltext gefunden — wird als
    /// Kommentarzeile über dem Routeneintrag ausgegeben.
    pub summary: Option<String>,
    /// HTTP-Pfad aus `Surface::Web { path, .. }`.
    pub path: String,
    /// `Surface::Web { readonly, .. }` — bestimmt `GET` (true) vs. `POST`.
    pub readonly: bool,
    /// Variantenname aus `PermissionTier::<Name>` (`OperationMeta::permission`).
    pub permission: String,
    /// Variantenname aus `ApprovalPolicy::<Name>` (`Surface::Web { approval, .. }`).
    pub approval: String,
}

/// Führt eine Oberflächen-Aufgabe aus.
///
/// # Description
/// Der einzige bekannte Aufgabenname ist `types`. Ein optionales `--check`
/// danach wechselt vom Erzeugen- in den Prüfmodus (siehe Moduldoku).
///
/// # Arguments
/// - `args` (`&[String]`): `["types"]` oder `["types", "--check"]`.
///
/// # Returns
/// `Ok(())` bei Erfolg (Datei erzeugt bzw. Datei bereits frisch).
///
/// # Errors
/// Ein `String` mit einer für Menschen lesbaren Beschreibung: unbekannte
/// Aufgabe, nicht lesbarer Workspace-Graph, nicht lesbare Quelldatei, oder —
/// im Prüfmodus — eine veraltete oder fehlende erzeugte Datei.
///
/// # Examples
/// ```rust,no_run
/// // cargo xtask webui types
/// // cargo xtask webui types --check
/// ```
pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("types") => {
            let check_only = args.iter().skip(1).any(|arg| arg == "--check");
            cmd_types(Path::new("."), check_only)
        }
        Some(other) => Err(format!(
            "unbekannte webui-Aufgabe '{other}' (bekannt: types)"
        )),
        None => Err("Aufruf: cargo xtask webui types [--check]".to_owned()),
    }
}

/// Erzeugt oder prüft `webui/lib/generated/operations.ts`.
///
/// # Description
/// Lädt den Workspace-Graphen ab `workspace_root`, extrahiert alle
/// `Surface::Web`-Routen (siehe [`discover_routes`]), rendert sie als
/// TypeScript (siehe [`render_typescript`]) und übergibt das Ergebnis an
/// [`write_or_check`].
///
/// # Arguments
/// - `workspace_root` (`&Path`): Wurzelverzeichnis des Cargo-Workspace.
/// - `check_only` (`bool`): `true` = nur prüfen, nicht schreiben.
///
/// # Returns
/// `Ok(())` bei Erfolg.
///
/// # Errors
/// Siehe [`run`].
fn cmd_types(workspace_root: &Path, check_only: bool) -> Result<(), String> {
    let graph = WorkspaceGraph::load(workspace_root)
        .map_err(|error| format!("Workspace-Graph unter '{}' nicht lesbar: {error}", workspace_root.display()))?;
    let routes = discover_routes(&graph)?;
    let rendered = render_typescript(&routes);
    let out_path = workspace_root.join(GENERATED_RELATIVE_PATH);
    write_or_check(&out_path, &rendered, check_only)
}

/// Schreibt `rendered` nach `out_path`, oder prüft nur, ob es dort bereits
/// steht.
///
/// # Description
/// Im Schreibmodus wird das Elternverzeichnis bei Bedarf angelegt. Im
/// Prüfmodus wird **nicht** geschrieben — eine fehlende oder abweichende
/// Datei ist ein `Err`, keine stille Reparatur.
///
/// # Arguments
/// - `out_path` (`&Path`): Zielpfad der erzeugten Datei.
/// - `rendered` (`&str`): der frisch gerenderte Inhalt.
/// - `check_only` (`bool`): `true` = nur prüfen.
///
/// # Returns
/// `Ok(())`, wenn geschrieben wurde bzw. die Datei bereits frisch ist.
///
/// # Errors
/// - Schreibmodus: das Elternverzeichnis lässt sich nicht anlegen, oder die
///   Datei lässt sich nicht schreiben.
/// - Prüfmodus: die Datei fehlt, oder ihr Inhalt weicht vom frisch
///   gerenderten Inhalt ab.
///
/// # Examples
/// ```rust,no_run
/// // siehe Modultests: test_write_or_check_reports_missing_file,
/// // test_write_or_check_reports_stale_file, test_write_or_check_accepts_fresh_file
/// ```
fn write_or_check(out_path: &Path, rendered: &str, check_only: bool) -> Result<(), String> {
    if check_only {
        return match fs::read_to_string(out_path) {
            Ok(existing) if existing == rendered => Ok(()),
            Ok(_) => Err(format!(
                "{} ist veraltet gegenüber den Surface::Web-Deklarationen im Quelltext — \
                 'cargo xtask webui types' erneut ausführen",
                out_path.display()
            )),
            Err(_) => Err(format!(
                "{} fehlt — 'cargo xtask webui types' ausführen",
                out_path.display()
            )),
        };
    }
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Verzeichnis '{}' nicht anlegbar: {error}", parent.display()))?;
    }
    fs::write(out_path, rendered)
        .map_err(|error| format!("'{}' nicht schreibbar: {error}", out_path.display()))
}

/// Sammelt alle `Surface::Web`-Routen über den gesamten Workspace.
///
/// # Description
/// Iteriert über jede [`harw_code_graph::CrateNode`] des Graphen, liest jede
/// `.rs`-Datei unter deren `src/`-Verzeichnis (Integrationstests unter
/// `tests/` liegen außerhalb von `src/` und werden dadurch bereits
/// ausgeschlossen) und ruft [`extract_routes_from_source`] auf jede Datei
/// auf. Das Ergebnis wird nach `path` sortiert, damit die erzeugte Datei
/// unabhängig von der Dateisystem-Durchlaufreihenfolge deterministisch ist.
///
/// # Arguments
/// - `graph` (`&WorkspaceGraph`): der geladene Workspace-Graph.
///
/// # Returns
/// Alle gefundenen Routen, sortiert nach `path`.
///
/// # Errors
/// Wenn ein `src/`-Verzeichnis oder eine `.rs`-Datei nicht lesbar ist.
///
/// # Concurrency
/// Rein sequentiell; keine Nebenläufigkeit.
fn discover_routes(graph: &WorkspaceGraph) -> Result<Vec<WebRouteDescriptor>, String> {
    let mut routes = Vec::new();
    for krate in &graph.crates {
        let src_dir = krate.dir.join("src");
        if !src_dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_rs_files(&src_dir, &mut files)?;
        for file in files {
            let content = fs::read_to_string(&file)
                .map_err(|error| format!("'{}' nicht lesbar: {error}", file.display()))?;
            routes.extend(extract_routes_from_source(&content));
        }
    }
    routes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(routes)
}

/// Sammelt rekursiv alle `.rs`-Dateipfade unter `dir`.
///
/// # Arguments
/// - `dir` (`&Path`): Startverzeichnis.
/// - `out` (`&mut Vec<PathBuf>`): Sammelziel; wird ergänzt, nicht geleert.
///
/// # Returns
/// `Ok(())` bei Erfolg — Ergebnisse stehen in `out`.
///
/// # Errors
/// Wenn `dir` oder ein Untereintrag nicht lesbar ist.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("'{}' nicht lesbar: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("Verzeichniseintrag in '{}' nicht lesbar: {error}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Extrahiert alle `Surface::Web`-Routen aus einer einzelnen Rust-Quelldatei.
///
/// # Description
/// Reine, dateisystemfreie Funktion — die eigentliche Extraktionslogik,
/// unabhängig getestet (siehe Modultests). Findet jeden
/// `OperationMeta { .. }`-Literalblock (klammer- und string-bewusst, siehe
/// [`matching_delimiter`]), verwirft Treffer innerhalb eines
/// `mod tests { .. }`-Blocks (siehe [`test_module_ranges`] und Moduldoku),
/// und liest aus jedem verbleibenden Block `name`, `summary`, `permission`
/// sowie jeden enthaltenen `Surface::Web { .. }`-Eintrag.
///
/// # Arguments
/// - `source` (`&str`): vollständiger Inhalt einer `.rs`-Datei.
///
/// # Returns
/// Alle in dieser Datei gefundenen Routen, in Fundreihenfolge.
fn extract_routes_from_source(source: &str) -> Vec<WebRouteDescriptor> {
    let cleaned = strip_comments(source);
    let source = cleaned.as_str();
    let test_ranges = test_module_ranges(source);
    let mut routes = Vec::new();

    for (open, close) in find_struct_blocks(source, "OperationMeta") {
        if test_ranges.iter().any(|(start, end)| open >= *start && open <= *end) {
            continue;
        }
        let block = &source[open..=close];
        let Some(name) = field_str(block, "name") else {
            continue;
        };
        let permission = field_str(block, "permission")
            .map(|raw| strip_path_prefix(&raw, "PermissionTier"))
            .unwrap_or_else(|| "Observer".to_owned());
        let summary = field_str(block, "summary");

        let Some(surfaces_span) = bracketed_field(block, "surfaces") else {
            continue;
        };
        let surfaces_block = &block[surfaces_span.0..=surfaces_span.1];

        for (web_open, web_close) in find_struct_blocks(surfaces_block, "Surface::Web") {
            let web_block = &surfaces_block[web_open..=web_close];
            let Some(path) = field_str(web_block, "path") else {
                continue;
            };
            let readonly = field_str(web_block, "readonly")
                .map(|raw| raw.trim() == "true")
                .unwrap_or(false);
            let approval = field_str(web_block, "approval")
                .map(|raw| strip_path_prefix(&raw, "ApprovalPolicy"))
                .unwrap_or_else(|| "None".to_owned());

            routes.push(WebRouteDescriptor {
                operation: name.clone(),
                summary: summary.clone(),
                path,
                readonly,
                permission: permission.clone(),
                approval,
            });
        }
    }

    routes
}

/// Entfernt ein `"Prefix::"`-Präfix von einem Enum-Pfad-Token, falls
/// vorhanden; sonst liefert sie den getrimmten Text unverändert.
///
/// # Arguments
/// - `raw` (`&str`): rohes Token, z. B. `"PermissionTier::Owner"` oder nur `"Owner"`.
/// - `prefix` (`&str`): erwartetes Präfix ohne `::`, z. B. `"PermissionTier"`.
///
/// # Returns
/// Den Variantennamen ohne Präfix, getrimmt.
fn strip_path_prefix(raw: &str, prefix: &str) -> String {
    let trimmed = raw.trim();
    let full_prefix = format!("{prefix}::");
    trimmed
        .strip_prefix(&full_prefix)
        .unwrap_or(trimmed)
        .trim()
        .to_owned()
}

/// `true`, wenn `c` Teil eines Rust-Bezeichners sein kann.
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Ersetzt jeden `//`-Zeilenkommentar und jeden `/* .. */`-Blockkommentar
/// durch Leerzeichen gleicher Länge — string- und zeichenliteral-bewusst.
///
/// # Description
/// Läuft dieser Extraktion **immer vorweg** (siehe Aufrufstelle in
/// [`extract_routes_from_source`]): dieser Workspace schreibt seine
/// Moduldokumentation fast durchgehend als `///`/`//!`-Kommentare mit
/// vollständigen, lauffähigen `OperationMeta`/`Surface::Web`-Beispielen
/// (siehe `harw-web`- und `harw-operations`-Moduldoku). Ohne diesen
/// Vorverarbeitungsschritt läse der textuelle Scanner Dokumentationsbeispiele
/// als echte Routen — ein Fehler, den ein Parser mit echtem Sprachverständnis
/// nicht hätte, den dieser bewusst einfache Scanner aber ohne diesen Schritt
/// gemacht hätte.
///
/// # Arguments
/// - `source` (`&str`): vollständiger Dateiinhalt.
///
/// # Returns
/// Eine Kopie von `source` gleicher Byte-Länge, in der jeder
/// Kommentar-Byte (außer dem Zeilenumbruch, der einen Zeilenkommentar
/// beendet) durch ein Leerzeichen ersetzt ist. String- und
/// Zeichenliteral-Inhalte bleiben unverändert erhalten.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();
    let mut i = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;

    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '\'' {
                in_char = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            i += 1;
            continue;
        }
        if c == '\'' {
            if looks_like_char_literal(bytes, i) {
                in_char = true;
            }
            i += 1;
            continue;
        }
        if c == '/' && bytes.get(i + 1) == Some(&b'/') {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            for byte in &mut out[start..i] {
                *byte = b' ';
            }
            continue;
        }
        if c == '/' && bytes.get(i + 1) == Some(&b'*') {
            let start = i;
            i += 2;
            while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            let end = (i + 2).min(bytes.len());
            for byte in &mut out[start..end] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
            i = end;
            continue;
        }
        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| source.to_owned())
}

/// Entscheidet, ob ein `'`-Byte an `idx` ein Zeichenliteral eröffnet oder ein
/// Lifetime-Suffix ist (`'a`, `'static`, …).
///
/// # Description
/// Ohne diese Unterscheidung würde jedes Vorkommen einer Lifetime — z. B.
/// `RouteDecision<'a>` in `harw-web::router` oder `&'static str` in
/// `OperationMeta` selbst — fälschlich einen Zeichenliteral-Modus eröffnen,
/// der dann bis zum nächsten zufälligen `'` im Rest der Datei alle Klammern,
/// Kommentare und Strings dazwischen verschluckt. Ein echtes
/// Zeichenliteral ist entweder eine Escape-Sequenz (`'\n'`, `'\\'`, `'\u{7f}'`)
/// oder genau ein Zeichen zwischen zwei `'` (`'x'`); eine Lifetime ist ein
/// `'` gefolgt von einem Bezeichner ohne unmittelbar folgendes schließendes
/// `'`.
///
/// # Arguments
/// - `bytes` (`&[u8]`): der vollständige Quelltext als Bytes.
/// - `idx` (`usize`): Index des öffnenden `'`.
///
/// # Returns
/// `true`, wenn an `idx` mit hoher Wahrscheinlichkeit ein Zeichenliteral
/// beginnt; `false`, wenn es sich eher um eine Lifetime handelt.
fn looks_like_char_literal(bytes: &[u8], idx: usize) -> bool {
    match bytes.get(idx + 1) {
        Some(b'\\') => bytes[idx + 1..]
            .iter()
            .take(12)
            .any(|&b| b == b'\''),
        Some(_) => bytes.get(idx + 2) == Some(&b'\''),
        None => false,
    }
}

/// Findet die Byte-Spannen aller `mod tests { .. }`-Blöcke einer Datei.
///
/// # Description
/// Erkennt das Schlüsselwort `mod`, gefolgt von Leerraum und dem Bezeichner
/// `tests` (mit Wortgrenzen auf beiden Seiten), gefolgt von optionalem
/// Leerraum und `{`. Ignoriert bewusst, ob ein `#[cfg(test)]` davor steht —
/// dieser Workspace benennt Testmodule ausnahmslos `tests` (siehe
/// `harw-web`- und `harw-operations`-Testmodule), und ein namensbasierter
/// Ausschluss ist robuster als eine Attribut-Erkennung gegenüber
/// Umformatierung.
///
/// # Arguments
/// - `source` (`&str`): vollständiger Dateiinhalt.
///
/// # Returns
/// Eine Liste von `(start, end)`-Byte-Indizes (inklusive), je einer pro
/// gefundenem `mod tests`-Block.
fn test_module_ranges(source: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut i = 0;
    while let Some(rel) = source[i..].find("mod") {
        let idx = i + rel;
        let before_ok = idx == 0 || !is_ident_char(bytes[idx - 1] as char);
        let after_mod = idx + 3;
        let mod_word_ok = before_ok
            && bytes
                .get(after_mod)
                .map(|&b| !is_ident_char(b as char))
                .unwrap_or(true);
        if mod_word_ok {
            let mut j = after_mod;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if source[j..].starts_with("tests") {
                let after_tests = j + 5;
                let tests_word_ok = bytes
                    .get(after_tests)
                    .map(|&b| !is_ident_char(b as char))
                    .unwrap_or(true);
                if tests_word_ok {
                    let mut k = after_tests;
                    while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                        k += 1;
                    }
                    if bytes.get(k) == Some(&b'{') {
                        if let Some(close) = matching_delimiter(source, k, '{', '}') {
                            ranges.push((k, close));
                            i = close + 1;
                            continue;
                        }
                    }
                }
            }
        }
        i = idx + 3;
    }
    ranges
}

/// Findet alle Vorkommen von `needle` gefolgt (nach optionalem Leerraum) von
/// `{`, und liefert je Fund die Byte-Spanne des geklammerten Blocks
/// (einschließlich der Klammern).
///
/// # Description
/// `needle` wird als zusammenhängender Text gesucht (z. B. `"OperationMeta"`
/// oder `"Surface::Web"`); vor und nach dem Textende wird geprüft, dass kein
/// Bezeichnerzeichen anschließt, damit z. B. `"MyOperationMeta"` nicht
/// fälschlich zu `"OperationMeta"` passt. Ein Treffer, dem am Ende kein `{`
/// folgt (z. B. ein Funktionsrückgabetyp `-> &OperationMeta` ohne
/// unmittelbar folgenden Literalblock), wird verworfen.
///
/// # Arguments
/// - `source` (`&str`): der zu durchsuchende Text.
/// - `needle` (`&str`): der gesuchte Bezeichner/Pfad, z. B. `"Surface::Web"`.
///
/// # Returns
/// `(open, close)`-Byte-Indexpaare, je eines pro gefundenem Block, in
/// Fundreihenfolge. `open`/`close` zeigen auf die öffnende bzw. schließende
/// geschweifte Klammer selbst.
fn find_struct_blocks(source: &str, needle: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut blocks = Vec::new();
    let mut i = 0;
    while let Some(rel) = source[i..].find(needle) {
        let idx = i + rel;
        let before_ok = idx == 0 || !is_ident_char(bytes[idx - 1] as char);
        let after = idx + needle.len();
        let after_ok = bytes
            .get(after)
            .map(|&b| !is_ident_char(b as char))
            .unwrap_or(true);
        if before_ok && after_ok {
            let mut j = after;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if bytes.get(j) == Some(&b'{') {
                if let Some(close) = matching_delimiter(source, j, '{', '}') {
                    blocks.push((j, close));
                    i = close + 1;
                    continue;
                }
            }
        }
        i = idx + needle.len();
    }
    blocks
}

/// Findet die Byte-Spanne des geklammerten Werts eines Feldes, z. B.
/// `surfaces: vec![ .. ]` oder `surfaces: [ .. ]`.
///
/// # Description
/// Sucht `field:` (siehe [`find_field_colon`]), dann das nächste `[` **ab
/// dieser Position** und matcht es klammer-bewusst gegen sein `]`. Ein
/// eventuelles `vec!` zwischen Doppelpunkt und `[` wird stillschweigend
/// übersprungen, da es kein Bestandteil des gesuchten Werts ist.
///
/// # Arguments
/// - `block` (`&str`): der zu durchsuchende Literalblock (bereits ohne
///   umgebenden Kontext).
/// - `field` (`&str`): der Feldname, z. B. `"surfaces"`.
///
/// # Returns
/// `Some((open, close))` — Byte-Indizes von `[` und `]` innerhalb von
/// `block`; `None`, wenn das Feld oder ein `[` danach fehlt.
fn bracketed_field(block: &str, field: &str) -> Option<(usize, usize)> {
    let colon_end = find_field_colon(block, field)?;
    let open = block[colon_end..].find('[')? + colon_end;
    let close = matching_delimiter(block, open, '[', ']')?;
    Some((open, close))
}

/// Liest den Wert eines einfachen Feldes (String-Literal oder
/// Pfad-/Bezeichner-Token) aus einem Literalblock.
///
/// # Description
/// Sucht `field:` (siehe [`find_field_colon`]) und liest den unmittelbar
/// folgenden Wert: ein String-Literal wird entquotet (einfache
/// `\`-Escape-Behandlung), alles andere wird als Roh-Token bis zum nächsten
/// `,` oder `}` auf derselben Verschachtelungstiefe gelesen und getrimmt.
///
/// # Arguments
/// - `block` (`&str`): der zu durchsuchende Literalblock.
/// - `field` (`&str`): der Feldname, z. B. `"path"`, `"permission"`.
///
/// # Returns
/// `Some(wert)`, wenn das Feld gefunden wurde; sonst `None`.
fn field_str(block: &str, field: &str) -> Option<String> {
    let mut pos = find_field_colon(block, field)?;
    let bytes = block.as_bytes();
    while pos < bytes.len() && (bytes[pos] as char).is_whitespace() {
        pos += 1;
    }
    if pos >= bytes.len() {
        return None;
    }
    if bytes[pos] == b'"' {
        let start = pos + 1;
        let mut end = start;
        let mut escaped = false;
        while end < bytes.len() {
            let c = bytes[end];
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                break;
            }
            end += 1;
        }
        Some(block[start..end].to_owned())
    } else {
        let start = pos;
        let mut end = start;
        while end < bytes.len() && bytes[end] != b',' && bytes[end] != b'}' {
            end += 1;
        }
        let value = block[start..end].trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_owned())
        }
    }
}

/// Findet die Byte-Position unmittelbar **nach** dem `:` eines Feldes.
///
/// # Description
/// Sucht `field` mit Wortgrenzen auf beiden Seiten, gefolgt (nach
/// optionalem Leerraum) von genau einem `:` (nicht `::`, damit ein Pfad wie
/// `Surface::path` nicht fälschlich als Feld `path` gilt).
///
/// # Arguments
/// - `block` (`&str`): der zu durchsuchende Text.
/// - `field` (`&str`): der gesuchte Feldname.
///
/// # Returns
/// `Some(index)` der Position direkt nach dem `:`; `None`, wenn kein
/// passendes Vorkommen existiert.
fn find_field_colon(block: &str, field: &str) -> Option<usize> {
    let bytes = block.as_bytes();
    let mut i = 0;
    while let Some(rel) = block[i..].find(field) {
        let idx = i + rel;
        let before_ok = idx == 0 || !is_ident_char(bytes[idx - 1] as char);
        let after = idx + field.len();
        let after_ok = bytes
            .get(after)
            .map(|&b| !is_ident_char(b as char))
            .unwrap_or(true);
        if before_ok && after_ok {
            let mut j = after;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if bytes.get(j) == Some(&b':') && bytes.get(j + 1) != Some(&b':') {
                return Some(j + 1);
            }
        }
        i = idx + field.len();
    }
    None
}

/// Findet die schließende Klammer, die zu der öffnenden Klammer an
/// `open_idx` gehört — string-, zeichen- und kommentarbewusst.
///
/// # Description
/// Zählt die Verschachtelungstiefe von `open`/`close` ab `open_idx`, ohne
/// Vorkommen innerhalb von `"..."`-Zeichenketten, `'.'`-Zeichenliteralen,
/// `//`-Zeilenkommentaren oder `/* .. */`-Blockkommentaren mitzuzählen.
/// Ohne diese Behandlung würde z. B. eine geschweifte Klammer in einem
/// Doku-Kommentar die Blockgrenze falsch verschieben.
///
/// # Arguments
/// - `source` (`&str`): der zu durchsuchende Text.
/// - `open_idx` (`usize`): Byte-Index der öffnenden Klammer selbst.
/// - `open` (`char`): das öffnende Klammerzeichen (`'{'` oder `'['`).
/// - `close` (`char`): das schließende Klammerzeichen (`'}'` oder `']'`).
///
/// # Returns
/// `Some(index)` der schließenden Klammer; `None`, wenn der Text vor
/// Erreichen der Tiefe `0` endet.
fn matching_delimiter(source: &str, open_idx: usize, open: char, close: char) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open_idx;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut escaped = false;

    while i < bytes.len() {
        let c = bytes[i] as char;

        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if c == '*' && bytes.get(i + 1) == Some(&b'/') {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '\'' {
                in_char = false;
            }
            i += 1;
            continue;
        }

        if c == '/' && bytes.get(i + 1) == Some(&b'/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if c == '/' && bytes.get(i + 1) == Some(&b'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if c == '"' {
            in_string = true;
            i += 1;
            continue;
        }
        if c == '\'' {
            if looks_like_char_literal(bytes, i) {
                in_char = true;
            }
            i += 1;
            continue;
        }

        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Rendert eine Liste von Routen als TypeScript-Quelltext.
///
/// # Description
/// Erzeugt Typalias für [`crate::webui`]-seitig bekannte Enums
/// (`PermissionTier`, `ApprovalPolicy`, `HttpMethod`), das
/// `WebRouteDescriptor`-Interface und ein `WEB_ROUTES`-Array — eines pro
/// übergebener Route, mit optionaler Kommentarzeile aus `summary`. Die
/// Ausgabe ist deterministisch bei sortierter Eingabe (siehe
/// [`discover_routes`]).
///
/// # Arguments
/// - `routes` (`&[WebRouteDescriptor]`): die zu rendernden Routen, bereits
///   in der gewünschten Ausgabereihenfolge.
///
/// # Returns
/// Vollständiger TypeScript-Quelltext, inklusive Kopfzeilen.
fn render_typescript(routes: &[WebRouteDescriptor]) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    out.push('\n');
    out.push_str("export type PermissionTier = \"Observer\" | \"Operator\" | \"Maintainer\" | \"Owner\";\n");
    out.push_str(
        "export type ApprovalPolicy =\n  | \"None\"\n  | \"Always\"\n  | \"RequireForScope\"\n  \
         | \"RequireForEffect\"\n  | \"RequireForRiskClass\";\n",
    );
    out.push_str("export type HttpMethod = \"GET\" | \"POST\";\n\n");
    out.push_str("export interface WebRouteDescriptor {\n");
    out.push_str("  readonly operation: string;\n");
    out.push_str("  readonly path: string;\n");
    out.push_str("  readonly method: HttpMethod;\n");
    out.push_str("  readonly permission: PermissionTier;\n");
    out.push_str("  readonly approval: ApprovalPolicy;\n");
    out.push_str("}\n\n");
    out.push_str("export const WEB_ROUTES: readonly WebRouteDescriptor[] = [\n");
    for route in routes {
        if let Some(summary) = &route.summary {
            out.push_str(&format!("  // {}\n", summary.replace('\n', " ")));
        }
        let method = if route.readonly { "GET" } else { "POST" };
        out.push_str(&format!(
            "  {{ operation: {op}, path: {path}, method: \"{method}\", permission: \"{perm}\", approval: \"{appr}\" }},\n",
            op = ts_string(&route.operation),
            path = ts_string(&route.path),
            perm = route.permission,
            appr = route.approval,
        ));
    }
    out.push_str("] as const;\n");
    out
}

/// Quotet einen String für den Einsatz als TypeScript-Zeichenkettenliteral.
///
/// # Arguments
/// - `value` (`&str`): der rohe Wert.
///
/// # Returns
/// `value`, in doppelte Anführungszeichen gefasst, mit escaptem `\` und `"`.
fn ts_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::{
        WebRouteDescriptor, extract_routes_from_source, render_typescript, write_or_check,
    };

    /// Eine Beispiel-Quelldatei mit genau einer produktiven `Surface::Web`-Route.
    const GOLDEN_SOURCE: &str = r#"
use harw_operations::operation::{
    ApprovalPolicy, OperationMeta, PermissionTier, Surface,
};

impl Operation for SessionListOp {
    fn meta(&self) -> &OperationMeta {
        static META: std::sync::OnceLock<OperationMeta> = std::sync::OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: "session.list",
            summary: "Listet alle Sessions.",
            domain: OperationDomain::Session,
            permission: PermissionTier::Operator,
            surfaces: vec![Surface::Web {
                path: "/api/session/list",
                readonly: true,
                approval: ApprovalPolicy::None,
            }],
            aliases: &[],
            category: OperationCategory::Session,
            args_schema: None,
        })
    }
}
"#;

    fn golden_route() -> WebRouteDescriptor {
        WebRouteDescriptor {
            operation: "session.list".to_owned(),
            summary: Some("Listet alle Sessions.".to_owned()),
            path: "/api/session/list".to_owned(),
            readonly: true,
            permission: "Operator".to_owned(),
            approval: "None".to_owned(),
        }
    }

    // ── Golden-Test: Extraktion ────────────────────────────────────────────

    #[test]
    fn test_extract_routes_from_source_golden() {
        let routes = extract_routes_from_source(GOLDEN_SOURCE);
        assert_eq!(routes, vec![golden_route()]);
    }

    #[test]
    fn test_extract_routes_from_source_golden_rendering() {
        let routes = extract_routes_from_source(GOLDEN_SOURCE);
        let rendered = render_typescript(&routes);
        assert!(rendered.starts_with("// ACHTUNG: automatisch erzeugte Datei"));
        assert!(rendered.contains("export const WEB_ROUTES"));
        assert!(rendered.contains("// Listet alle Sessions."));
        assert!(rendered.contains(
            "{ operation: \"session.list\", path: \"/api/session/list\", method: \"GET\", \
             permission: \"Operator\", approval: \"None\" },"
        ));
    }

    // ── Testfixturen werden nicht erfasst ──────────────────────────────────

    #[test]
    fn test_extract_routes_skips_mod_tests_block() {
        let source = format!(
            "{GOLDEN_SOURCE}\n#[cfg(test)]\nmod tests {{\n    struct FakeOp;\n    impl FakeOp {{\n        fn meta() -> OperationMeta {{\n            OperationMeta {{\n                name: \"fake-test-op\",\n                summary: \"nur ein Test\",\n                permission: PermissionTier::Owner,\n                surfaces: vec![Surface::Web {{ path: \"/api/fake-test\", readonly: true, approval: ApprovalPolicy::None }}],\n            }}\n        }}\n    }}\n}}\n"
        );
        let routes = extract_routes_from_source(&source);
        assert_eq!(
            routes,
            vec![golden_route()],
            "nur die produktive Route außerhalb von mod tests darf erscheinen"
        );
    }

    #[test]
    fn test_extract_routes_ignores_doc_comment_examples() {
        // Modul-Dokumentation in diesem Workspace enthält lauffähige
        // Beispiele mit erfundenen Pfaden (z. B. "/api/my") — diese dürfen
        // nicht als Routen erscheinen, nur die tatsächliche Deklaration.
        let source = format!(
            "//! ```rust,no_run\n//! OperationMeta {{\n//!     name: \"doc-example\",\n//!     permission: PermissionTier::Owner,\n//!     surfaces: vec![Surface::Web {{ path: \"/api/doc-example\", readonly: true, approval: ApprovalPolicy::None }}],\n//! }}\n//! ```\n{GOLDEN_SOURCE}"
        );
        let routes = extract_routes_from_source(&source);
        assert_eq!(
            routes,
            vec![golden_route()],
            "Dokumentationsbeispiele in //!-Kommentaren dürfen keine Route erzeugen"
        );
    }

    #[test]
    fn test_extract_routes_survives_lifetimes_in_surrounding_code() {
        // `<'a>` und `&'static` dürfen den Zeichenliteral-Modus nicht
        // fälschlich eröffnen — sonst verschluckt der Scanner den Rest der
        // Datei bis zum nächsten zufälligen `'`.
        let source = format!(
            "pub enum RouteDecision<'a> {{\n    Execute {{ route: &'a str }},\n}}\nconst ALIASES: &'static [&'static str] = &[];\n{GOLDEN_SOURCE}"
        );
        let routes = extract_routes_from_source(&source);
        assert_eq!(routes, vec![golden_route()]);
    }

    #[test]
    fn test_extract_routes_from_source_with_no_web_surface_is_empty() {
        let source = r#"
            pub struct Foo;
            impl Foo {
                fn bar() -> OperationMeta {
                    OperationMeta {
                        name: "no-web",
                        permission: PermissionTier::Observer,
                        surfaces: vec![],
                    }
                }
            }
        "#;
        assert!(extract_routes_from_source(source).is_empty());
    }

    #[test]
    fn test_extract_routes_handles_multiple_surfaces_on_one_operation() {
        let source = r#"
            OperationMeta {
                name: "two-web",
                permission: PermissionTier::Maintainer,
                surfaces: vec![
                    Surface::Web { path: "/api/alpha", readonly: true, approval: ApprovalPolicy::None },
                    Surface::Web { path: "/api/beta", readonly: false, approval: ApprovalPolicy::Always },
                ],
            }
        "#;
        let routes = extract_routes_from_source(source);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].path, "/api/alpha");
        assert!(routes[0].readonly);
        assert_eq!(routes[0].approval, "None");
        assert_eq!(routes[1].path, "/api/beta");
        assert!(!routes[1].readonly);
        assert_eq!(routes[1].approval, "Always");
    }

    // ── Prüfmodus meldet eine veraltete oder fehlende Datei ────────────────

    fn temp_file(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "harw-xtask-webui-test-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ))
    }

    #[test]
    fn test_write_or_check_reports_missing_file() {
        let path = temp_file("missing.ts");
        let result = write_or_check(&path, "content", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("fehlt"));
    }

    #[test]
    fn test_write_or_check_reports_stale_file() {
        let path = temp_file("stale.ts");
        std::fs::write(&path, "alter Inhalt").unwrap();
        let result = write_or_check(&path, "neuer Inhalt", true);
        std::fs::remove_file(&path).ok();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("veraltet"));
    }

    #[test]
    fn test_write_or_check_accepts_fresh_file() {
        let path = temp_file("fresh.ts");
        std::fs::write(&path, "gleicher Inhalt").unwrap();
        let result = write_or_check(&path, "gleicher Inhalt", true);
        std::fs::remove_file(&path).ok();
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_or_check_write_mode_creates_file_and_parent_dir() {
        let dir = temp_file("write-dir");
        let path = dir.join("nested").join("operations.ts");
        let result = write_or_check(&path, "erzeugter Inhalt", false);
        let content = std::fs::read_to_string(&path).ok();
        std::fs::remove_dir_all(&dir).ok();
        assert!(result.is_ok());
        assert_eq!(content.as_deref(), Some("erzeugter Inhalt"));
    }
}
