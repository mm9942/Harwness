//! Gate 3: Schreibbereichstabelle erzeugen und prüfen.
//!
//! # Verantwortungsbereich
//! Liest die Knotentabellen des Ausbauplans (Spalten „ID", „Titel",
//! „Schreibbereich", „Hängt an"), erzeugt daraus eine Tabelle
//! **Schreibbereich → Knoten** und prüft: wird ein Schreibbereich von zwei
//! Knoten berührt, zwischen denen **kein Pfad** im `Hängt an`-Graphen
//! existiert, ist das ein Fehler. Das ist der Ersatz für den von Hand
//! gepflegten „Anhang A" des Ausbauprogramms.
//!
//! # Warum es dieses Gate gibt
//! Das ursprüngliche Programm führte eine solche Tabelle von Hand und
//! erklärte sie ausdrücklich zum Prüfinstrument für Zellen-Disjunktheit. Sie
//! war an sechzehn Stellen unvollständig, und drei echte Kollisionen blieben
//! unentdeckt — darunter eine Dreifachkollision auf `harw-core` in der
//! wichtigsten Welle des Programms.
//!
//! Ein unvollständiges Prüfinstrument ist schlechter als keins, weil es
//! Sicherheit suggeriert. Deshalb wird die Tabelle **erzeugt**, nicht
//! gepflegt.
//!
//! # Was dieses Gate nicht prüft
//! Es liest Absicht, nicht Wirklichkeit: den deklarierten Schreibbereich, nicht
//! die tatsächlich geänderten Dateien. Ein Agent, der außerhalb seines
//! Bereichs schreibt, fällt hier nicht auf — dafür ist die Abnahme des
//! jeweiligen Knotens zuständig.
//!
//! # Die Überlappungsregel — Enthaltensein, nicht Gleichheit
//! Zwei Schreibbereiche kollidieren nicht nur, wenn ihre Zeichenketten
//! identisch sind, sondern auch, wenn der eine den anderen **einschließt**:
//! `harw-core/**` schließt `harw-core/src/session.rs` ein, obwohl beide
//! Zeichenketten verschieden sind. Der Plan enthält genau diesen Fall
//! zwischen `AW1-04` und `AW2-02`/`AW4-01` — ein reiner
//! Zeichenketten-Vergleich (`==`) hätte ihn nicht gefunden. [`covers`] prüft
//! deshalb Segment für Segment, mit `**` als „Rest beliebig" und `*` als
//! Wildcard **innerhalb** eines Segments (z. B. `security-*`). Eine
//! Verstoßmeldung nennt immer den **konkreteren** der beiden Pfade als
//! Überlappungsstelle.
//!
//! # Prosa-Zellen — ausgewiesen ignoriert, nie still
//! Eine Schreibbereich-Zelle kann Prosa statt eines Pfads enthalten (im
//! Plan: „alle neuen Crate-Stubs" bei `AW0-00`). Diese Implementierung
//! verwirft eine solche Zelle **nicht still**: jeder echte Pfad im Plan
//! steht in Backticks, jede Prosa-Zelle nicht — das ist die
//! Unterscheidungsregel. Ein Teil ohne Backticks wird als **ausgewiesen
//! ignoriert** behandelt: [`parse_scope_cell`] gibt dafür eine
//! Hinweiszeile zurück, die [`run`] über `stderr` ausgibt, und der Eintrag
//! zählt **nicht** in `checked` mit. Ein harter Fehler wäre hier die falsche
//! Wahl: „alle neuen Crate-Stubs" ist dauerhafter, gewollter Text im Plan,
//! kein Versehen, das behoben werden soll — ein `Err` würde das Gate
//! permanent rot machen, ohne einen echten Konflikt zu benennen.
//!
//! # Brace-Expansion
//! `harw-core/src/{child_controller,session}.rs` sind **zwei** Pfade, nicht
//! einer. [`expand_braces`] löst das rekursiv auf (auch mehrere,
//! nicht verschachtelte Gruppen in derselben Zelle), damit die Kollision auf
//! `harw-core/src/session.rs` sichtbar wird. Verschachtelte Braces kommen im
//! Plan nicht vor und werden nicht unterstützt.
//!
//! # Fehlende Datei
//! `run` nimmt keine Argumente; die Workspace-Wurzel wird über
//! [`find_workspace_root`] gefunden (kompilierzeitlich über
//! `CARGO_MANIFEST_DIR`, ersatzweise durch Aufstieg zu einem `Cargo.toml`
//! mit `[workspace]`). Fehlt `docs/aw-plan.md` unter dieser Wurzel oder kommt
//! die erwartete Tabellen-Kopfzeile nirgends vor, liefert `run` `Err` — ein
//! Gate, das ohne Plan grün meldet, ist wertlos.
//!
//! # Zyklen
//! Ein Zyklus im „Hängt an"-Graphen wäre selbst ein Befund. [`detect_cycles`]
//! erkennt ihn über eine Drei-Farben-Tiefensuche und meldet ihn als
//! Verstoß; [`reachable_from`] ist unabhängig davon mit einem
//! besuchten-Set gegen Endlosschleifen abgesichert, damit ein Zyklus die
//! restliche Auswertung nicht zum Hängen bringt.
//!
//! # Bereichs-IDs (`AW2-07..13`)
//! Der Plan fasst strukturgleiche Geschwisterknoten gelegentlich in einer
//! Zeile zusammen (`AW2-07..13`, sieben Sensor-Crates) und referenziert sie
//! später als Bereich (`AW2-07..AW2-17`). Diese Implementierung **spaltet**
//! eine solche Zeile nicht in sieben Einzelknoten auf — das würde
//! voraussetzen, dass die Reihenfolge der Brace-Alternativen im
//! Schreibbereich exakt der Reihenfolge der IDs im Bereich entspricht, eine
//! Annahme, die das Tabellenformat nicht garantiert. Stattdessen bleibt eine
//! solche Zeile **ein** Knoten mit der Vereinigung aller Schreibbereiche;
//! eine Bereichsreferenz wie `AW2-07..AW2-17` wird über
//! [`ids_overlap`] als numerische Intervallüberschneidung aufgelöst und
//! trifft sowohl den zusammengefassten Knoten als auch einzeln stehende
//! Knoten im selben Zahlenbereich. Das ist sicher für die
//! Kollisionsprüfung: die Vereinigung aller sieben Schreibbereiche in einem
//! Knoten verliert keine Überlappung, die eine Aufspaltung gefunden hätte.
//!
//! # Maskierte Pipes
//! Der Plan nutzt in der Titel-Spalte Rust-Summentypen wie
//! `SensorHandle<Unbound\|Bound>`. Das `\|` ist eine maskierte Pipe, kein
//! Spaltentrenner — [`split_row`] behandelt sie entsprechend. Eine naive
//! Zerlegung an jedem `|` hätte drei Knoten (`AW0-06`, `AW1-03`, `AW5-03`)
//! aus der Tabelle verloren und in der Folge fünfzehn scheinbar unauflösbare
//! Abhängigkeiten erzeugt — ein Parserfehler, kein Planfehler, aber einer,
//! der ohne Gegenprobe an einer konstruierten Tabelle unentdeckt geblieben
//! wäre.
//!
//! # Nicht auflösbare Abhängigkeiten
//! Ein „Hängt an"-Eintrag, der auf keinen bekannten Knoten passt, wird
//! ebenfalls nicht still verworfen, sondern als Verstoß gemeldet
//! ([`build_edges`]) — auch das ist „still ist keine Option", nur auf die
//! Kantenliste statt auf die Schreibbereichsliste angewandt.
//!
//! # Stand
//! Implementiert in Knoten AW0-10c. Gegen konstruierte Tabellen getestet,
//! nicht gegen `docs/aw-plan.md` selbst — der Plan ändert sich unter der
//! Implementierung, ein Test dagegen würde bei jeder Planänderung rot.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::GateReport;

/// Ein einzelnes ID-Atom: Präfix, Nummer und optionaler Buchstabensuffix.
///
/// Zwischenergebnis von [`parse_id_atom`], nie über eine Modulgrenze hinweg
/// sichtbar.
#[derive(Debug, Clone)]
struct IdAtom {
    /// Präfix inklusive Bindestrich, z. B. `"AW4-"` oder `"UI-"`.
    prefix: String,
    /// Die numerische ID, z. B. `2` für `AW4-02a`.
    num: u32,
    /// Der Buchstabensuffix, z. B. `"a"` für `AW4-02a`; `None` für reine
    /// Zahlen-IDs wie `AW4-02` oder `UI-00`.
    suffix: Option<String>,
}

/// Eine ID oder ein ID-**Bereich**, als numerisches Intervall über einem
/// gemeinsamen Präfix.
///
/// Vereinigt drei Fälle des Plans in einer Datenform: eine einzelne ID
/// (`start == end`), eine zusammengefasste Zeile wie `AW2-07..13`
/// (`start < end`), und eine Bereichsreferenz wie `AW2-07..AW2-17` in einer
/// „Hängt an"-Zelle. [`ids_overlap`] behandelt alle drei einheitlich als
/// Intervallüberschneidung.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IdSpan {
    /// Präfix inklusive Bindestrich.
    prefix: String,
    /// Erste Nummer des Intervalls.
    start: u32,
    /// Letzte Nummer des Intervalls (bei einer Einzel-ID gleich `start`).
    end: u32,
    /// Buchstabensuffix; nur bei Einzel-IDs gesetzt — Bereiche tragen im
    /// Plan nie einen Suffix.
    suffix: Option<String>,
}

/// Ein einzelner Eintrag aus der Schreibbereich-Spalte nach der
/// Brace-Expansion.
#[derive(Debug, Clone)]
struct ScopeEntry {
    /// Der Text, wie die Zelle ihn trägt, z. B.
    /// `"harw-core/src/{context_budget,turn_loop}.rs"` — für Meldungen, die
    /// ohne Nachschlagen im Plan verständlich sein müssen.
    source_text: String,
    /// Ein einzelner, konkreter Pfad oder Glob nach der Expansion, z. B.
    /// `"harw-core/src/context_budget.rs"`.
    path: String,
}

/// Ein Knoten aus einer Wellentabelle des Plans.
#[derive(Debug, Clone)]
struct PlanNode {
    /// Die bereinigte ID, wie sie in Meldungen erscheint — z. B.
    /// `"AW1-04b"` oder `"AW2-07..13"` für eine zusammengefasste Zeile.
    id: String,
    /// Die geparste Spanne für Präfix- und Bereichsvergleiche.
    span: IdSpan,
    /// Alle Schreibbereiche dieser Zeile, bereits Brace-expandiert.
    scopes: Vec<ScopeEntry>,
    /// Bereinigte „Hängt an"-Einträge, roh — noch nicht auf Knotenindizes
    /// aufgelöst.
    deps_raw: Vec<String>,
}

/// Ergebnis von [`parse_plan`]: alle Knoten plus Hinweise zu ignorierten
/// Prosa-Zellen.
#[derive(Debug, Clone)]
struct ParsedPlan {
    /// Alle geparsten Knoten, in Dokumentreihenfolge.
    nodes: Vec<PlanNode>,
    /// Menschlich lesbare Hinweise zu Schreibbereich-Zellen ohne
    /// Backtick-Pfad — ausgewiesen ignoriert, nicht Teil von `checked`.
    ignored: Vec<String>,
}

/// Zerlegt ein einzelnes ID-Token (ohne `..`) in Präfix, Nummer und Suffix.
///
/// # Description
/// Sucht den letzten Bindestrich im Token, liest danach eine Ziffernfolge
/// als Nummer und alles Weitere als Suffix — das Muster, das Wellen-IDs wie
/// `AW4-02a` oder `UI-00` erzeugen. Passt das Token nicht auf dieses Muster,
/// liefert die Funktion ein **undurchsichtiges** Atom zurück, dessen Präfix
/// das ganze Token ist: es passt dann nur exakt auf sich selbst, die
/// Auswertung stürzt aber nie ab.
///
/// # Arguments
/// - `token` (`&str`): ein ID-Text ohne Bereichsnotation.
///
/// # Returns
/// Das geparste [`IdAtom`].
///
/// # Examples
/// ```rust,ignore
/// let atom = parse_id_atom("AW4-02a");
/// assert_eq!(atom.num, 2);
/// assert_eq!(atom.suffix.as_deref(), Some("a"));
/// ```
fn parse_id_atom(token: &str) -> IdAtom {
    let token = token.trim();
    if let Some(dash_idx) = token.rfind('-') {
        let prefix = &token[..=dash_idx];
        let rest = &token[dash_idx + 1..];
        let digit_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digit_end > 0 {
            if let Ok(num) = rest[..digit_end].parse::<u32>() {
                let suffix = &rest[digit_end..];
                return IdAtom {
                    prefix: prefix.to_owned(),
                    num,
                    suffix: if suffix.is_empty() {
                        None
                    } else {
                        Some(suffix.to_owned())
                    },
                };
            }
        }
    }
    IdAtom {
        prefix: token.to_owned(),
        num: 0,
        suffix: None,
    }
}

/// Parst ein ID-Token, das auch eine Bereichsnotation (`..`) tragen kann.
///
/// # Description
/// Der Plan schreibt Bereiche in zwei Formen: kurz als `AW2-07..13`
/// (ID-Spalte, eine zusammengefasste Zeile für mehrere Geschwisterknoten)
/// und lang als `AW2-07..AW2-17` (Hängt-an-Spalte, ein Verweis auf mehrere
/// Knoten). Beide werden erkannt: das rechte Ende darf eine nackte Zahl oder
/// eine volle ID mit demselben Präfix sein. Lässt sich die Notation nicht
/// sicher auflösen (z. B. unterschiedliche Präfixe links und rechts),
/// fällt die Funktion auf ein undurchsichtiges Einzel-Atom zurück, statt zu
/// raten.
///
/// # Arguments
/// - `token` (`&str`): das rohe ID-Token, bereits von `**` befreit.
///
/// # Returns
/// Die [`IdSpan`], die diesen Knoten oder diese Knotengruppe beschreibt.
///
/// # Examples
/// ```rust,ignore
/// let span = parse_id_span("AW2-07..AW2-17");
/// assert_eq!((span.start, span.end), (7, 17));
/// ```
#[must_use]
fn parse_id_span(token: &str) -> IdSpan {
    let token = token.trim();
    if let Some(pos) = token.find("..") {
        let left = parse_id_atom(&token[..pos]);
        let right_raw = token[pos + 2..].trim();
        let end_num = if right_raw.contains('-') {
            let right = parse_id_atom(right_raw);
            if right.prefix == left.prefix {
                Some(right.num)
            } else {
                None
            }
        } else {
            right_raw.parse::<u32>().ok()
        };
        if left.suffix.is_none() {
            if let Some(end) = end_num {
                return IdSpan {
                    prefix: left.prefix,
                    start: left.num,
                    end,
                    suffix: None,
                };
            }
        }
        // Notation nicht sicher auflösbar: als undurchsichtiges Einzelstück
        // behandeln, das nur auf sich selbst passt — kein Absturz, kein Raten.
        let atom = parse_id_atom(token);
        return IdSpan {
            prefix: atom.prefix,
            start: atom.num,
            end: atom.num,
            suffix: atom.suffix,
        };
    }
    let atom = parse_id_atom(token);
    IdSpan {
        prefix: atom.prefix,
        start: atom.num,
        end: atom.num,
        suffix: atom.suffix,
    }
}

/// Ob sich zwei ID-Spannen überschneiden — die Grundlage jeder
/// Abhängigkeitsauflösung.
///
/// # Description
/// Gleicher Präfix und gleicher Suffix (beide `None` oder identisch) sind
/// Pflicht; danach genügt eine numerische Intervallüberschneidung. Das
/// deckt drei Fälle mit einer Regel ab: zwei einzelne IDs (Intervall der
/// Länge 1) sind gleich, eine Bereichs-ID trifft jede einzelne ID darin, und
/// zwei Bereichs-IDs überschneiden sich, wenn ihre Intervalle es tun.
///
/// # Arguments
/// - `a` (`&IdSpan`): erste Spanne.
/// - `b` (`&IdSpan`): zweite Spanne.
///
/// # Returns
/// `true` bei Überschneidung.
///
/// # Examples
/// ```rust,ignore
/// let group = parse_id_span("AW2-07..13");
/// let member = parse_id_span("AW2-09");
/// assert!(ids_overlap(&group, &member));
/// ```
#[must_use]
fn ids_overlap(a: &IdSpan, b: &IdSpan) -> bool {
    a.prefix == b.prefix && a.suffix == b.suffix && a.start <= b.end && b.start <= a.end
}

/// Entfernt Markdown-Fettung (`**`) und schneidet Leerraum ab.
///
/// # Description
/// Reine Hilfsfunktion für Zellen, in denen `**` nur der Hervorhebung dient
/// (z. B. `**AW0-05**` in einer „Hängt an"-Liste) und keine strukturelle
/// Bedeutung trägt.
///
/// # Arguments
/// - `s` (`&str`): der Rohtext.
///
/// # Returns
/// Den Text ohne `**`, getrimmt.
///
/// # Examples
/// ```rust,ignore
/// assert_eq!(strip_markdown_bold("**AW0-05**"), "AW0-05");
/// ```
#[must_use]
fn strip_markdown_bold(s: &str) -> String {
    s.replace("**", "").trim().to_owned()
}

/// Liest die Knoten-ID aus der ID-Spalte einer Tabellenzeile.
///
/// # Description
/// Die ID-Zelle trägt oft mehr als die ID: `**AW0-00** ⭐`,
/// `AW1-04 **[T][S]**`, `**AW2-07..13**`. Nach dem Entfernen von `**` ist
/// die ID immer das erste durch Leerraum abgetrennte Wort — Markierungen
/// wie `⭐` oder `[T]` folgen stets nach einem Leerzeichen, nie direkt an
/// die ID angehängt.
///
/// # Arguments
/// - `cell` (`&str`): der Rohinhalt der ID-Spalte.
///
/// # Returns
/// `Some(id)`, oder `None`, wenn die Zelle nach dem Bereinigen leer ist.
///
/// # Examples
/// ```rust,ignore
/// assert_eq!(extract_id("**AW1-04b** ⭐ **[T]**"), Some("AW1-04b".to_string()));
/// ```
#[must_use]
fn extract_id(cell: &str) -> Option<String> {
    let cleaned = strip_markdown_bold(cell);
    cleaned.split_whitespace().next().map(str::to_owned)
}

/// Zerlegt eine Zeichenkette an einem Trennzeichen, aber nur außerhalb von
/// `{}`-Gruppen.
///
/// # Description
/// Ein Komma **innerhalb** eines Brace-Ausdrucks wie
/// `harw-core/src/{child_controller,session}.rs` trennt keine zwei
/// Schreibbereiche, sondern zwei Alternativen **eines** Bereichs. Eine
/// naive Zerlegung an jedem Komma würde das falsch auseinanderreißen; diese
/// Funktion zählt die Klammertiefe mit und trennt nur auf Tiefe null.
///
/// # Arguments
/// - `s` (`&str`): die zu zerlegende Zelle.
/// - `sep` (`char`): das Trennzeichen (hier immer `,`).
///
/// # Returns
/// Die Teile, unbeschnitten — Trimmen ist Sache der Aufruferin.
///
/// # Examples
/// ```rust,ignore
/// let parts = split_top_level("a/{x,y}.rs, b.rs", ',');
/// assert_eq!(parts.len(), 2);
/// ```
#[must_use]
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in s.chars() {
        match ch {
            '{' => {
                depth += 1;
                current.push(ch);
            }
            '}' => {
                depth -= 1;
                current.push(ch);
            }
            c if c == sep && depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            c => current.push(c),
        }
    }
    out.push(current);
    out
}

/// Expandiert Brace-Ausdrücke wie `a/{b,c}.rs` zu `["a/b.rs", "a/c.rs"]`.
///
/// # Description
/// Rekursiv, damit auch mehrere, nicht verschachtelte Gruppen in derselben
/// Zeichenkette funktionieren. Ohne diese Expansion würde
/// `harw-core/src/{child_controller,session}.rs` als **ein** Pfad gezählt
/// und die Kollision mit `harw-core/src/session.rs` verpasst. Verschachtelte
/// Braces (`{a,{b,c}}`) kommen im Plan nicht vor und werden nicht
/// unterstützt — die innere Gruppe würde als Literal behandelt.
///
/// # Arguments
/// - `s` (`&str`): der Rohtext eines Schreibbereich-Eintrags.
///
/// # Returns
/// Alle konkreten Varianten; ohne `{}` genau eine, nämlich `s` selbst.
///
/// # Examples
/// ```rust,ignore
/// assert_eq!(
///     expand_braces("a/{x,y}.rs"),
///     vec!["a/x.rs".to_string(), "a/y.rs".to_string()]
/// );
/// ```
#[must_use]
fn expand_braces(s: &str) -> Vec<String> {
    if let Some(open) = s.find('{') {
        if let Some(close_rel) = s[open..].find('}') {
            let close = open + close_rel;
            let prefix = &s[..open];
            let alts = &s[open + 1..close];
            let suffix = &s[close + 1..];
            return alts
                .split(',')
                .flat_map(|alt| expand_braces(&format!("{prefix}{}{suffix}", alt.trim())))
                .collect();
        }
    }
    vec![s.to_owned()]
}

/// Holt den ersten Backtick-Abschnitt aus einem Zellenteil.
///
/// # Description
/// Im Plan steht jeder echte Schreibbereich in Backticks; Prosa wie „alle
/// neuen Crate-Stubs" tut das nicht. Das ist die Unterscheidungsregel
/// dieses Gates zwischen Pfad und Prosa. Text nach dem schließenden
/// Backtick (z. B. eine angehängte Anmerkung wie `` `pfad.rs` (K21) ``)
/// wird verworfen, ohne die Zelle deshalb als Prosa zu werten — der Pfad
/// wurde ja gefunden.
///
/// # Arguments
/// - `item` (`&str`): ein einzelner, bereits auf Komma-Ebene zerlegter
///   Zellenteil.
///
/// # Returns
/// `Some(pfad)` bei einem Backtick-Paar, sonst `None`.
///
/// # Examples
/// ```rust,ignore
/// assert_eq!(extract_backtick_span("`a/b.rs` (K21)"), Some("a/b.rs".to_string()));
/// assert_eq!(extract_backtick_span("alle neuen Crate-Stubs"), None);
/// ```
#[must_use]
fn extract_backtick_span(item: &str) -> Option<String> {
    let start = item.find('`')?;
    let rest = &item[start + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].trim().to_owned())
}

/// Zerlegt eine Schreibbereich-Zelle in geprüfte Pfade und Prosa-Hinweise.
///
/// # Description
/// Trennt zunächst auf Komma-Ebene ([`split_top_level`]), holt aus jedem
/// Teil den Backtick-Inhalt ([`extract_backtick_span`]) und expandiert
/// diesen ([`expand_braces`]). Ein Teil ohne Backticks ist Prosa: er wird
/// **nicht** stillschweigend übersprungen, sondern als Hinweiszeile
/// zurückgegeben, die [`run`] über `stderr` ausgibt.
///
/// # Arguments
/// - `cell` (`&str`): der Rohinhalt der Schreibbereich-Spalte.
/// - `node_id` (`&str`): die ID der Zeile, für die Hinweiszeile.
///
/// # Returns
/// Ein Tupel aus den geprüften [`ScopeEntry`]s und den Hinweiszeilen zu
/// ignorierten Prosa-Teilen.
///
/// # Examples
/// ```rust,ignore
/// let (scopes, ignored) = parse_scope_cell("`a.rs`, Prosa", "X-01");
/// assert_eq!(scopes.len(), 1);
/// assert_eq!(ignored.len(), 1);
/// ```
#[must_use]
fn parse_scope_cell(cell: &str, node_id: &str) -> (Vec<ScopeEntry>, Vec<String>) {
    let mut scopes = Vec::new();
    let mut ignored = Vec::new();
    for raw_item in split_top_level(cell, ',') {
        let item = raw_item.trim();
        if item.is_empty() {
            continue;
        }
        match extract_backtick_span(item) {
            Some(path_text) => {
                for expanded in expand_braces(&path_text) {
                    scopes.push(ScopeEntry {
                        source_text: path_text.clone(),
                        path: expanded,
                    });
                }
            }
            None => ignored.push(format!(
                "gate 'writescopes': Schreibbereich-Zelle von {node_id} ignoriert (keine \
                 Backtick-Pfadangabe): \"{item}\""
            )),
        }
    }
    (scopes, ignored)
}

/// Ob `line` der Tabellenkopf einer Knotentabelle ist.
///
/// # Description
/// Vergleicht ohne Leerraum, damit unterschiedliche Spaltenbreiten im
/// Markdown-Quelltext nicht stören. Dieser Kopf ist zugleich der Filter, der
/// Knotentabellen von den anderen Tabellen im Plan (Parallelisierung,
/// Entscheidungen) unterscheidet — die tragen andere Spaltenüberschriften.
///
/// # Arguments
/// - `line` (`&str`): eine einzelne Zeile aus dem Plan.
///
/// # Returns
/// `true`, wenn die Zeile exakt der Kopfzeile `| ID | Titel |
/// Schreibbereich | Hängt an |` entspricht (Leerraum ignoriert).
///
/// # Examples
/// ```rust,ignore
/// assert!(is_node_table_header("| ID | Titel | Schreibbereich | Hängt an |"));
/// assert!(!is_node_table_header("| Welle | Ebene 1 |"));
/// ```
#[must_use]
fn is_node_table_header(line: &str) -> bool {
    let normalized: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    normalized == "|ID|Titel|Schreibbereich|Hängtan|"
}

/// Ob `line` die Trennzeile direkt unter einem Tabellenkopf ist.
///
/// # Arguments
/// - `line` (`&str`): die Zeile nach dem vermuteten Tabellenkopf.
///
/// # Returns
/// `true` für Zeilen, die nur aus `|`, `-`, `:` und Leerraum bestehen und
/// mindestens einen Bindestrich enthalten.
///
/// # Examples
/// ```rust,ignore
/// assert!(is_separator_row("|---|---|---|---|"));
/// ```
#[must_use]
fn is_separator_row(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed.contains('-')
        && trimmed
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

/// Zerlegt eine Markdown-Tabellenzeile in ihre Zellen.
///
/// # Description
/// Respektiert mit `\|` maskierte Pipe-Zeichen — der Plan nutzt sie für
/// Rust-Summentypen wie `SensorHandle<Unbound\|Bound>` in der Titel-Spalte.
/// Eine naive Zerlegung an jedem `|` reißt eine solche Zeile in zu viele
/// Zellen und verliert dadurch den ganzen Knoten; das ist im Plan
/// nachweisbar (`AW0-06`, `AW1-03`, `AW5-03` tragen je ein maskiertes
/// Pipe-Zeichen in der Titel-Spalte).
///
/// # Arguments
/// - `line` (`&str`): eine Zeile, die mit `|` beginnt.
///
/// # Returns
/// Die Zellen, unmaskiert und getrimmt, ohne die leeren Ränder vor der
/// ersten und nach der letzten Pipe.
///
/// # Examples
/// ```rust,ignore
/// let cells = split_row(r"| A | x\|y | B | C |");
/// assert_eq!(cells[1], "x|y");
/// ```
#[must_use]
fn split_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let no_prefix = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = no_prefix.strip_suffix('|').unwrap_or(no_prefix);

    let mut cells = Vec::new();
    let mut current = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&'|') {
            current.push('|');
            chars.next();
        } else if ch == '|' {
            cells.push(std::mem::take(&mut current).trim().to_owned());
        } else {
            current.push(ch);
        }
    }
    cells.push(current.trim().to_owned());
    cells
}

/// Baut einen [`PlanNode`] aus den vier Zellen einer Tabellenzeile.
///
/// # Description
/// Liest die ID ([`extract_id`]), parst ihre Spanne ([`parse_id_span`]),
/// zerlegt den Schreibbereich ([`parse_scope_cell`]) und bereinigt die
/// „Hängt an"-Liste (Komma-getrennt, `**` entfernt, `—`/`-` als „keine
/// Abhängigkeit" verworfen). Die Titel-Spalte (`cells[1]`) wird nicht
/// ausgewertet — sie trägt keine für dieses Gate relevante Information.
///
/// # Arguments
/// - `cells` (`&[String]`): genau vier Zellen in der Reihenfolge ID, Titel,
///   Schreibbereich, Hängt an.
///
/// # Returns
/// Den Knoten plus die Prosa-Hinweise aus seinem Schreibbereich.
///
/// # Errors
/// Wenn `cells` nicht genau vier Einträge hat, oder die ID-Zelle nach dem
/// Bereinigen leer ist — beides deutet auf ein unerwartetes Tabellenformat
/// hin und wird nicht stillschweigend übergangen.
///
/// # Examples
/// ```rust,ignore
/// let cells = split_row("| A-01 | Titel | `a.rs` | — |");
/// let (node, _ignored) = parse_row(&cells)?;
/// assert_eq!(node.id, "A-01");
/// ```
fn parse_row(cells: &[String]) -> Result<(PlanNode, Vec<String>), String> {
    if cells.len() != 4 {
        return Err(format!(
            "Tabellenzeile mit {} statt 4 Spalten (ID, Titel, Schreibbereich, Hängt an): {cells:?}",
            cells.len()
        ));
    }
    let id = extract_id(&cells[0])
        .ok_or_else(|| format!("keine erkennbare Knoten-ID in Zelle '{}'", cells[0]))?;
    let span = parse_id_span(&id);
    let (scopes, ignored) = parse_scope_cell(&cells[2], &id);
    let deps_raw = split_top_level(&cells[3], ',')
        .into_iter()
        .map(|s| strip_markdown_bold(s.trim()))
        .filter(|s| !s.is_empty() && s != "—" && s != "-")
        .collect();

    Ok((
        PlanNode {
            id,
            span,
            scopes,
            deps_raw,
        },
        ignored,
    ))
}

/// Parst alle Knotentabellen aus dem Markdown-Text des Plans.
///
/// # Description
/// Sucht zeilenweise nach dem exakten Tabellenkopf
/// ([`is_node_table_header`]) — das genügt, um Knotentabellen von den
/// anderen Tabellen im Dokument (Parallelisierung, Entscheidungen) zu
/// unterscheiden, ohne auf `###`-Abschnittsgrenzen angewiesen zu sein.
/// Jede gefundene Tabelle wird bis zur ersten Zeile gelesen, die nicht mit
/// `|` beginnt. Das Ergebnis ist **kein** `Ok`, wenn keine einzige
/// Knotentabelle gefunden wurde — ein leerer, aber „erfolgreicher" Bericht
/// wäre genau die Sorte falscher Sicherheit, die dieses Gate ersetzen soll.
///
/// # Arguments
/// - `markdown` (`&str`): der vollständige Planinhalt (oder, in Tests, eine
///   konstruierte Tabelle als Zeichenkette).
///
/// # Returns
/// Die geparsten Knoten plus alle Prosa-Hinweise.
///
/// # Errors
/// - Wenn ein Tabellenkopf ohne Trennzeile (`|---|---|---|---|`) folgt.
/// - Wenn eine Tabellenzeile nicht genau vier Spalten hat oder ihre ID nicht
///   erkennbar ist ([`parse_row`]).
/// - Wenn im gesamten Text keine Knotentabelle gefunden wurde.
///
/// # Examples
/// ```rust,ignore
/// let markdown = "\
/// | ID | Titel | Schreibbereich | Hängt an |
/// |---|---|---|---|
/// | A-01 | eins | `a.rs` | — |
/// ";
/// let parsed = parse_plan(markdown)?;
/// assert_eq!(parsed.nodes.len(), 1);
/// ```
fn parse_plan(markdown: &str) -> Result<ParsedPlan, String> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut nodes = Vec::new();
    let mut ignored = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        if is_node_table_header(lines[i]) {
            let header_line_no = i + 1;
            i += 1;
            if i >= lines.len() || !is_separator_row(lines[i]) {
                return Err(format!(
                    "Tabellenkopf ohne Trennzeile (Zeile {header_line_no})"
                ));
            }
            i += 1;
            while i < lines.len() {
                let trimmed = lines[i].trim();
                if trimmed.is_empty() || !trimmed.starts_with('|') {
                    break;
                }
                let cells = split_row(trimmed);
                let (node, mut row_ignored) =
                    parse_row(&cells).map_err(|error| format!("Zeile {}: {error}", i + 1))?;
                ignored.append(&mut row_ignored);
                nodes.push(node);
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    if nodes.is_empty() {
        return Err(
            "keine Knotentabellen gefunden — die Kopfzeile 'ID | Titel | Schreibbereich | \
             Hängt an' kommt im Plan nicht vor; hat sich das Tabellenformat geändert?"
                .to_owned(),
        );
    }

    Ok(ParsedPlan { nodes, ignored })
}

/// Zerlegt einen Pfad in seine `/`-getrennten Segmente.
///
/// # Description
/// Reine Hilfsfunktion für [`covers`]; keine Sonderbehandlung von Wurzel-
/// oder Endpfaden nötig, da alle Pfade im Plan relativ zur Workspace-Wurzel
/// geschrieben sind.
///
/// # Arguments
/// - `path` (`&str`): der zu zerlegende Pfad oder Glob.
///
/// # Returns
/// Die Segmente in Reihenfolge.
///
/// # Examples
/// ```rust,ignore
/// assert_eq!(path_segments("a/b/c"), vec!["a", "b", "c"]);
/// ```
#[must_use]
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').collect()
}

/// Klassischer Wildcard-Abgleich für ein einzelnes Pfadsegment mit `*`.
///
/// # Description
/// Zwei-Zeiger-Algorithmus wie in Shell-Globs: `*` steht für eine
/// beliebige, auch leere Zeichenfolge **innerhalb** des Segments. Pfade
/// werden vorher in Segmente zerlegt ([`path_segments`]); dieser Abgleich
/// sieht nie ein `/`.
///
/// # Arguments
/// - `pattern` (`&str`): das Segment mit Sternen, z. B. `"security-*"`.
/// - `text` (`&str`): das zu prüfende, konkrete Segment.
///
/// # Returns
/// `true`, wenn `text` auf `pattern` passt.
///
/// # Examples
/// ```rust,ignore
/// assert!(glob_match("security-*", "security-triage"));
/// assert!(!glob_match("security-*", "warden"));
/// ```
#[must_use]
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut match_from = 0usize;

    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            match_from = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(star_idx) = star {
            pi = star_idx + 1;
            match_from += 1;
            ti = match_from;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Ob `actual` auf das Segmentmuster `pattern` passt.
///
/// # Description
/// Erst ein exakter Vergleich, dann — nur falls `pattern` einen Stern
/// enthält — der Wildcard-Abgleich ([`glob_match`]). Trennt den häufigen
/// Fall (kein Stern, exakte Übereinstimmung) vom seltenen.
///
/// # Arguments
/// - `pattern` (`&str`): ein Segment aus einem Schreibbereich.
/// - `actual` (`&str`): ein Segment aus dem zu prüfenden Pfad.
///
/// # Returns
/// `true` bei Übereinstimmung.
///
/// # Examples
/// ```rust,ignore
/// assert!(segment_matches("harw-core", "harw-core"));
/// assert!(segment_matches("security-*", "security-triage"));
/// ```
#[must_use]
fn segment_matches(pattern: &str, actual: &str) -> bool {
    pattern == actual || (pattern.contains('*') && glob_match(pattern, actual))
}

/// Ob der Schreibbereich `glob` den konkreten Pfad `candidate` **einschließt**.
///
/// # Description
/// Das ist die zentrale Entscheidung dieses Gates: zwei Schreibbereiche
/// kollidieren nicht nur bei Gleichheit, sondern auch, wenn einer den
/// anderen enthält — `harw-core/**` schließt `harw-core/src/session.rs` ein,
/// obwohl die Zeichenketten verschieden sind. Endet `glob` auf `**`, zählt
/// jedes Präfix-Segment davor exakt oder per `*`-Wildcard, alles danach ist
/// beliebig. Ohne `**` müssen beide Pfade dieselbe Tiefe haben und
/// segmentweise übereinstimmen — ein `harw-core/src/a.rs` deckt kein
/// `harw-core/src/a/b.rs` ab.
///
/// # Arguments
/// - `glob` (`&str`): der möglicherweise umfassendere Schreibbereich.
/// - `candidate` (`&str`): der zu prüfende, konkretere Pfad.
///
/// # Returns
/// `true`, wenn jeder Pfad, den `candidate` bezeichnet, auch von `glob`
/// erfasst wird.
///
/// # Examples
/// ```rust,ignore
/// assert!(covers("harw-core/**", "harw-core/src/session.rs"));
/// assert!(!covers("harw-core/src/session.rs", "harw-core/**"));
/// ```
#[must_use]
fn covers(glob: &str, candidate: &str) -> bool {
    if glob == candidate {
        return true;
    }
    let g = path_segments(glob);
    let c = path_segments(candidate);
    if let Some(star_pos) = g.iter().position(|&seg| seg == "**") {
        if star_pos > c.len() {
            return false;
        }
        (0..star_pos).all(|k| segment_matches(g[k], c[k]))
    } else {
        g.len() == c.len() && g.iter().zip(c.iter()).all(|(&gg, &cc)| segment_matches(gg, cc))
    }
}

/// Prüft zwei Schreibbereiche auf Überlappung und benennt die konkretere
/// Stelle, an der sie sich berühren.
///
/// # Description
/// Nutzt [`covers`] in beide Richtungen, weil Enthaltensein nicht
/// symmetrisch ist: `a` kann `b` einschließen, ohne dass `b` auch `a`
/// einschließt. Gemeldet wird immer der **konkretere** der beiden Pfade —
/// bei `harw-core/**` gegen `harw-core/src/session.rs` also Letzteres,
/// so wie es die Verstoßmeldung dieses Gates verlangt.
///
/// # Arguments
/// - `a` (`&str`): erster, bereits Brace-expandierter Pfad.
/// - `b` (`&str`): zweiter, bereits Brace-expandierter Pfad.
///
/// # Returns
/// `Some(pfad)` mit der überlappenden Stelle, sonst `None`.
///
/// # Examples
/// ```rust,ignore
/// assert_eq!(
///     scope_overlap("harw-core/**", "harw-core/src/session.rs"),
///     Some("harw-core/src/session.rs".to_string())
/// );
/// ```
#[must_use]
fn scope_overlap(a: &str, b: &str) -> Option<String> {
    if a == b {
        return Some(a.to_owned());
    }
    if covers(a, b) {
        return Some(b.to_owned());
    }
    if covers(b, a) {
        return Some(a.to_owned());
    }
    None
}

/// Baut den gerichteten „Hängt an"-Graphen und meldet nicht auflösbare
/// Abhängigkeiten.
///
/// # Description
/// Für jeden Knoten wird jede rohe Abhängigkeit über [`parse_id_span`] und
/// [`ids_overlap`] gegen alle bekannten Knoten aufgelöst — nicht nur per
/// Gleichheit, damit eine Bereichs-ID wie `AW2-07..13` sowohl als Ziel einer
/// Bereichsreferenz (`AW2-07..AW2-17`) als auch als eigenständiger Knoten
/// funktioniert. Eine Abhängigkeit, die auf keinen bekannten Knoten passt,
/// wird als Fund zurückgegeben statt stillschweigend verworfen.
///
/// # Arguments
/// - `nodes` (`&[PlanNode]`): alle geparsten Knoten in fester Reihenfolge;
///   ihre Indizes in dieser Liste sind zugleich die Knotenindizes im
///   Graphen.
///
/// # Returns
/// Die Adjazenzliste (Index = Knoten, Werte = Indizes der Abhängigkeiten,
/// „X hängt an Y" wird zu einer Kante `X → Y`) sowie eine Meldung je nicht
/// auflösbarer Abhängigkeit.
///
/// # Examples
/// ```rust,ignore
/// let (adj, unresolved) = build_edges(&nodes);
/// assert!(unresolved.is_empty());
/// ```
#[must_use]
fn build_edges(nodes: &[PlanNode]) -> (Vec<BTreeSet<usize>>, Vec<String>) {
    let mut adj = vec![BTreeSet::new(); nodes.len()];
    let mut findings = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        for dep_token in &node.deps_raw {
            let dep_span = parse_id_span(dep_token);
            let mut matched = false;
            for (j, other) in nodes.iter().enumerate() {
                if i != j && ids_overlap(&other.span, &dep_span) {
                    adj[i].insert(j);
                    matched = true;
                }
            }
            if !matched {
                findings.push(format!(
                    "{}: Abhängigkeit '{dep_token}' verweist auf keinen bekannten Knoten",
                    node.id
                ));
            }
        }
    }

    (adj, findings)
}

/// Erkennt Zyklen im „Hängt an"-Graphen, ohne im Kreis zu laufen.
///
/// # Description
/// Tiefensuche mit Drei-Farben-Markierung (weiß/grau/schwarz). Trifft die
/// Suche auf einen bereits **grauen** — also noch auf dem aktuellen Pfad
/// befindlichen — Knoten, ist das eine Rückwärtskante und damit ein Zyklus.
/// Jeder Knoten wird höchstens einmal vollständig besucht (danach schwarz
/// markiert); die Funktion terminiert deshalb auch bei einem echten Zyklus
/// im Plan. Das ist der Zweck: ein Zyklus soll gemeldet werden, nicht die
/// Prüfung zum Hängen bringen.
///
/// # Arguments
/// - `adj` (`&[BTreeSet<usize>]`): Adjazenzliste aus [`build_edges`].
/// - `nodes` (`&[PlanNode]`): dieselben Knoten, nur um lesbare IDs in die
///   Meldung zu setzen.
///
/// # Returns
/// Eine Meldung je gefundenem Zyklus, mit der Kette der beteiligten IDs.
/// Leer, wenn der Graph azyklisch ist.
///
/// # Examples
/// ```rust,ignore
/// let cycles = detect_cycles(&adj, &nodes);
/// assert!(cycles.is_empty(), "der Plan darf keine Zyklen enthalten");
/// ```
#[must_use]
fn detect_cycles(adj: &[BTreeSet<usize>], nodes: &[PlanNode]) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    // Rekursiver Helfer als gewöhnliche `fn` mit allen Zuständen als
    // Parameter — kein Environment-Capture, deshalb ohne Umweg rekursiv
    // aufrufbar.
    fn visit(
        u: usize,
        adj: &[BTreeSet<usize>],
        color: &mut [Color],
        path: &mut Vec<usize>,
        nodes: &[PlanNode],
        out: &mut Vec<String>,
    ) {
        color[u] = Color::Gray;
        path.push(u);
        for &v in &adj[u] {
            match color[v] {
                Color::White => visit(v, adj, color, path, nodes, out),
                Color::Gray => {
                    if let Some(pos) = path.iter().position(|&x| x == v) {
                        let mut chain: Vec<&str> =
                            path[pos..].iter().map(|&i| nodes[i].id.as_str()).collect();
                        chain.push(nodes[v].id.as_str());
                        let message = format!("Zyklus im Hängt-an-Graphen: {}", chain.join(" → "));
                        if !out.contains(&message) {
                            out.push(message);
                        }
                    }
                }
                Color::Black => {}
            }
        }
        path.pop();
        color[u] = Color::Black;
    }

    let mut color = vec![Color::White; nodes.len()];
    let mut path = Vec::new();
    let mut out = Vec::new();
    for i in 0..nodes.len() {
        if color[i] == Color::White {
            visit(i, adj, &mut color, &mut path, nodes, &mut out);
        }
    }
    out
}

/// Alle Knoten, die von `start` über gerichtete Kanten erreichbar sind.
///
/// # Description
/// Iterative Tiefensuche mit einem besuchten-Set als Wächter — sicher gegen
/// Zyklen, unabhängig davon, ob [`detect_cycles`] vorher einen gefunden hat.
/// Dieselbe Absicherung wie `transitive_hull` im Gate `edges`.
///
/// # Arguments
/// - `start` (`usize`): Index des Startknotens.
/// - `adj` (`&[BTreeSet<usize>]`): die Adjazenzliste.
///
/// # Returns
/// Die Indizes aller transitiv erreichbaren Knoten, ohne `start` selbst
/// (außer ein Zyklus führt zurück zu ihm).
///
/// # Examples
/// ```rust,ignore
/// let reach = reachable_from(0, &adj);
/// assert!(reach.contains(&2));
/// ```
#[must_use]
fn reachable_from(start: usize, adj: &[BTreeSet<usize>]) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![start];
    while let Some(u) = stack.pop() {
        for &v in &adj[u] {
            if seen.insert(v) {
                stack.push(v);
            }
        }
    }
    seen
}

/// Wertet einen bereits geparsten Plan aus.
///
/// # Description
/// Getrennt von [`run`], damit die Überlappungsregel gegen **konstruierte**
/// Tabellen prüfbar ist ([`parse_plan`] davor aufrufen) — derselbe Grund,
/// aus dem `edges::evaluate` vom Dateisystem getrennt ist. Baut den
/// Abhängigkeitsgraphen ([`build_edges`]), erkennt Zyklen ([`detect_cycles`],
/// gemeldet statt gehängt), bildet je Knoten die transitive Erreichbarkeit
/// ([`reachable_from`]) und prüft jedes Knotenpaar ohne Pfad in beide
/// Richtungen auf überlappende Schreibbereiche ([`scope_overlap`]).
///
/// # Arguments
/// - `parsed` (`&ParsedPlan`): das Ergebnis von [`parse_plan`].
///
/// # Returns
/// Den [`GateReport`]: `checked` ist die Zahl aller Schreibbereich-Einträge
/// nach Brace-Expansion (ignorierte Prosa-Zellen zählen nicht mit);
/// `violations` enthält überlappende Paare, Zyklen und nicht auflösbare
/// Abhängigkeiten, jede Meldung genau einmal.
///
/// # Examples
/// ```rust,ignore
/// let parsed = parse_plan(markdown)?;
/// let report = evaluate(&parsed);
/// assert!(report.is_green());
/// ```
#[must_use]
fn evaluate(parsed: &ParsedPlan) -> GateReport {
    let nodes = &parsed.nodes;
    let (adj, mut violations) = build_edges(nodes);

    for message in detect_cycles(&adj, nodes) {
        if !violations.contains(&message) {
            violations.push(message);
        }
    }

    let reach: Vec<BTreeSet<usize>> = (0..nodes.len()).map(|i| reachable_from(i, &adj)).collect();
    let checked: usize = nodes.iter().map(|n| n.scopes.len()).sum();

    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            if reach[i].contains(&j) || reach[j].contains(&i) {
                continue;
            }
            for scope_a in &nodes[i].scopes {
                for scope_b in &nodes[j].scopes {
                    if let Some(overlap_path) = scope_overlap(&scope_a.path, &scope_b.path) {
                        let message = format!(
                            "{} ({}) und {} ({}) überlappen auf {overlap_path}; kein Pfad in \
                             beide Richtungen",
                            nodes[i].id, scope_a.source_text, nodes[j].id, scope_b.source_text
                        );
                        if !violations.contains(&message) {
                            violations.push(message);
                        }
                    }
                }
            }
        }
    }

    GateReport {
        name: "writescopes",
        checked,
        violations,
    }
}

/// Findet die Workspace-Wurzel, unter der `docs/aw-plan.md` liegen muss.
///
/// # Description
/// `run` hat keine Argumente, die Wurzel muss also gefunden werden.
/// Bevorzugt wird `CARGO_MANIFEST_DIR` zur Kompilierzeit: `xtask` liegt
/// direkt unter der Workspace-Wurzel, ihr Elternverzeichnis ist sie also,
/// sofern dort ein `Cargo.toml` liegt. Das ist stabil, solange der Baum
/// nicht verschoben wird, nachdem `xtask` gebaut wurde. Als Rückfallebene
/// steigt die Funktion vom aktuellen Arbeitsverzeichnis auf und sucht nach
/// einem `Cargo.toml`, das `[workspace]` enthält — das funktioniert auch,
/// wenn das Binary aus einem verschobenen Checkout heraus läuft.
///
/// # Returns
/// Der Pfad zur Workspace-Wurzel.
///
/// # Errors
/// Wenn weder der kompilierzeitliche Pfad noch der Aufstieg vom aktuellen
/// Verzeichnis ein `Cargo.toml` mit `[workspace]` findet.
///
/// # Examples
/// ```rust,ignore
/// let root = find_workspace_root()?;
/// assert!(root.join("Cargo.toml").is_file());
/// ```
fn find_workspace_root() -> Result<PathBuf, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(parent) = manifest_dir.parent() {
        if parent.join("Cargo.toml").is_file() {
            return Ok(parent.to_path_buf());
        }
    }

    let mut dir = std::env::current_dir()
        .map_err(|error| format!("aktuelles Arbeitsverzeichnis nicht lesbar: {error}"))?;
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            if let Ok(content) = std::fs::read_to_string(&candidate) {
                if content.contains("[workspace]") {
                    return Ok(dir);
                }
            }
        }
        if !dir.pop() {
            break;
        }
    }

    Err(
        "Workspace-Wurzel nicht gefunden: weder unter CARGO_MANIFEST_DIR/.. noch durch \
         Aufstieg zu einem Cargo.toml mit [workspace]"
            .to_owned(),
    )
}

/// Führt Gate 3 aus.
///
/// # Description
/// Findet die Workspace-Wurzel ([`find_workspace_root`]), liest
/// `docs/aw-plan.md`, parst alle Knotentabellen ([`parse_plan`]) und wertet
/// sie aus ([`evaluate`]). Prosa-Zellen ohne Backtick-Pfad werden nicht
/// stillschweigend übersprungen: eine Hinweiszeile je Fund geht über
/// `stderr`, bevor der Bericht zurückkommt.
///
/// # Returns
/// Einen [`GateReport`] mit der Zahl der geprüften Schreibbereiche.
///
/// # Errors
/// - Wenn keine Workspace-Wurzel gefunden wird ([`find_workspace_root`]).
/// - Wenn `docs/aw-plan.md` unter dieser Wurzel fehlt oder nicht lesbar
///   ist — ein Gate ohne Plan meldet nie grün.
/// - Wenn keine Knotentabelle im erwarteten Format gefunden wird, oder eine
///   Tabellenzeile nicht die erwarteten vier Spalten hat ([`parse_plan`]).
///
/// # Examples
/// ```rust,ignore
/// match run() {
///     Ok(report) => println!("{}", report.summary()),
///     Err(message) => eprintln!("{message}"),
/// }
/// ```
pub fn run() -> Result<GateReport, String> {
    let root = find_workspace_root()?;
    let plan_path = root.join("docs").join("aw-plan.md");
    let content = std::fs::read_to_string(&plan_path).map_err(|error| {
        format!(
            "Ausbauplan nicht lesbar unter {}: {error}",
            plan_path.display()
        )
    })?;

    let parsed = parse_plan(&content)?;
    for notice in &parsed.ignored {
        eprintln!("{notice}");
    }

    Ok(evaluate(&parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &str) -> String {
        format!(
            "| ID | Titel | Schreibbereich | Hängt an |\n|---|---|---|---|\n{rows}"
        )
    }

    #[test]
    fn test_evaluate_same_scope_without_path_is_violation() {
        let markdown = table(
            "| A-01 | eins | `crate/a.rs` | — |\n\
             | A-02 | zwei | `crate/a.rs` | — |\n",
        );
        let parsed = parse_plan(&markdown).expect("sollte parsen");
        let report = evaluate(&parsed);

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("crate/a.rs") && v.contains("kein Pfad")),
            "{:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_same_scope_with_edge_is_green() {
        let markdown = table(
            "| A-01 | eins | `crate/a.rs` | — |\n\
             | A-02 | zwei | `crate/a.rs` | A-01 |\n",
        );
        let parsed = parse_plan(&markdown).expect("sollte parsen");
        let report = evaluate(&parsed);

        assert!(report.is_green(), "{:?}", report.violations);
    }

    #[test]
    fn test_evaluate_transitive_path_over_three_nodes_is_green() {
        // A-03 hängt an A-02, A-02 hängt an A-01: A-03 → A-02 → A-01 ist ein
        // Pfad, obwohl A-03 keine direkte Kante zu A-01 hat.
        let markdown = table(
            "| A-01 | eins | `crate/a.rs` | — |\n\
             | A-02 | zwei | `crate/b.rs` | A-01 |\n\
             | A-03 | drei | `crate/a.rs` | A-02 |\n",
        );
        let parsed = parse_plan(&markdown).expect("sollte parsen");
        let report = evaluate(&parsed);

        assert!(report.is_green(), "{:?}", report.violations);
    }

    #[test]
    fn test_scope_overlap_detects_containment_not_only_equality() {
        let markdown = table(
            "| A-01 | eins | `harw-core/**` | — |\n\
             | A-02 | zwei | `harw-core/src/session.rs` | — |\n",
        );
        let parsed = parse_plan(&markdown).expect("sollte parsen");
        let report = evaluate(&parsed);

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("harw-core/src/session.rs")),
            "Gleichheit allein hätte diese Kollision nicht gefunden: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_brace_expansion_checks_both_branches() {
        let markdown = table(
            "| A-01 | eins | `crate/{x,y}.rs` | — |\n\
             | A-02 | zwei | `crate/y.rs` | — |\n",
        );
        let parsed = parse_plan(&markdown).expect("sollte parsen");
        assert_eq!(parsed.nodes[0].scopes.len(), 2, "beide Zweige erwartet");

        let report = evaluate(&parsed);
        assert!(!report.is_green());
        assert!(
            report.violations.iter().any(|v| v.contains("crate/y.rs")),
            "{:?}",
            report.violations
        );
        assert!(
            !report.violations.iter().any(|v| v.contains("crate/x.rs")),
            "crate/x.rs überlappt mit nichts und darf nicht auftauchen: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_prose_cell_is_ignored_with_notice_not_silently() {
        let markdown = table("| A-01 | eins | alle neuen Crate-Stubs | — |\n");
        let parsed = parse_plan(&markdown).expect("sollte parsen");

        assert_eq!(parsed.nodes[0].scopes.len(), 0);
        assert_eq!(
            parsed.ignored.len(),
            1,
            "Prosa muss ausgewiesen werden, nicht verschwinden"
        );
        assert!(parsed.ignored[0].contains("alle neuen Crate-Stubs"));

        let report = evaluate(&parsed);
        assert_eq!(report.checked, 0, "Prosa zählt nicht als geprüfter Bereich");
        assert!(!report.is_green(), "reine Prosa prüft nichts (G-102)");
    }

    #[test]
    fn test_cycle_is_reported_and_does_not_hang() {
        let markdown = table(
            "| A-01 | eins | `crate/a.rs` | A-02 |\n\
             | A-02 | zwei | `crate/b.rs` | A-01 |\n",
        );
        let parsed = parse_plan(&markdown).expect("sollte parsen");
        // Terminiert die Testfunktion überhaupt (statt zu hängen), ist das
        // bereits der Kernbeweis; die Meldung wird zusätzlich geprüft.
        let report = evaluate(&parsed);

        assert!(
            report.violations.iter().any(|v| v.contains("Zyklus")),
            "{:?}",
            report.violations
        );
    }

    #[test]
    fn test_checked_counts_scope_entries_not_nodes() {
        let markdown = table(
            "| A-01 | eins | `crate/a.rs`, `crate/{x,y}.rs` | — |\n\
             | A-02 | zwei | `crate/b.rs` | — |\n",
        );
        let parsed = parse_plan(&markdown).expect("sollte parsen");
        let report = evaluate(&parsed);

        // A-01: crate/a.rs, crate/x.rs, crate/y.rs (3) + A-02: crate/b.rs (1) = 4.
        assert_eq!(report.checked, 4);
    }

    #[test]
    fn test_split_row_respects_escaped_pipe() {
        let cells = split_row(r"| A-01 | `SensorHandle<Unbound\|Bound>` | `crate/a.rs` | — |");
        assert_eq!(cells.len(), 4, "{cells:?}");
        assert_eq!(cells[1], "`SensorHandle<Unbound|Bound>`");
    }

    #[test]
    fn test_id_range_reference_resolves_to_grouped_and_individual_nodes() {
        let markdown = table(
            "| A-01 | eins | `crate/x.rs` | — |\n\
             | A-02..04 | gruppe | `crate/{m,n,o}.rs` | A-01 |\n\
             | A-05 | fünf | `crate/z.rs` | A-02 |\n\
             | A-06 | sechs | `crate/w.rs` | A-02..A-05 |\n",
        );
        let parsed = parse_plan(&markdown).expect("sollte parsen");
        let (adj, unresolved) = build_edges(&parsed.nodes);
        assert!(unresolved.is_empty(), "{unresolved:?}");

        let idx = |id: &str| parsed.nodes.iter().position(|n| n.id == id).unwrap();
        let group = idx("A-02..04");
        let a06 = idx("A-06");
        assert!(
            adj[a06].contains(&group),
            "A-06 muss über den Bereich A-02..A-05 auch die zusammengefasste \
             Zeile A-02..04 erreichen"
        );
        assert!(adj[a06].contains(&idx("A-05")));
    }

    #[test]
    fn test_parse_plan_without_node_table_is_err() {
        let result = parse_plan("# Kein Plan\n\nNur Text, keine Tabelle.\n");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_plan_missing_separator_row_is_err() {
        let markdown = "| ID | Titel | Schreibbereich | Hängt an |\n\
                         | A-01 | eins | `a.rs` | — |\n";
        assert!(parse_plan(markdown).is_err());
    }
}
