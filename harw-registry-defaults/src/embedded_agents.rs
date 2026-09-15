//! Die eingebauten Agenten-, Family- und Organisationsdefinitionen — TOML-
//! Quelltext und aufgelöste Form.
//!
//! Spezifikationsquelle: AP W3-04..05 (Definitionen), AW6-00
//! (Verzeichniskonvention); TOML-Format nach `agent-definition-dsl.md` §8/§11
//! (Agenten), §14 (Families) und §15/§22 (Organisationen) sowie der Fixture
//! `harw-agent-dsl/tests/fixtures/minimal.toml`.
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt ausschließlich die *eingebaute* Schicht
//! ([`DefinitionLayer::BuiltIn`]) der Definitionen: es bettet den Verzeichnisbaum
//! `harw-registry-defaults/agents/` per `include_dir!` ein und senkt die
//! gefundenen Definitionen über die DSL-Pipeline
//! `parse_toml → resolve_definition → lower` zu [`ExecutableAgentIr`]. Das
//! Laden lokaler Definitionen aus `.harw/agents/` gehört **nicht** hierher; der
//! Aufrufer reicht das Ergebnis dieses Ladens als `existing` herein.
//!
//! # Drei Ebenen, drei Verzeichnisse
//! - `agents/*.toml` — die fünf startbaren Rollen plus ihre Basis
//!   ([`builtin_agent_toml`], [`builtin_agent_definitions`]).
//! - `agents/family/*.toml` — welche Rollen ein Clan überhaupt führen darf
//!   ([`builtin_family_toml`], [`builtin_family_definitions`]).
//! - `agents/organization/default.toml` — Root, Clans und Zellen eines Laufs
//!   ([`builtin_organization_toml`], [`default_organization`]).
//!
//! Die Organisation ist die Eingabe von
//! `harw_plan_bridge::CellPlan::from_cell`: ohne sie gibt es keine Zelle, gegen
//! die ein Orchestrator seinen Fan-out auflösen könnte. [`clan_cell`] ist der
//! kürzeste Weg von der aufgelösten Organisation zu genau diesem Paar aus Clan
//! und Zelle.
//!
//! # Verzeichniskonvention
//! Vor AW6-00 zählte dieses Modul jede Datei per Hand über `include_str!` in
//! einer Rust-Liste auf (Zeilen `(WORKER_BASE_NAME, include_str!(...))`, …).
//! Das Ausbauprogramm sieht aber **vier Knoten in drei Wellen** vor, die ohne
//! erkennbaren Pfad zueinander dieselbe Crate `harw-agent-dsl` ändern — nach
//! eigener Lesart des Plans „parallel fahrbar“. Die Definitionen selbst sind
//! bereits Daten (TOML unter `agents/`); eine feste `include_str!`-Aufzählung
//! verschob die Kollision nur eine Ebene tiefer, denn fünf Knoten hätten dann
//! dieselbe Rust-Datei geändert. Eine Verzeichniskonvention macht „eine
//! Definition hinzufügen“ wieder zu „eine Datei anlegen“ — nie zu „eine
//! geteilte Datei ändern“.
//!
//! Der gesamte Baum wird zur Bauzeit über `include_dir!` eingebettet
//! ([`AGENTS_DIR`]) und beim ersten Aufruf einmalig durchlaufen (gecacht in
//! einem `OnceLock`); zur Laufzeit findet weiterhin kein Dateizugriff statt.
//! Die Regel, nach der ein Pfad zu einem Definitionsnamen wird:
//!
//! - **Agentenrollen**: jede `.toml`-Datei irgendwo unter `agents/`, außer
//!   unter `agents/family/`, `agents/families/` oder `agents/organization/`,
//!   ist eine Rolle. Ihr Name ist das letzte Pfadsegment ohne `.toml` —
//!   die Verzeichnistiefe ist dabei beliebig: `agents/explorer.toml` definiert
//!   `explorer` genauso, wie ein künftiges
//!   `agents/roles/security-triage-1/security-triage-1.toml` die Rolle
//!   `security-triage-1` definieren würde. `agents/worker-base.toml` fällt
//!   unter dieselbe Sammelregel; sein Sonderstatus (Layer, keine startbare
//!   Rolle) entsteht erst bei der *Verwendung* in [`builtin_agent_definitions`]
//!   über den Namensvergleich mit [`WORKER_BASE_NAME`] bzw.
//!   [`role_names::ALL`] — nicht beim Einsammeln.
//! - **Families**: jede `.toml`-Datei unter `agents/family/` oder (für
//!   künftige Knoten) `agents/families/` trägt als Namen ihren vollen Pfad
//!   relativ zu `agents/`, ohne `.toml` — `agents/family/research.toml` wird
//!   zu `"family/research"`. Der Pfad bleibt Teil des Namens, weil
//!   [`RESEARCH_FAMILY_NAME`] und [`CODING_FAMILY_NAME`] ihn schon vor der
//!   Umstellung so trugen; ein künftiges `agents/families/security/*.toml`
//!   bekäme entsprechend einen Namen wie `"families/security/…"`.
//! - **Organisation**: `agents/organization/default.toml` ist als einzige
//!   erwartete Datei fest über ihren Pfad nachgeschlagen
//!   ([`builtin_organization_toml`]) — eine Sammelregel wäre hier Overhead,
//!   solange der Plan keine zweite Organisationsdatei kennt.
//!
//! Eine Datei, die nicht auf `.toml` endet oder deren Inhalt kein gültiges
//! UTF-8 ist, wird von der Sammlung stillschweigend übersprungen (unter
//! `agents/` nicht erwartet, aber kein Absturz — eine `README.md` neben den
//! Definitionen ist damit unschädlich). Eine Datei, die zwar eingesammelt
//! wird, aber nicht parst, meldet sich dagegen laut: jeder Fehler dieses
//! Moduls trägt den Namen der betroffenen Definition
//! ([`RegistryDefaultsError::AgentDefinition`]) — sonst sucht man sie unter
//! fünfzig. Fehlt sogar die Basis selbst (kein `worker-base.toml` im Baum) —
//! sei es weil `agents/` fehlt oder weil es leer ist —, meldet
//! [`builtin_agent_definitions`] das ebenfalls namentlich über
//! [`require_worker_base`], statt eine leere Registry stillschweigend als
//! gültig durchzuwinken (eine leere Registry ist von einer kaputten sonst
//! nicht zu unterscheiden) oder erst tief in der `extends`-Auflösung mit
//! einer fremden Fehlermeldung zu scheitern.
//!
//! # Namenskollision — eine neue Fehlerklasse
//! Vor AW6-00 konnte es zwei Definitionen mit demselben Namen nicht geben: ein
//! Mensch pflegte die `include_str!`-Liste von Hand und hätte denselben
//! Schlüssel nie zweimal eingetragen. Die Verzeichnis-Sammlung leitet Namen
//! dagegen aus dem Dateisystem ab — zwei verschiedene Dateien können jetzt
//! denselben Namen ergeben, etwa ein versehentliches
//! `agents/roles/foo/explorer.toml` neben dem bestehenden
//! `agents/explorer.toml`. [`reject_duplicate_names`] fängt das für Rollen und
//! (strukturell heute unerreichbar, aber ebenso geprüft) für Families als
//! harten, benannten Fehler ab, bevor die Kollision `resolve_definition` als
//! stillen Overlay über die zuerst gefundene Datei erreicht.
//!
//! Weil die Zuordnung Name → Definition allein aus dem Pfad folgt, braucht ein
//! neues Unterverzeichnis — etwa `agents/families/security/`,
//! `agents/context-programs/`, `agents/roles/security-*/`,
//! `agents/roles/context-steward/` oder `agents/roles/intel-scout/` — **keine**
//! Änderung an dieser Datei, sondern nur neue TOML-Dateien an der richtigen
//! Stelle im Baum. Die Tests `test_role_discovery_recurses_into_new_directories_and_skips_reserved_ones`
//! und `test_family_discovery_also_recurses_into_the_pluralized_future_root`
//! belegen das an einem synthetischen Baum, ohne `agents/` selbst anzufassen.
//!
//! # Vorrang lokaler Definitionen
//! Eine lokale Definition gewinnt immer: Namen, die bereits in `existing`
//! vorkommen, überspringt [`builtin_agent_definitions`] vollständig — es wird
//! nichts zusammengeführt und nichts überschrieben. Wer eine eingebaute Rolle
//! anpassen will, ersetzt sie damit als Ganzes, statt eine halb gemischte
//! Definition zu erhalten.
//!
//! # Die Basis ist ein eigener Layer
//! `worker-base.toml` ist selbst keine startbare Rolle, wird aber als Layer
//! mitgegeben, damit das `extends` der eingebetteten Rollen auflöst. Gesenkt werden nur
//! die Rollen aus [`crate::role_names::ALL`].
//!
//! # Fehler
//! Alle Fehler dieses Moduls sind
//! [`RegistryDefaultsError::AgentDefinition`] und tragen den Namen der
//! betroffenen Definition sowie den typisierten [`DslError`] als Ursache.
//!
//! # Nebenläufigkeit
//! Alle Funktionen dieses Moduls sind zustandslos und von jedem Thread aus
//! sicher; das einzige I/O ist die Systemuhr für die Auflösungs-Traces. Die
//! Verzeichnis-Sammlung läuft höchstens einmal pro Prozess (gecacht in einem
//! `std::sync::OnceLock`) und ist danach reine Sicht auf `'static`-Daten.

use std::collections::HashMap;
use std::sync::OnceLock;

use harw_agent_dsl::error::DslError;
use harw_agent_dsl::family::{RawFamilyDefinition, resolve_family};
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::organization::{RawOrganizationDefinition, resolve_organization};
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::raw::RawAgentDefinition;
use harw_agent_dsl::resolve::resolve_definition;
use harw_agent_dsl::roles::AgentRoleId;
use harw_agent_dsl::{ExecutableAgentIr, lower};
use include_dir::{Dir, DirEntry, File, include_dir};
use time::OffsetDateTime;

use crate::error::{RegistryDefaultsError, RegistryDefaultsResult};
use crate::profile::role_names;

/// Die aufgelöste Family-Definition, wie [`builtin_family_definitions`] sie liefert.
///
/// Re-Export aus `harw-agent-dsl`, damit ein Konsument dieses Crates die
/// Rückgabe benennen kann, ohne selbst von der DSL-Crate abzuhängen.
pub use harw_agent_dsl::family::ResolvedFamily;
/// Die Organisations-Typen, die [`default_organization`] und [`clan_cell`] liefern.
///
/// Re-Export aus `harw-agent-dsl` aus demselben Grund wie [`ResolvedFamily`]:
/// `harw-ops` konsumiert Clan und Zelle, hängt aber nicht direkt von der
/// DSL-Crate ab.
pub use harw_agent_dsl::organization::{RawCellSpec, RawClanSpec, ResolvedOrganization};

/// Der Name der gemeinsamen Basisdefinition der eingebauten Rollen.
///
/// Sie ist selbst keine startbare Rolle (sie steht nicht in
/// [`role_names::ALL`]), wird aber als Layer benötigt, damit `extends` auflöst.
/// Sie wird von der Verzeichnis-Sammlung wie jede andere Rollen-Datei
/// eingesammelt (siehe Modul-Dokumentation, Abschnitt „Verzeichniskonvention“)
/// — ihr Sonderstatus entsteht erst durch den Namensvergleich in
/// [`builtin_agent_definitions`].
pub const WORKER_BASE_NAME: &str = "worker-base";

// ─── Verzeichnis-Sammlung ────────────────────────────────────────────────

/// Der eingebettete Wurzelbaum von `harw-registry-defaults/agents/`.
///
/// Zur Bauzeit über `include_dir!` eingebettet; zur Laufzeit findet kein
/// Dateizugriff mehr statt. Siehe Modul-Dokumentation, Abschnitt
/// „Verzeichniskonvention“.
static AGENTS_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/agents");

/// Cache für [`builtin_agent_toml`] — der Baum wird nur beim ersten Aufruf durchlaufen.
static AGENT_TOML: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();

/// Cache für [`builtin_family_toml`] — der Baum wird nur beim ersten Aufruf durchlaufen.
static FAMILY_TOML: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();

/// Cache für [`builtin_membership_toml`] — der Baum wird nur beim ersten Aufruf durchlaufen.
static MEMBERSHIP_TOML: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();

/// Root-Unterverzeichnisse, die *nicht* als Agentenrollen eingesammelt werden,
/// weil sie ihre eigene Sammelregel haben.
///
/// `context-programs` kam nachträglich dazu, und der Grund ist lehrreich:
/// AW6-00 hat die Konvention als *jede `.toml` unter `agents/` außer
/// `family/`, `families/`, `organization/`* formuliert, AW6-05 hat seine zehn
/// Kontextprogramme regelkonform unter `agents/context-programs/` abgelegt --
/// und damit wurden aus sechs Rollen achtzehn. **Beide Knoten haben sich an
/// ihre Vorgabe gehalten; die Vorgabe kannte die dritte Kategorie nicht.**
///
/// Ein Kontextprogramm ist keine Rolle: es beschreibt, *was ein Agent sieht*,
/// nicht *wer er ist*. Als Rolle eingesammelt würde es beim Auflösen als
/// startbarer Agent behandelt.
const NON_ROLE_ROOT_DIRS: &[&str] = &["family", "families", "organization", "context-programs"];

/// Root-Unterverzeichnisse, die als Families eingesammelt werden — `family`
/// ist der heutige Name, `families` der für spätere Knoten vorgesehene
/// (siehe Modul-Dokumentation).
const FAMILY_ROOT_DIRS: &[&str] = &["family", "families"];

/// Reservierter Unterverzeichnisname unterhalb einer Family, der
/// Mitgliedschaftsanträge trägt statt einer weiteren Family-Definition
/// (Knoten AW6-02, additiv — siehe [`harw_agent_dsl::family::RawFamilyMembership`]).
///
/// Ein Verzeichnis mit diesem Namen wird von [`discover_family_toml`]
/// übersprungen (es ist keine Family) und stattdessen ausschließlich von
/// [`discover_membership_toml`] eingesammelt. Damit legt ein Folgeknoten
/// (AW6-03, AW6-08, AW7-06) seine Aufnahme in
/// `agents/families/security/members/<rolle>.toml` ab, ohne
/// `agents/families/security/security.toml` selbst anzufassen.
const FAMILY_MEMBERS_DIR_NAME: &str = "members";

/// Wie ein eingebetteter Pfad zu einem Definitionsnamen wird.
///
/// Siehe Modul-Dokumentation, Abschnitt „Verzeichniskonvention“.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefinitionNaming {
    /// Nur das letzte Pfadsegment ohne `.toml` — für Agentenrollen.
    FileStem,
    /// Der volle Pfad relativ zur eingebetteten Wurzel, ohne `.toml` — für Families.
    RelativePath,
}

/// Der eingebettete Pfad einer Datei relativ zur Verzeichniswurzel, ohne die
/// Endung `.toml`.
///
/// `None`, wenn der Pfad nicht auf `.toml` endet oder kein gültiges UTF-8 ist
/// — eine solche Datei wird stillschweigend übersprungen (siehe
/// Modul-Dokumentation).
fn toml_relative_path<'a>(file: &File<'a>) -> Option<&'a str> {
    file.path().to_str()?.strip_suffix(".toml")
}

/// Das letzte Segment eines mit `/` getrennten relativen Pfads.
///
/// Für einen Pfad ohne `/` (Datei direkt unter der Wurzel) ist das der ganze
/// Pfad — das deckt sowohl die heutigen flachen Rollen-Dateien als auch
/// künftige, beliebig tief verschachtelte Rollen-Verzeichnisse ab.
fn file_stem(relative_path: &str) -> &str {
    relative_path.rsplit('/').next().unwrap_or(relative_path)
}

/// Sammelt rekursiv alle `.toml`-Dateien unter `dir` nach der gegebenen Namensregel.
///
/// # Beschreibung
/// Läuft den gesamten Unterbaum von `dir` ab; jede gefundene `.toml`-Datei
/// wird mit dem nach `naming` berechneten Namen und ihrem UTF-8-Inhalt an
/// `out` angehängt. Eine Datei, die kein gültiges UTF-8 enthält, wird
/// übersprungen.
fn collect_toml<'a>(dir: &Dir<'a>, naming: DefinitionNaming, out: &mut Vec<(&'a str, &'a str)>) {
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(sub) => collect_toml(sub, naming, out),
            DirEntry::File(file) => {
                let Some(relative) = toml_relative_path(file) else {
                    continue;
                };
                // `contents_utf8` muss direkt auf dem hier gebundenen `file`
                // aufgerufen werden: `file` trägt bereits den vollen Baum-Typ
                // `&'a File<'a>` aus `dir.entries()`, sodass die andernfalls
                // auf `&self` verkürzte Rückgabe-Lifetime hier exakt `'a`
                // auflöst.
                let Some(contents) = file.contents_utf8() else {
                    continue;
                };
                let name = match naming {
                    DefinitionNaming::FileStem => file_stem(relative),
                    DefinitionNaming::RelativePath => relative,
                };
                out.push((name, contents));
            }
        }
    }
}

/// Sammelt alle eingebetteten Agentenrollen-Dateien (siehe „Verzeichniskonvention“).
///
/// Läuft `root` auf oberster Ebene ab und rekursiert in jedes
/// Unterverzeichnis außer den in [`NON_ROLE_ROOT_DIRS`] reservierten Namen.
fn discover_role_toml<'a>(root: &Dir<'a>) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    for entry in root.entries() {
        match entry {
            DirEntry::File(file) => {
                let Some(relative) = toml_relative_path(file) else {
                    continue;
                };
                // Siehe Kommentar in `collect_toml`: direkter Aufruf auf dem
                // frisch gebundenen `file`, damit die Lifetime `'a` bleibt.
                let Some(contents) = file.contents_utf8() else {
                    continue;
                };
                out.push((file_stem(relative), contents));
            }
            DirEntry::Dir(sub) => {
                let is_reserved = sub
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| NON_ROLE_ROOT_DIRS.contains(&name));
                if !is_reserved {
                    collect_toml(sub, DefinitionNaming::FileStem, &mut out);
                }
            }
        }
    }
    out
}

/// Baut die Rückgabe von [`builtin_agent_toml`]: die Basis an Index 0, danach
/// alle gefundenen Rollen alphabetisch sortiert.
///
/// Die Sortierung ist ein reiner Lesbarkeits-/Stabilitätsentscheid — keine
/// heute eingebettete Datei und kein Konsument hängt an einer bestimmten
/// Reihenfolge der eingebetteten Rollen untereinander, nur die Basis muss zuerst
/// stehen (siehe Test `test_builtin_agent_toml_lists_base_and_all_roles`).
fn discover_agent_toml<'a>(root: &Dir<'a>) -> Vec<(&'a str, &'a str)> {
    let mut found = discover_role_toml(root);
    found.sort_by_key(|(name, _)| *name);
    if let Some(base_index) = found.iter().position(|(name, _)| *name == WORKER_BASE_NAME) {
        let base = found.remove(base_index);
        found.insert(0, base);
    }
    found
}

/// Wie [`collect_toml`], überspringt beim Rekursieren aber jedes
/// Unterverzeichnis namens [`FAMILY_MEMBERS_DIR_NAME`] — ein solches
/// Verzeichnis trägt Mitgliedschaftsanträge (Knoten AW6-02), keine weiteren
/// Family-Definitionen. [`discover_membership_toml`] sammelt genau diese
/// übersprungenen Verzeichnisse separat ein.
fn collect_family_toml<'a>(dir: &Dir<'a>, out: &mut Vec<(&'a str, &'a str)>) {
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(sub) => {
                let is_members_dir = sub
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == FAMILY_MEMBERS_DIR_NAME);
                if !is_members_dir {
                    collect_family_toml(sub, out);
                }
            }
            DirEntry::File(file) => {
                let Some(relative) = toml_relative_path(file) else {
                    continue;
                };
                let Some(contents) = file.contents_utf8() else {
                    continue;
                };
                out.push((relative, contents));
            }
        }
    }
}

/// Sammelt alle eingebetteten Family-Dateien (siehe „Verzeichniskonvention“).
///
/// Durchsucht sowohl `agents/family/` (heutiger Name) als auch
/// `agents/families/` (für spätere Knoten vorgesehen); beide Bäume landen im
/// selben, nach Namen sortierten Ergebnis. Ein Unterverzeichnis namens
/// [`FAMILY_MEMBERS_DIR_NAME`] wird dabei nicht als Family eingesammelt (siehe
/// [`collect_family_toml`]).
fn discover_family_toml<'a>(root: &Dir<'a>) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    for family_root_name in FAMILY_ROOT_DIRS {
        if let Some(family_root) = root.get_dir(family_root_name) {
            collect_family_toml(family_root, &mut out);
        }
    }
    out.sort_by_key(|(name, _)| *name);
    out
}

/// Sammelt alle eingebetteten Mitgliedschafts-Dateien unterhalb eines
/// [`FAMILY_MEMBERS_DIR_NAME`]-Verzeichnisses irgendwo im Family-Baum (Knoten
/// AW6-02).
///
/// # Beschreibung
/// Rekursiert durch `dir`, bis ein Unterverzeichnis namens
/// [`FAMILY_MEMBERS_DIR_NAME`] gefunden wird; darunter werden alle
/// `.toml`-Dateien rekursiv per [`collect_toml`] mit
/// [`DefinitionNaming::RelativePath`] eingesammelt (Name = voller Pfad relativ
/// zur eingebetteten Wurzel, ohne `.toml` — z. B.
/// `"families/security/members/security-triage-1"`).
fn collect_membership_toml<'a>(dir: &Dir<'a>, out: &mut Vec<(&'a str, &'a str)>) {
    for entry in dir.entries() {
        if let DirEntry::Dir(sub) = entry {
            let is_members_dir = sub
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == FAMILY_MEMBERS_DIR_NAME);
            if is_members_dir {
                collect_toml(sub, DefinitionNaming::RelativePath, out);
            } else {
                collect_membership_toml(sub, out);
            }
        }
    }
}

/// Sammelt alle eingebetteten Mitgliedschafts-Dateien (Knoten AW6-02, additiv).
///
/// Durchsucht denselben Family-Baum wie [`discover_family_toml`]
/// (`agents/family/` und `agents/families/`), sammelt aber nur `.toml`-Dateien
/// unterhalb eines [`FAMILY_MEMBERS_DIR_NAME`]-Verzeichnisses. Nach Namen
/// (vollem relativen Pfad) sortiert — dieselbe Stabilitätsregel wie für
/// Families selbst (AW6-00): eine Glob-artige Auflösung, deren Ergebnis von
/// der Dateisystemreihenfolge abhinge, würde Golden-Tests stromabwärts
/// instabil machen.
fn discover_membership_toml<'a>(root: &Dir<'a>) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    for family_root_name in FAMILY_ROOT_DIRS {
        if let Some(family_root) = root.get_dir(family_root_name) {
            collect_membership_toml(family_root, &mut out);
        }
    }
    out.sort_by_key(|(name, _)| *name);
    out
}

/// Die eingebauten Mitgliedschafts-Dateien als (Name, TOML-Quelltext) (Knoten
/// AW6-02).
///
/// # Rückgabe
/// Ein statischer, nach Namen sortierter Ausschnitt aller eingebetteten
/// Mitgliedschafts-Dateien. Heute leer (kein `members/`-Verzeichnis existiert
/// im ausgelieferten Baum) — [`builtin_family_definitions`] behandelt eine
/// leere Liste identisch zu "keine Mitgliedschaften", nicht als Fehler.
///
/// # Nebenläufigkeit
/// Der Baum wird höchstens einmal pro Prozess durchlaufen ([`OnceLock`]);
/// danach ist der Zugriff eine reine, threadsichere Sicht auf `'static`-Daten.
#[must_use]
pub fn builtin_membership_toml() -> &'static [(&'static str, &'static str)] {
    MEMBERSHIP_TOML
        .get_or_init(|| discover_membership_toml(&AGENTS_DIR))
        .as_slice()
}

/// Parst eine eingebettete Mitgliedschafts-Definition.
///
/// # Errors
/// - [`RegistryDefaultsError::AgentDefinition`]: der TOML-Quelltext ist
///   syntaktisch fehlerhaft oder passt nicht auf
///   [`harw_agent_dsl::family::RawFamilyMembership`].
fn parse_membership_toml(
    name: &str,
    source: &str,
) -> Result<harw_agent_dsl::family::RawFamilyMembership, RegistryDefaultsError> {
    toml::from_str::<harw_agent_dsl::family::RawFamilyMembership>(source)
        .map_err(|error| definition_error(name, dsl_error_from_toml(error.to_string())))
}

/// Prüft, ob die Basis unter den eingebetteten Agentendefinitionen vorkommt.
///
/// # Errors
/// - [`RegistryDefaultsError::AgentDefinition`]: `worker-base.toml` fehlt im
///   eingebetteten Baum. Ohne sie kann kein `extends` der eingebetteten Rollen
///   auflösen; ohne diese Prüfung würde der Fehler stattdessen tief in
///   [`resolve_definition`] auftauchen und nicht [`WORKER_BASE_NAME`] nennen.
fn require_worker_base(entries: &[(&str, &str)]) -> Result<(), RegistryDefaultsError> {
    if entries.iter().any(|(name, _)| *name == WORKER_BASE_NAME) {
        Ok(())
    } else {
        Err(definition_error(
            WORKER_BASE_NAME,
            DslError::Parse(format!(
                "'{WORKER_BASE_NAME}.toml' fehlt im eingebetteten Verzeichnisbaum agents/ — die Basis wird für 'extends' benötigt"
            )),
        ))
    }
}

/// Prüft, dass kein Name in `entries` mehrfach vorkommt.
///
/// # Beschreibung
/// Vor AW6-00 konnte eine Namenskollision nicht entstehen: ein Mensch pflegte
/// die `include_str!`-Liste von Hand und hätte denselben Schlüssel nie zweimal
/// eingetragen. Die Verzeichnis-Sammlung leitet Namen dagegen aus dem
/// Dateisystem ab ([`file_stem`] für Rollen, der volle relative Pfad für
/// Families) — zwei verschiedene Dateien können jetzt denselben Namen ergeben,
/// etwa ein versehentliches `agents/roles/foo/explorer.toml` neben dem
/// bestehenden `agents/explorer.toml`. Ohne diese Prüfung würde
/// [`resolve_definition`]/[`resolve_family`] die zweite Datei stillschweigend
/// als Overlay über die erste legen (beide landen im selben
/// [`DefinitionLayer::BuiltIn`] und werden dort wie aufeinanderfolgende
/// Schichten desselben Ziels behandelt) — ein stiller Vorrang, nicht der
/// harte Fehler, den dieser Knoten für eine neue Fehlerklasse verlangt.
///
/// # Arguments
/// - `entries` (`&[(&str, &str)]`): die gesammelten (Name, TOML-Quelltext)-Paare.
/// - `kind` (`&str`): Bezeichner für die Fehlermeldung, z. B. `"Agentenrolle"`
///   oder `"Family"` — macht die Meldung ohne Rätselraten lesbar.
///
/// # Returns
/// `Ok(())`, wenn jeder Name höchstens einmal vorkommt.
///
/// # Errors
/// - [`RegistryDefaultsError::AgentDefinition`]: mindestens ein Name kommt
///   mehrfach vor; der Fehler nennt den betroffenen Namen.
///
/// # Examples
/// ```rust,ignore
/// // Intern verwendet von `builtin_agent_definitions`/`builtin_family_definitions`;
/// // siehe die Tests `test_duplicate_role_name_is_a_hard_error_not_silent_precedence`
/// // und `test_duplicate_family_name_is_a_hard_error`.
/// ```
fn reject_duplicate_names(
    entries: &[(&str, &str)],
    kind: &str,
) -> Result<(), RegistryDefaultsError> {
    let mut seen: Vec<&str> = Vec::with_capacity(entries.len());
    for (name, _) in entries {
        if seen.contains(name) {
            return Err(definition_error(
                name,
                DslError::Parse(format!(
                    "mehrere {kind}-Dateien deklarieren denselben Namen '{name}' — das ist eine Namenskollision, kein stiller Vorrang"
                )),
            ));
        }
        seen.push(name);
    }
    Ok(())
}

/// Die eingebauten Agentendefinitionen als (Name, TOML-Quelltext).
///
/// # Beschreibung
/// Der Name ist der Rollenname aus [`role_names`] bzw. [`WORKER_BASE_NAME`] für
/// die Basis; er ist zugleich der Schlüssel, unter dem eine lokale Definition
/// aus `.harw/agents/` dieselbe Rolle ersetzt. Der TOML-Quelltext ist zur
/// Bauzeit über `include_dir!` eingebettet und wird beim ersten Aufruf aus dem
/// Verzeichnisbaum entdeckt (siehe Modul-Dokumentation, Abschnitt
/// „Verzeichniskonvention“) — zur Laufzeit findet kein Dateizugriff statt.
///
/// # Rückgabe
/// Ein statischer Ausschnitt aller eingebetteten Definitionen: zuerst die
/// Basis, dann die gefundenen Rollen in alphabetischer Reihenfolge.
///
/// # Nebenläufigkeit
/// Der Baum wird höchstens einmal pro Prozess durchlaufen ([`OnceLock`]);
/// danach ist der Zugriff eine reine, threadsichere Sicht auf `'static`-Daten.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::embedded_agents::builtin_agent_toml;
///
/// let names: Vec<&str> = builtin_agent_toml().iter().map(|(name, _)| *name).collect();
/// assert!(names.contains(&"explorer"));
/// // Bewusst keine feste Zahl: jede neue Definitionsdatei vergrößert die
/// // Liste, und ein Beispiel, das daran bricht, ist die abgeschaffte
/// // `include_str!`-Liste in Doktest-Form.
/// assert!(names.contains(&"worker-base"));
/// assert!(builtin_agent_toml().len() >= 6);
/// ```
#[must_use]
pub fn builtin_agent_toml() -> &'static [(&'static str, &'static str)] {
    AGENT_TOML.get_or_init(|| discover_agent_toml(&AGENTS_DIR)).as_slice()
}

/// Senkt die eingebauten Rollen zu [`ExecutableAgentIr`].
///
/// # Beschreibung
/// Parst alle eingebetteten Definitionen (inklusive der Basis) zu einem
/// gemeinsamen [`DefinitionLayer::BuiltIn`]-Schichtstapel und löst daraus jede
/// Rolle aus [`role_names::ALL`] auf. Lokale Definitionen aus `.harw/agents/`
/// gewinnen: Namen, die in `existing` vorkommen, werden übersprungen.
///
/// Die Basis `worker-base` erscheint nie im Ergebnis — sie ist ein Layer, keine
/// startbare Rolle.
///
/// # Argumente
/// - `existing` (`&HashMap<String, ExecutableAgentIr>`): bereits geladene
///   lokale Definitionen, geschlüsselt nach Rollennamen. Nur gelesen.
///
/// # Rückgabe
/// `Ok(map)` mit einem Eintrag je eingebauter Rolle, die `existing` nicht
/// bereits belegt.
///
/// # Fehler
/// - [`RegistryDefaultsError::AgentDefinition`]: `worker-base.toml` fehlt im
///   eingebetteten Baum, zwei Dateien deklarieren denselben Rollennamen
///   (siehe [`reject_duplicate_names`]), eine eingebettete Definition ist
///   syntaktisch fehlerhaft, ihre Basis fehlt, sie verletzt die
///   Authority-Monotonie oder das Senken schlägt fehl. Der Fehler benennt die
///   betroffene Definition und trägt den [`DslError`] als Ursache.
///
/// # Nebenläufigkeit
/// Zustandslos; von jedem Thread aus sicher. Kein I/O außer der Systemuhr für
/// die Trace-Zeitstempel.
///
/// # Beispiele
/// ```rust
/// use std::collections::HashMap;
/// use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
///
/// let builtin = builtin_agent_definitions(&HashMap::new())?;
/// // Keine feste Anzahl hier: sie wüchse mit jeder neuen Rolle unter
/// // `agents/roles/` mit und stünde dann nur noch neben der Liste, die sie
/// // zählt — das ist keine Prüfung, sondern eine Wiederholung, die beim
/// // nächsten Rollenzuwachs wieder rot wird (siehe `role_names::ALL`, das
/// // aus demselben Grund gegen eine unabhängige Quelle statt gegen eine
/// // Zahl getestet wird). Die eigentliche Aussage dieses Beispiels ist die
/// // Mitgliedschaft: eine Rolle ist enthalten, die Basis-Vorlage nicht.
/// assert!(builtin.contains_key("explorer"));
/// assert!(!builtin.contains_key("worker-base"));
/// # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
/// ```
pub fn builtin_agent_definitions(
    existing: &HashMap<String, ExecutableAgentIr>,
) -> Result<HashMap<String, ExecutableAgentIr>, RegistryDefaultsError> {
    require_worker_base(builtin_agent_toml())?;
    reject_duplicate_names(builtin_agent_toml(), "Agentenrolle")?;

    let now = OffsetDateTime::now_utc();

    // 1. Alle eingebetteten Definitionen parsen. Auch die Basis wandert in den
    //    Schichtstapel, damit `extends` der eingebetteten Rollen auflösen kann.
    let mut layers: Vec<(DefinitionLayer, RawAgentDefinition)> =
        Vec::with_capacity(builtin_agent_toml().len());
    let mut targets: Vec<(&'static str, DefinitionId)> = Vec::with_capacity(role_names::ALL.len());

    for (name, source) in builtin_agent_toml() {
        let raw = parse_toml(source).map_err(|error| definition_error(name, error))?;
        if role_names::ALL.contains(name) {
            targets.push((*name, raw.id.clone()));
        }
        layers.push((DefinitionLayer::BuiltIn, raw));
    }

    // 2. Nur die startbaren Rollen auflösen und senken — und nur, wenn keine
    //    lokale Definition denselben Namen belegt.
    let mut definitions = HashMap::with_capacity(targets.len());
    for (name, id) in targets {
        if existing.contains_key(name) {
            continue;
        }
        let resolved =
            resolve_definition(&id, &layers, now).map_err(|error| definition_error(name, error))?;
        let ir = lower(&resolved).map_err(|error| definition_error(name, error))?;
        definitions.insert(name.to_owned(), ir);
    }

    Ok(definitions)
}

/// Baut den typisierten Fehler für eine benannte eingebaute Definition.
///
/// Die Variante heißt `AgentDefinition`, trägt hier aber auch Family- und
/// Organisationsfehler: alle drei sind Defekte einer eingebetteten TOML-Datei,
/// nie eine Nutzereingabe, und `name` benennt die betroffene Datei
/// (`"explorer"`, `"family/research"`, `"organization/default"`).
fn definition_error(name: &str, source: DslError) -> RegistryDefaultsError {
    RegistryDefaultsError::AgentDefinition {
        name: name.to_owned(),
        source,
    }
}

// ─── Families und Organisation ──────────────────────────────────────────────

/// Der Schlüssel der eingebauten Research-Family in [`builtin_family_toml`].
pub const RESEARCH_FAMILY_NAME: &str = "family/research";

/// Der Schlüssel der eingebauten Coding-Family in [`builtin_family_toml`].
pub const CODING_FAMILY_NAME: &str = "family/coding";

/// Der Name der eingebauten Default-Organisation in Fehlermeldungen.
pub const DEFAULT_ORGANIZATION_NAME: &str = "organization/default";

/// Der eingebettete Pfad der Default-Organisation, relativ zu `agents/`.
///
/// Anders als Rollen und Families ist die Organisation fest über diesen Pfad
/// nachgeschlagen statt eingesammelt (siehe Modul-Dokumentation, Abschnitt
/// „Verzeichniskonvention“): der Plan kennt bislang keine zweite
/// Organisationsdatei.
const DEFAULT_ORGANIZATION_PATH: &str = "organization/default.toml";

/// Die Namensraum-ID der eingebauten Default-Organisation (§5).
///
/// [`default_organization`] prüft, dass die eingebettete TOML-Datei genau diese
/// ID trägt — Konstante und Datei können damit nicht auseinanderlaufen.
pub const DEFAULT_ORGANIZATION_ID: &str = "harwness.organization.default@1";

/// Die Clan-ID der Recherche-/Analysespur der Default-Organisation.
///
/// Ein Konsument, der Plan-Knoten für diesen Clan anlegt, bildet ihre `TaskId`
/// aus dieser Konstante (`research-<name>`), damit sie in `plan_scope` und
/// `members_from_plan` des Clans fallen.
pub const RESEARCH_CLAN_ID: &str = "research";

/// Die Clan-ID der Implementierungsspur der Default-Organisation.
pub const CODING_CLAN_ID: &str = "coding";

/// Die Clan-ID der Verdichtungsspur der Default-Organisation.
pub const SYNTHESIS_CLAN_ID: &str = "synthesis";

/// Die eingebauten Family-Definitionen als (Name, TOML-Quelltext).
///
/// # Beschreibung
/// Der Name ist der Schlüssel, unter dem [`builtin_family_definitions`] die
/// aufgelöste Family ablegt ([`RESEARCH_FAMILY_NAME`], [`CODING_FAMILY_NAME`]).
/// Der TOML-Quelltext ist zur Bauzeit über `include_dir!` eingebettet und wird
/// beim ersten Aufruf aus `agents/family/` (und, für spätere Knoten,
/// `agents/families/`) entdeckt — zur Laufzeit findet kein Dateizugriff statt.
///
/// # Rückgabe
/// Ein statischer Ausschnitt aller eingebetteten Family-Definitionen, nach
/// Namen sortiert. Kein Konsument hängt an einer bestimmten Reihenfolge
/// zwischen Families — nur an der Menge der Namen.
///
/// # Nebenläufigkeit
/// Der Baum wird höchstens einmal pro Prozess durchlaufen ([`OnceLock`]);
/// danach ist der Zugriff eine reine, threadsichere Sicht auf `'static`-Daten.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::embedded_agents::{builtin_family_toml, RESEARCH_FAMILY_NAME};
///
/// let names: Vec<&str> = builtin_family_toml().iter().map(|(name, _)| *name).collect();
/// assert!(names.contains(&RESEARCH_FAMILY_NAME));
/// ```
#[must_use]
pub fn builtin_family_toml() -> &'static [(&'static str, &'static str)] {
    FAMILY_TOML.get_or_init(|| discover_family_toml(&AGENTS_DIR)).as_slice()
}

/// Der TOML-Quelltext der eingebauten Default-Organisation.
///
/// # Rückgabe
/// Der zur Bauzeit über `include_dir!` eingebettete Inhalt von
/// `harw-registry-defaults/agents/organization/default.toml`, abgerufen über
/// den festen Pfad [`DEFAULT_ORGANIZATION_PATH`]. Fehlte die Datei im
/// eingebetteten Baum — was der heutige Inhalt von `agents/` ausschließt —,
/// läge eine leere Zeichenkette vor; [`default_organization`] scheitert dann
/// beim Parsen und nennt [`DEFAULT_ORGANIZATION_NAME`] als betroffene
/// Definition, statt mit `panic!` abzubrechen.
///
/// # Nebenläufigkeit
/// Rein; liefert nur einen `'static`-Verweis.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::embedded_agents::builtin_organization_toml;
///
/// assert!(builtin_organization_toml().contains("harwness.organization/v1"));
/// ```
#[must_use]
pub fn builtin_organization_toml() -> &'static str {
    match AGENTS_DIR.get_file(DEFAULT_ORGANIZATION_PATH) {
        // `contents_utf8` wird direkt auf `file` aufgerufen (siehe Kommentar
        // in `collect_toml`), damit die Rückgabe die volle `'static`-Lifetime
        // von `AGENTS_DIR` behält statt auf die Lebensdauer von `&self`
        // verkürzt zu werden.
        Some(file) => file.contents_utf8().unwrap_or_default(),
        None => "",
    }
}

/// Löst die eingebauten Families über die Layer-Kaskade auf (§14).
///
/// # Beschreibung
/// Parst alle Einträge aus [`builtin_family_toml`] zu einem gemeinsamen
/// [`DefinitionLayer::BuiltIn`]-Schichtstapel und löst jede Family daraus auf.
/// Da alle eingebauten Families im selben Layer liegen und keine `extends`
/// führen, ist das Ergebnis je Family ihre eigene Definition mit einem
/// einzigen Trace-Schritt.
///
/// # Rückgabe
/// `Ok(map)` mit einem Eintrag je eingebauter Family, geschlüsselt nach dem
/// Namen aus [`builtin_family_toml`].
///
/// # Fehler
/// - [`RegistryDefaultsError::AgentDefinition`]: zwei Dateien deklarieren
///   denselben Family-Namen (siehe [`reject_duplicate_names`] — bei der
///   heutigen pfadbasierten Namensregel strukturell ausgeschlossen, aber
///   ebenso geprüft wie bei Rollen, falls sich die Regel je ändert), eine
///   eingebettete Family ist syntaktisch fehlerhaft oder ihre `extends`-Basis
///   fehlt. Der Fehler benennt die betroffene Datei und trägt den
///   [`DslError`] als Ursache.
///
/// # Nebenläufigkeit
/// Zustandslos; von jedem Thread aus sicher. Kein I/O außer der Systemuhr für
/// die Trace-Zeitstempel.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::embedded_agents::{builtin_family_definitions, CODING_FAMILY_NAME};
///
/// let families = builtin_family_definitions()?;
/// assert!(families.contains_key(CODING_FAMILY_NAME));
/// # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
/// ```
pub fn builtin_family_definitions() -> RegistryDefaultsResult<HashMap<String, ResolvedFamily>> {
    reject_duplicate_names(builtin_family_toml(), "Family")?;

    let now = OffsetDateTime::now_utc();

    let mut layers: Vec<(DefinitionLayer, RawFamilyDefinition)> =
        Vec::with_capacity(builtin_family_toml().len());
    let mut targets: Vec<(&'static str, DefinitionId)> =
        Vec::with_capacity(builtin_family_toml().len());

    for (name, source) in builtin_family_toml() {
        let raw = parse_family_toml(name, source)?;
        targets.push((*name, raw.id.clone()));
        layers.push((DefinitionLayer::BuiltIn, raw));
    }

    // Mitgliedschaften (Knoten AW6-02, additiv): jede Datei unter einem
    // `members/`-Verzeichnis meldet die Aufnahme genau einer Rolle in genau
    // eine Familie, geprüft gegen deren `universe`. Heute ist diese Liste
    // leer — kein `members/`-Verzeichnis existiert im ausgelieferten Baum —
    // und `merge_memberships_into` ist dann ein No-Op je Familie.
    let mut memberships: Vec<harw_agent_dsl::family::RawFamilyMembership> =
        Vec::with_capacity(builtin_membership_toml().len());
    for (name, source) in builtin_membership_toml() {
        memberships.push(parse_membership_toml(name, source)?);
    }

    let mut families = HashMap::with_capacity(targets.len());
    for (name, id) in targets {
        let mut resolved =
            resolve_family(&id, &layers, now).map_err(|error| definition_error(name, error))?;
        harw_agent_dsl::family::merge_memberships_into(&mut resolved, &memberships)
            .map_err(|error| definition_error(name, error))?;
        families.insert(name.to_owned(), resolved);
    }
    Ok(families)
}

/// Löst die eingebaute Default-Organisation auf (§15, §22).
///
/// # Beschreibung
/// Parst [`builtin_organization_toml`], prüft die deklarierte ID gegen
/// [`DEFAULT_ORGANIZATION_ID`] und löst die Organisation über einen
/// [`DefinitionLayer::BuiltIn`]-Stapel auf. Dabei greifen die Invarianten aus
/// §22: doppelte `clan.id` und Zellen ohne existierenden Clan sind Fehler.
///
/// Die Funktion validiert **nicht**, dass die referenzierten Agenten- und
/// Family-Definitionen existieren — das tut die DSL an dieser Stelle nicht, und
/// es wäre hier auch die falsche Ebene. Der Test
/// `test_organization_references_only_embedded_roles` hält die Referenzen
/// stattdessen an [`role_names::ALL`] gebunden.
///
/// # Rückgabe
/// `Ok(ResolvedOrganization)` mit Root, Clans und Zellen.
///
/// # Fehler
/// - [`RegistryDefaultsError::AgentDefinition`]: die eingebettete TOML-Datei ist
///   syntaktisch fehlerhaft, trägt eine andere ID als
///   [`DEFAULT_ORGANIZATION_ID`], oder die Auflösung verletzt eine §22-Invariante.
///
/// # Nebenläufigkeit
/// Zustandslos; von jedem Thread aus sicher. Kein I/O außer der Systemuhr für
/// die Trace-Zeitstempel.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::embedded_agents::{default_organization, RESEARCH_CLAN_ID};
///
/// let organization = default_organization()?;
/// assert!(organization.clans.iter().any(|clan| clan.id == RESEARCH_CLAN_ID));
/// # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
/// ```
pub fn default_organization() -> RegistryDefaultsResult<ResolvedOrganization> {
    let now = OffsetDateTime::now_utc();

    let raw = parse_organization_toml(DEFAULT_ORGANIZATION_NAME, builtin_organization_toml())?;
    let expected = DefinitionId::parse(DEFAULT_ORGANIZATION_ID)
        .map_err(|error| definition_error(DEFAULT_ORGANIZATION_NAME, error))?;
    if raw.id != expected {
        return Err(definition_error(
            DEFAULT_ORGANIZATION_NAME,
            DslError::Parse(format!(
                "die eingebettete Organisation trägt die ID '{}', erwartet war '{}'",
                raw.id, expected
            )),
        ));
    }

    let layers = vec![(DefinitionLayer::BuiltIn, raw)];
    resolve_organization(&expected, &layers, now)
        .map_err(|error| definition_error(DEFAULT_ORGANIZATION_NAME, error))
}

/// Sucht einen Clan und die erste ihm zugeordnete Zelle.
///
/// # Beschreibung
/// Die Zell-Auflösung (`harw_plan_bridge::CellPlan::from_cell`) braucht beides
/// gemeinsam: die Zelle liefert Muster, Barriere und Schreibtrennung, der Clan
/// grenzt die Auswahl über seinen `plan_scope` ein. Diese Funktion ist der
/// kürzeste Weg von der aufgelösten Organisation zu genau diesem Paar.
///
/// Gibt es mehrere Zellen für denselben Clan, gewinnt die erste in
/// Deklarationsreihenfolge; die eingebaute Organisation führt genau eine Zelle
/// je Clan.
///
/// # Argumente
/// - `organization` (`&ResolvedOrganization`): die aufgelöste Organisation; nur gelesen.
/// - `clan_id` (`&str`): die gesuchte Clan-ID, z. B. [`RESEARCH_CLAN_ID`].
///
/// # Rückgabe
/// `Some((clan, cell))`, wenn der Clan existiert **und** eine Zelle auf ihn
/// verweist; sonst `None`. Ein Aufrufer muss `None` behandeln — es ist der
/// normale Fall für eine Organisation, die diese Spur nicht kennt, kein Fehler.
///
/// # Nebenläufigkeit
/// Rein; keine Seiteneffekte.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::embedded_agents::{clan_cell, default_organization, RESEARCH_CLAN_ID};
///
/// let organization = default_organization()?;
/// let found = clan_cell(&organization, RESEARCH_CLAN_ID);
/// assert!(found.is_some());
/// assert!(clan_cell(&organization, "gibt-es-nicht").is_none());
/// # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
/// ```
#[must_use]
pub fn clan_cell<'a>(
    organization: &'a ResolvedOrganization,
    clan_id: &str,
) -> Option<(&'a RawClanSpec, &'a RawCellSpec)> {
    let clan = organization.clans.iter().find(|clan| clan.id == clan_id)?;
    let cell = organization
        .cells
        .iter()
        .find(|cell| cell.clan == clan.id)?;
    Some((clan, cell))
}

/// Parst eine eingebettete Family-Definition.
///
/// # Errors
/// - [`RegistryDefaultsError::AgentDefinition`]: der TOML-Quelltext ist
///   syntaktisch fehlerhaft oder passt nicht auf [`RawFamilyDefinition`].
fn parse_family_toml(
    name: &str,
    source: &str,
) -> Result<RawFamilyDefinition, RegistryDefaultsError> {
    toml::from_str::<RawFamilyDefinition>(source)
        .map_err(|error| definition_error(name, dsl_error_from_toml(error.to_string())))
}

/// Parst eine eingebettete Organisationsdefinition.
///
/// # Errors
/// - [`RegistryDefaultsError::AgentDefinition`]: der TOML-Quelltext ist
///   syntaktisch fehlerhaft oder passt nicht auf [`RawOrganizationDefinition`].
fn parse_organization_toml(
    name: &str,
    source: &str,
) -> Result<RawOrganizationDefinition, RegistryDefaultsError> {
    toml::from_str::<RawOrganizationDefinition>(source)
        .map_err(|error| definition_error(name, dsl_error_from_toml(error.to_string())))
}

/// Ordnet eine TOML-Fehlermeldung derselben Aufteilung zu wie
/// `harw_agent_dsl::parse::parse_toml`: Syntaxfehler werden zu
/// [`DslError::Parse`], Strukturfehler zu [`DslError::Toml`].
fn dsl_error_from_toml(message: String) -> DslError {
    if message.contains("TOML parse error") || message.contains("unexpected") {
        DslError::Parse(message)
    } else {
        DslError::Toml(message)
    }
}

// ─── Organisatorisches Regelwerk (Addendum D+E) ─────────────────────────────

/// Regelwerk-Text der UIA (`knowledge/roles/uia.md`).
const UIA_KNOWLEDGE: &str = include_str!("../knowledge/roles/uia.md");
/// Regelwerk-Text des Root-Orchestrators (`knowledge/roles/root-orchestrator.md`).
const ROOT_ORCHESTRATOR_KNOWLEDGE: &str = include_str!("../knowledge/roles/root-orchestrator.md");
/// Regelwerk-Text eines Sub-/Child-Orchestrators (`knowledge/roles/sub-orchestrator.md`).
const SUB_ORCHESTRATOR_KNOWLEDGE: &str = include_str!("../knowledge/roles/sub-orchestrator.md");
/// Regelwerk-Text eines Workers (`knowledge/roles/worker.md`), gilt für alle
/// eingebauten Agentenrollen mit organisatorischer Rolle `AgentRoleId::Worker`
/// (siehe `test_every_builtin_toml_parses`).
const WORKER_KNOWLEDGE: &str = include_str!("../knowledge/roles/worker.md");
/// Regelwerk-Text des `uia-worker` (`knowledge/roles/uia-worker.md`,
/// Addendum J): eigene, vollständig abgekapselte Organisationsrolle
/// (`AgentRoleId::UiaWorker`), kein Abschnitt in `worker.md` mehr.
const UIA_WORKER_KNOWLEDGE: &str = include_str!("../knowledge/roles/uia-worker.md");
/// Regelwerk-Text des `agent-steward` (`knowledge/roles/agent-steward.md`,
/// Addendum K + Nachtrag K/K2): eigene Organisationsrolle
/// (`AgentRoleId::AgentSteward`), setzt Agentendefinitionen und
/// UIA-Bündel um, validiert immer vor dem Schreiben, endet bei einem
/// Root-gestarteten Lauf als Vorschlag statt als sofortiger Commit.
const AGENT_STEWARD_KNOWLEDGE: &str = include_str!("../knowledge/roles/agent-steward.md");

/// Organisationswissen (Addendum K): Hierarchie, Zuständigkeiten, Delegation.
const ORGANIZATION_KNOWLEDGE: &str =
    include_str!("../knowledge/organization/agent-organization.md");
/// Bauplan-Wissen für Agentendefinitionen (Addendum K + Nachtrag K).
const AUTHORING_KNOWLEDGE: &str = include_str!("../knowledge/authoring/agent-authoring.md");

/// Liefert den eingebauten Regelwerk-Text für eine organisatorische Rolle
/// (Addendum D+E).
///
/// # Beschreibung
/// Bildet [`AgentRoleId`] auf ihren statischen Regelwerk-Text ab, der zur
/// Bauzeit über `include_str!` aus `harw-registry-defaults/knowledge/roles/`
/// eingebettet ist — außerhalb von `agents/`, also nicht Teil der
/// Verzeichnis-Sammlung dieser Datei. Der Aufrufer (z. B.
/// [`crate::profile::assemble_registry_for_sandbox`]) reicht das Ergebnis an
/// [`harw_instructions::AgentIdentity::with_organization_knowledge`] weiter.
///
/// # Argumente
/// - `role` (`AgentRoleId`): die organisatorische Rolle im Agenten-Baum.
///
/// # Rückgabe
/// Der statische, stabile Regelwerk-Text der Rolle (cachebares
/// Prompt-Präfix — kein Zeitstempel, keine Laufzeitdaten).
///
/// # Nebenläufigkeit
/// Rein; liefert nur einen `'static`-Verweis, von jedem Thread aus sicher.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::roles::AgentRoleId;
/// use harw_registry_defaults::embedded_agents::builtin_role_knowledge;
///
/// let text = builtin_role_knowledge(AgentRoleId::Worker);
/// assert!(text.contains("Worker"));
/// ```
#[must_use]
pub fn builtin_role_knowledge(role: AgentRoleId) -> &'static str {
    match role {
        AgentRoleId::UserInterface => UIA_KNOWLEDGE,
        AgentRoleId::RootOrchestrator => ROOT_ORCHESTRATOR_KNOWLEDGE,
        AgentRoleId::ChildOrchestrator => SUB_ORCHESTRATOR_KNOWLEDGE,
        AgentRoleId::Worker => WORKER_KNOWLEDGE,
        AgentRoleId::UiaWorker => UIA_WORKER_KNOWLEDGE,
        AgentRoleId::AgentSteward => AGENT_STEWARD_KNOWLEDGE,
    }
}

/// Liefert das vollständige Regelwerk für eine organisatorische Rolle,
/// inklusive des einkompilierten Organisations- und Bauplan-Wissens
/// (Addendum K).
///
/// # Beschreibung
/// Setzt sich für jede Rolle unterschiedlich zusammen: die UIA und
/// `agent-steward` bekommen Organisationswissen **und** den
/// Agentendefinitions-Bauplan **und** ihre eigene Rollenregel angehängt (sie
/// beraten bzw. setzen Agentendefinitionen um); Root- und
/// Child-Orchestrator bekommen Organisationswissen plus Rollenregel, aber
/// keinen Bauplan (sie schreiben selbst keine Agentendefinitionen); Worker
/// und `uia-worker` bekommen ausschließlich ihre Rollenregel — sie
/// delegieren nie und brauchen deshalb kein Organisationswissen. Der
/// Aufrufer ([`crate::profile::assemble_registry_for_sandbox`]) reicht das
/// Ergebnis an [`harw_instructions::AgentIdentity::with_organization_knowledge`]
/// weiter, anstelle von [`builtin_role_knowledge`] direkt.
///
/// # Argumente
/// - `role` (`AgentRoleId`): die organisatorische Rolle im Agenten-Baum.
///
/// # Rückgabe
/// Der zusammengesetzte, cachbare Regelwerk-Text der Rolle (kein
/// Zeitstempel, keine Laufzeitdaten — nur zur Laufzeit zusammengefügte,
/// zur Bauzeit eingebettete Bausteine).
///
/// # Nebenläufigkeit
/// Rein; jeder Thread darf gleichzeitig aufrufen.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::roles::AgentRoleId;
/// use harw_registry_defaults::embedded_agents::builtin_organization_knowledge;
///
/// let uia_text = builtin_organization_knowledge(AgentRoleId::UserInterface);
/// assert!(uia_text.contains("agent-steward"));
/// let worker_text = builtin_organization_knowledge(AgentRoleId::Worker);
/// assert!(!worker_text.contains("harwness.knowledge.agent-organization"));
/// ```
#[must_use]
pub fn builtin_organization_knowledge(role: AgentRoleId) -> String {
    match role {
        AgentRoleId::UserInterface | AgentRoleId::AgentSteward => {
            format!(
                "{ORGANIZATION_KNOWLEDGE}\n\n{AUTHORING_KNOWLEDGE}\n\n{}",
                builtin_role_knowledge(role)
            )
        }
        AgentRoleId::RootOrchestrator | AgentRoleId::ChildOrchestrator => {
            format!("{ORGANIZATION_KNOWLEDGE}\n\n{}", builtin_role_knowledge(role))
        }
        AgentRoleId::Worker | AgentRoleId::UiaWorker => builtin_role_knowledge(role).to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use harw_agent_dsl::organization::{CellBarrier, CellKind, CellWritePartition};

    fn builtin() -> HashMap<String, ExecutableAgentIr> {
        builtin_agent_definitions(&HashMap::new()).expect("eingebaute Definitionen müssen lowern")
    }

    #[test]
    fn test_builtin_agent_toml_lists_base_and_all_roles() {
        let names: Vec<&str> = builtin_agent_toml().iter().map(|(name, _)| *name).collect();
        assert_eq!(names[0], WORKER_BASE_NAME, "die Basis muss zuerst stehen");
        for role in role_names::ALL {
            assert!(names.contains(role), "fehlt: {role}");
        }
        // Bewusst **keine** Gleichheit auf die Länge: `role_names::ALL` ist
        // die Liste der ursprünglich fest verdrahteten Rollen, und jede neue
        // Definitionsdatei (AW6-08 `context-steward`, AW7-06 `intel-scout`,
        // ...) darf sie vergrößern. Ein Test, der bei jeder neuen Rolle
        // bricht, ist die `include_str!`-Liste in Testform -- genau das, was
        // AW6-00 abgeschafft hat. Geprüft wird, dass nichts **fehlt**.
        assert!(
            names.len() > role_names::ALL.len(),
            "keine der fest verdrahteten Rollen darf verschwinden"
        );
    }

    #[test]
    fn test_every_builtin_toml_parses() {
        for (name, source) in builtin_agent_toml() {
            let raw = parse_toml(source)
                .unwrap_or_else(|error| panic!("{name} parst nicht: {error}"));
            assert_eq!(raw.schema, "harwness.agent/v1", "{name}");
            assert!(
                matches!(
                    raw.role,
                    harw_agent_dsl::roles::AgentRoleId::Worker
                        | harw_agent_dsl::roles::AgentRoleId::UiaWorker
                        | harw_agent_dsl::roles::AgentRoleId::AgentSteward
                ),
                "{name}: eingebaute Rollen tragen organisatorisch Worker, UiaWorker oder \
                 AgentSteward (Addendum J + K)"
            );
            assert_eq!(raw.specialization, *name, "{name}");
        }
    }

    #[test]
    fn test_builtin_definitions_lower_every_role_but_not_the_base() {
        let definitions = builtin();
        assert_eq!(definitions.len(), role_names::ALL.len());
        for role in role_names::ALL {
            assert!(definitions.contains_key(*role), "fehlt: {role}");
        }
        assert!(
            !definitions.contains_key(WORKER_BASE_NAME),
            "die Basis ist keine startbare Rolle"
        );
    }

    #[test]
    fn test_worker_base_is_a_layer_not_a_startable_role() {
        // Verfügbar als Layer: die Basis steht in der Rohliste, die
        // `builtin_agent_definitions` an `extends` weitergibt …
        assert!(
            builtin_agent_toml()
                .iter()
                .any(|(name, _)| *name == WORKER_BASE_NAME),
            "worker-base muss als Layer in der Rohliste stehen, damit 'extends' auflöst"
        );
        // … ist aber keine startbare Rolle: weder in `role_names::ALL` …
        assert!(!role_names::ALL.contains(&WORKER_BASE_NAME));
        // … noch im Ergebnis von `builtin_agent_definitions`.
        assert!(!builtin().contains_key(WORKER_BASE_NAME));
    }

    #[test]
    fn test_require_worker_base_names_the_file_when_missing() {
        let entries: [(&str, &str); 1] = [(role_names::EXPLORER, "irrelevant")];
        let error =
            require_worker_base(&entries).expect_err("ohne Basis darf 'extends' nicht auflösen");
        assert!(
            error.to_string().contains(WORKER_BASE_NAME),
            "die Fehlermeldung muss '{WORKER_BASE_NAME}' nennen: {error}"
        );
    }

    #[test]
    fn test_require_worker_base_passes_when_present() {
        let entries: [(&str, &str); 2] =
            [(WORKER_BASE_NAME, "irrelevant"), (role_names::EXPLORER, "irrelevant")];
        assert!(require_worker_base(&entries).is_ok());
    }

    #[test]
    fn test_directory_discovery_still_finds_every_previously_hardcoded_definition() {
        // Namensmenge, wie sie die alte `include_str!`-Liste in dieser Datei
        // vor AW6-00 aufzählte. Eine Umstellung, die eine Definition still
        // verliert, fiele sonst erst beim Start auf (siehe
        // Modul-Dokumentation, Abschnitt "Verzeichniskonvention").
        let expected_agent_names: HashSet<&str> = [
            WORKER_BASE_NAME,
            role_names::EXPLORER,
            role_names::RESEARCHER_DEPS,
            role_names::RESEARCHER_WEB,
            role_names::PLANNER,
            role_names::ANALYST,
        ]
        .into_iter()
        .collect();
        let found_agent_names: HashSet<&str> =
            builtin_agent_toml().iter().map(|(name, _)| *name).collect();
        // Teilmenge, nicht Gleichheit -- aus demselben Grund wie beim
        // Familien-Teil unten: der Zweck dieses Tests ist "der Umbau von der
        // `include_str!`-Liste auf die Verzeichniskonvention hat nichts
        // verloren", nicht "es ist nichts dazugekommen".
        assert!(
            expected_agent_names.is_subset(&found_agent_names),
            "fehlend: {:?}",
            expected_agent_names
                .difference(&found_agent_names)
                .collect::<Vec<_>>()
        );

        // Vor Knoten AW6-01 war das hier noch eine `assert_eq!` gegen genau
        // diese zwei Namen. `agents/families/security/security.toml` (Knoten
        // AW6-01) machte daraus einen Fehlschlag, obwohl der Zweck des Tests —
        // "die Umstellung von der `include_str!`-Liste auf die
        // Verzeichniskonvention hat nichts verloren" — durch eine dritte,
        // zusätzliche Family nicht verletzt wird. Ein Test, der bei jeder
        // neuen Family-Datei erneut angefasst werden müsste, wäre derselbe
        // Fehler wie die `include_str!`-Liste selbst — nur in dieser
        // Testdatei statt in Produktionscode. Die Zusicherung ist deshalb auf
        // eine Teilmengen-Prüfung geschwächt: jeder früher fest verdrahtete
        // Name muss weiterhin gefunden werden, eine vierte, fünfte, … Family
        // darf dazukommen, ohne diesen Test zu brechen.
        let expected_family_names: HashSet<&str> =
            [RESEARCH_FAMILY_NAME, CODING_FAMILY_NAME].into_iter().collect();
        let found_family_names: HashSet<&str> =
            builtin_family_toml().iter().map(|(name, _)| *name).collect();
        assert!(
            expected_family_names.is_subset(&found_family_names),
            "jede frueher fest verdrahtete Family muss weiterhin gefunden werden: \
             erwartet mindestens {expected_family_names:?}, gefunden {found_family_names:?}"
        );
    }

    /// Baut eine `'static` `Dir` aus geleakten Einträgen.
    ///
    /// Nur für Tests: erlaubt, synthetische Verzeichnisbäume beliebiger Tiefe
    /// zu bauen, ohne die Lifetime-Verschachtelung mehrerer lokaler Arrays von
    /// Hand durchzuhalten — jeder Baustein ist danach genauso `'static` wie
    /// die reale [`AGENTS_DIR`].
    fn leaked_dir(path: &'static str, entries: Vec<DirEntry<'static>>) -> Dir<'static> {
        Dir::new(path, Vec::leak(entries))
    }

    #[test]
    fn test_role_discovery_recurses_into_new_directories_and_skips_reserved_ones() {
        // Simuliert einen künftigen Knoten, der z. B.
        // `agents/roles/security-triage-1/` anlegt: ein synthetischer Baum mit
        // derselben Form, ohne `agents/` selbst zu berühren.
        // `discover_role_toml` nimmt jeden `&Dir<'_>` entgegen, nicht nur die
        // reale `AGENTS_DIR` — genau das macht die Simulation möglich.
        let nested_role = File::new(
            "roles/security-triage-1/security-triage-1.toml",
            b"schema = \"harwness.agent/v1\"",
        );
        let nested_dir = leaked_dir("roles/security-triage-1", vec![DirEntry::File(nested_role)]);
        let roles_dir = leaked_dir("roles", vec![DirEntry::Dir(nested_dir)]);

        let flat_role = File::new("explorer.toml", b"schema = \"harwness.agent/v1\"");

        // Eine Datei unter `family/` darf trotz `.toml`-Endung nicht als Rolle
        // auftauchen — das reservierte Root-Verzeichnis gehört der
        // Family-Sammlung.
        let family_like = File::new(
            "family/should-not-be-a-role.toml",
            b"schema = \"harwness.family/v1\"",
        );
        let family_dir = leaked_dir("family", vec![DirEntry::File(family_like)]);

        let root = leaked_dir(
            "",
            vec![
                DirEntry::Dir(roles_dir),
                DirEntry::File(flat_role),
                DirEntry::Dir(family_dir),
            ],
        );

        let discovered = discover_role_toml(&root);
        let names: Vec<&str> = discovered.iter().map(|(name, _)| *name).collect();

        assert!(
            names.contains(&"security-triage-1"),
            "eine neu angelegte, beliebig tief verschachtelte Rollen-Datei muss ohne Codeänderung gefunden werden"
        );
        assert!(names.contains(&"explorer"));
        assert!(
            !names.contains(&"should-not-be-a-role"),
            "agents/family/** gehört zur Family-Sammlung, nicht zu den Rollen"
        );
    }

    #[test]
    fn test_family_discovery_also_recurses_into_the_pluralized_future_root() {
        // `agents/families/security/` ist der von einem späteren Knoten
        // vorgesehene Pfad (siehe Modul-Dokumentation) — die Family-Sammlung
        // muss ihn finden, ohne dass diese Datei angefasst wird.
        let security_family = File::new(
            "families/security/security.toml",
            b"schema = \"harwness.family/v1\"",
        );
        let security_dir = leaked_dir("families/security", vec![DirEntry::File(security_family)]);
        let families_dir = leaked_dir("families", vec![DirEntry::Dir(security_dir)]);

        let research_family = File::new("family/research.toml", b"schema = \"harwness.family/v1\"");
        let family_dir = leaked_dir("family", vec![DirEntry::File(research_family)]);

        let root = leaked_dir("", vec![DirEntry::Dir(family_dir), DirEntry::Dir(families_dir)]);

        let discovered = discover_family_toml(&root);
        let names: Vec<&str> = discovered.iter().map(|(name, _)| *name).collect();

        assert!(names.contains(&"family/research"));
        assert!(
            names.contains(&"families/security/security"),
            "ein künftiger Knoten darf agents/families/<name>/ anlegen, ohne src/ zu ändern"
        );
    }

    // -----------------------------------------------------------------------
    // Knoten AW6-02 — `members/`-Verzeichniskonvention für Mitgliedschaften
    // -----------------------------------------------------------------------
    //
    // `agents/families/security/security.toml` selbst gehört den drei
    // Folgeknoten (AW6-03, AW6-08, AW7-06) und wird hier nicht angefasst;
    // diese Tests bauen stattdessen einen synthetischen Baum derselben Form,
    // um die Sammelregel selbst zu belegen — genau wie die beiden Tests oben
    // es für Rollen- und Family-Verzeichnisse tun.

    /// Baut einen synthetischen Family-Baum mit einer Family-Datei und einer
    /// Mitgliedschafts-Datei unter `members/` derselben Family.
    fn synthetic_security_tree_with_one_member() -> Dir<'static> {
        let security_family = File::new(
            "families/security/security.toml",
            b"schema = \"harwness.family/v1\"",
        );
        let member = File::new(
            "families/security/members/security-triage-1.toml",
            b"schema = \"harwness.family-membership/v1\"",
        );
        let members_dir = leaked_dir(
            "families/security/members",
            vec![DirEntry::File(member)],
        );
        let security_dir = leaked_dir(
            "families/security",
            vec![DirEntry::File(security_family), DirEntry::Dir(members_dir)],
        );
        let families_dir = leaked_dir("families", vec![DirEntry::Dir(security_dir)]);
        leaked_dir("", vec![DirEntry::Dir(families_dir)])
    }

    #[test]
    fn test_family_discovery_skips_the_members_subdirectory() {
        let root = synthetic_security_tree_with_one_member();
        let discovered = discover_family_toml(&root);
        let names: Vec<&str> = discovered.iter().map(|(name, _)| *name).collect();

        assert!(names.contains(&"families/security/security"));
        assert!(
            !names.iter().any(|name| name.contains("/members/")),
            "eine Mitgliedschafts-Datei unter members/ darf nicht als weitere Family auftauchen: {names:?}"
        );
    }

    #[test]
    fn test_membership_discovery_finds_files_under_members_directory() {
        let root = synthetic_security_tree_with_one_member();
        let discovered = discover_membership_toml(&root);
        let names: Vec<&str> = discovered.iter().map(|(name, _)| *name).collect();

        assert_eq!(
            names,
            vec!["families/security/members/security-triage-1"],
            "eine neu angelegte Mitgliedschafts-Datei unter members/ muss ohne Codeänderung gefunden werden"
        );
    }

    #[test]
    fn test_membership_discovery_ignores_family_root_without_members_dir() {
        // Ohne `members/`-Verzeichnis liefert die Mitgliedschafts-Sammlung
        // eine leere Liste — kein Fehler, kein falscher Fund. Das ist der
        // heutige Zustand des echten `agents/`-Baums.
        let research_family = File::new("family/research.toml", b"schema = \"harwness.family/v1\"");
        let family_dir = leaked_dir("family", vec![DirEntry::File(research_family)]);
        let root = leaked_dir("", vec![DirEntry::Dir(family_dir)]);

        assert!(discover_membership_toml(&root).is_empty());
    }

    #[test]
    fn test_membership_discovery_is_sorted_by_full_relative_path() {
        let member_b = File::new(
            "families/security/members/security-triage-2.toml",
            b"schema = \"harwness.family-membership/v1\"",
        );
        let member_a = File::new(
            "families/security/members/security-triage-1.toml",
            b"schema = \"harwness.family-membership/v1\"",
        );
        // Absichtlich in "falscher" (nicht alphabetischer) Reihenfolge
        // eingefügt — die Sammlung muss selbst sortieren, nicht sich auf die
        // Einfügereihenfolge verlassen (Reihenfolgestabilität, AW6-00-Regel).
        let members_dir = leaked_dir(
            "families/security/members",
            vec![DirEntry::File(member_b), DirEntry::File(member_a)],
        );
        let security_dir = leaked_dir("families/security", vec![DirEntry::Dir(members_dir)]);
        let families_dir = leaked_dir("families", vec![DirEntry::Dir(security_dir)]);
        let root = leaked_dir("", vec![DirEntry::Dir(families_dir)]);

        let discovered = discover_membership_toml(&root);
        let names: Vec<&str> = discovered.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                "families/security/members/security-triage-1",
                "families/security/members/security-triage-2",
            ]
        );
    }

    #[test]
    fn test_role_discovery_order_is_alphabetical_regardless_of_filesystem_entry_order() {
        // Die Reihenfolge, in der das Dateisystem (bzw. `include_dir!` zur
        // Bauzeit) Einträge liefert, ist keine verlässliche Größe — sie kann
        // je nach Betriebssystem, Dateisystem oder Build-Maschine variieren.
        // Eine Registry, deren Reihenfolge davon abhinge, machte `SnapshotId`
        // instabil: `ExecutableAgentIr` hasht sie hinein
        // (`harw-agent-dsl/src/executable.rs`). Dieser Test baut die Einträge
        // absichtlich in umgekehrter alphabetischer Reihenfolge auf und
        // erwartet trotzdem ein alphabetisch sortiertes Ergebnis — der Beweis,
        // dass `discover_agent_toml` selbst sortiert, statt sich auf eine
        // zufällig schon passende Eingabereihenfolge zu verlassen.
        let zebra = File::new("zebra.toml", b"schema = \"harwness.agent/v1\"");
        let mango = File::new("mango.toml", b"schema = \"harwness.agent/v1\"");
        let apple = File::new("apple.toml", b"schema = \"harwness.agent/v1\"");
        let root = leaked_dir(
            "",
            vec![DirEntry::File(zebra), DirEntry::File(mango), DirEntry::File(apple)],
        );

        let discovered = discover_agent_toml(&root);
        let names: Vec<&str> = discovered.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec!["apple", "mango", "zebra"],
            "die Ausgabe muss alphabetisch sortiert sein, unabhängig von der Eingabereihenfolge"
        );
    }

    #[test]
    fn test_family_discovery_order_is_alphabetical_regardless_of_filesystem_entry_order() {
        let zebra_family = File::new("family/zebra.toml", b"schema = \"harwness.family/v1\"");
        let apple_family = File::new("family/apple.toml", b"schema = \"harwness.family/v1\"");
        let family_dir = leaked_dir(
            "family",
            vec![DirEntry::File(zebra_family), DirEntry::File(apple_family)],
        );
        let root = leaked_dir("", vec![DirEntry::Dir(family_dir)]);

        let discovered = discover_family_toml(&root);
        let names: Vec<&str> = discovered.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec!["family/apple", "family/zebra"],
            "auch Families müssen alphabetisch sortiert erscheinen, unabhängig von der Eingabereihenfolge"
        );
    }

    #[test]
    fn test_repeated_loading_of_builtin_toml_yields_the_same_order() {
        // `builtin_agent_toml`/`builtin_family_toml` cachen im `OnceLock`,
        // aber der Vertrag muss auch für den Aufrufer sichtbar sein: derselbe
        // Prozess liefert bei jedem Aufruf exakt dieselbe Reihenfolge — sonst
        // wären `SnapshotId` und Golden-Tests instabil.
        let first_agent_order: Vec<&str> =
            builtin_agent_toml().iter().map(|(name, _)| *name).collect();
        let second_agent_order: Vec<&str> =
            builtin_agent_toml().iter().map(|(name, _)| *name).collect();
        assert_eq!(first_agent_order, second_agent_order);

        let first_family_order: Vec<&str> =
            builtin_family_toml().iter().map(|(name, _)| *name).collect();
        let second_family_order: Vec<&str> =
            builtin_family_toml().iter().map(|(name, _)| *name).collect();
        assert_eq!(first_family_order, second_family_order);
    }

    #[test]
    fn test_empty_agents_tree_is_a_hard_error_not_a_valid_empty_registry() {
        // Ein leerer (oder fehlender) `agents/`-Baum darf nicht als gültige,
        // leere Registry durchgehen — sonst ist er von einer kaputten
        // Installation nicht zu unterscheiden (vgl. `GateReport::checked` in
        // `xtask`, das für Gate-Ergebnisse genau dieselbe Verwechslung
        // auflöst: "nichts geprüft" ist kein Ersatz für "alles geprüft und
        // grün").
        let empty_root = leaked_dir("", Vec::new());

        let discovered_roles = discover_agent_toml(&empty_root);
        assert!(discovered_roles.is_empty(), "ein leerer Baum liefert keine Rollen");
        let error = require_worker_base(&discovered_roles)
            .expect_err("eine leere Rollen-Sammlung darf die Basis-Prüfung nicht bestehen");
        assert!(error.to_string().contains(WORKER_BASE_NAME));

        let discovered_families = discover_family_toml(&empty_root);
        assert!(
            discovered_families.is_empty(),
            "ein leerer Baum liefert keine Families"
        );
    }

    #[test]
    fn test_duplicate_role_name_is_a_hard_error_not_silent_precedence() {
        // Zwei verschiedene Dateien, die denselben Dateinamen-Stamm tragen,
        // konnten vor AW6-00 nicht entstehen (ein Mensch pflegte die Liste von
        // Hand). Die Verzeichnis-Sammlung muss das jetzt als harten Fehler
        // erkennen, statt die zweite Datei still über die erste zu legen.
        let first = File::new("explorer.toml", b"schema = \"harwness.agent/v1\"");
        let nested = leaked_dir(
            "roles/duplicate",
            vec![DirEntry::File(File::new(
                "roles/duplicate/explorer.toml",
                b"schema = \"harwness.agent/v1\"",
            ))],
        );
        let roles_dir = leaked_dir("roles", vec![DirEntry::Dir(nested)]);
        let root = leaked_dir("", vec![DirEntry::File(first), DirEntry::Dir(roles_dir)]);

        let discovered = discover_agent_toml(&root);
        let error = reject_duplicate_names(&discovered, "Agentenrolle").expect_err(
            "zwei Dateien mit demselben Namen müssen als Kollision scheitern, nicht still gewinnen/verlieren",
        );
        assert!(
            error.to_string().contains("explorer"),
            "die Fehlermeldung muss den kollidierenden Namen nennen: {error}"
        );
    }

    #[test]
    fn test_duplicate_family_name_is_a_hard_error() {
        // Bei der heutigen pfadbasierten Namensregel ist eine Family-Kollision
        // strukturell ausgeschlossen (zwei Dateien können nicht denselben
        // Pfad tragen). `reject_duplicate_names` wird trotzdem generisch für
        // beide Sammlungen aufgerufen; dieser Test prüft sie deshalb direkt
        // gegen eine konstruierte Liste statt über einen synthetischen Baum.
        let entries: [(&str, &str); 2] = [
            ("family/research", "irrelevant"),
            ("family/research", "irrelevant, anderer Inhalt"),
        ];
        let error = reject_duplicate_names(&entries, "Family")
            .expect_err("zwei Family-Einträge mit demselben Namen müssen scheitern");
        assert!(error.to_string().contains("family/research"));
    }

    #[test]
    fn test_non_toml_file_is_silently_skipped_not_an_error() {
        // Eine `README.md` neben den Definitionen ist keine Definition und
        // kein Fehler — sie hat schlicht nicht die erwartete Endung.
        let readme = File::new("README.md", b"# nicht geparst");
        let role = File::new("explorer.toml", b"schema = \"harwness.agent/v1\"");
        let root = leaked_dir("", vec![DirEntry::File(readme), DirEntry::File(role)]);

        let discovered = discover_agent_toml(&root);
        let names: Vec<&str> = discovered.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, vec!["explorer"], "README.md darf nicht als Rolle auftauchen");
    }

    #[test]
    fn test_parse_failure_names_the_offending_file() {
        let name = "roles/broken-role";
        let broken_source = "this is not = = valid toml";
        let error = parse_toml(broken_source)
            .map_err(|dsl_error| definition_error(name, dsl_error))
            .expect_err("ungültiges TOML darf nicht parsen");
        let message = error.to_string();
        assert!(
            message.contains(name),
            "die Fehlermeldung muss den Dateinamen nennen, sonst sucht man ihn unter fünfzig: {message}"
        );
    }

    #[test]
    fn test_broken_definition_in_the_tree_is_a_hard_error_not_silently_skipped() {
        // Eine Datei, die eingesammelt wird (sie endet auf `.toml` und ist
        // gültiges UTF-8), aber deren Inhalt kein gültiges TOML ist, darf
        // nicht stillschweigend aus dem Ergebnis verschwinden — sie muss beim
        // Parsen laut scheitern, genau wie `builtin_agent_definitions` es für
        // die eingebetteten Definitionen tut.
        let broken = File::new("broken-role.toml", b"this is not = = valid toml");
        let root = leaked_dir("", vec![DirEntry::File(broken)]);

        let discovered = discover_agent_toml(&root);
        assert_eq!(
            discovered.len(),
            1,
            "eine kaputte, aber lesbare .toml-Datei wird eingesammelt, nicht übersprungen"
        );

        let (name, source) = discovered[0];
        let error = parse_toml(source)
            .map_err(|dsl_error| definition_error(name, dsl_error))
            .expect_err("kaputtes TOML darf nicht parsen");
        assert!(
            error.to_string().contains("broken-role"),
            "die Fehlermeldung muss die betroffene Datei nennen: {error}"
        );
    }

    #[test]
    fn test_every_builtin_role_is_a_non_pausing_worker_with_a_return_contract() {
        for (role, ir) in builtin() {
            assert!(
                matches!(
                    ir.role(),
                    harw_agent_dsl::roles::AgentRoleId::Worker
                        | harw_agent_dsl::roles::AgentRoleId::UiaWorker
                        | harw_agent_dsl::roles::AgentRoleId::AgentSteward
                ),
                "{role} muss Rolle 'worker', 'uia-worker' oder 'agent-steward' tragen (Addendum J + K)"
            );
            assert_eq!(ir.specialization(), role, "{role}");
            assert!(
                !ir.lifecycle_machine().allow_pause(),
                "{role}: read-only Kinder dürfen nie auf eine Rückfrage warten"
            );
            assert!(ir.lifecycle_machine().allow_rerun(), "{role}");
            assert_eq!(ir.lifecycle_machine().max_attempts(), Some(2), "{role}");
            assert!(
                ir.return_pipeline().contract().is_some(),
                "{role} braucht einen Rückgabevertrag"
            );
        }
    }

    #[test]
    fn test_every_builtin_role_forbids_write_and_shell_tools() {
        // Ausdrückliche Ausnahmeliste (Slice B7) — genau `executor`, kein
        // Wildcard. `executor` (`RegistryProfile::Full`, siehe
        // `harw-registry-defaults/src/profile.rs::profile_for_role`) darf
        // `fs.write` und `shell.exec` zulassen: er führt Befehls- und
        // Dateioperationen im Auftrag des Haupt-Agenten aus, damit die TUI
        // Befehlsfolgen delegieren kann, statt sie als viele einzelne
        // Tool-Aufrufe im Hauptfenster zu zeigen (siehe
        // `agents/executor.toml`). Das ist keine Lockerung der
        // Freigabegrenze: jeder Aufruf, den `executor` ausführt, läuft
        // weiterhin durch die Freigabekette des Elternteils
        // (`DefaultApprovalPolicy`) — diese Rolle bekommt nur eine
        // zusätzliche Werkzeugoberfläche, keinen eigenen, ungeprüften
        // Freigabeweg. Eine zweite schreibende Rolle muss diese Liste (und
        // den begleitenden Test unten) bewusst anfassen, statt
        // stillschweigend durchzurutschen.
        //
        // Addendum I: `uia-worker` (`RegistryProfile::UiaQuickHelper`)
        // admittiert jetzt ebenfalls `shell.exec` — der exklusive
        // Schnellhelfer der UIA braucht die Shell für schnelle
        // Schnelleingriffe. Kein `fs.write` (siehe `agents/uia-worker.toml`).
        const ALLOWED_TO_WRITE_AND_EXEC: &[&str] =
            &[role_names::EXECUTOR, role_names::UIA_WORKER];
        // `memory-steward` (Memory v3, §5.3, `RegistryProfile::MemoryStewardship`)
        // darf `fs.write` zulassen — die Konsolidierung muss Fakten und
        // `MEMORY.md` tatsächlich schreiben können —, muss aber weiterhin
        // `shell.exec` ausdrücklich verbieten (siehe `agents/memory-steward.toml`).
        const ALLOWED_TO_WRITE_ONLY: &[&str] = &[role_names::MEMORY_STEWARD];

        for (role, ir) in builtin() {
            if ALLOWED_TO_WRITE_AND_EXEC.contains(&role.as_str()) {
                continue;
            }
            let admitted = ir.tool_surface().admitted();
            let forbidden = ir.tool_surface().forbidden();
            let may_write = ALLOWED_TO_WRITE_ONLY.contains(&role.as_str());
            for tool in ["fs.write", "shell.exec"] {
                if may_write && tool == "fs.write" {
                    assert!(
                        admitted.iter().any(|name| name == tool),
                        "{role} muss {tool} zulassen"
                    );
                    continue;
                }
                assert!(
                    !admitted.iter().any(|name| name == tool),
                    "{role} darf {tool} nicht zulassen"
                );
                assert!(
                    forbidden.iter().any(|name| name == tool),
                    "{role} muss {tool} ausdrücklich verbieten"
                );
            }
        }
    }

    /// Ergänzung zu [`test_every_builtin_role_forbids_write_and_shell_tools`]:
    /// die Ausnahmen dort dürfen sich nicht unbemerkt ausweiten. Dieser Test
    /// prüft in beide Richtungen — `executor` ist die EINZIGE Rolle, die
    /// `shell.exec` zulässt und bekommt tatsächlich
    /// [`crate::profile::RegistryProfile::ShellExecution`]; `memory-steward`
    /// ist die EINZIGE Rolle, die `fs.write` zulässt und bekommt tatsächlich
    /// [`crate::profile::RegistryProfile::MemoryStewardship`] (nicht nur eine
    /// TOML, die zufällig dieselben Werkzeugnamen admittiert).
    #[test]
    fn test_only_executor_gets_the_shell_execution_profile() {
        use crate::profile::{RegistryProfile, profile_for_role};

        for (role, ir) in builtin() {
            let admitted = ir.tool_surface().admitted();
            if role == role_names::EXECUTOR {
                assert_eq!(
                    admitted.iter().map(String::as_str).collect::<Vec<_>>(),
                    vec!["shell.exec"],
                    "executor must expose only shell.exec"
                );
            } else if role == role_names::UIA_WORKER {
                // Addendum I: `uia-worker` admits `shell.exec` too, but as
                // part of the broader `UiaQuickHelper` surface (fs.read.*
                // plus shell.exec plus web.fetch), not shell.exec alone.
                assert!(
                    admitted.iter().any(|name| name == "shell.exec"),
                    "uia-worker must admit shell.exec (RegistryProfile::UiaQuickHelper)"
                );
            } else {
                assert!(
                    !admitted.iter().any(|name| name == "shell.exec"),
                    "{role} admits shell.exec, but only executor and uia-worker may do so"
                );
            }
            if role == role_names::MEMORY_STEWARD {
                assert!(
                    admitted.iter().any(|name| name == "fs.write"),
                    "{role} must admit fs.write to consolidate memory facts"
                );
            } else {
                assert!(
                    !admitted.iter().any(|name| name == "fs.write"),
                    "{role} must not admit fs.write; process execution and file mutation are separate capabilities"
                );
            }
        }

        assert_eq!(
            profile_for_role(role_names::EXECUTOR),
            Some(RegistryProfile::ShellExecution),
            "the executor must get the shell-only profile"
        );
        assert_eq!(
            profile_for_role(role_names::MEMORY_STEWARD),
            Some(RegistryProfile::MemoryStewardship),
            "the memory steward must get the fs-write-only profile"
        );
        for role in role_names::ALL {
            if *role == role_names::EXECUTOR {
                continue;
            }
            assert_ne!(
                profile_for_role(role),
                Some(RegistryProfile::ShellExecution),
                "{role} must not receive the dedicated process profile"
            );
        }
    }

    /// Befund (siehe Abschlussbericht dieses Knotens): `builtin_agent_definitions`
    /// filtert `targets` über `role_names::ALL`
    /// (`harw-registry-defaults/src/profile.rs`), und diese Liste kennt nach
    /// wie vor nur die fünf ursprünglichen Rollen. Die vier neuen
    /// `security-*`-Rollendateien werden von `discover_agent_toml`/
    /// `builtin_agent_toml` zwar eingesammelt, aber NIE in `targets`
    /// aufgenommen und damit NIE gesenkt — `builtin()` (und folglich auch
    /// `test_every_builtin_role_forbids_write_and_shell_tools`, das exakt
    /// über `builtin()` iteriert) erreicht sie deshalb nicht. Der
    /// Kopfkommentar von `agents/families/security/security.toml` behauptet
    /// das Gegenteil ("geprüft durch
    /// `test_every_builtin_role_forbids_write_and_shell_tools`") — nach
    /// dieser Prüfung ist das nicht zutreffend, bis `role_names::ALL` (oder
    /// der Filter in `builtin_agent_definitions`) um die vier Namen erweitert
    /// wird. Das ist ein Befund für einen Folgeknoten, kein Auftrag dieses
    /// Knotens: nur der `#[cfg(test)]`-Block dieser Datei ist Schreibbereich,
    /// `builtin_agent_definitions`/`role_names::ALL` sind Produktionscode.
    ///
    /// Diese Prüfung schließt die Lücke innerhalb des erlaubten
    /// Schreibbereichs: sie senkt die vier Rollendateien direkt über dieselbe
    /// Pipeline (`parse_toml → resolve_definition → lower`), ohne über
    /// `role_names::ALL` zu filtern, und prüft dieselbe Invariante wie
    /// `test_every_builtin_role_forbids_write_and_shell_tools`. Bis die
    /// Produktionslücke behoben ist, ist dies die einzige Prüfung, die diese
    /// vier Rollen tatsächlich lowert und ihre Werkzeugoberfläche verifiziert.
    #[test]
    fn test_security_triage_roles_forbid_write_and_shell_tools_despite_the_role_names_all_gap() {
        const SECURITY_ROLE_NAMES: [&str; 4] = [
            "security-egress-triage",
            "security-baseline-triage",
            "security-structure-triage",
            "security-endpoint-triage",
        ];

        require_worker_base(builtin_agent_toml()).expect("worker-base muss im eingebetteten Baum liegen");

        let now = OffsetDateTime::now_utc();
        let mut layers: Vec<(DefinitionLayer, RawAgentDefinition)> = Vec::new();
        let mut targets: Vec<(&str, DefinitionId)> = Vec::new();
        for (name, source) in builtin_agent_toml() {
            let raw =
                parse_toml(source).unwrap_or_else(|error| panic!("{name} parst nicht: {error}"));
            if SECURITY_ROLE_NAMES.contains(name) {
                targets.push((*name, raw.id.clone()));
            }
            layers.push((DefinitionLayer::BuiltIn, raw));
        }

        assert_eq!(
            targets.len(),
            SECURITY_ROLE_NAMES.len(),
            "alle vier Security-Triage-Rollendateien muessen im eingebetteten Baum liegen"
        );

        for (name, id) in targets {
            let resolved = resolve_definition(&id, &layers, now)
                .unwrap_or_else(|error| panic!("{name} muss aufloesen: {error}"));
            let ir =
                lower(&resolved).unwrap_or_else(|error| panic!("{name} muss lowern: {error}"));
            let admitted = ir.tool_surface().admitted();
            let forbidden = ir.tool_surface().forbidden();
            for tool in ["fs.write", "shell.exec"] {
                assert!(
                    !admitted.iter().any(|t| t == tool),
                    "{name} darf {tool} nicht zulassen"
                );
                assert!(
                    forbidden.iter().any(|t| t == tool),
                    "{name} muss {tool} ausdruecklich verbieten"
                );
            }
        }
    }

    #[test]
    fn test_builtin_roles_carry_the_shared_context_program_from_the_base() {
        for (role, ir) in builtin() {
            assert_eq!(
                ir.context_program().must_include(),
                ["task.objective", "task.read_scope", "new.trigger_return"],
                "{role}"
            );
            assert_eq!(
                ir.context_program().exclude(),
                ["full_parent_transcript", "sibling_transcripts"],
                "{role}"
            );
        }
    }

    #[test]
    fn test_explorer_tool_surface_and_budget() {
        let definitions = builtin();
        let explorer = &definitions[role_names::EXPLORER];
        assert_eq!(
            explorer.tool_surface().admitted(),
            [
                "fs.read",
                "fs.list",
                "fs.search",
                "fs.glob",
                "fs.grep",
                "deps.graph",
                "deps.locked",
                "deps.source_read",
                "deps.source_search",
                "deps.source_list",
            ]
        );
        assert_eq!(explorer.spawn_contract().max_depth(), Some(1));
        let budget = explorer
            .spawn_contract()
            .budget()
            .expect("[spawn.budget] muss gesetzt sein");
        assert_eq!(budget.max_tokens(), Some(60_000));
        assert_eq!(budget.max_tool_calls(), Some(40));
        assert_eq!(budget.max_wall_secs(), Some(180));
        assert_eq!(
            explorer.return_pipeline().contract(),
            Some("harwness.return.research-finding@1")
        );
    }

    #[test]
    fn test_researcher_web_is_the_only_role_with_web_tools() {
        // Addendum I: `uia-worker` (`RegistryProfile::UiaQuickHelper`)
        // admittiert seit der Korrektur ebenfalls ein Netz-Werkzeug —
        // ausschließlich `web.fetch`, siehe `agents/uia-worker.toml`. Er ist
        // damit die zweite (und einzige weitere) Rolle mit `web.*`.
        let definitions = builtin();
        for (role, ir) in &definitions {
            let has_web = ir
                .tool_surface()
                .admitted()
                .iter()
                .any(|name| name.starts_with("web."));
            let expects_web =
                role == role_names::RESEARCHER_WEB || role == role_names::UIA_WORKER;
            assert_eq!(
                has_web, expects_web,
                "{role}: web.* darf nur der Web-Rechercheur und der UIA-Schnellhelfer führen"
            );
        }
    }

    /// W1-05: `plan`/`goal` deklarieren `model_tool(approval = "always")`
    /// (`harw-ops/src/plan.rs:479`, `harw-ops/src/goal.rs:321`) und wurden aus
    /// `agents/planner.toml` `[tools].admitted` entfernt — der Planner hatte im
    /// Kind nie einen Executor dafür. `may_change_plan = false` bleibt
    /// wahrheitsgemäß; die Rückgabe läuft weiterhin über den
    /// `plan-proposal`-Contract.
    #[test]
    fn test_planner_does_not_admit_plan_or_goal_operations() {
        let definitions = builtin();
        let planner = &definitions[role_names::PLANNER];
        let admitted = planner.tool_surface().admitted();
        assert!(!admitted.iter().any(|name| name == "plan"));
        assert!(!admitted.iter().any(|name| name == "goal"));
        assert_eq!(
            planner.return_pipeline().contract(),
            Some("harwness.return.plan-proposal@1")
        );
    }

    /// Der Analyst ist die einzige Rolle, die zwei Ebenen darf.
    ///
    /// Diese Aussage ist unverändert; nur ihre Prüfung nennt jetzt die drei
    /// zulässigen Tiefen **je Rollenklasse** statt einer Ausnahmeliste:
    ///
    /// - `2` — ausschließlich `analyst`: der einzige Verdichter, der
    ///   read-only Kinder starten und deren Rückgaben zusammenfassen darf.
    /// - `0` — die vier `security-*-triage`-Rollen: sie lesen
    ///   angreiferkontrollierte Sensorfelder und sollen ausdrücklich kein
    ///   Kind erzeugen. `Some(0)` heißt „unter dieser Rolle entsteht keine
    ///   weitere Ebene“, nicht „diese Rolle darf nicht existieren“.
    /// - `1` — jeder übrige Worker.
    ///
    /// Die abschließende Zusicherung ist dadurch schärfer als vorher: nicht
    /// nur trägt der Analyst `2`, es trägt auch **keine andere** Rolle diesen
    /// Wert — und das gilt unabhängig davon, wie viele Rollen mit Tiefe `0`
    /// oder `1` noch dazukommen.
    ///
    /// Offener Punkt für einen Folgeknoten: `context-steward` und
    /// `intel-scout` tragen ebenfalls `max_depth = 0`, stehen aber (bewusst,
    /// siehe `role_names::ALL`) nicht im gesenkten Inventar. Wer sie dort
    /// aufnimmt, trägt sie hier in `ZERO_DEPTH_ROLES` nach — sonst erwartet
    /// dieser Test für sie `1` und schlägt fehl. Dasselbe gilt für ihren
    /// fehlenden `[return].contract` im Vertragstest weiter oben.
    ///
    /// `memory-steward` (Memory v3, §5.3) trägt ebenfalls `max_depth = 0`
    /// (`agents/memory-steward.toml`, `[spawn] max_depth = 0`): eine
    /// Konsolidierung ist ein einzelner, in sich geschlossener Lauf über
    /// bereits gelieferten Kontext, kein Fan-out.
    #[test]
    fn test_analyst_is_the_only_role_allowed_to_spawn_two_levels() {
        // Die Tiefe-0-Klasse: Rollen ohne eigene Ebene darunter — die vier
        // Triage-Rollen plus `memory-steward` und `agent-steward` (Addendum
        // K: ein Umsetzungslauf ist ein einzelner, in sich geschlossener
        // Validieren-dann-Schreiben-Schritt, kein Fan-out) plus `uia-worker`
        // (Addendum J/I: ein Schnelleingriff ist ein einzelner, in sich
        // geschlossener Lauf, kein Fan-out — siehe `agents/uia-worker.toml`
        // `[spawn] max_depth = 0`) plus `executor` (Slice B7: ein
        // Ausführungs-Job führt die ihm übergebene Befehlsfolge selbst aus
        // und meldet zurück, statt weiter zu delegieren — siehe
        // `agents/executor.toml` `[spawn] max_depth = 0`).
        const ZERO_DEPTH_ROLES: [&str; 8] = [
            role_names::SECURITY_EGRESS_TRIAGE,
            role_names::SECURITY_BASELINE_TRIAGE,
            role_names::SECURITY_STRUCTURE_TRIAGE,
            role_names::SECURITY_ENDPOINT_TRIAGE,
            role_names::MEMORY_STEWARD,
            role_names::AGENT_STEWARD,
            role_names::UIA_WORKER,
            role_names::EXECUTOR,
        ];

        let definitions = builtin();
        let mut two_level_roles: Vec<&str> = Vec::new();
        for (role, ir) in &definitions {
            let expected = if role == role_names::ANALYST {
                2
            } else if ZERO_DEPTH_ROLES.contains(&role.as_str()) {
                0
            } else {
                1
            };
            let max_depth = ir.spawn_contract().max_depth();
            assert_eq!(max_depth, Some(expected), "{role}: unerwartete Spawn-Tiefe");
            if max_depth == Some(2) {
                two_level_roles.push(role.as_str());
            }
        }
        assert_eq!(
            two_level_roles,
            [role_names::ANALYST],
            "genau eine eingebaute Rolle darf zwei Ebenen — der Analyst"
        );
        let analyst = &definitions[role_names::ANALYST];
        assert!(
            !analyst
                .tool_surface()
                .admitted()
                .iter()
                .any(|name| name.starts_with("web.")),
            "der Analyst arbeitet ohne Netzzugang"
        );
    }

    #[test]
    fn test_local_definition_overrides_builtin_role() {
        // Eine bereits geladene lokale Definition belegt den Namen; die
        // eingebaute Rolle wird komplett übersprungen, nicht gemischt.
        let local_explorer = builtin()
            .remove(role_names::EXPLORER)
            .expect("explorer muss eingebaut existieren");
        let mut existing = HashMap::new();
        existing.insert(role_names::EXPLORER.to_owned(), local_explorer);

        let definitions =
            builtin_agent_definitions(&existing).expect("Restrollen müssen weiter lowern");

        assert!(
            !definitions.contains_key(role_names::EXPLORER),
            "die lokale Definition gewinnt; die eingebaute wird übersprungen"
        );
        assert_eq!(definitions.len(), role_names::ALL.len() - 1);
        for role in role_names::ALL.iter().filter(|r| **r != role_names::EXPLORER) {
            assert!(definitions.contains_key(*role), "fehlt: {role}");
        }
    }

    // ── Families und Organisation ───────────────────────────────────────────

    /// Löst die eingebauten Families auf; ein Fehler ist ein Defekt der TOML-Dateien.
    fn families() -> HashMap<String, ResolvedFamily> {
        match builtin_family_definitions() {
            Ok(families) => families,
            Err(error) => panic!("eingebaute Families müssen auflösen: {error}"),
        }
    }

    /// Holt eine aufgelöste Family aus der Map oder scheitert mit ihrem Namen.
    fn family(name: &str) -> ResolvedFamily {
        match families().get(name) {
            Some(resolved) => resolved.clone(),
            None => panic!("Family '{name}' fehlt in builtin_family_definitions()"),
        }
    }

    /// Löst die eingebaute Organisation auf; ein Fehler ist ein Defekt der TOML-Datei.
    fn organization() -> ResolvedOrganization {
        match default_organization() {
            Ok(organization) => organization,
            Err(error) => panic!("die eingebaute Organisation muss auflösen: {error}"),
        }
    }

    /// Die Rollennamen eines Rosters in Deklarationsreihenfolge.
    fn role_names_of(roster: &[harw_agent_dsl::ids::DefinitionRef]) -> Vec<&str> {
        roster.iter().map(|entry| entry.id.name.as_str()).collect()
    }

    #[test]
    fn test_builtin_family_toml_lists_research_and_coding() {
        // Reihenfolge ist kein Vertrag der Verzeichnis-Sammlung (sie sortiert
        // alphabetisch) — nur die Menge der Namen ist es. Vor AW6-00 prüfte
        // dieser Test eine feste Deklarationsreihenfolge einer Rust-Liste; die
        // gibt es nicht mehr, die Namensmenge bleibt aber identisch.
        //
        // Knoten AW6-01 hat mit `agents/families/security/security.toml` eine
        // dritte Family hinzugefügt und damit die frühere `assert_eq!` (exakt
        // diese zwei Namen) zum Scheitern gebracht — aus demselben Grund wie
        // bei `test_directory_discovery_still_finds_every_previously_hardcoded_definition`
        // oben, auf dieselbe Teilmengen-Prüfung umgestellt: eine vierte Family
        // darf dazukommen, ohne diesen Test zu brechen.
        //
        // Befund: nach dieser Umstellung prüft dieser Test dieselbe Aussage
        // wie der Family-Teil von `test_directory_discovery_still_finds_every_previously_hardcoded_definition`
        // — beide verifizieren "research und coding sind unter den gefundenen
        // Family-Namen" gegen dieselbe Quelle (`builtin_family_toml`). Er wird
        // hier nicht gelöscht (bestehende Tests werden erweitert, nicht
        // entkernt), bringt aber keine zusätzliche Prüfkraft mehr mit.
        let toml = builtin_family_toml();
        let names: HashSet<&str> = toml.iter().map(|(name, _)| *name).collect();
        let expected: HashSet<&str> = [RESEARCH_FAMILY_NAME, CODING_FAMILY_NAME].into_iter().collect();
        assert!(
            expected.is_subset(&names),
            "research und coding muessen weiterhin unter den gefundenen Family-Namen sein: {names:?}"
        );
    }

    #[test]
    fn test_builtin_families_resolve_with_rosters_and_invariants() {
        for name in [RESEARCH_FAMILY_NAME, CODING_FAMILY_NAME] {
            let resolved = family(name);
            assert_eq!(resolved.id.kind, "family", "{name}");
            assert!(!resolved.workers.is_empty(), "{name}: leeres Worker-Roster");
            assert!(
                !resolved.orchestrators.is_empty(),
                "{name}: leeres Orchestrator-Roster"
            );
            assert!(!resolved.invariants.is_empty(), "{name}: keine Invarianten");
            let steps = resolved.trace.steps.len();
            assert_eq!(steps, 1, "{name}: erwartet genau einen Base-Schritt");
        }
    }

    #[test]
    fn test_research_family_lists_exactly_the_read_only_research_roles() {
        let resolved = family(RESEARCH_FAMILY_NAME);
        assert_eq!(
            role_names_of(&resolved.workers),
            vec![
                role_names::EXPLORER,
                role_names::RESEARCHER_DEPS,
                role_names::RESEARCHER_WEB,
            ]
        );
        assert_eq!(
            role_names_of(&resolved.orchestrators),
            vec![role_names::ANALYST],
            "nur der Analyst darf zwei Ebenen tief delegieren"
        );
    }

    #[test]
    fn test_coding_family_lists_only_roles_that_exist_in_role_names() {
        // Seit Slice B7 admittiert `executor` `fs.write`/`shell.exec` und
        // führt damit Befehls- und Dateioperationen im Auftrag des
        // Haupt-Agenten aus — er entwirft aber keinen Code, er führt bereits
        // entschiedene Operationen aus und fasst sie zusammen. Das Roster
        // nennt deshalb weiterhin sowohl die Rollen, die die Coding-Spur
        // vorbereiten (`planner`, `explorer`), als auch `executor`, der sie
        // ausführt. Erfundene IDs wären eine Zusage, die der Spawn nicht
        // einlösen kann.
        let resolved = family(CODING_FAMILY_NAME);
        for name in role_names_of(&resolved.workers) {
            assert!(
                role_names::ALL.contains(&name),
                "coding-Family nennt die unbekannte Rolle '{name}'"
            );
        }
        assert!(
            role_names_of(&resolved.workers).contains(&role_names::PLANNER),
            "der Planer trägt die Coding-Spur"
        );
    }

    #[test]
    fn test_every_family_role_reference_is_an_embedded_agent() {
        let embedded: Vec<&str> = builtin_agent_toml().iter().map(|(name, _)| *name).collect();
        for family_name in [RESEARCH_FAMILY_NAME, CODING_FAMILY_NAME] {
            let resolved = family(family_name);
            for entry in resolved.workers.iter().chain(resolved.orchestrators.iter()) {
                assert_eq!(entry.id.kind, "agent", "{family_name}");
                assert!(
                    embedded.contains(&entry.id.name.as_str()),
                    "{family_name}: '{}' ist keine eingebettete Definition",
                    entry.id.name
                );
            }
        }
    }

    #[test]
    fn test_default_organization_id_matches_the_constant() {
        assert_eq!(organization().id.as_string(), DEFAULT_ORGANIZATION_ID);
    }

    /// Knoten AW6-03 hat mit `security` einen vierten Clan eingetragen
    /// (`agents/organization/default.toml`) — der Vorgänger-Test bestand aus
    /// einer festen `assert_eq!` auf genau drei Clan-IDs in einer festen
    /// Reihenfolge und brach deshalb. Eine Zahl, die neben der Liste steht,
    /// die sie zählt, ist keine Prüfung, sondern eine Wiederholung; sie wäre
    /// beim nächsten Clan wieder rot, ohne zu sagen, WAS eigentlich gelten
    /// soll. Diese Fassung prüft stattdessen die tragenden Invarianten:
    ///
    /// 1. Referenzielle Integrität in beide Richtungen zwischen Clans und
    ///    Zellen — jede Zelle nennt einen tatsächlich existierenden Clan
    ///    (`resolve_organization` erzwingt das bereits als `MissingBase`,
    ///    dieser Test hält es zusätzlich als Konsumenten-Erwartung fest), und
    ///    jeder Clan hat genau eine Zelle (Zuschnitt der heutigen
    ///    Default-Organisation — vier Clans, vier Zellen, je 1:1).
    /// 2. Die vier heute bekannten Clan-IDs (`research`, `coding`,
    ///    `synthesis`, `security`) sind weiterhin unter den aufgelösten
    ///    Clans — als Teilmengen-Prüfung, nicht als Gleichheit und ohne feste
    ///    Reihenfolge, damit ein fünfter Clan diesen Test nicht bricht.
    #[test]
    fn test_every_clan_has_exactly_one_cell_and_every_cell_references_a_known_clan() {
        // Lokale Test-Konstante statt einer neuen `pub const` in Produktionscode:
        // `RESEARCH_CLAN_ID`/`CODING_CLAN_ID`/`SYNTHESIS_CLAN_ID` sind bereits
        // eingeführte Konstanten dieses Moduls; `security` bekommt hier bewusst
        // keine gespiegelte `pub const`, weil dieser Knoten nur den
        // `#[cfg(test)]`-Block ändern darf.
        const SECURITY_CLAN_ID: &str = "security";

        let resolved = organization();
        let clan_ids: HashSet<&str> = resolved.clans.iter().map(|clan| clan.id.as_str()).collect();

        // Referenzielle Integrität: jede Zelle referenziert einen bekannten Clan.
        for cell in &resolved.cells {
            assert!(
                clan_ids.contains(cell.clan.as_str()),
                "Zelle '{}' referenziert den unbekannten Clan '{}'",
                cell.id,
                cell.clan
            );
        }

        // Jeder Clan hat genau eine Zelle — Zuschnitt der heutigen
        // Default-Organisation, unabhängig von der Anzahl der Clans.
        for clan in &resolved.clans {
            let cells_for_clan = resolved
                .cells
                .iter()
                .filter(|cell| cell.clan == clan.id)
                .count();
            assert_eq!(cells_for_clan, 1, "Clan '{}' braucht genau eine Zelle", clan.id);
        }

        let expected: HashSet<&str> = [
            RESEARCH_CLAN_ID,
            CODING_CLAN_ID,
            SYNTHESIS_CLAN_ID,
            SECURITY_CLAN_ID,
        ]
        .into_iter()
        .collect();
        assert!(
            expected.is_subset(&clan_ids),
            "die vier bekannten Clans muessen weiterhin unter den aufgeloesten Clans sein: {clan_ids:?}"
        );
    }

    #[test]
    fn test_research_cell_is_a_write_partitioned_fanout_over_research_nodes() {
        let resolved = organization();
        let Some((clan, cell)) = clan_cell(&resolved, RESEARCH_CLAN_ID) else {
            panic!("der Research-Clan muss eine Zelle haben");
        };
        assert_eq!(cell.kind, CellKind::Fanout);
        assert_eq!(cell.barrier, CellBarrier::AllTerminal);
        assert_eq!(cell.write_partition, CellWritePartition::Required);
        assert_eq!(cell.members_from_plan, "research-*");
        assert_eq!(clan.plan_scope, "research-*");
        assert_eq!(clan.child_depth_cost, 1, "Standard-Tiefenkosten");
    }

    #[test]
    fn test_coding_cell_is_sequential_and_write_partitioned() {
        let resolved = organization();
        let Some((clan, cell)) = clan_cell(&resolved, CODING_CLAN_ID) else {
            panic!("der Coding-Clan muss eine Zelle haben");
        };
        assert_eq!(cell.kind, CellKind::Sequential);
        assert_eq!(cell.write_partition, CellWritePartition::Required);
        assert_eq!(cell.members_from_plan, "coding-*");
        assert_eq!(clan.plan_scope, "coding-*");
    }

    #[test]
    fn test_synthesis_cell_uses_explicit_join() {
        let resolved = organization();
        let Some((_, cell)) = clan_cell(&resolved, SYNTHESIS_CLAN_ID) else {
            panic!("der Synthesis-Clan muss eine Zelle haben");
        };
        assert_eq!(cell.barrier, CellBarrier::ExplicitJoin);
    }

    #[test]
    fn test_clan_cell_is_none_for_an_unknown_clan() {
        let resolved = organization();
        assert!(
            clan_cell(&resolved, "gibt-es-nicht").is_none(),
            "ein unbekannter Clan ist kein Fehler, sondern schlicht keine Zelle"
        );
    }

    #[test]
    fn test_organization_references_only_embedded_roles() {
        let resolved = organization();
        let mut agents = vec![&resolved.root.agent];
        agents.extend(resolved.clans.iter().map(|clan| &clan.leader));
        for reference in agents {
            assert_eq!(reference.id.kind, "agent");
            assert!(
                role_names::ALL.contains(&reference.id.name.as_str()),
                "die Organisation verweist auf die unbekannte Rolle '{}'",
                reference.id.name
            );
        }
    }

    #[test]
    fn test_every_organization_family_reference_is_an_embedded_family() {
        let resolved = organization();
        let known: Vec<String> = families()
            .values()
            .map(|resolved| resolved.id.as_string())
            .collect();
        let mut referenced = vec![&resolved.root.family];
        referenced.extend(resolved.clans.iter().map(|clan| &clan.family));
        for reference in referenced {
            assert!(
                known.contains(&reference.id.as_string()),
                "'{}' ist keine eingebettete Family",
                reference.id
            );
        }
    }

    #[test]
    fn test_snapshot_ids_are_stable_across_repeated_lowering() {
        let first = builtin();
        let second = builtin();
        for role in role_names::ALL {
            assert_eq!(
                first[*role].snapshot_id(),
                second[*role].snapshot_id(),
                "{role}: gleiche Definition muss denselben Snapshot-Digest ergeben"
            );
        }
    }

    // ─── Addendum K: eingebettetes Organisations-/Bauplan-Wissen ──────────

    /// Größenlimits der Wissensdokumente (Addendum K + Nachtrag K): die
    /// Dokumente sind Prompt-Präfixe, die bei jedem Lauf der jeweiligen Rolle
    /// mitgesendet werden — ein Budget hält sie knapp, statt unbegrenzt zu
    /// wachsen.
    #[test]
    fn test_knowledge_documents_stay_within_their_byte_budget() {
        assert!(
            ORGANIZATION_KNOWLEDGE.len() <= 3500,
            "agent-organization.md: {} Bytes > 3500",
            ORGANIZATION_KNOWLEDGE.len()
        );
        assert!(
            AUTHORING_KNOWLEDGE.len() <= 7000,
            "agent-authoring.md: {} Bytes > 7000",
            AUTHORING_KNOWLEDGE.len()
        );
        assert!(
            AGENT_STEWARD_KNOWLEDGE.len() <= 1500,
            "roles/agent-steward.md: {} Bytes > 1500",
            AGENT_STEWARD_KNOWLEDGE.len()
        );
        for (name, text) in [
            ("uia.md", UIA_KNOWLEDGE),
            ("root-orchestrator.md", ROOT_ORCHESTRATOR_KNOWLEDGE),
            ("sub-orchestrator.md", SUB_ORCHESTRATOR_KNOWLEDGE),
        ] {
            assert!(text.len() <= 1500, "roles/{name}: {} Bytes > 1500", text.len());
        }
    }

    /// `builtin_organization_knowledge` hängt für UIA und `agent-steward`
    /// Organisation, Bauplan und Rollenregel an; für Root-/Sub-Orchestrator
    /// nur Organisation plus Rollenregel; für Worker/`uia-worker` nur die
    /// Rollenregel — siehe die Begründung an der Funktion selbst.
    #[test]
    fn test_builtin_organization_knowledge_composes_the_right_fragments_per_role() {
        use harw_agent_dsl::roles::AgentRoleId;

        for role in [AgentRoleId::UserInterface, AgentRoleId::AgentSteward] {
            let text = builtin_organization_knowledge(role);
            assert!(text.contains("harwness.knowledge.agent-organization@1"), "{role:?}");
            assert!(text.contains("harwness.knowledge.agent-authoring@1"), "{role:?}");
            assert!(text.ends_with(builtin_role_knowledge(role)), "{role:?}");
        }
        for role in [AgentRoleId::RootOrchestrator, AgentRoleId::ChildOrchestrator] {
            let text = builtin_organization_knowledge(role);
            assert!(text.contains("harwness.knowledge.agent-organization@1"), "{role:?}");
            assert!(!text.contains("harwness.knowledge.agent-authoring@1"), "{role:?}");
            assert!(text.ends_with(builtin_role_knowledge(role)), "{role:?}");
        }
        for role in [AgentRoleId::Worker, AgentRoleId::UiaWorker] {
            assert_eq!(builtin_organization_knowledge(role), builtin_role_knowledge(role));
        }
    }
}
