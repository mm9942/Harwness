//! Gate 4 (Warden-Abhängigkeitszahl) und Gate 5 (kein C-Build im
//! Warden-Teilbaum) — beide betreffen dieselbe transitive Laufzeit-Hülle von
//! `harw-warden` und leben deshalb in einer Datei.
//!
//! # Auftrag
//! Entscheidung Nr. 1 des Verifikationsplans, präzisiert durch K53:
//! **höchstens zwölf Crates in der transitiven Laufzeit-Hülle von
//! `harw-warden`, gezählt über `[dependencies]`, ohne
//! `[dev-dependencies]`, ohne `[build-dependencies]` und ohne
//! Proc-Macro-Crates.** Keine dieser drei Kategorien läuft im ausgelieferten
//! Warden-Prozess — sie zu zählen misst etwas anderes als das, wovor die
//! Regel schützt, und war der Grund, warum zwei unabhängige Messungen zuvor
//! 65 meldeten (`syn`, `quote`, `proc-macro2` liefen dort als Bauzeit-Ketten
//! mit).
//!
//! # Wie die Hülle gezählt wird
//! Ausgangspunkt ist `harw-warden` selbst (nicht mitgezählt — wie bei
//! `gate_privileges.rs::reachable_with_chain` zählt eine Hülle nur, was von
//! der Wurzel aus *erreicht* wird, nicht die Wurzel selbst). Die
//! Traversierung folgt ausschließlich normalen `[dependencies]`-Kanten,
//! sowohl innerhalb des Workspace ([`CrateNode::deps`]/
//! [`CrateNode::external_deps`], von `harw-code-graph` bereits nach Art
//! getrennt) als auch außerhalb (externe Crates über `Cargo.lock` und den
//! lokalen Registry-Cache, siehe unten). Ein `[lib] proc-macro = true`
//! markiertes Crate wird **nicht** in die Hülle aufgenommen und seine
//! eigenen Abhängigkeiten werden **nicht** weiterverfolgt — ein Proc-Macro
//! läuft zur Kompilierzeit des abhängigen Crates, nie im ausgelieferten
//! Binary (dieselbe Begründung wie in `gate_privileges.rs`, Abschnitt
//! „Proc-Macros").
//!
//! Für Crates außerhalb des Workspace gibt es keinen `harw-code-graph`-Typ,
//! der Abhängigkeitsart trennt — deshalb liest dieses Modul für jedes
//! erreichte externe Crate dessen **eigenes** `Cargo.toml` aus dem lokalen
//! Registry-Cache (`~/.cargo/registry/src/...`, aufgelöst über
//! [`RegistrySourceLocator`]) und wertet `[dependencies]` bzw.
//! `[target.'cfg(...)'.dependencies]` selbst aus — nur Abschnitte, deren
//! `cfg(...)`-Ausdruck für das tatsächliche Zielsystem dieses Workspace
//! (`x86_64-unknown-linux-gnu`) zutrifft, zählen; ein `cfg(windows)`- oder
//! `cfg(target_arch = "wasm32")`-Abschnitt eines beliebigen Crates in der
//! Hülle bringt sonst Dutzende Windows-/Wasm-Crates ins Bild, die auf
//! diesem Linux-Ziel nie gebaut werden. `[build-dependencies]` und
//! `[dev-dependencies]` werden bei externen Crates ebenso wenig verfolgt
//! wie bei internen.
//!
//! Es wird **kein** `toml`-Crate verwendet (`xtask` hängt bewusst nur von
//! `harw-code-graph` ab, siehe dessen `Cargo.toml`-Kommentar, und diese
//! Datei darf laut Auftrag keine neue Abhängigkeit einführen) — sowohl
//! `Cargo.lock` als auch die externen `Cargo.toml`-Dateien werden über einen
//! schlanken, zeilenbasierten Parser gelesen ([`parse_lock_entries`],
//! [`parse_manifest_facts`]). Beide sind bewusst auf das begrenzt, was für
//! diese beiden Gates gebraucht wird (Namen, keine Werte) — kein
//! allgemeiner TOML-Parser.
//!
//! # Versions-Eindeutigkeit über die eigene Lockfile-Kante
//! Ein Crate-Name kann in `Cargo.lock` mehrfach mit unterschiedlichen
//! Versionen auftauchen (z. B. `rustix` 0.38 *und* 1.1 irgendwo in diesem
//! ca. 190 Crates großen Workspace) — `harw-code-graph`s `CrateNode`
//! kennt für externe Abhängigkeiten nur den *Namen*, keine Version. Um
//! trotzdem die richtige Version zu treffen, liest dieses Modul für **jedes**
//! erreichte Crate (intern wie extern) dessen eigenen `[[package]]`-Eintrag
//! in `Cargo.lock` — der trägt Cargos eigene, bereits aufgelöste
//! `dependencies`-Liste, in der eine mehrdeutige Abhängigkeit von Cargo
//! selbst mit einer Versionsangabe versehen wird (`"rustix 1.1.4"` statt nur
//! `"rustix"`). Erst wenn auch das nicht eindeutig ist (kein eigener
//! Lockfile-Eintrag mit passendem Namen gefunden, oder mehrdeutig ganz ohne
//! Disambiguierung), gilt die Kante als nicht auflösbar — siehe nächster
//! Abschnitt.
//!
//! # Was passiert, wenn eine Information fehlt
//! Weder stilles Überspringen noch ein hartes `Err`, das den gesamten
//! Gate-Lauf abbricht: eine nicht auflösbare Kante (kein Lockfile-Eintrag,
//! mehrdeutige Version ohne Disambiguierung, kein entpacktes
//! Registry-Verzeichnis, kein lesbares `Cargo.toml` an der erwarteten
//! Stelle) wird als **Verstoß** in den jeweiligen [`GateReport`]
//! aufgenommen und zählt in `checked` mit — ein Gate, das bei einer
//! unbeschaffbaren Information schweigend grün bleibt, ist genau die
//! Verwechslung, gegen die `checked` eingeführt wurde (siehe
//! `xtask/src/gates.rs`-Moduldoku).
//!
//! # Gate 5: kein C-Übersetzer im Warden-Teilbaum
//! Kein Crate in derselben Hülle darf zur Bauzeit einen C-Übersetzer
//! brauchen. Ein `build.rs` allein ist **kein** C-Build — `rustix` und `nix`
//! haben beide eines (reine Feature-/Architekturerkennung bzw.
//! `cfg_aliases`), ohne dass beide je einen C-Header übersetzen. Das
//! verlässliche Kennzeichen ist der `links`-Schlüssel im `[package]`-
//! Abschnitt des Manifests (reserviert genau für Crates, die eine native
//! Bibliothek verlinken) und/oder eine `[build-dependencies]`-Kante auf `cc`
//! oder `bindgen` — beide sind die Werkzeuge, mit denen ein Rust-Build.rs
//! tatsächlich C-Code übersetzt oder gegen einen C-Header bindet. Dieses
//! Gate prüft **ausschließlich** diese beiden Signale, nie die bloße
//! Existenz eines `build.rs` — ein Gate, das jedes `build.rs` verböte, wäre
//! beim ersten Lauf rot und liefe Gefahr, abgeschaltet zu werden.
//!
//! Eine kleine, datierte Ausnahmetabelle ([`C_BUILD_EXCEPTIONS`]) erlaubt
//! zusätzlich gezielte, begründete Abweichungen je Crate **und** Werkzeug
//! (`cc`/`bindgen` einzeln, nicht das ganze Crate pauschal) — sie ersetzt
//! keine Prüfung, sondern dokumentiert eine bewusst getroffene, nachprüfbare
//! Entscheidung (Datum + Begründung), siehe Befund G-002.
//!
//! # Warum dieselbe Hülle für beide Gates
//! Beide Gates fragen unterschiedliche Dinge über denselben Baum ("wie
//! viele Knoten?" vs. "hat einer davon eine native Bauzeit-Kante?"), aber
//! die Traversierung ist identisch teuer (Cargo.lock lesen, Registry-Cache
//! auflösen, jedes Manifest einmal parsen) — [`compute_closure`] läuft (seit
//! der Feature-Auflösungs-Korrektur unten bis zum Fixpunkt) und liefert eine
//! [`ClosureResult`], aus der [`evaluate_dependency_budget`] und
//! [`evaluate_c_build`] je einen eigenen [`GateReport`] ableiten.
//!
//! # Feature-Auflösung: eine optionale Abhängigkeit zählt nur, wenn sie aktiviert wird
//! **Befund:** Der erste echte Lauf dieses Gates zählte 66 Crates und
//! meldete `defmt` als C-Build-Verstoß. Nachgemessen: `defmt` wird nie
//! gebaut. `jiff` 0.2.32 bietet es hinter einem rein optionalen Feature an
//! (`[features] defmt = ["dep:defmt"]` in der gepinnten Registry-Quelle
//! unter `~/.cargo/registry/src/.../jiff-0.2.32/Cargo.toml`), und **keine**
//! der rund fünfzig Stellen in diesem Workspace, die `jiff` referenzieren,
//! aktiviert dieses Feature — die Wurzel setzt in
//! `[workspace.dependencies]` ausschließlich `features = ["serde"]`.
//!
//! **Ursache:** `Cargo.lock` ist keine Aussage darüber, was gebaut wird.
//! Sein `dependencies`-Array pro Paket listet, was bei irgendeiner im
//! Workspace vorkommenden Feature-Kombination gebraucht werden könnte —
//! genau damit ein späteres Zuschalten eines Features keine neue
//! Versionsauflösung erzwingt —, nicht was bei der tatsächlich aktivierten
//! Kombination gebraucht wird. Die Vorfassung dieses Gates folgte jeder
//! `Cargo.lock`-Kante bedingungslos und zählte deshalb Code mit, der nie im
//! Binary landet.
//!
//! **Korrektur:** [`compute_closure`] führt seine Traversierung
//! ([`ClosureBuilder`]) jetzt mehrfach aus, bis zum Fixpunkt. Jeder
//! Durchlauf entscheidet für jede Kante, ob sie **optional** ist
//! ([`DependencyEdgeAttrs::optional`], aus [`ManifestFacts::dependency_attrs`]);
//! ist sie es, wird sie nur gefolgt, wenn das im *vorigen* Durchlauf
//! bekannte Feature-Set ihres Elternteils sie tatsächlich aktiviert
//! ([`resolve_activated_optional_deps`], über die `[features]`-Tabelle des
//! Elternteils, [`ManifestFacts::feature_defs`], inklusive impliziter
//! Features für nicht namensraum-gebundene optionale Abhängigkeiten,
//! `dep:name`-Syntax, `pkg/feature`- und `pkg?/feature`-Tokens). Jede
//! gefolgte Kante trägt additiv ihre eigenen `features`/`default-features`
//! in das Feature-Set des Kindes für den *nächsten* Durchlauf
//! ([`ClosureBuilder::maybe_follow_edge`]) — das macht die Aktivierung
//! **transitiv**: `jiff`s eigene `features = ["serde"]`-Kante wird so zur
//! Grundlage dafür, welche von `jiff`s **eigenen** optionalen Abhängigkeiten
//! aktiv sind. Eine Abhängigkeit mit `dep.workspace = true` wird zuerst über
//! [`resolve_workspace_edge`] mit der Wurzel-`[workspace.dependencies]`-
//! Angabe zusammengeführt ([`parse_workspace_dependencies`]) — lokale
//! zusätzliche `features` additiv, `default-features` von der Wurzel.
//!
//! **Was bleibt:** `blake3`s `cc`-Build-Dependency steht in dessen Manifest
//! **ohne** `optional = true` — sie besteht für jede Feature-Kombination.
//! Das `pure`-Feature schaltet nur die SIMD-Erzeugung ab, nicht die Kante
//! selbst. Diese Korrektur ändert daran nichts.
//!
//! **Nachtrag 2026-09-13 (G-002):** Genau diese eine Kante ist inzwischen
//! geprüft und bewusst freigegeben — siehe [`C_BUILD_EXCEPTIONS`]. Sie
//! existiert weiterhin und wird weiterhin in [`ClosureResult::facts`]
//! erfasst, zählt aber ab hier nicht mehr als Verstoß in
//! [`evaluate_c_build`]. Ein zuvor unbekanntes Crate mit derselben Art von
//! Kante bleibt rot — die Ausnahme gilt ausschließlich für den eingetragenen
//! Namen/Werkzeug-Paar, nicht pauschal für "jedes `build.rs` mit `cc`".
//!
//! **Grenzen dieser Auflösung** — bewusst nicht geschlossen:
//!
//! - Ein `pkg/feature`-Token aktiviert `pkg`, aber die konkrete `feature`
//!   wird nicht als zusätzliche Anforderung an `pkg` weitergereicht — nur
//!   die direkt an einer Kante deklarierten `features`/`default-features`
//!   fließen additiv ein. Für die Warden-Hülle wurde geprüft, dass diese
//!   Form nirgends eine weitere optionale Kaskade auslöst (`jiff`,
//!   `blake3`, `harw-types`).
//! - Umbenennungs-Aliase (`foo = { package = "wirklicher-name" }`) werden
//!   weiterhin nicht aufgelöst — unverändert gegenüber der Vorfassung.
//! - Die neue Zahl wurde nicht durch einen `cargo`-Lauf verifiziert (der
//!   Auftrag verbietet `cargo` in dieser Datei), sondern von Hand aus
//!   `Cargo.lock` und den gepinnten Registry-Manifesten hergeleitet, siehe
//!   [`MAX_WARDEN_RUNTIME_DEPS`]. Der tatsächliche Gate-Lauf ist die
//!   verbindliche Quelle und geht im Zweifel vor.
//!
//! # Einstiegspunkte
//! [`dependency_budget::run`] und [`c_build::run`] — je in der Form von
//! [`super::privileges::run`]/[`super::edges::run`]
//! (`pub fn run() -> Result<GateReport, String>`), damit
//! `xtask/src/gates.rs` sie unverändert neben die drei bestehenden Gates
//! stellen kann.
//!
//! # Stand
//! Beide Gates sind neu; sie wurden laut Auftrag bisher nie gebaut, obwohl
//! der Verifikationsplan sie als Teil der Abnahme führt.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use harw_code_graph::{CrateNode, RegistrySourceLocator, WorkspaceGraph};

use super::GateReport;

/// Der Crate-Name der Wurzel, deren Hülle geprüft wird.
pub const WARDEN_ROOT: &str = "harw-warden";

/// Das Budget aus K53, neu hergeleitet nach dem ersten echten Gate-Lauf.
///
/// # Description
/// K53 verlangte ursprünglich „höchstens zwölf Crates in der transitiven
/// Laufzeit-Hülle von `harw-warden`" — das war in sich widersprüchlich: die
/// Zahl zwölf stammte aus der Zählung der **direkten** `[dependencies]`-
/// Einträge in `harw-warden/Cargo.toml` (dort stimmt sie, es sind genau
/// zwölf), nicht aus der **transitiven** Hülle. Die transitive Hülle war
/// beim allerersten Lauf dieses Gates 66 Crates (intern 3, extern 63) und
/// war nach Aussage des Verifikationsplans nie 12 — niemand hatte das vorher
/// gemessen, weil dieses Gate vorher nicht gebaut war.
///
/// Zwei Zählweisen kommen infrage. Die direkte Zahl misst, wie viele Namen
/// in einer Datei stehen; sie sagt nichts darüber, was tatsächlich im
/// laufenden Warden-Prozess mit erhöhten Rechten ausgeführt wird, weil jede
/// der zwölf direkten Abhängigkeiten selbst wieder Abhängigkeiten mitbringt.
/// Die transitive Zahl misst genau das, wovor K53 schützen soll — Code, der
/// mit erhöhten Rechten läuft und den niemand im Team geschrieben hat. Diese
/// Konstante zählt deshalb **transitiv**, wie es [`evaluate_dependency_budget`]
/// bereits über [`compute_closure`]/[`ClosureResult::total_resolved`] tut,
/// und nicht direkt.
///
/// Der Wert 66 ist keine gerundete oder irgendwie „glatte" Grenze — eine
/// willkürlich gerundete Grenze wäre keine Grenze. Er ist der am Tag dieser
/// Korrektur gemessene Stand der Hülle, als **Sperrklinke** gesetzt: er darf
/// nicht steigen, jede Erhöhung ist eine bewusste, begründete Entscheidung
/// (und ein neuer Kommentar hier). Er darf jederzeit sinken, sobald die
/// Hülle tatsächlich schrumpft.
///
/// **Nachkorrektur (Feature-Auflösung):** Der ursprüngliche Lauf zählte
/// jeden in `Cargo.lock` genannten Namen mit, unabhängig davon, ob ein
/// Feature ihn je aktiviert — `Cargo.lock`s `dependencies`-Array pro Paket
/// listet, was bei *irgendeiner* im Workspace vorkommenden Feature-
/// Kombination gebraucht werden könnte, nicht was bei der tatsächlich
/// aktivierten Kombination gebraucht wird (siehe Moduldoku, Abschnitt
/// „Feature-Auflösung"). Nachgemessen: `defmt` (`jiff`s optionales
/// `dep:defmt` hinter dem Feature `defmt`) und `jiff-static` (`jiff`s
/// optionales `dep:jiff-static` hinter `static-tz`; das in `jiff`s
/// `default`-Featureliste enthaltene `tz-fat = ["jiff-static?/tz-fat"]` ist
/// eine **schwache** Referenz und aktiviert `jiff-static` gerade nicht)
/// werden von **niemandem** im Workspace angefordert — die Wurzel setzt für
/// `jiff` ausschließlich `features = ["serde"]`. Mit ihnen verschwindet
/// auch `bitflags` in Version `1.3.2`, die einzige Version, die
/// ausschließlich über `defmt` erreicht wurde. `log` bleibt Teil der Hülle:
/// `harw-warden` hängt über `tracing-subscriber`s Default-Feature
/// `tracing-log` **nicht-optional** davon ab, unabhängig von `jiff`.
/// `defmt-macros` war schon vorher ausgeschlossen (`proc-macro = true`) und
/// zählte nie separat mit; `defmt-parser` wurde nie erreicht.
///
/// **66 minus diese drei Knoten (`defmt`, `jiff-static`, `bitflags`
/// 1.3.2) ergibt 63.** Dieser Wert wurde von Hand aus `Cargo.lock` und den
/// gepinnten Registry-Manifesten unter `~/.cargo/registry/src/...`
/// hergeleitet. **Der tatsächliche Gate-Lauf ist die verbindliche Quelle**,
/// und er hat die Handrechnung korrigiert: erwartet waren 63, **gemessen
/// wurden 46** (Lauf vom 2026-09-02, nachdem `parse_workspace_dependencies`
/// auch die Kurzform `name = "1.2.3"` in `[workspace.dependencies]` liest —
/// vorher blieben ganze Teilhüllen hinter `blake3`, `serde_json`, `tracing`
/// und `rustix` unaufgelöst). Die Differenz von 17 ist kein Puffer, sondern
/// war eine Fehlmessung; die Konstante steht deshalb auf dem gemessenen
/// Wert.
///
/// Der Wert bleibt eine **Sperrklinke**: er darf nicht steigen, jede
/// Erhöhung ist eine bewusste, begründete Entscheidung (und ein neuer
/// Kommentar hier). Er darf jederzeit sinken, sobald die Hülle tatsächlich
/// schrumpft — zum Beispiel durch ein `cargo update`, das die in
/// `harw-dod-warden-proto/Cargo.toml` vermutete veraltete `jiff`-Auflösung
/// auffrischt.
///
/// **Sperrklinke 46 → 54 (2026-09-24), mit Begründung je Knoten.** Seit
/// `harw-warden` im eigenen DoD-Workspace unter `dod/` liegt, fand dieses Gate
/// seine Wurzel nicht mehr („nicht im Workspace-Graphen") und hat nichts
/// gemessen. In dieser Zeit ist die Hülle ungemessen gewachsen. Nach dem
/// Umbau auf den zusammengeführten Graphen und `dod/Cargo.lock` misst der Lauf
/// 54 (intern 5, extern 49). Die acht Knoten lassen sich einzeln zuordnen:
///
/// - **+5 über `harw-types` → `tokio-util`** (`tokio-util`, `tokio`, `bytes`,
///   `futures-core`, `futures-sink`): `CancelToken` wurde aus `harw-core`
///   nach `harw-types` verschoben (F-160/G-017) und wickelt
///   `tokio_util::sync::CancellationToken`. Der Warden benutzt
///   `CancelToken` nicht, er braucht aus `harw-types` nur `CgroupId`,
///   `FindingId`, `ApprovalActor` und `ContentDigest`. Die Kante ist für den
///   Warden also unnötig, aber nicht mit einer Zeile lösbar: ein
///   abschaltbares Feature in `harw-types` würde beim gemeinsamen Bau der
///   vier DoD-Binaries (`dod/Makefile`, `build`) durch Feature-Vereinigung
///   wieder eingeschaltet, dieses Gate zählte dann weniger, als tatsächlich
///   gebaut wird. Die ehrliche Lösung ist, `CancelToken` aus `harw-types`
///   herauszulösen (rund fünfzehn Verwender). Danach sinkt diese Zahl um
///   fünf.
/// - **+2 über `harw-completions`** (`harw-completions`, `clap_complete`):
///   der Warden hat bewusst einen `completions`-Unterbefehl (Feature
///   `clap-args`); `clap` selbst war schon vorher in der Hülle.
/// - **+1 `harw-digest`**: `ContentDigest` ist aus `harw-types` in diese
///   Leaf-Crate gewandert (Regel L7 im `edges`-Gate). Kein neuer Fremdcode —
///   `blake3` und `serde` waren schon vorher in der Hülle —, nur ein neuer
///   Knoten im selben Baum.
///
/// 46 + 5 + 2 + 1 = 54. Die Klinke gilt ab hier wieder: nicht steigen ohne
/// neuen Absatz hier, jederzeit sinken.
///
/// **PL-60 (DoD im Wurzel-Workspace).** Seit `dod/` kein eigener Workspace
/// mehr ist, misst dieses Gate gegen das Wurzel-`Cargo.lock`. Die
/// Zusammenführung darf die Zahl **nicht** erhöhen: ein Anstieg hieße, dass
/// die gemeinsame Auflösung dem Warden neue Knoten untergeschoben hat —
/// siehe `docs/architecture/dod-workspace-merge-plan.md`, Abschnitt
/// „TCB-Invarianten".
pub const MAX_WARDEN_RUNTIME_DEPS: usize = 54;

/// Crate-Namen, deren Anwesenheit als `[build-dependencies]`-Kante ein
/// verlässliches Zeichen für einen echten C-Übersetzer-Bauschritt ist —
/// siehe Moduldoku, Abschnitt „Gate 5".
const C_BUILD_TOOL_CRATES: &[&str] = &["cc", "bindgen"];

/// Eine einzelne, datierte und begründete Ausnahme vom C-Build-Verbot aus
/// Gate 5 — siehe Moduldoku, Abschnitt „Gate 5".
///
/// # Description
/// Jede Ausnahme gilt für genau **ein** Crate **und** genau **ein**
/// C-Bau-Werkzeug (`"cc"` oder `"bindgen"`, wie in [`C_BUILD_TOOL_CRATES`]).
/// Ein Crate, das über mehrere Werkzeuge oder zusätzlich über den
/// `links`-Schlüssel als C-Build erkannt wird, bleibt für die nicht
/// eingetragenen Signale weiterhin ein Verstoß — eine Ausnahme deckt nie
/// mehr ab, als sie ausdrücklich benennt.
#[derive(Debug, Clone, Copy)]
struct CBuildException {
    /// Name des betroffenen Crates.
    krate: &'static str,
    /// Das konkrete Werkzeug, für das die Ausnahme gilt (`"cc"` oder
    /// `"bindgen"`).
    tool: &'static str,
    /// Warum diese Ausnahme vertretbar ist — für Menschen, die den
    /// Gate-Lauf später lesen, nicht nur für den Moment der Eintragung.
    reason: &'static str,
    /// Datum (ISO 8601), seit dem diese Ausnahme gilt.
    since: &'static str,
}

/// Die aktuell geprüften und freigegebenen Ausnahmen — siehe
/// [`CBuildException`] und Moduldoku, Abschnitt „Feature-Auflösung",
/// Unterabschnitt „Was bleibt".
const C_BUILD_EXCEPTIONS: &[CBuildException] = &[CBuildException {
    krate: "blake3",
    tool: "cc",
    reason: "blake3 kompiliert optional C/Assembler-SIMD; kein Laufzeit-Netz/Privileg",
    since: "2026-09-13",
}];

/// Prüft, ob eine gemeldete C-Bau-Werkzeug-Kante (Crate + Werkzeugname)
/// durch [`C_BUILD_EXCEPTIONS`] gedeckt ist.
///
/// # Arguments
/// - `krate` (`&str`): der Name des Crates, das die Kante trägt.
/// - `tool` (`&str`): der Name des C-Bau-Werkzeugs (`"cc"`/`"bindgen"`).
///
/// # Returns
/// `true`, wenn genau dieses Paar in [`C_BUILD_EXCEPTIONS`] eingetragen ist.
///
/// # Errors
/// Keine — totale, panikfreie Funktion.
#[must_use]
fn is_c_build_exception(krate: &str, tool: &str) -> bool {
    C_BUILD_EXCEPTIONS
        .iter()
        .any(|exception| exception.krate == krate && exception.tool == tool)
}

/// Ein einzelner `[[package]]`-Eintrag aus `Cargo.lock`, so weit reduziert,
/// wie diese Datei ihn braucht.
///
/// # Description
/// `harw_code_graph::lockfile::LockedPackage` trägt keine `dependencies`-
/// Liste (sie wird dort nicht gebraucht) — diese Datei parst `Cargo.lock`
/// deshalb selbst noch einmal, minimal, nur für dieses eine zusätzliche
/// Feld. Siehe Moduldoku, Abschnitt „Versions-Eindeutigkeit".
#[derive(Debug, Clone)]
struct LockEntry {
    name: String,
    version: String,
    /// Rohe Abhängigkeits-Einträge, wie Cargo sie schreibt: `"foo"` oder
    /// `"foo 1.2.3"` (Version nur, wenn der Name allein mehrdeutig wäre).
    dependencies: Vec<String>,
}

/// Trennt einen rohen `Cargo.lock`-Abhängigkeitseintrag in Name und
/// optionale Versionsangabe.
///
/// # Arguments
/// - `entry` (`&str`): ein Element aus [`LockEntry::dependencies`].
///
/// # Returns
/// `(name, Some(version))`, wenn Cargo eine Version angehängt hat, sonst
/// `(name, None)`.
///
/// # Errors
/// Keine — totale, panikfreie Funktion.
#[must_use]
fn split_lock_edge(entry: &str) -> (&str, Option<&str>) {
    match entry.split_once(' ') {
        Some((name, version)) => (name, Some(version)),
        None => (entry, None),
    }
}

/// Entfernt umschließende Anführungszeichen und Randleerraum von einem
/// TOML-Wert, wie er nach einem `key = ` in `Cargo.lock` steht.
#[must_use]
fn unquote(raw: &str) -> String {
    raw.trim().trim_matches('"').to_owned()
}

/// Parst `<root>/Cargo.lock` in eine flache Liste von [`LockEntry`].
///
/// # Description
/// Zeilenbasierter Parser für exakt das Teilformat, das Cargo selbst
/// erzeugt: eine Folge von `[[package]]`-Blöcken mit `name = "..."`,
/// `version = "..."` und optional `dependencies = [...]` (ein- oder
/// mehrzeilig). Alle anderen Schlüssel (`source`, `checksum`, …) werden
/// ignoriert. Kein allgemeiner TOML-Parser — siehe Moduldoku.
///
/// # Arguments
/// - `root` (`&Path`): Workspace-Wurzel, in der `Cargo.lock` erwartet wird.
///
/// # Returns
/// Alle im Lockfile gefundenen Paket-Einträge, in Dateireihenfolge.
///
/// # Errors
/// Wenn `Cargo.lock` nicht existiert oder nicht lesbar ist — anders als
/// `harw_code_graph::lockfile::parse_lockfile` liefert diese Funktion dann
/// **keinen** leeren Vektor: für dieses Gate ist ein fehlendes Lockfile ein
/// Zustand, in dem gar nichts geprüft werden kann, kein "nichts zu prüfen".
///
/// # Examples
/// ```text
/// let entries = parse_lock_entries(Path::new("."))?;
/// assert!(entries.iter().any(|e| e.name == "harw-warden"));
/// ```
fn parse_lock_entries(root: &Path) -> Result<Vec<LockEntry>, String> {
    let path = root.join("Cargo.lock");
    let content = fs::read_to_string(&path).map_err(|error| {
        format!(
            "Cargo.lock nicht lesbar unter '{}': {error}",
            path.display()
        )
    })?;

    let mut entries = Vec::new();
    let mut current: Option<LockEntry> = None;
    let mut in_deps_array = false;

    for raw_line in content.lines() {
        let line = raw_line.trim();

        if line == "[[package]]" {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(LockEntry {
                name: String::new(),
                version: String::new(),
                dependencies: Vec::new(),
            });
            in_deps_array = false;
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue;
        };

        if in_deps_array {
            if line == "]" {
                in_deps_array = false;
            } else {
                let item = unquote(line.trim_end_matches(','));
                if !item.is_empty() {
                    entry.dependencies.push(item);
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("name = ") {
            entry.name = unquote(rest);
        } else if let Some(rest) = line.strip_prefix("version = ") {
            entry.version = unquote(rest);
        } else if let Some(rest) = line.strip_prefix("dependencies = [") {
            let rest = rest.trim();
            if let Some(inline) = rest.strip_suffix(']') {
                for item in inline.split(',') {
                    let item = unquote(item);
                    if !item.is_empty() {
                        entry.dependencies.push(item);
                    }
                }
            } else {
                in_deps_array = true;
            }
        }
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }

    Ok(entries)
}

/// Attribute einer einzelnen `[dependencies]`-Deklaration, wie sie das
/// jeweilige Manifest für einen Abhängigkeitsnamen gesetzt hat — Ergänzung
/// zu [`ManifestFacts::normal_dep_names`] für die Feature-Auflösung (siehe
/// Moduldoku, Abschnitt „Feature-Auflösung").
///
/// # Description
/// `optional = true` allein sagt noch nicht, ob die Abhängigkeit aktiviert
/// wird — das hängt vom Feature-Set des *Aufrufers* ab
/// ([`resolve_activated_optional_deps`]). `workspace_inherited` markiert
/// `dep = { workspace = true }` (bzw. die Kurzform `dep.workspace = true`):
/// `default_features`/`features` müssen dann noch mit der Wurzel-
/// `[workspace.dependencies]`-Angabe zusammengeführt werden, siehe
/// [`resolve_workspace_edge`].
#[derive(Debug, Clone)]
struct DependencyEdgeAttrs {
    /// `true`, wenn `optional = true` gesetzt ist.
    optional: bool,
    /// `true`, sofern nicht `default-features = false` gesetzt ist.
    default_features: bool,
    /// Explizit angeforderte Features.
    features: Vec<String>,
    /// `true`, wenn diese Deklaration `workspace = true` trägt.
    workspace_inherited: bool,
}

impl Default for DependencyEdgeAttrs {
    fn default() -> Self {
        Self {
            optional: false,
            default_features: true,
            features: Vec::new(),
            workspace_inherited: false,
        }
    }
}

/// Die für diese beiden Gates relevanten Eigenschaften eines einzelnen
/// Manifests (`Cargo.toml`), intern oder extern.
///
/// # Description
/// Ergebnis von [`parse_manifest_facts`]. Trägt die Proc-Macro-Markierung
/// (Gate 4, Ausschluss), das C-Build-Kennzeichen (Gate 5), die Namen der
/// normalen `[dependencies]` (zum Weiterverfolgen der Hülle) sowie —
/// ergänzt durch die Feature-Auflösungs-Korrektur — die Attribute jeder
/// Abhängigkeit ([`DependencyEdgeAttrs`]) und die eigene `[features]`-
/// Tabelle, beide gebraucht, um zu entscheiden, ob eine *optionale*
/// Abhängigkeit tatsächlich aktiviert wird (siehe Moduldoku, Abschnitt
/// „Feature-Auflösung").
#[derive(Debug, Clone, Default)]
struct ManifestFacts {
    /// `true`, wenn `[lib] proc-macro = true` gesetzt ist.
    is_proc_macro: bool,
    /// `true`, wenn `[package]` einen `links`-Schlüssel trägt.
    has_links: bool,
    /// `true`, wenn `[build-dependencies]` (ggf. unter einem passenden
    /// `[target.'cfg(...)'.build-dependencies]`) `cc` oder `bindgen` nennt.
    build_needs_c_compiler: bool,
    /// Die konkreten Namen aus [`C_BUILD_TOOL_CRATES`], die als
    /// `[build-dependencies]`-Kante gefunden wurden (Teilmenge von
    /// `["cc", "bindgen"]`, je nach Manifest leer, ein- oder zweielementig).
    /// Getrennt von `build_needs_c_compiler` gehalten, damit
    /// [`evaluate_c_build`] eine Ausnahme aus [`C_BUILD_EXCEPTIONS`] auf das
    /// konkrete Werkzeug anwenden kann, statt nur auf das pauschale Signal.
    build_tool_names: Vec<String>,
    /// Namen aller normalen `[dependencies]`, eingeschränkt auf Abschnitte,
    /// deren `cfg(...)` (falls vorhanden) für `x86_64-unknown-linux-gnu`
    /// zutrifft.
    normal_dep_names: Vec<String>,
    /// [`DependencyEdgeAttrs`] je Eintrag aus `normal_dep_names`.
    dependency_attrs: HashMap<String, DependencyEdgeAttrs>,
    /// Die eigene `[features]`-Tabelle: Featurename auf seine rohen
    /// Aktivierungs-Tokens (`"dep:foo"`, `"other-feature"`, `"pkg/feature"`,
    /// `"pkg?/feature"`).
    feature_defs: HashMap<String, Vec<String>>,
}

/// Die fünf Abschnittsarten, die dieser Parser unterscheidet — alles
/// andere (`[package.metadata]`, `[[bin]]`, …) fällt unter
/// [`ManifestSection::Other`] und wird ignoriert.
///
/// `Features` kam mit der Feature-Auflösungs-Korrektur hinzu (siehe
/// Moduldoku, Abschnitt „Feature-Auflösung: eine optionale Abhängigkeit
/// zählt nur, wenn sie aktiviert wird") — vorher wurde `[features]`
/// stillschweigend unter `Other` mitgezählt, weil niemand seinen Inhalt
/// brauchte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestSection {
    Package,
    Lib,
    Dependencies,
    BuildDependencies,
    Features,
    Other,
}

/// Ordnet eine Abschnitts-Kopfzeile (ohne die umschließenden `[`/`]`) einer
/// [`ManifestSection`] zu und liefert, falls der Kopf selbst schon einen
/// Abhängigkeitsnamen trägt (`dependencies.foo`), diesen Namen mit.
///
/// # Arguments
/// - `header` (`&str`): der Inhalt zwischen `[` und `]`, z. B.
///   `"dependencies"`, `"dependencies.foo"`,
///   `"target.'cfg(unix)'.dependencies"`.
///
/// # Returns
/// Die erkannte Abschnittsart und, bei einem Unterabschnitt je Crate, den
/// Namen dieses Crates. Ein `target.'cfg(...)'`-Kopf, dessen Bedingung für
/// `x86_64-unknown-linux-gnu` **nicht** zutrifft, liefert immer
/// `(Other, None)` — siehe [`cfg_predicate_matches_target`].
///
/// # Errors
/// Keine — totale, panikfreie Funktion; ein nicht erkannter Kopf liefert
/// `(Other, None)`.
#[must_use]
fn classify_manifest_section(header: &str) -> (ManifestSection, Option<String>) {
    if let Some(after_target) = header.strip_prefix("target.") {
        let mut chars = after_target.chars();
        let Some(quote) = chars.next().filter(|c| *c == '\'' || *c == '"') else {
            return (ManifestSection::Other, None);
        };
        let Some(end) = after_target[1..].find(quote) else {
            return (ManifestSection::Other, None);
        };
        let predicate = &after_target[1..1 + end];
        let after_predicate = &after_target[1 + end + 1..];
        let remainder = after_predicate.strip_prefix('.').unwrap_or(after_predicate);
        if !cfg_predicate_matches_target(predicate) {
            return (ManifestSection::Other, None);
        }
        return classify_dependency_remainder(remainder);
    }

    if let Some(after_workspace) = header.strip_prefix("workspace.") {
        // `[workspace.dependencies]` / `[workspace.dependencies.foo]` der
        // Wurzel-`Cargo.toml` werden wie ein gewöhnlicher
        // `[dependencies]`-Abschnitt behandelt, damit
        // `parse_workspace_dependencies` denselben Parser wiederverwenden
        // kann — siehe Moduldoku, Abschnitt „Feature-Auflösung".
        if after_workspace == "dependencies" {
            return (ManifestSection::Dependencies, None);
        }
        if let Some(name) = after_workspace.strip_prefix("dependencies.") {
            return (ManifestSection::Dependencies, Some(name.to_owned()));
        }
        return (ManifestSection::Other, None);
    }

    match header {
        "package" => (ManifestSection::Package, None),
        "lib" => (ManifestSection::Lib, None),
        "features" => (ManifestSection::Features, None),
        other => classify_dependency_remainder(other),
    }
}

/// Ordnet den Teil eines Abschnittskopfs nach einem etwaigen
/// `target.'cfg(...)'.`-Präfix (oder den unveränderten Kopf) einer
/// Abhängigkeitsart zu.
///
/// # Arguments
/// - `remainder` (`&str`): z. B. `"dependencies"`, `"dependencies.foo"`,
///   `"build-dependencies"`, `"dev-dependencies"`.
///
/// # Returns
/// Siehe [`classify_manifest_section`].
///
/// # Errors
/// Keine — totale, panikfreie Funktion.
#[must_use]
fn classify_dependency_remainder(remainder: &str) -> (ManifestSection, Option<String>) {
    if remainder == "dependencies" {
        return (ManifestSection::Dependencies, None);
    }
    if let Some(name) = remainder.strip_prefix("dependencies.") {
        return (ManifestSection::Dependencies, Some(name.to_owned()));
    }
    if remainder == "build-dependencies" {
        return (ManifestSection::BuildDependencies, None);
    }
    if let Some(name) = remainder.strip_prefix("build-dependencies.") {
        return (ManifestSection::BuildDependencies, Some(name.to_owned()));
    }
    (ManifestSection::Other, None)
}

/// Liest den Schlüssel vor einem `=` am Zeilenanfang, sofern die Zeile wie
/// eine TOML-Schlüsselzuweisung aussieht.
///
/// # Description
/// Erkennt sowohl `foo = "1.0"` als auch `foo = { version = "1.0" }`
/// (mehrzeilige Fortsetzungen einer solchen Inline-Tabelle beginnen in
/// realen `Cargo.toml`-Dateien praktisch nie erneut mit einem
/// `bezeichner =`-Muster, daher genügt eine reine Zeilenprüfung ohne
/// Klammerzählung). Eine dotted-key-Definition wie `foo.version = "1.0"`
/// wird ebenfalls als Abhängigkeit `foo` erkannt (Punkt zählt nicht als
/// gültiges Namenszeichen, der Teil vor dem ersten Punkt wird genommen).
///
/// # Arguments
/// - `line` (`&str`): eine bereits getrimmte Zeile innerhalb eines
///   Abhängigkeitsabschnitts.
///
/// # Returns
/// `Some(name)`, wenn die Zeile wie eine Schlüsseldefinition aussieht,
/// sonst `None` (z. B. für eine Kommentarzeile oder eine
/// Array-Fortsetzungszeile).
///
/// # Errors
/// Keine — totale, panikfreie Funktion.
#[must_use]
fn extract_leading_key(line: &str) -> Option<String> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let eq_pos = line.find('=')?;
    let key = line[..eq_pos].trim();
    if key.is_empty() {
        return None;
    }
    let first = key.chars().next()?;
    if !(first.is_ascii_alphanumeric() || first == '_') {
        return None;
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return None;
    }
    let base = key.split('.').next().unwrap_or(key);
    Some(base.to_owned())
}

/// Zerlegt eine TOML-Werteliste (Inhalt eines `{ ... }` oder `[ ... ]`, ohne
/// die äußeren Klammern) an Kommas, die nicht innerhalb eines
/// verschachtelten `[...]` liegen.
///
/// # Description
/// Neu mit der Feature-Auflösungs-Korrektur (siehe Moduldoku, Abschnitt
/// „Feature-Auflösung"). Reicht für die in diesem Workspace vorkommenden
/// Inline-Tabellen (z. B. `optional = true, features = ["a", "b"]`) — ein
/// Komma innerhalb der `features`-Liste darf die Inline-Tabelle nicht
/// vorzeitig aufteilen.
#[must_use]
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Wertet ein einzelnes `Schlüssel = Wert`-Paar einer Abhängigkeits-
/// deklaration aus und schreibt es in `attrs`.
///
/// # Description
/// Neu mit der Feature-Auflösungs-Korrektur — gemeinsame Stelle für alle
/// drei Schreibweisen, die eine Abhängigkeitsdeklaration in Cargo haben
/// kann: die Inline-Tabelle (`foo = { optional = true, ... }`), die
/// explizite Tabelle (`[dependencies.foo]` gefolgt von einzelnen Zeilen)
/// und die dotted-key-Kurzform (`foo.workspace = true`).
///
/// # Returns
/// `true`, wenn `value` ein **mehrzeiliges** `features = [` einleitet, das
/// über die Folgezeilen bis zum schließenden `]` weitergelesen werden muss
/// (siehe [`parse_manifest_facts`]); sonst `false`.
fn apply_dependency_attr_kv(attrs: &mut DependencyEdgeAttrs, key: &str, value: &str) -> bool {
    let value = value.trim();
    match key {
        "optional" => {
            attrs.optional = value == "true";
            false
        }
        "default-features" | "default_features" => {
            attrs.default_features = value != "false";
            false
        }
        "workspace" => {
            attrs.workspace_inherited = value == "true";
            false
        }
        "features" => {
            if let Some(inline) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                for item in split_top_level_commas(inline) {
                    let item = unquote(item);
                    if !item.is_empty() {
                        attrs.features.push(item);
                    }
                }
                false
            } else {
                value.starts_with('[')
            }
        }
        _ => false,
    }
}

/// Liest die Wurzel-`[workspace.dependencies]`-Tabelle einmalig ein.
///
/// # Description
/// Neu mit der Feature-Auflösungs-Korrektur (siehe Moduldoku, Abschnitt
/// „Feature-Auflösung"). Syntaktisch ist diese Tabelle ein gewöhnlicher
/// `[dependencies]`-Abschnitt (siehe [`classify_manifest_section`]),
/// deshalb wird [`parse_manifest_facts`] wiederverwendet und nur dessen
/// `dependency_attrs` zurückgegeben — kein zusätzlicher Parser.
///
/// # Arguments
/// - `root` (`&Path`): Workspace-Wurzel, in der `Cargo.toml` erwartet wird.
///
/// # Errors
/// Wenn die Wurzel-`Cargo.toml` nicht lesbar ist.
fn parse_workspace_dependencies(
    root: &Path,
) -> Result<HashMap<String, DependencyEdgeAttrs>, String> {
    let path = root.join("Cargo.toml");
    let content = fs::read_to_string(&path).map_err(|error| {
        format!(
            "Wurzel-Cargo.toml nicht lesbar unter '{}': {error}",
            path.display()
        )
    })?;
    Ok(parse_manifest_facts(&content).dependency_attrs)
}

/// Führt `dep.workspace = true` auf die Wurzel-`[workspace.dependencies]`-
/// Angabe zurück.
///
/// # Description
/// Neu mit der Feature-Auflösungs-Korrektur. Lokale zusätzliche `features`
/// werden **additiv** ergänzt, nicht ersetzt — das entspricht Cargos
/// eigenem Verhalten für vererbte Abhängigkeiten. `optional` bleibt
/// unverändert lokal bestimmt, da `[workspace.dependencies]` selbst kein
/// `optional` setzen kann.
///
/// # Returns
/// Die aufgelösten Attribute sowie, falls kein Wurzel-Eintrag existiert,
/// eine Problem-Meldung (nicht beschaffbare Information, siehe Moduldoku,
/// Abschnitt „Was passiert, wenn eine Information fehlt") statt einer
/// stillschweigenden Default-Annahme.
fn resolve_workspace_edge(
    name: &str,
    attrs: &DependencyEdgeAttrs,
    workspace_deps: &HashMap<String, DependencyEdgeAttrs>,
) -> (DependencyEdgeAttrs, Option<String>) {
    if !attrs.workspace_inherited {
        return (attrs.clone(), None);
    }
    match workspace_deps.get(name) {
        Some(base) => {
            let mut merged = base.clone();
            merged.optional = attrs.optional;
            for feature in &attrs.features {
                if !merged.features.contains(feature) {
                    merged.features.push(feature.clone());
                }
            }
            (merged, None)
        }
        None => (
            attrs.clone(),
            Some(format!(
                "Abhängigkeit '{name}' referenziert 'workspace = true', aber '[workspace.dependencies.{name}]' wurde in der Wurzel-Cargo.toml nicht gefunden"
            )),
        ),
    }
}

/// Die `[workspace.dependencies]`-Tabellen aller geladenen Workspaces, je
/// mit ihrer Wurzel.
///
/// # Description
/// Entstanden, als DoD ein eigener Workspace war: ein Crate unter `dod/`
/// erbte seine `workspace = true`-Angaben aus `dod/Cargo.toml`, ein
/// Produkt-Crate aus der Wurzel-`Cargo.toml`. Seit PL-60 gibt es nur noch
/// die Wurzel; [`load_and_compute`] lädt deshalb genau eine Tabelle. Die
/// Struktur bleibt mehrwurzelig, weil sie korrekt bleibt, falls je wieder
/// ein verschachtelter Workspace entsteht: je Elternteil wird die Tabelle
/// des Workspace gewählt, in dessen Verzeichnis sein Manifest liegt
/// ([`Self::for_crate`]) — eine einzige Tabelle für mehrere Workspaces
/// würde eine fehlende Angabe verdecken oder eine fremde unterschieben.
#[derive(Debug, Default)]
struct WorkspaceDependencyTables {
    /// `(Workspace-Wurzel, Tabelle)`, die tiefste Wurzel zuerst, damit
    /// `dod/` vor `.` gewinnt.
    tables: Vec<(std::path::PathBuf, HashMap<String, DependencyEdgeAttrs>)>,
    /// Leere Tabelle für Elternteile außerhalb jedes geladenen Workspace
    /// (Registry-Crates). Deren veröffentlichte Manifeste sind von Cargo
    /// normalisiert und tragen kein `workspace = true`; tritt es doch auf,
    /// meldet [`resolve_workspace_edge`] den fehlenden Eintrag als Problem.
    empty: HashMap<String, DependencyEdgeAttrs>,
}

impl WorkspaceDependencyTables {
    /// Liest `[workspace.dependencies]` aus jeder Wurzel in `roots`.
    ///
    /// # Errors
    /// Wenn eine der Wurzel-`Cargo.toml`-Dateien nicht lesbar ist.
    fn load(roots: &[&Path]) -> Result<Self, String> {
        let mut tables = Vec::with_capacity(roots.len());
        for root in roots {
            tables.push((root.to_path_buf(), parse_workspace_dependencies(root)?));
        }
        tables.sort_by_key(|(root, _)| std::cmp::Reverse(root.components().count()));
        Ok(Self {
            tables,
            empty: HashMap::new(),
        })
    }

    /// Die Tabelle, gegen die `workspace = true`-Kanten von `parent`
    /// aufgelöst werden: die des Workspace, unter dessen Wurzel das
    /// Crate-Verzeichnis liegt; für Crates außerhalb des Graphen die leere
    /// Tabelle.
    fn for_crate(
        &self,
        graph: &WorkspaceGraph,
        parent: &str,
    ) -> &HashMap<String, DependencyEdgeAttrs> {
        let Some(node) = graph.get(parent) else {
            return &self.empty;
        };
        self.tables
            .iter()
            .find(|(root, _)| node.dir.starts_with(root))
            .map_or(&self.empty, |(_, table)| table)
    }
}

/// Ergänzt implizite Features für nicht namensraum-gebundene optionale
/// Abhängigkeiten zur `[features]`-Tabelle eines Manifests.
///
/// # Description
/// Neu mit der Feature-Auflösungs-Korrektur (siehe Moduldoku, Abschnitt
/// „Feature-Auflösung"). Cargo erzeugt für jede optionale Abhängigkeit
/// automatisch ein Feature desselben Namens (`optional = true` ⇒ Feature
/// `foo` aktiviert `dep:foo`) — **außer**, irgendeine Feature-Definition
/// des Manifests referenziert die Abhängigkeit bereits explizit über
/// `dep:foo` (namensraum-gebundene Syntax); dann existiert das implizite
/// Feature nicht. `jiff`s `defmt = ["dep:defmt"]` ist genau dieser Fall.
#[must_use]
fn effective_feature_defs(facts: &ManifestFacts) -> HashMap<String, Vec<String>> {
    let mut namespaced: HashSet<String> = HashSet::new();
    for values in facts.feature_defs.values() {
        for token in values {
            if let Some(dep_name) = token.strip_prefix("dep:") {
                namespaced.insert(dep_name.to_owned());
            }
        }
    }

    let mut effective = facts.feature_defs.clone();
    for (name, attrs) in &facts.dependency_attrs {
        if attrs.optional && !namespaced.contains(name) && !effective.contains_key(name) {
            effective.insert(name.clone(), vec![format!("dep:{name}")]);
        }
    }
    effective
}

/// Berechnet, welche *optionalen* Abhängigkeiten eines Manifests bei einem
/// gegebenen Feature-Set tatsächlich aktiviert werden.
///
/// # Description
/// Neu mit der Feature-Auflösungs-Korrektur — Kernstück der Korrektur,
/// siehe Moduldoku, Abschnitt „Feature-Auflösung". Ein Fixpunkt läuft über
/// [`effective_feature_defs`]: ausgehend von `default` (falls
/// `default_enabled`) und den explizit angeforderten Features wird jedes
/// aktive Feature expandiert — `dep:name` aktiviert die Abhängigkeit
/// `name`, `pkg/feature` aktiviert `pkg` (die konkrete `feature`-
/// Weiterleitung in `pkg` hinein wird nicht tiefer aufgelöst — siehe
/// Moduldoku, Abschnitt „Feature-Auflösung", Unterabschnitt „Grenzen"),
/// `pkg?/feature` ist eine **schwache** Referenz und aktiviert `pkg`
/// **nicht**, ein einfacher Name wird als weiteres Feature in die
/// Warteschlange gestellt. Die Verwendung eines `HashSet` als "bereits
/// gesehen"-Menge macht die Schleife terminierend, auch wenn sich Features
/// gegenseitig referenzieren.
///
/// # Returns
/// Die Namen aller Abhängigkeiten, die unter diesem Feature-Set tatsächlich
/// gebaut werden (optional wie nicht-optional — der Aufrufer interessiert
/// sich nur für die optionalen darunter).
#[must_use]
fn resolve_activated_optional_deps(
    facts: &ManifestFacts,
    requested_features: &HashSet<String>,
    default_enabled: bool,
) -> HashSet<String> {
    let effective = effective_feature_defs(facts);
    let mut active: HashSet<String> = HashSet::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = Vec::new();
    if default_enabled {
        queue.push("default".to_owned());
    }
    queue.extend(requested_features.iter().cloned());

    while let Some(feature) = queue.pop() {
        if !seen.insert(feature.clone()) {
            continue;
        }
        let Some(tokens) = effective.get(&feature) else {
            continue;
        };
        for token in tokens {
            if let Some(dep_name) = token.strip_prefix("dep:") {
                active.insert(dep_name.to_owned());
            } else if let Some((pkg, feat_part)) = token.split_once('/') {
                let is_weak = pkg.ends_with('?');
                let pkg_name = pkg.strip_suffix('?').unwrap_or(pkg);
                if !is_weak {
                    active.insert(pkg_name.to_owned());
                }
                let _ = feat_part;
            } else {
                queue.push(token.clone());
            }
        }
    }
    active
}

/// Parst ein `Cargo.toml`-Manifest (Inhalt bereits als Zeichenkette
/// gelesen) in seine für diese beiden Gates relevanten [`ManifestFacts`].
///
/// # Description
/// Zeilenbasiert, Abschnitt für Abschnitt — siehe Moduldoku, Abschnitt
/// „Wie die Hülle gezählt wird". Ein Umbenennungs-Alias
/// (`foo = { package = "wirklicher-name", version = "1" }`) wird **nicht**
/// aufgelöst: der hier extrahierte Name ist der lokale Schlüssel `foo`,
/// nicht `wirklicher-name`. Das ist eine bekannte, dokumentierte Lücke
/// dieses Parsers (siehe Abschlussbericht) — Umbenennungen sind unter den
/// in der Warden-Hülle beobachteten Crates nicht aufgetreten.
///
/// Seit der Feature-Auflösungs-Korrektur (siehe Moduldoku, Abschnitt
/// „Feature-Auflösung") erkennt diese Funktion zusätzlich `optional`,
/// `default-features`/`default_features`, `features` und `workspace` je
/// `[dependencies]`-Eintrag ([`DependencyEdgeAttrs`], in
/// [`ManifestFacts::dependency_attrs`]) sowie die volle `[features]`-
/// Tabelle des Manifests ([`ManifestFacts::feature_defs`]). Dieselbe
/// Funktion liest damit auch die Wurzel-`[workspace.dependencies]`-Tabelle
/// (siehe [`parse_workspace_dependencies`]), da sie syntaktisch identisch
/// zu einem `[dependencies]`-Abschnitt behandelt wird.
///
/// # Arguments
/// - `content` (`&str`): der vollständige Inhalt eines `Cargo.toml`.
///
/// # Returns
/// Die extrahierten [`ManifestFacts`].
///
/// # Errors
/// Keine — totale, panikfreie Funktion; ein nicht erkanntes Konstrukt wird
/// stillschweigend ignoriert (kein Abbruch), das begrenzt aber nur die
/// Präzision, nie die Sicherheit in Richtung "zu wenig geprüft, als grün
/// gemeldet" — ein nicht erkannter `links`- oder `cc`/`bindgen`-Eintrag
/// würde diesen Parser zu lax machen; dem wird durch die enge, wörtliche
/// Erkennung dieser beiden Schlüssel begegnet.
#[must_use]
fn parse_manifest_facts(content: &str) -> ManifestFacts {
    let mut facts = ManifestFacts::default();
    let mut section = ManifestSection::Other;
    // Ab hier neu mit der Feature-Auflösungs-Korrektur (siehe Moduldoku,
    // Abschnitt „Feature-Auflösung"): `current_dep_name` verfolgt, ob wir
    // uns innerhalb eines `[dependencies.foo]`-Unterabschnitts befinden
    // (dessen Folgezeilen `optional`/`features`/... statt weiterer
    // Abhängigkeitsnamen sind), `pending_array` sammelt ein mehrzeiliges
    // `features = [` (unter einer Abhängigkeit oder unter `[features]`)
    // über mehrere Zeilen ein.
    let mut current_dep_name: Option<String> = None;
    let mut pending_array: Option<(bool, String)> = None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((is_dependency_features, name)) = pending_array.clone() {
            if line == "]" {
                pending_array = None;
            } else {
                let item = unquote(line.trim_end_matches(','));
                if !item.is_empty() {
                    if is_dependency_features {
                        facts
                            .dependency_attrs
                            .entry(name)
                            .or_default()
                            .features
                            .push(item);
                    } else {
                        facts.feature_defs.entry(name).or_default().push(item);
                    }
                }
            }
            continue;
        }

        if line.starts_with('[') {
            let header = line.trim_start_matches('[').trim_end_matches(']');
            let (new_section, immediate_key) = classify_manifest_section(header);
            section = new_section;
            current_dep_name = None;
            if let Some(key) = immediate_key {
                match section {
                    ManifestSection::Dependencies => {
                        facts.normal_dep_names.push(key.clone());
                        facts.dependency_attrs.entry(key.clone()).or_default();
                        current_dep_name = Some(key);
                    }
                    ManifestSection::BuildDependencies => {
                        if C_BUILD_TOOL_CRATES.contains(&key.as_str()) {
                            facts.build_needs_c_compiler = true;
                            facts.build_tool_names.push(key.clone());
                        }
                    }
                    ManifestSection::Package
                    | ManifestSection::Lib
                    | ManifestSection::Features
                    | ManifestSection::Other => {}
                }
            }
            continue;
        }

        match section {
            ManifestSection::Lib => {
                if let Some(rest) = line.strip_prefix("proc-macro") {
                    if let Some(value) = rest.trim_start().strip_prefix('=') {
                        if value.trim() == "true" {
                            facts.is_proc_macro = true;
                        }
                    }
                }
            }
            ManifestSection::Package => {
                if let Some(rest) = line.strip_prefix("links") {
                    if rest.trim_start().starts_with('=') {
                        facts.has_links = true;
                    }
                }
            }
            ManifestSection::Dependencies => {
                if let Some(dep_name) = current_dep_name.clone() {
                    // Innerhalb eines `[dependencies.foo]`-Unterabschnitts:
                    // jede Zeile ist ein Attribut von `foo`, kein weiterer
                    // Abhängigkeitsname (das war vor dieser Korrektur ein
                    // latenter Fehler — `version`/`optional` wurden hier
                    // ebenfalls als Abhängigkeitsnamen aufgenommen, siehe
                    // Abschlussbericht).
                    if let Some(eq_pos) = line.find('=') {
                        let key = line[..eq_pos].trim();
                        let value = &line[eq_pos + 1..];
                        let attrs = facts.dependency_attrs.entry(dep_name.clone()).or_default();
                        if apply_dependency_attr_kv(attrs, key, value) {
                            pending_array = Some((true, dep_name));
                        }
                    }
                    continue;
                }
                if let Some(name) = extract_leading_key(line) {
                    facts.normal_dep_names.push(name);
                }
                // Neu mit der Feature-Auflösungs-Korrektur: zusätzlich zum
                // Basisnamen (oben, unverändert) werden hier auch
                // `optional`/`default-features`/`features`/`workspace`
                // erkannt — sowohl aus einer Inline-Tabelle
                // (`foo = { optional = true }`) als auch aus der
                // dotted-key-Kurzform (`foo.workspace = true`).
                if let Some(eq_pos) = line.find('=') {
                    let raw_key = line[..eq_pos].trim();
                    let value = line[eq_pos + 1..].trim();
                    if let Some(first) = raw_key.chars().next() {
                        if first.is_ascii_alphanumeric() || first == '_' {
                            let mut parts = raw_key.splitn(2, '.');
                            let base = parts.next().unwrap_or(raw_key).to_owned();
                            match parts.next() {
                                Some(sub_key) => {
                                    let attrs =
                                        facts.dependency_attrs.entry(base.clone()).or_default();
                                    if apply_dependency_attr_kv(attrs, sub_key, value) {
                                        pending_array = Some((true, base));
                                    }
                                }
                                None => {
                                    // Neu (Nachkorrektur): der Eintrag wird
                                    // jetzt IMMER angelegt, auch für die
                                    // reine Zeichenketten-Schreibweise
                                    // (`foo = "1.2.3"`, ohne `{...}`) — vorher
                                    // fehlte er für diese Form komplett, was
                                    // `parse_workspace_dependencies` blind für
                                    // genau die Wurzel-Einträge machte, die
                                    // keine Inline-Tabelle sind (`blake3`,
                                    // `serde_json`, `tracing`, `rustix`; siehe
                                    // Abschlussbericht). Der reine
                                    // Default-Eintrag (nicht optional,
                                    // Default-Features an, keine Features)
                                    // ist für diese Form korrekt.
                                    let attrs =
                                        facts.dependency_attrs.entry(base.clone()).or_default();
                                    if let Some(inner) =
                                        value.strip_prefix('{').and_then(|v| v.strip_suffix('}'))
                                    {
                                        for part in split_top_level_commas(inner) {
                                            if let Some(part_eq) = part.find('=') {
                                                let k = part[..part_eq].trim();
                                                let v = &part[part_eq + 1..];
                                                apply_dependency_attr_kv(attrs, k, v);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ManifestSection::BuildDependencies => {
                if let Some(name) = extract_leading_key(line) {
                    if C_BUILD_TOOL_CRATES.contains(&name.as_str()) {
                        facts.build_needs_c_compiler = true;
                        facts.build_tool_names.push(name);
                    }
                }
            }
            ManifestSection::Features => {
                if let Some(eq_pos) = line.find('=') {
                    let key = line[..eq_pos].trim();
                    let value = line[eq_pos + 1..].trim();
                    let is_valid_key = key
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                        && key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                    if is_valid_key {
                        if let Some(inline) =
                            value.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
                        {
                            let entry = facts.feature_defs.entry(key.to_owned()).or_default();
                            for item in split_top_level_commas(inline) {
                                let item = unquote(item);
                                if !item.is_empty() {
                                    entry.push(item);
                                }
                            }
                        } else if value.starts_with('[') {
                            facts.feature_defs.entry(key.to_owned()).or_default();
                            pending_array = Some((false, key.to_owned()));
                        }
                    }
                }
            }
            ManifestSection::Other => {}
        }
    }

    facts
}

/// Ein einzelner Knoten im rekursiven Abstieg eines `cfg(...)`-Ausdrucks.
#[derive(Debug, Clone)]
enum CfgExpr {
    Flag(String),
    Eq(String, String),
    Any(Vec<CfgExpr>),
    All(Vec<CfgExpr>),
    Not(Box<CfgExpr>),
}

/// Zerlegt den Innenteil eines `cfg(...)`-Ausdrucks in Token.
///
/// # Description
/// Erkennt Bezeichner, Zeichenketten (`"linux"`), sowie `(`, `)`, `,`, `=`.
/// Reiner Tokenizer ohne Semantik — die eigentliche Struktur baut
/// [`parse_cfg_expr`].
#[must_use]
fn tokenize_cfg(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '(' | ')' | ',' | '=' => {
                tokens.push(c.to_string());
                chars.next();
            }
            '"' => {
                chars.next();
                let mut buf = String::new();
                for ch in chars.by_ref() {
                    if ch == '"' {
                        break;
                    }
                    buf.push(ch);
                }
                tokens.push(format!("\"{buf}\""));
            }
            _ => {
                let mut buf = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_alphanumeric() || ch == '_' {
                        buf.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if buf.is_empty() {
                    chars.next();
                } else {
                    tokens.push(buf);
                }
            }
        }
    }
    tokens
}

/// Baut einen [`CfgExpr`]-Baum aus einer Token-Liste, beginnend bei `*pos`.
///
/// # Errors
/// `None`, wenn die Token-Folge kein gültiger `cfg(...)`-Ausdruck ist —
/// der Aufrufer behandelt das konservativ als "trifft nicht zu" (siehe
/// [`cfg_predicate_matches_target`]).
fn parse_cfg_expr(tokens: &[String], pos: &mut usize) -> Option<CfgExpr> {
    let name = tokens.get(*pos)?.clone();
    *pos += 1;

    if tokens.get(*pos).map(String::as_str) == Some("(") {
        *pos += 1;
        let mut args = Vec::new();
        loop {
            if tokens.get(*pos).map(String::as_str) == Some(")") {
                *pos += 1;
                break;
            }
            args.push(parse_cfg_expr(tokens, pos)?);
            if tokens.get(*pos).map(String::as_str) == Some(",") {
                *pos += 1;
            }
        }
        match name.as_str() {
            "any" => Some(CfgExpr::Any(args)),
            "all" => Some(CfgExpr::All(args)),
            "not" => args.into_iter().next().map(|c| CfgExpr::Not(Box::new(c))),
            _ => None,
        }
    } else if tokens.get(*pos).map(String::as_str) == Some("=") {
        *pos += 1;
        let value = tokens.get(*pos)?.trim_matches('"').to_owned();
        *pos += 1;
        Some(CfgExpr::Eq(name, value))
    } else {
        Some(CfgExpr::Flag(name))
    }
}

/// Liefert den Wert eines `target_*`-Schlüssels für das tatsächliche
/// Zielsystem dieses Workspace.
///
/// # Description
/// Fest verdrahtet auf `x86_64-unknown-linux-gnu` — dasselbe Ziel, für das
/// `harw-warden` laut seinem `Cargo.toml` tatsächlich gebaut wird
/// (`rustix`-Feature `net`, `landlock`, `sd-listen-fds`: alles
/// Linux-spezifisch). Ein Mehrplattform-Ziel-Support ist für dieses Gate
/// nicht vorgesehen.
#[must_use]
fn target_fact(key: &str) -> Option<&'static str> {
    match key {
        "target_os" => Some("linux"),
        "target_family" => Some("unix"),
        "target_arch" => Some("x86_64"),
        "target_env" => Some("gnu"),
        "target_pointer_width" => Some("64"),
        "target_endian" => Some("little"),
        "target_vendor" => Some("unknown"),
        _ => None,
    }
}

/// Ob `target_has_atomic = "<breite>"` auf dem Zielsystem gilt.
///
/// # Description
/// `target_has_atomic` ist ein mehrwertiger Schlüssel: auf
/// `x86_64-unknown-linux-gnu` gelten `8`, `16`, `32`, `64` und `ptr`
/// gleichzeitig, `128` nicht. Ohne diese Auswertung gälte etwa
/// `cfg(not(target_has_atomic = "ptr"))` (jiffs Abhängigkeit auf
/// `portable-atomic`) fälschlich als aktiv, und das Gate suchte Quellen von
/// Crates, die auf diesem Ziel nie gebaut und deshalb auf frischen
/// CI-Runnern auch nie entpackt werden.
#[must_use]
fn target_has_atomic(width: &str) -> bool {
    matches!(width, "8" | "16" | "32" | "64" | "ptr")
}

/// Wertet einen geparsten `cfg(...)`-Ausdruck gegen das Zielsystem aus
/// (siehe [`target_fact`]).
///
/// # Description
/// `unix` ist das einzige nackte Flag, das auf diesem Ziel zutrifft;
/// `windows` und alle anderen nackten Flags gelten als falsch. Ein
/// unbekannter `target_*`-Schlüssel gilt ebenfalls als falsch — konservativ
/// im Sinne von „nicht in die Hülle aufnehmen, wenn unklar", siehe
/// Moduldoku, Abschnitt „Wie die Hülle gezählt wird".
#[must_use]
fn eval_cfg(expr: &CfgExpr) -> bool {
    match expr {
        CfgExpr::Flag(name) => name == "unix",
        CfgExpr::Eq(key, value) if key == "target_has_atomic" => target_has_atomic(value),
        CfgExpr::Eq(key, value) => target_fact(key) == Some(value.as_str()),
        CfgExpr::Any(items) => items.iter().any(eval_cfg),
        CfgExpr::All(items) => items.iter().all(eval_cfg),
        CfgExpr::Not(inner) => !eval_cfg(inner),
    }
}

/// Prüft, ob ein `target.'<predicate>'`-Prädikat aus einem `Cargo.toml` für
/// das Zielsystem dieses Workspace zutrifft.
///
/// # Arguments
/// - `predicate` (`&str`): der Inhalt zwischen den Anführungszeichen, z. B.
///   `"cfg(unix)"` oder eine rohe Ziel-Tripel-Zeichenkette wie
///   `"x86_64-unknown-linux-gnu"`.
///
/// # Returns
/// `true`, wenn der Abschnitt auf `x86_64-unknown-linux-gnu` aktiv wäre.
/// Ein nicht parsbares `cfg(...)` gilt als `false` — siehe [`eval_cfg`].
///
/// # Errors
/// Keine — totale, panikfreie Funktion.
#[must_use]
fn cfg_predicate_matches_target(predicate: &str) -> bool {
    let predicate = predicate.trim();
    if let Some(inner) = predicate
        .strip_prefix("cfg(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let tokens = tokenize_cfg(inner);
        let mut pos = 0usize;
        parse_cfg_expr(&tokens, &mut pos)
            .map(|expr| eval_cfg(&expr))
            .unwrap_or(false)
    } else {
        predicate == "x86_64-unknown-linux-gnu"
    }
}

/// Das Ergebnis einer einmaligen Traversierung der Warden-Laufzeit-Hülle —
/// Grundlage für beide Gate-Berichte (siehe Moduldoku, Abschnitt „Warum
/// dieselbe Hülle für beide Gates").
#[derive(Debug, Clone, Default)]
struct ClosureResult {
    /// Interne (Workspace-)Crates in der Hülle, ohne die Wurzel selbst und
    /// ohne Proc-Macros.
    internal: Vec<String>,
    /// Externe Crates in der Hülle, ohne Proc-Macros.
    external: Vec<String>,
    /// [`ManifestFacts`] je erreichtem Crate (intern wie extern), für
    /// Gate 5. Schlüssel ist der Crate-Name.
    facts: HashMap<String, ManifestFacts>,
    /// Nicht auflösbare Kanten oder fehlende Information — siehe Moduldoku,
    /// Abschnitt „Was passiert, wenn eine Information fehlt". Jeder Eintrag
    /// ist sowohl ein Verstoß als auch ein geprüfter (aber gescheiterter)
    /// Kandidat.
    problems: Vec<String>,
}

impl ClosureResult {
    /// Die Gesamtzahl der Crates in der Hülle (intern + extern), ohne die
    /// nicht auflösbaren Kanten aus [`Self::problems`].
    #[must_use]
    fn total_resolved(&self) -> usize {
        self.internal.len() + self.external.len()
    }

    /// Wie viele Kandidaten insgesamt tatsächlich geprüft wurden — inklusive
    /// der nicht auflösbaren, die als Verstoß zählen, aber eben doch ein
    /// Prüfversuch waren. Siehe `xtask/src/gates.rs`-Moduldoku zu `checked`.
    #[must_use]
    fn checked(&self) -> usize {
        self.total_resolved() + self.problems.len()
    }
}

/// Baut die Traversierungs-Zustände für [`compute_closure`] und läuft die
/// Tiefensuche über beide Ebenen (Workspace-intern über [`WorkspaceGraph`],
/// extern über [`LockEntry`]/[`RegistrySourceLocator`]).
///
/// Seit der Feature-Auflösungs-Korrektur (siehe Moduldoku, Abschnitt
/// „Feature-Auflösung") führt [`compute_closure`] diese Traversierung
/// **mehrfach** aus, bis die je Crate angeforderten Features
/// (`prev_requested_features`/`prev_default_enabled`, aus dem *vorigen*
/// Durchlauf) sich nicht mehr ändern (`next_requested_features`/
/// `next_default_enabled`, gesammelt für den *nächsten* Durchlauf) — ein
/// Fixpunkt über der bestehenden Traversierung, keine neue Traversierung
/// daneben. Eine optionale Abhängigkeit wird beim Entscheiden, ob eine
/// Kante gefolgt wird, nur dann besucht, wenn das aktuell bekannte
/// Feature-Set ihres Elternteils sie tatsächlich aktiviert — siehe
/// [`Self::maybe_follow_edge`].
struct ClosureBuilder<'a> {
    graph: &'a WorkspaceGraph,
    lock_by_name: HashMap<&'a str, Vec<&'a LockEntry>>,
    lock_by_key: HashMap<(String, String), &'a LockEntry>,
    locator: &'a RegistrySourceLocator,
    workspace_deps: &'a WorkspaceDependencyTables,
    /// Je Crate-Name angeforderte Features / `default-features`-Stand aus
    /// dem *vorigen* Durchlauf von [`compute_closure`]s Fixpunkt-Schleife —
    /// read-only während dieses Durchlaufs.
    prev_requested_features: &'a HashMap<String, HashSet<String>>,
    prev_default_enabled: &'a HashMap<String, bool>,
    /// Dasselbe, aber für den *nächsten* Durchlauf gesammelt — additiv über
    /// alle in diesem Durchlauf gefolgten Kanten (siehe
    /// [`Self::maybe_follow_edge`]).
    next_requested_features: HashMap<String, HashSet<String>>,
    next_default_enabled: HashMap<String, bool>,
    visited_internal: HashSet<String>,
    visited_external: HashSet<(String, String)>,
    result: ClosureResult,
}

impl<'a> ClosureBuilder<'a> {
    fn new(
        graph: &'a WorkspaceGraph,
        lock: &'a [LockEntry],
        locator: &'a RegistrySourceLocator,
        workspace_deps: &'a WorkspaceDependencyTables,
        prev_requested_features: &'a HashMap<String, HashSet<String>>,
        prev_default_enabled: &'a HashMap<String, bool>,
    ) -> Self {
        let mut lock_by_name: HashMap<&'a str, Vec<&'a LockEntry>> = HashMap::new();
        let mut lock_by_key: HashMap<(String, String), &'a LockEntry> = HashMap::new();
        for entry in lock {
            lock_by_name
                .entry(entry.name.as_str())
                .or_default()
                .push(entry);
            lock_by_key.insert((entry.name.clone(), entry.version.clone()), entry);
        }
        Self {
            graph,
            lock_by_name,
            lock_by_key,
            locator,
            workspace_deps,
            prev_requested_features,
            prev_default_enabled,
            next_requested_features: HashMap::new(),
            next_default_enabled: HashMap::new(),
            visited_internal: HashSet::new(),
            visited_external: HashSet::new(),
            result: ClosureResult::default(),
        }
    }

    /// Entscheidet, ob eine über `Cargo.lock` bestätigte
    /// `[dependencies]`-Kante von `parent_name` zu `dep_name` in diesem
    /// Durchlauf gefolgt wird, und pflegt dabei
    /// `next_requested_features`/`next_default_enabled` für den nächsten
    /// Durchlauf.
    ///
    /// # Description
    /// Neu mit der Feature-Auflösungs-Korrektur — der einzige Ort, an dem
    /// die alte, bedingungslose `self.visit(dep_name, dep_version)`-Kante
    /// jetzt durch eine Aktivierungsprüfung läuft (siehe Moduldoku,
    /// Abschnitt „Feature-Auflösung"). `dep_name = { workspace = true }`
    /// wird zuerst über [`resolve_workspace_edge`] aufgelöst. Ist die Kante
    /// **optional**, wird sie nur gefolgt, wenn
    /// [`resolve_activated_optional_deps`] — angewandt auf `parent_facts`
    /// und das aus dem *vorigen* Durchlauf bekannte Feature-Set von
    /// `parent_name` — `dep_name` tatsächlich aktiviert. Ist die Kante
    /// nicht optional (oder aktiviert), werden ihre eigenen
    /// `features`/`default-features` additiv in das Feature-Set von
    /// `dep_name` für den *nächsten* Durchlauf übernommen — genau das
    /// macht die Aktivierung transitiv: `jiff`s eigene `features =
    /// ["serde"]`-Kante wird so im nächsten Durchlauf zur Grundlage dafür,
    /// welche von `jiff`s **eigenen** optionalen Abhängigkeiten (`defmt`,
    /// `jiff-static`, `log`) aktiviert sind.
    fn maybe_follow_edge(
        &mut self,
        parent_name: &str,
        parent_facts: &ManifestFacts,
        dep_name: &str,
        dep_version: Option<&str>,
    ) {
        let raw_attrs = parent_facts
            .dependency_attrs
            .get(dep_name)
            .cloned()
            .unwrap_or_default();
        let table = self.workspace_deps.for_crate(self.graph, parent_name);
        let (attrs, problem) = resolve_workspace_edge(dep_name, &raw_attrs, table);
        if let Some(msg) = problem {
            self.result.problems.push(msg);
        }

        if attrs.optional {
            let parent_requested = self
                .prev_requested_features
                .get(parent_name)
                .cloned()
                .unwrap_or_default();
            let parent_default = *self.prev_default_enabled.get(parent_name).unwrap_or(&false);
            let activated =
                resolve_activated_optional_deps(parent_facts, &parent_requested, parent_default);
            if !activated.contains(dep_name) {
                return;
            }
        }

        let child_req = self
            .next_requested_features
            .entry(dep_name.to_owned())
            .or_default();
        for feature in &attrs.features {
            child_req.insert(feature.clone());
        }
        let child_def = self
            .next_default_enabled
            .entry(dep_name.to_owned())
            .or_insert(false);
        if attrs.default_features {
            *child_def = true;
        }

        self.visit(dep_name, dep_version);
    }

    /// Startet die Traversierung an der Wurzel: verfolgt ihre Kanten, ohne
    /// die Wurzel selbst der Hülle hinzuzufügen (siehe Moduldoku, Abschnitt
    /// „Wie die Hülle gezählt wird"). Liest zusätzlich das eigene Manifest
    /// der Wurzel (neu mit der Feature-Auflösungs-Korrektur), um die
    /// `optional`/`features`-Attribute ihrer Kanten zu kennen — vorher
    /// wurde die Wurzel selbst nie geparst, weil sie ohnehin nicht gezählt
    /// wird.
    fn run_from_root(&mut self, root: &str) {
        let Some(node) = self.graph.get(root) else {
            self.result.problems.push(format!(
                "Wurzel-Crate '{root}' nicht im Workspace-Graphen gefunden"
            ));
            return;
        };
        self.visited_internal.insert(root.to_owned());

        let root_facts = match fs::read_to_string(&node.manifest_path) {
            Ok(content) => parse_manifest_facts(&content),
            Err(error) => {
                self.result.problems.push(format!(
                    "Manifest der Wurzel '{root}' unter '{}' nicht lesbar: {error}",
                    node.manifest_path.display()
                ));
                return;
            }
        };

        let normal_names: HashSet<&str> = node
            .deps
            .iter()
            .map(String::as_str)
            .chain(node.external_deps.iter().map(String::as_str))
            .collect();

        match self.lock_by_name.get(root).and_then(|v| v.first()) {
            Some(own) => {
                let edges: Vec<String> = own.dependencies.clone();
                for edge in &edges {
                    let (dep_name, dep_version) = split_lock_edge(edge);
                    if !normal_names.contains(dep_name) {
                        continue;
                    }
                    self.maybe_follow_edge(root, &root_facts, dep_name, dep_version);
                }
            }
            None => {
                for dep in &node.deps {
                    self.maybe_follow_edge(root, &root_facts, dep, None);
                }
                for dep in &node.external_deps {
                    self.maybe_follow_edge(root, &root_facts, dep, None);
                }
            }
        }
    }

    /// Verzweigt an Hand des Namens auf [`Self::visit_internal`] oder
    /// [`Self::visit_external`].
    fn visit(&mut self, name: &str, version: Option<&str>) {
        if self.graph.get(name).is_some() {
            self.visit_internal(name, version);
        } else {
            self.visit_external(name, version);
        }
    }

    fn visit_internal(&mut self, name: &str, _version: Option<&str>) {
        if self.visited_internal.contains(name) {
            return;
        }
        self.visited_internal.insert(name.to_owned());

        let Some(node) = self.graph.get(name) else {
            self.result.problems.push(format!(
                "internes Crate '{name}' nicht im Workspace-Graphen gefunden"
            ));
            return;
        };

        let content = match fs::read_to_string(&node.manifest_path) {
            Ok(c) => c,
            Err(error) => {
                self.result.problems.push(format!(
                    "Manifest von '{name}' unter '{}' nicht lesbar: {error}",
                    node.manifest_path.display()
                ));
                return;
            }
        };
        let facts = parse_manifest_facts(&content);
        if facts.is_proc_macro {
            return;
        }
        self.result.internal.push(name.to_owned());
        self.result.facts.insert(name.to_owned(), facts.clone());

        self.follow_own_lock_edges(name, node, &facts);
    }

    /// Folgt den Kanten eines bereits als Nicht-Proc-Macro erkannten
    /// Crates weiter, über dessen eigenen `Cargo.lock`-Eintrag versions-
    /// disambiguiert (siehe Moduldoku). `facts` sind die bereits geparsten
    /// [`ManifestFacts`] dieses Crates — gebraucht, damit
    /// [`Self::maybe_follow_edge`] seine `optional`/`features`-Attribute
    /// kennt (neu mit der Feature-Auflösungs-Korrektur).
    fn follow_own_lock_edges(&mut self, name: &str, node: &CrateNode, facts: &ManifestFacts) {
        let normal_names: HashSet<&str> = node
            .deps
            .iter()
            .map(String::as_str)
            .chain(node.external_deps.iter().map(String::as_str))
            .collect();

        match self.lock_by_name.get(name).and_then(|v| v.first()) {
            Some(own) => {
                let edges: Vec<String> = own.dependencies.clone();
                for edge in &edges {
                    let (dep_name, dep_version) = split_lock_edge(edge);
                    if !normal_names.contains(dep_name) {
                        continue;
                    }
                    self.maybe_follow_edge(name, facts, dep_name, dep_version);
                }
            }
            None => {
                self.result.problems.push(format!(
                    "kein Lockfile-Eintrag für internes Crate '{name}' gefunden"
                ));
            }
        }
    }

    fn visit_external(&mut self, name: &str, version_hint: Option<&str>) {
        let version = match version_hint {
            Some(v) => v.to_owned(),
            None => match self.lock_by_name.get(name).map(Vec::as_slice) {
                Some([single]) => single.version.clone(),
                Some(candidates) if candidates.len() > 1 => {
                    self.result.problems.push(format!(
                        "'{name}' ist im Lockfile mehrdeutig ({} Versionen) und konnte ohne Versionsangabe an der Kante nicht disambiguiert werden",
                        candidates.len()
                    ));
                    return;
                }
                _ => {
                    self.result
                        .problems
                        .push(format!("'{name}' hat keinen Eintrag in Cargo.lock"));
                    return;
                }
            },
        };

        let key = (name.to_owned(), version.clone());
        if self.visited_external.contains(&key) {
            return;
        }
        self.visited_external.insert(key);

        let src_dir = match self.locator.resolve(name, &version) {
            Ok(dir) => dir,
            Err(error) => {
                self.result.problems.push(format!(
                    "keine lokale Registry-Quelle für '{name}-{version}' gefunden: {error}"
                ));
                return;
            }
        };
        let manifest_path = src_dir.join("Cargo.toml");
        let content = match fs::read_to_string(&manifest_path) {
            Ok(c) => c,
            Err(error) => {
                self.result.problems.push(format!(
                    "Manifest von '{name}-{version}' unter '{}' nicht lesbar: {error}",
                    manifest_path.display()
                ));
                return;
            }
        };
        let facts = parse_manifest_facts(&content);
        if facts.is_proc_macro {
            return;
        }
        self.result.external.push(name.to_owned());
        self.result.facts.insert(name.to_owned(), facts.clone());

        if let Some(pkg) = self.lock_by_key.get(&(name.to_owned(), version.clone())) {
            let edges = pkg.dependencies.clone();
            for edge in &edges {
                let (dep_name, dep_version) = split_lock_edge(edge);
                if !facts.normal_dep_names.iter().any(|n| n == dep_name) {
                    continue;
                }
                self.maybe_follow_edge(name, &facts, dep_name, dep_version);
            }
        } else {
            self.result.problems.push(format!(
                "kein Lockfile-Eintrag für '{name}-{version}' zum Weiterverfolgen seiner Kanten gefunden"
            ));
        }
    }

    /// Zerlegt den Builder nach Abschluss eines Durchlaufs in sein Ergebnis
    /// und die für den nächsten Durchlauf gesammelten Feature-Anforderungen
    /// (siehe [`compute_closure`]).
    fn into_parts(
        self,
    ) -> (
        ClosureResult,
        HashMap<String, HashSet<String>>,
        HashMap<String, bool>,
    ) {
        (
            self.result,
            self.next_requested_features,
            self.next_default_enabled,
        )
    }
}

/// Berechnet die transitive Laufzeit-Hülle von `root` — Grundlage für beide
/// Gate-Berichte.
///
/// # Description
/// Seit der Feature-Auflösungs-Korrektur (siehe Moduldoku, Abschnitt
/// „Feature-Auflösung") ist das kein einzelner Durchlauf mehr, sondern ein
/// Fixpunkt über [`ClosureBuilder`]-Durchläufen: jeder Durchlauf nutzt die
/// im *vorigen* Durchlauf gesammelten Feature-Anforderungen, um zu
/// entscheiden, welche optionalen Abhängigkeiten aktiviert sind, und
/// sammelt dabei selbst die Feature-Anforderungen für den *nächsten*
/// Durchlauf. Die Wurzel gilt immer als aktiviert, mit ihren eigenen
/// Default-Features (kein externer Aufrufer setzt für sie etwas). Da sowohl
/// die je Crate angeforderten Features als auch der `default-features`-
/// Stand über die Durchläufe hinweg nur wachsen können (kein Rückschritt),
/// terminiert diese Schleife immer; `safety_limit` ist ein reines
/// Sicherheitsnetz gegen einen Implementierungsfehler, kein erwarteter
/// Abbruch — der Workspace hat rund 190 Crates, die Hülle konvergiert weit
/// darunter.
///
/// # Arguments
/// - `root` (`&str`): der Wurzel-Crate-Name (praktisch immer
///   [`WARDEN_ROOT`]).
/// - `graph` (`&WorkspaceGraph`): der geladene Workspace-Graph.
/// - `lock` (`&[LockEntry]`): die geparsten `Cargo.lock`-Einträge.
/// - `locator` (`&RegistrySourceLocator`): Auflösung externer
///   Registry-Quellverzeichnisse.
/// - `workspace_deps` (`&HashMap<String, DependencyEdgeAttrs>`): Ergebnis
///   von [`parse_workspace_dependencies`], für `workspace = true`-Kanten.
///
/// # Returns
/// Die vollständige [`ClosureResult`] des letzten (stabilen) Durchlaufs,
/// inklusive aller nicht auflösbaren Kanten.
///
/// # Errors
/// Keine — totale Funktion; Auflösungsfehler landen in
/// [`ClosureResult::problems`], kein `Result` nötig.
#[must_use]
fn compute_closure(
    root: &str,
    graph: &WorkspaceGraph,
    lock: &[LockEntry],
    locator: &RegistrySourceLocator,
    workspace_deps: &WorkspaceDependencyTables,
) -> ClosureResult {
    let mut requested_features: HashMap<String, HashSet<String>> = HashMap::new();
    let mut default_enabled: HashMap<String, bool> = HashMap::new();
    default_enabled.insert(root.to_owned(), true);

    let mut last_result = ClosureResult::default();
    let safety_limit = 128usize;

    for _ in 0..safety_limit {
        let mut builder = ClosureBuilder::new(
            graph,
            lock,
            locator,
            workspace_deps,
            &requested_features,
            &default_enabled,
        );
        builder.run_from_root(root);
        let (result, next_requested, mut next_default) = builder.into_parts();
        next_default.insert(root.to_owned(), true);

        let stable = next_requested == requested_features && next_default == default_enabled;
        requested_features = next_requested;
        default_enabled = next_default;
        last_result = result;
        if stable {
            break;
        }
    }

    last_result
}

/// Baut den [`GateReport`] für Gate 4 (Abhängigkeitszahl) aus einer bereits
/// berechneten [`ClosureResult`].
///
/// # Description
/// Rot, wenn entweder die Hülle größer als [`MAX_WARDEN_RUNTIME_DEPS`] ist,
/// oder wenn [`ClosureResult::problems`] nicht leer ist (eine nicht
/// beschaffbare Information ist selbst ein Verstoß, siehe Moduldoku).
#[must_use]
fn evaluate_dependency_budget(result: &ClosureResult) -> GateReport {
    let mut violations = result.problems.clone();
    let total = result.total_resolved();

    if total > MAX_WARDEN_RUNTIME_DEPS {
        let mut all: Vec<&str> = result
            .internal
            .iter()
            .chain(result.external.iter())
            .map(String::as_str)
            .collect();
        all.sort_unstable();
        violations.push(format!(
            "'{WARDEN_ROOT}' erreicht {total} Laufzeit-Crates (intern {}, extern {}) — erlaubt sind höchstens {MAX_WARDEN_RUNTIME_DEPS}: {}",
            result.internal.len(),
            result.external.len(),
            all.join(", ")
        ));
    }

    GateReport {
        name: "warden-dependency-budget",
        checked: result.checked(),
        violations,
    }
}

/// Baut den [`GateReport`] für Gate 5 (kein C-Build) aus einer bereits
/// berechneten [`ClosureResult`].
///
/// # Description
/// Rot für jedes Crate in der Hülle, dessen [`ManifestFacts::has_links`]
/// gesetzt ist, oder das mindestens ein nicht durch [`C_BUILD_EXCEPTIONS`]
/// gedecktes Werkzeug in [`ManifestFacts::build_tool_names`] trägt — siehe
/// Moduldoku, Abschnitt „Gate 5". Ein `build.rs` ohne diese Signale bleibt
/// unbeanstandet. Die Ausnahmetabelle wirkt ausschließlich auf das
/// `cc`/`bindgen`-Signal je einzelnem Werkzeug, nie auf `has_links` — ein
/// Crate mit sowohl `links` als auch einem freigegebenen `cc` bliebe wegen
/// `links` weiterhin rot.
#[must_use]
fn evaluate_c_build(result: &ClosureResult) -> GateReport {
    let mut violations = result.problems.clone();

    let mut offending: Vec<(&str, bool, Vec<&str>)> = result
        .facts
        .iter()
        .filter_map(|(name, facts)| {
            let unexempted_tools: Vec<&str> = facts
                .build_tool_names
                .iter()
                .map(String::as_str)
                .filter(|tool| !is_c_build_exception(name, tool))
                .collect();
            if facts.has_links || !unexempted_tools.is_empty() {
                Some((name.as_str(), facts.has_links, unexempted_tools))
            } else {
                None
            }
        })
        .collect();
    offending.sort_unstable_by_key(|(name, _, _)| *name);

    for (name, has_links, unexempted_tools) in offending {
        violations.push(format!(
            "'{name}' braucht einen C-Übersetzer im Warden-Teilbaum (links-Schlüssel={has_links}, cc/bindgen-Build-Dependency={})",
            !unexempted_tools.is_empty()
        ));
    }

    GateReport {
        name: "warden-no-c-build",
        checked: result.checked(),
        violations,
    }
}

/// Lädt Workspace-Graph, Lockfile und Registry-Locator und berechnet die
/// Hülle einmalig — gemeinsame Grundlage für beide `run()`-Funktionen.
///
/// # Description
/// `harw-warden` liegt unter `dod/crates/` und ist seit PL-60 Member des
/// Wurzel-Workspace. Maßgeblich für seine Hülle ist deshalb das
/// Wurzel-`Cargo.lock` — das einzige Lockfile, mit dem das Binary gebaut
/// wird. Der Graph ist der Wurzel-Workspace
/// ([`super::load_all_workspaces`]); `workspace = true` wird gegen die
/// `[workspace.dependencies]` der Wurzel aufgelöst
/// ([`WorkspaceDependencyTables`]).
///
/// # Arguments
/// - `repo_root` (`&Path`): Wurzel des Repositorys.
///
/// # Errors
/// Wenn der Workspace-Graph, `Cargo.lock` oder
/// `CARGO_HOME`/`HOME` nicht gelesen werden können. Ein einzelner nicht
/// auflösbarer *Knoten* in der Hülle ist dagegen kein `Err` — siehe
/// Moduldoku.
fn load_and_compute(repo_root: &Path) -> Result<ClosureResult, String> {
    let graph = super::load_all_workspaces(repo_root)?;
    let lock = parse_lock_entries(repo_root)?;
    let locator = RegistrySourceLocator::from_env()
        .map_err(|error| format!("Registry-Locator nicht verfügbar: {error}"))?;
    // Neu mit der Feature-Auflösungs-Korrektur: die
    // `[workspace.dependencies]`-Tabellen werden für `dep.workspace = true`
    // gebraucht (siehe Moduldoku, Abschnitt „Feature-Auflösung").
    let workspace_deps = WorkspaceDependencyTables::load(&[repo_root])?;

    Ok(compute_closure(
        WARDEN_ROOT,
        &graph,
        &lock,
        &locator,
        &workspace_deps,
    ))
}

/// Gate 4: Warden-Abhängigkeitszahl (K53).
pub mod dependency_budget {
    use std::path::Path;

    use super::{GateReport, evaluate_dependency_budget, load_and_compute};

    /// Führt Gate 4 aus.
    ///
    /// # Description
    /// Lädt den echten Workspace samt `Cargo.lock` und Registry-Cache (kein
    /// `cargo`-Subprozess) und meldet, ob `harw-warden`s transitive
    /// Laufzeit-Hülle das Budget aus K53 einhält — siehe Moduldoku dieser
    /// Datei.
    ///
    /// # Returns
    /// Einen [`GateReport`] mit der Zahl der tatsächlich geprüften
    /// Kandidaten (siehe [`super::ClosureResult::checked`]).
    ///
    /// # Errors
    /// Wenn Workspace-Graph, `Cargo.lock` oder `CARGO_HOME`/`HOME` nicht
    /// gelesen werden können. Eine nicht auflösbare einzelne Kante ist kein
    /// `Err`, sondern ein Verstoß im Bericht.
    ///
    /// # Examples
    /// ```text
    /// let report = dependency_budget::run()?;
    /// println!("{}", report.summary());
    /// ```
    pub fn run() -> Result<GateReport, String> {
        let result = load_and_compute(Path::new("."))?;
        Ok(evaluate_dependency_budget(&result))
    }
}

/// Gate 5: kein C-Übersetzer im Warden-Teilbaum.
pub mod c_build {
    use std::path::Path;

    use super::{GateReport, evaluate_c_build, load_and_compute};

    /// Führt Gate 5 aus.
    ///
    /// # Description
    /// Nutzt dieselbe Traversierung wie [`super::dependency_budget::run`]
    /// (ein zweiter, unabhängiger Lauf — beide Funktionen sind bewusst
    /// eigenständig aufrufbar, siehe `xtask/src/gates.rs`, das jedes Gate
    /// einzeln selektierbar hält) und prüft jedes erreichte Crate auf
    /// `links`-Schlüssel bzw. `cc`/`bindgen`-Build-Dependency.
    ///
    /// # Returns
    /// Einen [`GateReport`] mit der Zahl der tatsächlich geprüften
    /// Kandidaten.
    ///
    /// # Errors
    /// Wie [`super::dependency_budget::run`].
    ///
    /// # Examples
    /// ```text
    /// let report = c_build::run()?;
    /// println!("{}", report.summary());
    /// ```
    pub fn run() -> Result<GateReport, String> {
        let result = load_and_compute(Path::new("."))?;
        // R3-03: `CBuildException::reason`/`::since` sind sonst nirgends
        // gelesen (einziger anderer Zugriff ist `.krate`/`.tool` in
        // `is_c_build_exception`) und würden clippys `-D warnings`
        // („fields … are never read") brechen. Hier fließen sie in eine
        // nachvollziehbare Meldung, statt per `#[allow(dead_code)]`
        // stillgelegt zu werden — wer den Gate-Lauf liest, sieht damit auch,
        // *warum* eine an sich verdächtige C-Bau-Kante nicht rot wird.
        for exception in super::C_BUILD_EXCEPTIONS {
            println!(
                "warden-no-c-build: Ausnahme {}/{} seit {}: {}",
                exception.krate, exception.tool, exception.since, exception.reason
            );
        }
        Ok(evaluate_c_build(&result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// `build_tool_names` ersetzt den vormaligen reinen `bool`-Parameter:
    /// er trägt die konkreten Werkzeugnamen (`"cc"`/`"bindgen"`), damit
    /// Tests der Ausnahmetabelle ([`C_BUILD_EXCEPTIONS`]) dasselbe Signal
    /// erzeugen können, das [`parse_manifest_facts`] tatsächlich liefert.
    fn facts(
        is_proc_macro: bool,
        has_links: bool,
        build_tool_names: &[&str],
        deps: &[&str],
    ) -> ManifestFacts {
        ManifestFacts {
            is_proc_macro,
            has_links,
            build_needs_c_compiler: !build_tool_names.is_empty(),
            build_tool_names: build_tool_names.iter().map(|t| (*t).to_owned()).collect(),
            normal_dep_names: deps.iter().map(|d| (*d).to_owned()).collect(),
            // Neu mit der Feature-Auflösungs-Korrektur: für diese Tests
            // (Gate 4/5 auf einer bereits fertigen `ClosureResult`, ohne
            // Aktivierungsprüfung) spielen die Feature-Attribute keine
            // Rolle -- leer lassen genügt.
            ..Default::default()
        }
    }

    fn closure_with(
        internal: &[&str],
        external: &[&str],
        facts_map: Vec<(&str, ManifestFacts)>,
    ) -> ClosureResult {
        ClosureResult {
            internal: internal.iter().map(|s| (*s).to_owned()).collect(),
            external: external.iter().map(|s| (*s).to_owned()).collect(),
            facts: facts_map
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            problems: Vec::new(),
        }
    }

    #[test]
    fn test_evaluate_dependency_budget_twelve_crates_is_green() {
        let internal = ["a", "b", "c"];
        let external = ["d", "e", "f", "g", "h", "i", "j", "k", "l"];
        assert_eq!(internal.len() + external.len(), 12);
        let result = closure_with(&internal, &external, Vec::new());

        let report = evaluate_dependency_budget(&result);

        assert!(report.is_green(), "erwartet grün bei genau 12: {report:?}");
        assert_eq!(report.checked, 12);
    }

    #[test]
    fn test_evaluate_dependency_budget_exactly_max_is_green() {
        // Nachgezogen für die Feature-Auflösungs-Korrektur: prüft die
        // Invariante relativ zu `MAX_WARDEN_RUNTIME_DEPS`, statt gegen eine
        // absolute Zahl, die bei jeder Anpassung der Konstante erneut
        // bricht (siehe Auftrag).
        let names: Vec<String> = (0..MAX_WARDEN_RUNTIME_DEPS)
            .map(|i| format!("crate-{i}"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let result = closure_with(&refs, &[], Vec::new());

        let report = evaluate_dependency_budget(&result);

        assert!(
            report.is_green(),
            "erwartet grün bei genau MAX_WARDEN_RUNTIME_DEPS ({MAX_WARDEN_RUNTIME_DEPS}): {report:?}"
        );
        assert_eq!(report.checked, MAX_WARDEN_RUNTIME_DEPS);
    }

    #[test]
    fn test_evaluate_dependency_budget_one_over_max_turns_red() {
        // Vormals `test_evaluate_dependency_budget_thirteenth_edge_turns_red`
        // gegen die feste Zahl 13 (aus der ursprünglichen, in sich
        // widersprüchlichen K53-Lesart) — jetzt relativ zu
        // `MAX_WARDEN_RUNTIME_DEPS` formuliert, siehe Auftrag.
        let names: Vec<String> = (0..=MAX_WARDEN_RUNTIME_DEPS)
            .map(|i| format!("crate-{i}"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(refs.len(), MAX_WARDEN_RUNTIME_DEPS + 1);
        let result = closure_with(&refs, &[], Vec::new());

        let report = evaluate_dependency_budget(&result);

        assert!(!report.is_green());
        assert_eq!(report.violations.len(), 1);
        assert!(
            report.violations[0]
                .contains(&format!("{} Laufzeit-Crates", MAX_WARDEN_RUNTIME_DEPS + 1))
        );
    }

    #[test]
    fn test_evaluate_dependency_budget_unresolvable_edge_is_a_violation_not_a_silent_pass() {
        let mut result = closure_with(&["a"], &["b"], Vec::new());
        result
            .problems
            .push("'irgendwas' hat keinen Eintrag in Cargo.lock".to_owned());

        let report = evaluate_dependency_budget(&result);

        assert!(!report.is_green());
        assert_eq!(report.checked, 3); // 2 aufgelöst + 1 Problem
    }

    #[test]
    fn test_evaluate_dependency_budget_reports_checked_greater_than_zero() {
        let result = closure_with(&["a"], &[], Vec::new());
        let report = evaluate_dependency_budget(&result);
        assert!(report.checked > 0);
    }

    #[test]
    fn test_evaluate_c_build_plain_build_script_does_not_turn_red() {
        // `rustix`/`nix`-Fall: build.rs vorhanden (impliziert durch die
        // Existenz eines Facts-Eintrags), aber weder links noch cc/bindgen.
        let result = closure_with(
            &[],
            &["rustix"],
            vec![("rustix", facts(false, false, &[], &["bitflags"]))],
        );

        let report = evaluate_c_build(&result);

        assert!(
            report.is_green(),
            "build.rs allein darf nicht röten: {report:?}"
        );
        assert!(report.checked > 0);
    }

    #[test]
    fn test_evaluate_c_build_links_key_turns_red() {
        let result = closure_with(
            &[],
            &["rustables"],
            vec![("rustables", facts(false, true, &[], &[]))],
        );

        let report = evaluate_c_build(&result);

        assert!(!report.is_green());
        assert!(report.violations[0].contains("rustables"));
        assert!(report.violations[0].contains("links-Schlüssel=true"));
    }

    #[test]
    fn test_evaluate_c_build_cc_bindgen_build_dependency_turns_red() {
        // Absichtlich NICHT "blake3" als Fixture-Name: blake3 steht seit
        // G-002 in `C_BUILD_EXCEPTIONS` und würde diesen Test fälschlich
        // grün machen. Ein noch nicht eingetragenes Crate mit derselben Art
        // Kante muss weiterhin rot bleiben — das prüft dieser Test.
        let result = closure_with(
            &[],
            &["some-other-crate"],
            vec![("some-other-crate", facts(false, false, &["cc"], &[]))],
        );

        let report = evaluate_c_build(&result);

        assert!(!report.is_green());
        assert!(report.violations[0].contains("some-other-crate"));
        assert!(report.violations[0].contains("cc/bindgen-Build-Dependency=true"));
    }

    #[test]
    fn test_evaluate_c_build_blake3_exception_is_not_a_violation() {
        // G-002: blake3s `cc`-Build-Dependency ist eine geprüfte, datierte
        // Ausnahme (siehe C_BUILD_EXCEPTIONS) — dieselbe Kante, die der
        // vorige Test für ein unbekanntes Crate rot werden lässt, darf für
        // blake3 kein Verstoß sein.
        let result = closure_with(
            &[],
            &["blake3"],
            vec![("blake3", facts(false, false, &["cc"], &[]))],
        );

        let report = evaluate_c_build(&result);

        assert!(
            report.is_green(),
            "blake3s dokumentierte cc-Ausnahme darf nicht röten: {report:?}"
        );
    }

    #[test]
    fn test_evaluate_c_build_exception_does_not_cover_unlisted_tool_on_same_crate() {
        // Eine Ausnahme deckt nur das eingetragene Werkzeug ab. Trüge
        // blake3 zusätzlich einen `links`-Schlüssel, bliebe es deswegen rot
        // — das ist hier nachgebildet, weil `links` nie durch
        // `C_BUILD_EXCEPTIONS` gedeckt wird (siehe `evaluate_c_build`-Doku).
        let result = closure_with(
            &[],
            &["blake3"],
            vec![("blake3", facts(false, true, &["cc"], &[]))],
        );

        let report = evaluate_c_build(&result);

        assert!(
            !report.is_green(),
            "links-Schlüssel bleibt trotz cc-Ausnahme ein Verstoß"
        );
        assert!(report.violations[0].contains("links-Schlüssel=true"));
    }

    #[test]
    fn test_evaluate_c_build_reports_checked_greater_than_zero() {
        let result = closure_with(&["a"], &["b"], Vec::new());
        let report = evaluate_c_build(&result);
        assert!(report.checked > 0);
    }

    #[test]
    fn test_is_c_build_exception_matches_only_listed_pair() {
        assert!(is_c_build_exception("blake3", "cc"));
        assert!(!is_c_build_exception("blake3", "bindgen"));
        assert!(!is_c_build_exception("some-other-crate", "cc"));
    }

    #[test]
    fn test_split_lock_edge_with_and_without_version() {
        assert_eq!(split_lock_edge("serde"), ("serde", None));
        assert_eq!(split_lock_edge("serde 1.0.228"), ("serde", Some("1.0.228")));
    }

    #[test]
    fn test_parse_manifest_facts_detects_proc_macro() {
        let content = "[package]\nname = \"foo\"\n\n[lib]\nproc-macro = true\n";
        let facts = parse_manifest_facts(content);
        assert!(facts.is_proc_macro);
    }

    #[test]
    fn test_parse_manifest_facts_detects_links_key() {
        let content = "[package]\nname = \"foo\"\nlinks = \"onig\"\n";
        let facts = parse_manifest_facts(content);
        assert!(facts.has_links);
    }

    #[test]
    fn test_parse_manifest_facts_plain_build_script_without_links_is_not_flagged() {
        let content = "[package]\nname = \"rustix\"\n\n[dependencies]\nbitflags = \"2\"\n";
        let facts = parse_manifest_facts(content);
        assert!(!facts.has_links);
        assert!(!facts.build_needs_c_compiler);
    }

    #[test]
    fn test_parse_manifest_facts_cc_build_dependency_is_flagged() {
        let content = "[package]\nname = \"foo\"\n\n[build-dependencies]\ncc = \"1\"\n";
        let facts = parse_manifest_facts(content);
        assert!(facts.build_needs_c_compiler);
        assert_eq!(facts.build_tool_names, vec!["cc".to_owned()]);
    }

    #[test]
    fn test_parse_manifest_facts_bindgen_via_dotted_dependency_table_is_flagged() {
        let content =
            "[package]\nname = \"foo\"\n\n[build-dependencies.bindgen]\nversion = \"0.60\"\n";
        let facts = parse_manifest_facts(content);
        assert!(facts.build_needs_c_compiler);
        assert_eq!(facts.build_tool_names, vec!["bindgen".to_owned()]);
    }

    #[test]
    fn test_parse_manifest_facts_collects_normal_dependency_names() {
        let content =
            "[dependencies]\nserde = \"1\"\nfoo = { version = \"2\", features = [\"a\"] }\n";
        let facts = parse_manifest_facts(content);
        assert!(facts.normal_dep_names.contains(&"serde".to_owned()));
        assert!(facts.normal_dep_names.contains(&"foo".to_owned()));
    }

    #[test]
    fn test_parse_manifest_facts_ignores_dev_and_build_dependencies_for_normal_deps() {
        let content = "[dependencies]\nserde = \"1\"\n\n[dev-dependencies]\ntempfile = \"3\"\n\n[build-dependencies]\ncc = \"1\"\n";
        let facts = parse_manifest_facts(content);
        assert_eq!(facts.normal_dep_names, vec!["serde".to_owned()]);
    }

    // -- Feature-Auflösungs-Korrektur: neue Tests ---------------------------

    #[test]
    fn test_parse_manifest_facts_detects_optional_inline_table() -> TestResult {
        let content = "[dependencies]\ndefmt = { version = \"1\", optional = true }\n";
        let facts = parse_manifest_facts(content);
        assert!(
            facts
                .dependency_attrs
                .get("defmt")
                .ok_or(TestError::Missing("defmt gefunden"))?
                .optional
        );
        Ok(())
    }

    #[test]
    fn test_parse_manifest_facts_detects_workspace_shorthand() -> TestResult {
        let content = "[dependencies]\njiff.workspace = true\n";
        let facts = parse_manifest_facts(content);
        assert!(
            facts
                .dependency_attrs
                .get("jiff")
                .ok_or(TestError::Missing("jiff gefunden"))?
                .workspace_inherited
        );
        Ok(())
    }

    #[test]
    fn test_parse_manifest_facts_dotted_table_multiline_features() -> TestResult {
        let content = "[dependencies.foo]\nversion = \"1\"\noptional = true\nfeatures = [\n \"a\",\n \"b\",\n]\n";
        let facts = parse_manifest_facts(content);
        let attrs = facts
            .dependency_attrs
            .get("foo")
            .ok_or(TestError::Missing("foo gefunden"))?;
        assert!(attrs.optional);
        assert_eq!(attrs.features, vec!["a".to_owned(), "b".to_owned()]);
        // Der latente Fehler der Vorfassung ist behoben: `version`/
        // `optional`/`features` landen nicht mehr als eigene
        // Abhängigkeitsnamen in `normal_dep_names`.
        assert_eq!(facts.normal_dep_names, vec!["foo".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_parse_manifest_facts_features_section_multiline_and_inline() {
        let content =
            "[features]\ndefault = [\n \"std\",\n \"serde\",\n]\ndefmt = [\"dep:defmt\"]\n";
        let facts = parse_manifest_facts(content);
        assert_eq!(
            facts.feature_defs.get("default"),
            Some(&vec!["std".to_owned(), "serde".to_owned()])
        );
        assert_eq!(
            facts.feature_defs.get("defmt"),
            Some(&vec!["dep:defmt".to_owned()])
        );
    }

    fn facts_with_optional_dep(dep_name: &str) -> ManifestFacts {
        let mut f = ManifestFacts::default();
        f.normal_dep_names.push(dep_name.to_owned());
        f.dependency_attrs.insert(
            dep_name.to_owned(),
            DependencyEdgeAttrs {
                optional: true,
                ..Default::default()
            },
        );
        f
    }

    #[test]
    fn test_resolve_activated_optional_deps_not_requested_is_excluded() {
        // Der wichtigste Test: entspricht exakt dem `jiff`/`defmt`-Fall aus
        // dem Befund. Er wäre vor dieser Korrektur rot gewesen.
        let f = facts_with_optional_dep("defmt");
        let active = resolve_activated_optional_deps(&f, &HashSet::new(), true);
        assert!(
            !active.contains("defmt"),
            "defmt darf ohne Anforderung nicht aktiv sein"
        );
    }

    #[test]
    fn test_resolve_activated_optional_deps_explicitly_requested_is_included() {
        let mut f = facts_with_optional_dep("defmt");
        f.feature_defs
            .insert("defmt".to_owned(), vec!["dep:defmt".to_owned()]);
        let requested: HashSet<String> = ["defmt".to_owned()].into_iter().collect();
        let active = resolve_activated_optional_deps(&f, &requested, true);
        assert!(active.contains("defmt"));
    }

    #[test]
    fn test_resolve_activated_optional_deps_default_feature_activates_dependency() {
        let mut f = facts_with_optional_dep("jiff-tzdb-platform");
        f.feature_defs.insert(
            "default".to_owned(),
            vec!["tzdb-bundle-platform".to_owned()],
        );
        f.feature_defs.insert(
            "tzdb-bundle-platform".to_owned(),
            vec!["dep:jiff-tzdb-platform".to_owned()],
        );
        let active = resolve_activated_optional_deps(&f, &HashSet::new(), true);
        assert!(active.contains("jiff-tzdb-platform"));
    }

    #[test]
    fn test_resolve_activated_optional_deps_default_features_disabled_excludes_default_only_dep() {
        let mut f = facts_with_optional_dep("jiff-tzdb-platform");
        f.feature_defs.insert(
            "default".to_owned(),
            vec!["tzdb-bundle-platform".to_owned()],
        );
        f.feature_defs.insert(
            "tzdb-bundle-platform".to_owned(),
            vec!["dep:jiff-tzdb-platform".to_owned()],
        );
        let active = resolve_activated_optional_deps(&f, &HashSet::new(), false);
        assert!(!active.contains("jiff-tzdb-platform"));
    }

    #[test]
    fn test_resolve_activated_optional_deps_feature_enables_feature_fixpoint() {
        let mut f = facts_with_optional_dep("backend");
        f.feature_defs
            .insert("full".to_owned(), vec!["extra".to_owned()]);
        f.feature_defs
            .insert("extra".to_owned(), vec!["dep:backend".to_owned()]);
        let requested: HashSet<String> = ["full".to_owned()].into_iter().collect();
        let active = resolve_activated_optional_deps(&f, &requested, false);
        assert!(
            active.contains("backend"),
            "full -> extra -> dep:backend muss die Kette durchlaufen"
        );
    }

    #[test]
    fn test_resolve_activated_optional_deps_weak_reference_does_not_activate() {
        // Nachbildung von `jiff`s `tz-fat = ["jiff-static?/tz-fat"]`: eine
        // schwache Referenz aktiviert `jiff-static` gerade nicht.
        let mut f = facts_with_optional_dep("jiff-static");
        f.feature_defs
            .insert("default".to_owned(), vec!["tz-fat".to_owned()]);
        f.feature_defs
            .insert("tz-fat".to_owned(), vec!["jiff-static?/tz-fat".to_owned()]);
        let active = resolve_activated_optional_deps(&f, &HashSet::new(), true);
        assert!(!active.contains("jiff-static"));
    }

    #[test]
    fn test_resolve_workspace_edge_merges_features_additively() {
        let mut workspace_deps = HashMap::new();
        workspace_deps.insert(
            "jiff".to_owned(),
            DependencyEdgeAttrs {
                optional: false,
                default_features: true,
                features: vec!["serde".to_owned()],
                workspace_inherited: false,
            },
        );
        let local = DependencyEdgeAttrs {
            workspace_inherited: true,
            features: vec!["local-extra".to_owned()],
            ..Default::default()
        };

        let (merged, problem) = resolve_workspace_edge("jiff", &local, &workspace_deps);

        assert!(problem.is_none());
        assert!(merged.features.contains(&"serde".to_owned()));
        assert!(merged.features.contains(&"local-extra".to_owned()));
    }

    #[test]
    fn test_resolve_workspace_edge_missing_root_entry_is_a_problem_not_a_silent_default()
    -> TestResult {
        let workspace_deps: HashMap<String, DependencyEdgeAttrs> = HashMap::new();
        let local = DependencyEdgeAttrs {
            workspace_inherited: true,
            ..Default::default()
        };

        let (_, problem) = resolve_workspace_edge("ghost", &local, &workspace_deps);

        assert!(
            problem
                .ok_or(TestError::Missing("Problem erwartet"))?
                .contains("ghost")
        );
        Ok(())
    }

    // -- Nachkorrektur: `[workspace.dependencies]` in allen drei
    // Schreibweisen -----------------------------------------------------
    //
    // Befund (Gate-Lauf): `parse_workspace_dependencies` legte für die reine
    // Zeichenketten-Schreibweise (`foo = "1.2.3"`) nie einen Eintrag an —
    // `blake3`, `serde_json`, `tracing` und `rustix` stehen genau so in der
    // Wurzel-`Cargo.toml` und wurden deshalb fälschlich als "workspace =
    // true referenziert, aber kein Wurzel-Eintrag gefunden" gemeldet, obwohl
    // der Eintrag da war. Die drei folgenden Tests decken je eine
    // Schreibweise ab; ein vierter belegt, dass ein wirklich fehlender
    // Eintrag weiterhin ein Verstoß bleibt.

    fn write_root_manifest(
        dir: &std::path::Path,
        workspace_dependencies_block: &str,
    ) -> TestResult {
        fs::create_dir_all(dir).map_err(ctx("Scratch-Verzeichnis anlegen"))?;
        let content = format!(
            "[workspace]\nmembers = []\n\n[workspace.dependencies]\n{workspace_dependencies_block}\n"
        );
        fs::write(dir.join("Cargo.toml"), content).map_err(ctx("Cargo.toml schreiben"))?;
        Ok(())
    }

    #[test]
    fn test_parse_workspace_dependencies_plain_string_form() -> TestResult {
        // Die Schreibweise, die den Fehler auslöste: `name = "1.2.3"`, ohne
        // Inline-Tabelle.
        let dir = std::env::temp_dir().join(format!("gate-warden-ws-plain-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_root_manifest(&dir, "serde_json = \"1.0.150\"\nblake3 = \"1.8.7\"\n")?;

        let deps = parse_workspace_dependencies(&dir).map_err(ctx("Wurzel-Cargo.toml parsen"))?;

        assert!(
            deps.contains_key("serde_json"),
            "serde_json fehlt: {deps:?}"
        );
        assert!(deps.contains_key("blake3"), "blake3 fehlt: {deps:?}");
        let serde_json = &deps["serde_json"];
        assert!(!serde_json.optional);
        assert!(serde_json.default_features);
        assert!(serde_json.features.is_empty());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_parse_workspace_dependencies_inline_table_form() -> TestResult {
        let dir =
            std::env::temp_dir().join(format!("gate-warden-ws-inline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_root_manifest(
            &dir,
            "jiff = { version = \"0.2.32\", features = [\"serde\"] }\n",
        )?;

        let deps = parse_workspace_dependencies(&dir).map_err(ctx("Wurzel-Cargo.toml parsen"))?;

        let jiff = deps
            .get("jiff")
            .ok_or(TestError::Missing("jiff gefunden"))?;
        assert_eq!(jiff.features, vec!["serde".to_owned()]);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_parse_workspace_dependencies_subtable_form() -> TestResult {
        let dir =
            std::env::temp_dir().join(format!("gate-warden-ws-subtable-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).map_err(ctx("Scratch-Verzeichnis anlegen"))?;
        let content = "[workspace]\nmembers = []\n\n[workspace.dependencies.foo]\nversion = \"1\"\noptional = false\n";
        fs::write(dir.join("Cargo.toml"), content).map_err(ctx("Cargo.toml schreiben"))?;

        let deps = parse_workspace_dependencies(&dir).map_err(ctx("Wurzel-Cargo.toml parsen"))?;

        assert!(deps.contains_key("foo"), "foo fehlt: {deps:?}");

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_parse_workspace_dependencies_then_resolve_missing_entry_stays_a_violation() -> TestResult
    {
        // Belegt, dass die Korrektur das richtige Verhalten nicht abschwächt:
        // ein Name, der wirklich nicht in `[workspace.dependencies]` steht,
        // bleibt ein Verstoß -- kein stilles Überspringen.
        let dir =
            std::env::temp_dir().join(format!("gate-warden-ws-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_root_manifest(&dir, "serde_json = \"1.0.150\"\n")?;

        let deps = parse_workspace_dependencies(&dir).map_err(ctx("Wurzel-Cargo.toml parsen"))?;
        let local = DependencyEdgeAttrs {
            workspace_inherited: true,
            ..Default::default()
        };
        let (_, problem) = resolve_workspace_edge("tracing", &local, &deps);

        assert!(
            problem
                .ok_or(TestError::Missing(
                    "Problem erwartet, da 'tracing' nicht in [workspace.dependencies] steht"
                ))?
                .contains("tracing")
        );

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_cfg_predicate_matches_target_unix_true_windows_false() {
        assert!(cfg_predicate_matches_target("cfg(unix)"));
        assert!(!cfg_predicate_matches_target("cfg(windows)"));
    }

    #[test]
    fn test_cfg_predicate_matches_target_any_all_not() {
        assert!(cfg_predicate_matches_target(
            "cfg(any(windows, target_os = \"linux\"))"
        ));
        assert!(!cfg_predicate_matches_target(
            "cfg(all(unix, target_arch = \"wasm32\"))"
        ));
        assert!(cfg_predicate_matches_target("cfg(not(windows))"));
        // Mehrwertiger Schlüssel: jiffs `portable-atomic`-Kante gilt auf
        // x86_64 nicht.
        assert!(!cfg_predicate_matches_target(
            "cfg(not(target_has_atomic = \"ptr\"))"
        ));
        assert!(cfg_predicate_matches_target(
            "cfg(target_has_atomic = \"64\")"
        ));
        assert!(!cfg_predicate_matches_target(
            "cfg(target_has_atomic = \"128\")"
        ));
    }

    #[test]
    fn test_cfg_predicate_matches_target_raw_triple() {
        assert!(cfg_predicate_matches_target("x86_64-unknown-linux-gnu"));
        assert!(!cfg_predicate_matches_target("x86_64-pc-windows-msvc"));
    }

    #[test]
    fn test_parse_lock_entries_reads_multiline_and_inline_dependency_arrays() -> TestResult {
        let dir = std::env::temp_dir().join(format!("gate-warden-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).map_err(ctx("Scratch-Verzeichnis anlegen"))?;
        let lock_content = r#"
version = 4

[[package]]
name = "harw-warden"
version = "0.1.0"
dependencies = [
 "clap",
 "rustix 1.1.4",
]

[[package]]
name = "leaf"
version = "1.0.0"
dependencies = []
"#;
        fs::write(dir.join("Cargo.lock"), lock_content).map_err(ctx("Cargo.lock schreiben"))?;

        let entries = parse_lock_entries(&dir).map_err(ctx("Lockfile parsen"))?;
        let warden = entries
            .iter()
            .find(|e| e.name == "harw-warden")
            .ok_or(TestError::Missing("harw-warden gefunden"))?;
        assert_eq!(
            warden.dependencies,
            vec!["clap".to_owned(), "rustix 1.1.4".to_owned()]
        );

        let leaf = entries
            .iter()
            .find(|e| e.name == "leaf")
            .ok_or(TestError::Missing("leaf gefunden"))?;
        assert!(leaf.dependencies.is_empty());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_parse_lock_entries_missing_file_is_an_error_not_an_empty_vec() -> TestResult {
        let dir =
            std::env::temp_dir().join(format!("gate-warden-lock-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).map_err(ctx("Scratch-Verzeichnis anlegen"))?;

        let result = parse_lock_entries(&dir);
        assert!(result.is_err());

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn test_workspace_dependency_tables_pick_the_table_of_the_crates_own_workspace() -> TestResult {
        let base =
            std::env::temp_dir().join(format!("gate-warden-ws-tables-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let dod = base.join("dod");
        fs::create_dir_all(&dod).map_err(ctx("Scratch-Verzeichnisse anlegen"))?;
        fs::write(
            base.join("Cargo.toml"),
            "[workspace.dependencies]\nonly-root = \"1\"\n",
        )
        .map_err(ctx("Wurzel-Cargo.toml schreiben"))?;
        fs::write(
            dod.join("Cargo.toml"),
            "[workspace.dependencies]\nonly-dod = \"2\"\n",
        )
        .map_err(ctx("dod/Cargo.toml schreiben"))?;

        let tables = WorkspaceDependencyTables::load(&[base.as_path(), dod.as_path()])
            .map_err(TestError::Unexpected)?;
        let node = |name: &str, dir: std::path::PathBuf| CrateNode {
            name: name.to_owned(),
            version: "0.0.0".to_owned(),
            manifest_path: dir.join("Cargo.toml"),
            dir,
            deps: Vec::new(),
            dev_deps: Vec::new(),
            build_deps: Vec::new(),
            external_deps: Vec::new(),
            is_leaf: true,
            level: 0,
        };
        let graph = WorkspaceGraph {
            root: base.clone(),
            crates: vec![
                node("prod-crate", base.join("prod-crate")),
                node("dod-crate", dod.join("crates").join("dod-crate")),
            ],
        };

        assert!(
            tables
                .for_crate(&graph, "prod-crate")
                .contains_key("only-root")
        );
        assert!(
            !tables
                .for_crate(&graph, "prod-crate")
                .contains_key("only-dod")
        );
        assert!(
            tables
                .for_crate(&graph, "dod-crate")
                .contains_key("only-dod")
        );
        assert!(
            !tables
                .for_crate(&graph, "dod-crate")
                .contains_key("only-root")
        );
        assert!(tables.for_crate(&graph, "registry-crate").is_empty());

        fs::remove_dir_all(&base).ok();
        Ok(())
    }

    /// Gegen den echten Repo-Stand: die Warden-Hülle wird über DoD- und
    /// Produkt-Crates hinweg berechnet (seit PL-60 ein Workspace, ein
    /// Lockfile) und prüft etwas. Früher fand das Gate seine Wurzel nicht
    /// und meldete nur ein Problem.
    #[test]
    fn test_load_and_compute_on_real_repo_reaches_across_the_workspace_boundary() -> TestResult {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("Repo-Wurzel über xtask/"))?;

        let result = load_and_compute(repo_root).map_err(TestError::Unexpected)?;

        assert!(result.checked() > 0);
        assert!(
            !result
                .problems
                .iter()
                .any(|p| p.contains("nicht im Workspace-Graphen")),
            "{:?}",
            result.problems
        );
        assert!(
            result.internal.iter().any(|c| c == "harw-dod-warden"),
            "DoD-interne Kante fehlt: {:?}",
            result.internal
        );
        assert!(
            result.internal.iter().any(|c| c == "harw-types"),
            "Kante DoD → Produkt fehlt: {:?}",
            result.internal
        );
        Ok(())
    }
}
