//! Test-Fixtures für `harw-plan` und seine Konsumenten.
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt die *Bauform* von Test-Plänen und Test-Knoten. Es
//! ersetzt die handkopierten `make_node` / `test_node` / `make_plan` /
//! `test_plan`-Helfer, die in `config.rs`, `graph.rs`, `actions.rs`,
//! `validate.rs`, `admission.rs`, `mutation.rs`, `goal.rs`, `memory_store.rs`,
//! `file_store.rs`, `types.rs` und `lib.rs` jeweils dasselbe
//! 18-Felder-Struct-Literal wiederholen — und in `harw-plan-bridge`,
//! `harw-ops` und `harw-tui` noch einmal.
//!
//! Es besitzt **nicht** die Semantik der Typen: Defaults werden hier nur
//! gewählt, nicht erfunden. Sie folgen der Mehrheitskonvention der
//! abgelösten Fixtures (siehe [`base_node`]).
//!
//! # Exportierte Bausteine
//! - Makros: [`plan_node!`](crate::plan_node), [`plan!`](crate::plan),
//!   [`impl_store_conformance_tests!`](crate::impl_store_conformance_tests)
//! - Basis-Fixtures: [`base_node`], [`base_plan`], [`fixture_time`]
//! - Konverter: [`task_ids`], [`scopes`], [`plan_nodes`]
//! - Leer-Konstanten: [`NO_DEPS`], [`NO_SCOPES`]
//! - Assertion-Helfer ohne `unwrap()`: [`expect_ok`], [`expect_err`]
//!
//! # Sichtbarkeit
//! Das Modul ist **unbedingt** `pub` — nicht `#[cfg(test)]` und nicht hinter
//! einem `testing`-Feature. Begründung:
//!
//! 1. `#[cfg(test)]` scheidet aus: so gegattete Items sind für *andere*
//!    Crates unsichtbar. Genau deshalb ist das Pendant in
//!    `harw-plan-bridge/src/lib.rs` (`#[cfg(test)] pub(crate) mod testing`)
//!    dort auch nur crate-lokal nutzbar.
//! 2. Der Hauptteil dieses Moduls sind `macro_rules!`-Makros. Eine
//!    Makro-Definition erzeugt keinen Code; ein Feature-Gate spart daran
//!    nichts.
//! 3. Ein `testing`-Feature verlagert die Kosten auf jeden Konsumenten (eine
//!    zusätzliche `[dev-dependencies]`-Zeile mit `features = ["testing"]`)
//!    und quittiert das Vergessen mit einem Fehler *aus einer
//!    Makro-Expansion heraus* — die schlechteste Fehlermeldung, die diese
//!    API haben kann.
//! 4. Die Features im Workspace (`chatgpt-oauth`, `keyring`, `browser`)
//!    schalten allesamt optionale *Abhängigkeiten* zu. Test-Fixtures sind
//!    kein solcher Fall, und dieses Modul zieht keine einzige zusätzliche
//!    Abhängigkeit — insbesondere kein `tempfile`: welchen Store
//!    [`impl_store_conformance_tests!`](crate::impl_store_conformance_tests)
//!    prüft und woher dessen Verzeichnis kommt, entscheidet die Aufrufstelle.
//!
//! Die verbleibenden Funktionen sind ein knappes Dutzend kleiner Konstruktoren,
//! die in einem Release-Build ungenutzt und damit wegoptimiert sind.
//!
//! # Concurrency
//! Alle Funktionen sind rein und ohne geteilten Zustand; die erzeugten Werte
//! sind `Send + Sync`. [`fixture_time`] liest bewusst keine Systemuhr, damit
//! Fixtures deterministisch bleiben.
//!
//! # Errors
//! Dieses Modul erzeugt keine Fehler. [`expect_ok`] und [`expect_err`]
//! *konsumieren* fremde [`crate::error::PlanResult`]-Werte und brechen den
//! Test per `panic!` mit Kontext ab, statt `unwrap()`/`expect()` zu verwenden.
//!
//! # Examples
//! ```rust,no_run
//! use harw_plan::{plan, plan_node};
//!
//! let root = plan_node!("t-root", kind: Contract, write: ["src/api.rs"]);
//! let leaf = plan_node!("t-leaf", deps: ["t-root"], status: Ready);
//! let plan = plan!(goal: "API festziehen", nodes: [root, leaf]);
//!
//! assert_eq!(plan.nodes.len(), 2);
//! ```

use time::OffsetDateTime;

use crate::ids::{PathOrSymbol, PlanId, RevisionId, TaskId};
use crate::types::{Plan, PlanNode, PlanNodeKind, PlanNodeStatus};

/// Leere Dependency-Liste für die `deps:`-Schlüssel der Makros.
///
/// # Description
/// Ein nacktes `[]` lässt den Elementtyp offen und ist daher nicht
/// inferierbar. Diese Konstante bindet ihn auf `&str` fest.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::{plan_node, testing::NO_DEPS};
/// let node = plan_node!("t-1", deps: NO_DEPS);
/// assert!(node.dependencies.is_empty());
/// ```
pub const NO_DEPS: [&str; 0] = [];

/// Leere Scope-Liste für die `read:`/`write:`/`forbidden:`-Schlüssel.
///
/// # Description
/// Gegenstück zu [`NO_DEPS`]. Wird vor allem für `write: NO_SCOPES`
/// gebraucht — ein Knoten, der nichts schreiben darf (Fall aus
/// `admission.rs`).
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::{plan_node, testing::NO_SCOPES};
/// let node = plan_node!("t-1", write: NO_SCOPES);
/// assert!(node.write_scope.is_empty());
/// ```
pub const NO_SCOPES: [&str; 0] = [];

/// Fester Zeitstempel für alle Fixtures.
///
/// # Description
/// `OffsetDateTime::UNIX_EPOCH` — die Konvention sämtlicher abgelöster
/// `make_node`-Helfer in `harw-plan`. Bewusst keine Systemuhr: Fixtures
/// müssen reproduzierbar sein, und die Stores überschreiben `created_at` /
/// `updated_at` ohnehin bei jeder Mutation (Design-Doc §7).
///
/// # Returns
/// `OffsetDateTime` — immer derselbe Wert.
///
/// # Concurrency
/// Rein funktional, thread-sicher.
pub fn fixture_time() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH
}

/// Baut den kanonischen Test-Knoten.
///
/// # Description
/// Die Defaults sind nicht frei gewählt, sondern die Mehrheitskonvention der
/// abgelösten Fixtures:
///
/// | Feld | Default | Herkunft |
/// |---|---|---|
/// | `objective` | `"test objective"` | `graph.rs`, `lib.rs`, `validate.rs` |
/// | `write_scope` | `["src/<id>.rs"]` | zehn Fixtures, u. a. `actions.rs`, `mutation.rs`, `memory_store.rs` |
/// | `status` | [`PlanNodeStatus::Draft`] | `config.rs`, `memory_store.rs`, `file_store.rs` |
/// | `kind` | [`PlanNodeKind::Coding`] (= `default()`) | `actions.rs`, `types.rs` |
/// | Zeitstempel | [`fixture_time`] | alle |
///
/// Der id-abhängige `write_scope` ist der wichtigste dieser Defaults: die
/// Standardkonfiguration prüft Schreibkonflikte
/// (`PlanToolConfig::validate_write_conflicts`), ein konstanter Scope würde
/// jeden Mehr-Knoten-Plan kollidieren lassen.
///
/// Alle übrigen Listenfelder sind leer, alle `Option`-Felder `None`.
///
/// # Arguments
/// - `id` (`impl AsRef<str>`): Task-ID; geht auch in den Default-`write_scope`
///   ein. Darf nicht leer sein, sonst weist `validate` den Knoten ab.
///
/// # Returns
/// Einen vollständig befüllten [`PlanNode`].
///
/// # Concurrency
/// Rein funktional, thread-sicher.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::testing::base_node;
/// let node = base_node("t-1");
/// assert_eq!(node.write_scope.len(), 1);
/// ```
pub fn base_node(id: impl AsRef<str>) -> PlanNode {
    let id = id.as_ref();
    PlanNode {
        id: TaskId::new(id),
        objective: "test objective".to_owned(),
        dependencies: Vec::new(),
        input_contracts: Vec::new(),
        output_contracts: Vec::new(),
        read_scope: Vec::new(),
        write_scope: vec![PathOrSymbol::new(format!("src/{id}.rs"))],
        forbidden_scope: Vec::new(),
        acceptance_criteria: Vec::new(),
        invalidation_conditions: Vec::new(),
        status: PlanNodeStatus::Draft,
        evidence: Vec::new(),
        kind: PlanNodeKind::Coding,
        wave: None,
        assignment: None,
        parent: None,
        created_at: fixture_time(),
        updated_at: fixture_time(),
    }
}

/// Baut den kanonischen Test-Plan (ohne Knoten).
///
/// # Description
/// Entspricht den abgelösten `make_plan`/`test_plan`-Helfern: Revision 1,
/// keine Vorgänger-Revision, keine Ziel-Bindung, feste Zeitstempel.
///
/// # Returns
/// Einen leeren [`Plan`] mit der ID `p-test`.
///
/// # Concurrency
/// Rein funktional, thread-sicher.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::testing::base_plan;
/// assert!(base_plan().nodes.is_empty());
/// ```
pub fn base_plan() -> Plan {
    Plan {
        id: PlanId::new("p-test"),
        revision: RevisionId::new(1),
        parent_revision: None,
        goal_statement: "test goal".to_owned(),
        goal_id: None,
        nodes: Vec::new(),
        created_at: fixture_time(),
        updated_at: fixture_time(),
    }
}

/// Wandelt Zeichenketten in [`TaskId`]-Werte.
///
/// # Arguments
/// - `ids` (`impl IntoIterator<Item = impl Into<String>>`): z. B. ein Array
///   `["t-1", "t-2"]` oder ein `Vec<String>`.
///
/// # Returns
/// `Vec<TaskId>` in Eingabereihenfolge.
///
/// # Concurrency
/// Rein funktional, thread-sicher.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::testing::task_ids;
/// assert_eq!(task_ids(["t-1"]).len(), 1);
/// ```
pub fn task_ids<I, S>(ids: I) -> Vec<TaskId>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    ids.into_iter().map(TaskId::new).collect()
}

/// Wandelt Zeichenketten in dateiweite [`PathOrSymbol::Path`]-Einträge.
///
/// # Description
/// Erzeugt ausschließlich `Path`-Varianten — dieselbe Semantik wie
/// [`PathOrSymbol::new`]. Symbol-Scopes werden im Test direkt über
/// [`PathOrSymbol::symbol`] gesetzt, damit die Fixture keine Heuristik
/// einführt, die der Typ gerade abgeschafft hat.
///
/// # Arguments
/// - `entries` (`impl IntoIterator<Item = impl Into<String>>`): Pfade,
///   Verzeichnisse oder Glob-Muster.
///
/// # Returns
/// `Vec<PathOrSymbol>` in Eingabereihenfolge.
///
/// # Concurrency
/// Rein funktional, thread-sicher.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::testing::scopes;
/// assert_eq!(scopes(["src/lib.rs"])[0].as_str(), "src/lib.rs");
/// ```
pub fn scopes<I, S>(entries: I) -> Vec<PathOrSymbol>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    entries.into_iter().map(PathOrSymbol::new).collect()
}

/// Sammelt Knoten in einen `Vec<PlanNode>`.
///
/// # Description
/// Existiert, damit `nodes: [a, b]` (Array) und `nodes: vorhandener_vec`
/// im [`plan!`](crate::plan)-Makro dieselbe Schreibweise haben.
///
/// # Arguments
/// - `nodes` (`impl IntoIterator<Item = PlanNode>`): Array, `Vec` oder
///   Iterator.
///
/// # Returns
/// `Vec<PlanNode>` in Eingabereihenfolge.
///
/// # Concurrency
/// Rein funktional, thread-sicher.
pub fn plan_nodes(nodes: impl IntoIterator<Item = PlanNode>) -> Vec<PlanNode> {
    nodes.into_iter().collect()
}

/// Packt ein `Ok` aus oder bricht den Test mit Kontext ab.
///
/// # Description
/// Ersetzt `unwrap()`/`expect()` in Tests: Der Fehler wird über
/// [`std::fmt::Display`] ausgegeben, nicht über `Debug`, und trägt eine
/// aufrufseitige Beschreibung.
///
/// # Arguments
/// - `result` (`Result<T, E>`): das zu entpackende Ergebnis.
/// - `context` (`&str`): was versucht wurde, z. B. `"Create anwenden"`.
///
/// # Returns
/// Den `Ok`-Wert.
///
/// # Panics
/// Wenn `result` ein `Err` ist — der beabsichtigte Testabbruch.
///
/// # Concurrency
/// Rein funktional, thread-sicher.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::testing::expect_ok;
/// let value: Result<u8, String> = Ok(1);
/// assert_eq!(expect_ok(value, "Beispiel"), 1);
/// ```
pub fn expect_ok<T, E>(result: Result<T, E>, context: &str) -> T
where
    E: std::fmt::Display,
{
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: unerwarteter Fehler: {error}"),
    }
}

/// Packt ein `Err` aus oder bricht den Test mit Kontext ab.
///
/// # Arguments
/// - `result` (`Result<T, E>`): das zu entpackende Ergebnis.
/// - `context` (`&str`): welcher Fehler erwartet wurde.
///
/// # Returns
/// Den `Err`-Wert zur weiteren Prüfung (z. B. per `matches!`).
///
/// # Panics
/// Wenn `result` ein `Ok` ist — der Test erwartete einen Fehler.
///
/// # Concurrency
/// Rein funktional, thread-sicher.
pub fn expect_err<T, E>(result: Result<T, E>, context: &str) -> E
where
    T: std::fmt::Debug,
{
    match result {
        Ok(value) => panic!("{context}: Fehler erwartet, war aber Ok({value:?})"),
        Err(error) => error,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Makros
// ──────────────────────────────────────────────────────────────────────────────

/// Baut einen [`PlanNode`](crate::types::PlanNode) aus ID plus benannten
/// Abweichungen.
///
/// # Description
/// Ersetzt das 18-Felder-Struct-Literal der `make_node`-Kopien. Genannt wird
/// nur, was vom Default aus [`base_node`] abweicht — jedes künftige
/// `PlanNode`-Feld erweitert damit nur noch [`base_node`], nicht mehr ein
/// Dutzend Testmodule.
///
/// Die Form weicht bewusst von der Skizze `node!(id, kind: …, deps: […])` ab:
///
/// - **Name `plan_node!` statt `node!`.** `#[macro_export]` legt das Makro im
///   Crate-Root ab; `harw_plan::node` wäre in Konsumenten-Crates ein
///   auffällig kollisionsanfälliger Name.
/// - **`write:` / `read:` / `forbidden:` statt nur `write:`.** `admission.rs`
///   braucht `forbidden_scope`, `graph.rs` und `harw-plan-bridge` brauchen
///   `read_scope`.
/// - **`status:` zusätzlich zu `kind:`.** Sieben der abgelösten Fixtures
///   nehmen den Status als Parameter, nur eine den `kind`.
///
/// # Schlüssel
/// | Schlüssel | Zieltyp | Umwandlung |
/// |---|---|---|
/// | `kind` | [`PlanNodeKind`] | Varianten sind unqualifiziert schreibbar (`kind: Coding`) |
/// | `status` | [`PlanNodeStatus`] | ebenso (`status: Ready`) |
/// | `objective` | `String` | `String::from` |
/// | `deps` | `Vec<TaskId>` | [`task_ids`] |
/// | `read` / `write` / `forbidden` | `Vec<PathOrSymbol>` | [`scopes`] |
/// | `wave` | `Option<u32>` | in `Some` gehüllt |
/// | `parent` | `Option<TaskId>` | in `Some` gehüllt |
/// | `at` | `OffsetDateTime` | setzt `created_at` **und** `updated_at` |
///
/// `kind:` und `status:` akzeptieren beides — die unqualifizierte Variante
/// (`status: Draft`) und einen beliebigen Ausdruck (`status: erwarteter`),
/// weil die Expansion die Varianten nur im Zuweisungsblock in den Namensraum
/// holt. Eine lokale Bindung überschattet die Variante nicht, solange sie
/// nicht exakt so heißt (Variantennamen sind `CamelCase`, Bindungen
/// `snake_case`).
///
/// Felder ohne Schlüssel (`acceptance_criteria`, `evidence`,
/// `invalidation_conditions`, `input_contracts`, `output_contracts`,
/// `assignment`, `goal_id`) werden nachträglich am `mut`-Knoten gesetzt —
/// sie kommen in den abgelösten Fixtures nirgends als Konstruktionsparameter
/// vor.
///
/// Für leere Listen: [`NO_DEPS`] bzw. [`NO_SCOPES`] (ein nacktes `[]` ist
/// nicht typinferierbar).
///
/// # Panics
/// Keine.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::plan_node;
/// use harw_plan::types::PlanNodeStatus;
///
/// let node = plan_node!(
///     "t-api",
///     kind: Contract,
///     status: Ready,
///     deps: ["t-explore"],
///     write: ["src/api.rs"],
///     forbidden: ["src/secret.rs"],
/// );
/// assert_eq!(node.status, PlanNodeStatus::Ready);
/// assert_eq!(node.dependencies.len(), 1);
/// ```
#[macro_export]
macro_rules! plan_node {
    ($id:expr $(, $field:ident : $value:expr)* $(,)?) => {{
        // `unused_mut` ist hier unvermeidbar: wird das Makro ohne Felder
        // aufgerufen, folgt keine Mutation. Das `allow` gehoert an diese
        // Bindung und nicht an den Aufrufer — nur das Makro weiss, dass
        // die Mutation optional ist.
        #[allow(unused_mut)]
        let mut node = $crate::testing::base_node($id);
        $( $crate::__plan_node_field!(node, $field, $value); )*
        node
    }};
}

/// Interne Feld-Zuweisung für [`plan_node!`](crate::plan_node).
///
/// Nicht direkt aufrufen. Ein unbekannter Schlüssel schlägt hier fehl
/// ("no rules expected the token …") und benennt damit genau den Tippfehler.
#[doc(hidden)]
#[macro_export]
macro_rules! __plan_node_field {
    ($node:ident, kind, $value:expr) => {{
        // Holt `Coding`, `Contract`, … in den Namensraum, ohne einen
        // beliebigen Ausdruck auszuschließen.
        #[allow(unused_imports)]
        use $crate::types::PlanNodeKind::*;
        $node.kind = $value;
    }};
    ($node:ident, status, $value:expr) => {{
        #[allow(unused_imports)]
        use $crate::types::PlanNodeStatus::*;
        $node.status = $value;
    }};
    ($node:ident, objective, $value:expr) => {
        $node.objective = ::std::string::String::from($value);
    };
    ($node:ident, deps, $value:expr) => {
        $node.dependencies = $crate::testing::task_ids($value);
    };
    ($node:ident, read, $value:expr) => {
        $node.read_scope = $crate::testing::scopes($value);
    };
    ($node:ident, write, $value:expr) => {
        $node.write_scope = $crate::testing::scopes($value);
    };
    ($node:ident, forbidden, $value:expr) => {
        $node.forbidden_scope = $crate::testing::scopes($value);
    };
    ($node:ident, wave, $value:expr) => {
        $node.wave = ::core::option::Option::Some($value);
    };
    ($node:ident, parent, $value:expr) => {
        $node.parent = ::core::option::Option::Some($crate::ids::TaskId::new($value));
    };
    ($node:ident, at, $value:expr) => {{
        let at = $value;
        $node.created_at = at;
        $node.updated_at = at;
    }};
}

/// Baut einen [`Plan`](crate::types::Plan) aus benannten Abweichungen.
///
/// # Description
/// Ersetzt die `make_plan`/`test_plan`-Kopien. Ohne Argumente entsteht
/// [`base_plan`].
///
/// # Schlüssel
/// | Schlüssel | Zieltyp | Umwandlung |
/// |---|---|---|
/// | `id` | [`PlanId`] | `PlanId::new` |
/// | `goal` | `String` | `String::from` |
/// | `nodes` | `Vec<PlanNode>` | [`plan_nodes`] |
/// | `revision` | [`RevisionId`] | `RevisionId::new` (nimmt `u64`) |
/// | `parent_revision` | `Option<RevisionId>` | `Some(RevisionId::new(…))` |
/// | `at` | `OffsetDateTime` | setzt `created_at` **und** `updated_at` |
///
/// `goal_id` wird nachträglich gesetzt — keine der abgelösten Fixtures
/// belegt es bei der Konstruktion.
///
/// # Panics
/// Keine.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::{plan, plan_node};
///
/// let p = plan!(
///     id: "p-graph-test",
///     goal: "graph test",
///     nodes: [plan_node!("t-1"), plan_node!("t-2")],
/// );
/// assert_eq!(p.nodes.len(), 2);
/// ```
#[macro_export]
macro_rules! plan {
    ($($field:ident : $value:expr),* $(,)?) => {{
        // `unused_mut` ist hier unvermeidbar: wird das Makro ohne Felder
        // aufgerufen, folgt keine Mutation. Das `allow` gehoert an diese
        // Bindung und nicht an den Aufrufer — nur das Makro weiss, dass
        // die Mutation optional ist.
        #[allow(unused_mut)]
        let mut plan = $crate::testing::base_plan();
        $( $crate::__plan_field!(plan, $field, $value); )*
        plan
    }};
}

/// Interne Feld-Zuweisung für [`plan!`](crate::plan). Nicht direkt aufrufen.
#[doc(hidden)]
#[macro_export]
macro_rules! __plan_field {
    ($plan:ident, id, $value:expr) => {
        $plan.id = $crate::ids::PlanId::new($value);
    };
    ($plan:ident, goal, $value:expr) => {
        $plan.goal_statement = ::std::string::String::from($value);
    };
    ($plan:ident, nodes, $value:expr) => {
        $plan.nodes = $crate::testing::plan_nodes($value);
    };
    ($plan:ident, revision, $value:expr) => {
        $plan.revision = $crate::ids::RevisionId::new($value);
    };
    ($plan:ident, parent_revision, $value:expr) => {
        $plan.parent_revision =
            ::core::option::Option::Some($crate::ids::RevisionId::new($value));
    };
    ($plan:ident, at, $value:expr) => {{
        let at = $value;
        $plan.created_at = at;
        $plan.updated_at = at;
    }};
}

/// Erzeugt die gemeinsame Testliste für eine
/// [`PlanStore`](crate::store::PlanStore)-Implementierung.
///
/// # Description
/// Der Vertrag von `PlanStore` ist implementierungsunabhängig; die Tests
/// dafür waren es bisher nicht. Dieses Makro erzeugt zehn Testfunktionen in
/// einem eigenen Modul und macht damit ausdrücklich, was heute Zufall ist:
/// dass `InMemoryPlanStore` und `FilePlanStore` sich gleich verhalten.
///
/// Geprüft werden ausschließlich Verhaltensweisen, die *jede*
/// Implementierung erfüllen muss — Persistenz-Interna (`tmp`+`rename`,
/// `history.jsonl`-Anhängen, Neuladen nach Neustart) bleiben in
/// `file_store.rs`, weil sie dort hingehören.
///
/// Die Revision wird bewusst nur auf **Monotonie** geprüft, nicht auf feste
/// Zahlen: die Startrevision ist Implementierungsdetail.
///
/// # Arguments
/// - `$module` (`ident`): Name des erzeugten Testmoduls.
/// - `$factory` (`expr`): ein Closure ohne Argumente, das ein Tupel
///   `(store, guard)` liefert. `store` implementiert
///   [`PlanStore`](crate::store::PlanStore) und ist **frisch** (kein Plan);
///   `guard` hält alles am Leben, was der Store zum Arbeiten braucht — für
///   In-Memory-Stores `()`, für dateibasierte Stores das `TempDir`, das sonst
///   sofort gelöscht würde. Das Closure wird pro Test einmal ausgewertet.
///
/// # Panics
/// Die erzeugten Tests brechen per `panic!` ab, wenn der Store den Vertrag
/// verletzt (über [`expect_ok`] / [`expect_err`], nicht über `unwrap()`).
///
/// # Concurrency
/// Die erzeugten Tests teilen keinen Zustand; jeder baut seinen eigenen
/// Store. Sie laufen unter dem parallelen Standard-Testharness.
///
/// # Examples
/// ```rust,ignore
/// // In-Memory: kein Guard nötig.
/// harw_plan::impl_store_conformance_tests!(
///     memory_conformance,
///     || (InMemoryPlanStore::new(), ())
/// );
///
/// // Dateibasiert: das TempDir wird als Guard zurückgegeben und lebt
/// // dadurch bis zum Testende.
/// harw_plan::impl_store_conformance_tests!(file_conformance, || {
///     let dir = match tempfile::tempdir() {
///         Ok(dir) => dir,
///         Err(error) => panic!("tempdir: {error}"),
///     };
///     let store = match FilePlanStore::new(dir.path()) {
///         Ok(store) => store,
///         Err(error) => panic!("FilePlanStore::new: {error}"),
///     };
///     (store, dir)
/// });
/// ```
#[macro_export]
macro_rules! impl_store_conformance_tests {
    ($module:ident, $factory:expr $(,)?) => {
        #[cfg(test)]
        mod $module {
            #[allow(unused_imports)]
            use super::*;

            /// Baut einen frischen Store samt Lebensdauer-Guard.
            macro_rules! fresh_store {
                () => {{
                    let factory = $factory;
                    factory()
                }};
            }

            /// Legt den Plan an, über den die meisten Tests arbeiten.
            fn seed<S: $crate::store::PlanStore>(store: &S) -> $crate::actions::PlanEvent {
                $crate::testing::expect_ok(
                    $crate::store::PlanStore::apply(
                        store,
                        $crate::actions::PlanAction::Create {
                            plan_id: $crate::ids::PlanId::new("p-conformance"),
                            goal: ::std::string::String::from("Conformance-Ziel"),
                        },
                        "conformance",
                    ),
                    "Create anwenden",
                )
            }

            /// Fügt einen Knoten mit Default-Feldern hinzu.
            fn add<S: $crate::store::PlanStore>(
                store: &S,
                node: $crate::types::PlanNode,
            ) -> $crate::error::PlanResult<$crate::actions::PlanEvent> {
                $crate::store::PlanStore::apply(
                    store,
                    $crate::actions::PlanAction::AddNode { node },
                    "conformance",
                )
            }

            #[test]
            fn current_without_plan_reports_plan_not_found() {
                let (store, _guard) = fresh_store!();
                let error = $crate::testing::expect_err(
                    $crate::store::PlanStore::current(&store),
                    "current() ohne Plan",
                );
                assert!(
                    matches!(error, $crate::error::PlanError::PlanNotFound),
                    "erwartet PlanNotFound, war: {error}"
                );
            }

            #[test]
            fn create_makes_goal_and_id_visible() {
                let (store, _guard) = fresh_store!();
                seed(&store);

                let plan = $crate::testing::expect_ok(
                    $crate::store::PlanStore::current(&store),
                    "current() nach Create",
                );
                assert_eq!(plan.id, $crate::ids::PlanId::new("p-conformance"));
                assert_eq!(plan.goal_statement, "Conformance-Ziel");
                assert!(plan.nodes.is_empty(), "neuer Plan hat keine Knoten");
            }

            #[test]
            fn second_create_is_rejected_with_plan_exists() {
                let (store, _guard) = fresh_store!();
                seed(&store);

                let error = $crate::testing::expect_err(
                    $crate::store::PlanStore::apply(
                        &store,
                        $crate::actions::PlanAction::Create {
                            plan_id: $crate::ids::PlanId::new("p-zweiter"),
                            goal: ::std::string::String::from("darf nicht überschreiben"),
                        },
                        "conformance",
                    ),
                    "zweites Create",
                );
                assert!(
                    matches!(error, $crate::error::PlanError::PlanExists { .. }),
                    "erwartet PlanExists, war: {error}"
                );
            }

            #[test]
            fn add_node_without_plan_reports_plan_not_found() {
                let (store, _guard) = fresh_store!();
                let error = $crate::testing::expect_err(
                    add(&store, $crate::testing::base_node("t-1")),
                    "AddNode ohne Plan",
                );
                assert!(
                    matches!(error, $crate::error::PlanError::PlanNotFound),
                    "erwartet PlanNotFound, war: {error}"
                );
            }

            #[test]
            fn added_node_is_visible_in_current() {
                let (store, _guard) = fresh_store!();
                seed(&store);
                $crate::testing::expect_ok(
                    add(&store, $crate::testing::base_node("t-1")),
                    "AddNode",
                );

                let plan = $crate::testing::expect_ok(
                    $crate::store::PlanStore::current(&store),
                    "current() nach AddNode",
                );
                assert_eq!(plan.nodes.len(), 1);
                assert_eq!(
                    plan.nodes.first().map(|node| node.id.clone()),
                    ::core::option::Option::Some($crate::ids::TaskId::new("t-1"))
                );
            }

            #[test]
            fn duplicate_node_id_is_rejected() {
                let (store, _guard) = fresh_store!();
                seed(&store);
                $crate::testing::expect_ok(
                    add(&store, $crate::testing::base_node("t-1")),
                    "erstes AddNode",
                );

                let error = $crate::testing::expect_err(
                    add(&store, $crate::testing::base_node("t-1")),
                    "zweites AddNode mit gleicher ID",
                );
                assert!(
                    matches!(error, $crate::error::PlanError::DuplicateNode { .. }),
                    "erwartet DuplicateNode, war: {error}"
                );
            }

            #[test]
            fn node_with_unknown_dependency_is_rejected() {
                let (store, _guard) = fresh_store!();
                seed(&store);

                let mut node = $crate::testing::base_node("t-orphan");
                node.dependencies = $crate::testing::task_ids(["t-missing"]);

                let error = $crate::testing::expect_err(
                    add(&store, node),
                    "AddNode mit unbekannter Dependency",
                );
                assert!(
                    matches!(error, $crate::error::PlanError::NodeMissing { .. }),
                    "erwartet NodeMissing, war: {error}"
                );
            }

            #[test]
            fn revision_advances_with_every_applied_action() {
                let (store, _guard) = fresh_store!();
                seed(&store);
                let after_create = $crate::store::PlanStore::revision(&store);

                $crate::testing::expect_ok(
                    add(&store, $crate::testing::base_node("t-1")),
                    "AddNode",
                );
                let after_add = $crate::store::PlanStore::revision(&store);

                assert!(
                    after_add > after_create,
                    "Revision muss steigen: {after_create} -> {after_add}"
                );
            }

            #[test]
            fn history_records_applied_actions_in_order() {
                let (store, _guard) = fresh_store!();
                let create_event = seed(&store);
                let add_event = $crate::testing::expect_ok(
                    add(&store, $crate::testing::base_node("t-1")),
                    "AddNode",
                );

                let events = $crate::testing::expect_ok(
                    $crate::store::PlanStore::history(&store, ::core::option::Option::None),
                    "history(None)",
                );
                assert_eq!(events.len(), 2, "beide Aktionen müssen in der History stehen");
                assert_eq!(
                    events.first().map(|event| event.revision),
                    ::core::option::Option::Some(create_event.revision),
                    "Create steht vor AddNode"
                );
                assert_eq!(
                    events.get(1).map(|event| event.revision),
                    ::core::option::Option::Some(add_event.revision)
                );
            }

            #[test]
            fn history_since_filters_by_revision() {
                let (store, _guard) = fresh_store!();
                seed(&store);
                let add_event = $crate::testing::expect_ok(
                    add(&store, $crate::testing::base_node("t-1")),
                    "AddNode",
                );

                let events = $crate::testing::expect_ok(
                    $crate::store::PlanStore::history(
                        &store,
                        ::core::option::Option::Some(add_event.revision),
                    ),
                    "history(Some(revision))",
                );
                assert_eq!(events.len(), 1, "`since` ist inklusiv und filtert davor weg");
                assert_eq!(
                    events.first().map(|event| event.revision),
                    ::core::option::Option::Some(add_event.revision)
                );
            }

            #[test]
            fn ready_nodes_lists_draft_node_without_dependencies() {
                let (store, _guard) = fresh_store!();
                seed(&store);
                $crate::testing::expect_ok(
                    add(&store, $crate::testing::base_node("t-ready")),
                    "AddNode",
                );

                let ready = $crate::testing::expect_ok(
                    $crate::store::PlanStore::ready_nodes(&store),
                    "ready_nodes()",
                );
                assert_eq!(
                    ready,
                    vec![$crate::ids::TaskId::new("t-ready")],
                    "ein abhängigkeitsfreier Draft-Knoten ist ausführbar"
                );
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_store::InMemoryPlanStore;

    // Führt die gemeinsame Store-Testliste an einer Stelle vor. Der
    // In-Memory-Store braucht keinen Guard, deshalb `()`.
    crate::impl_store_conformance_tests!(
        in_memory_conformance,
        || (InMemoryPlanStore::new(), ())
    );

    #[test]
    fn base_node_uses_the_majority_defaults_of_the_replaced_fixtures() {
        let node = base_node("t-1");

        assert_eq!(node.id, TaskId::new("t-1"));
        assert_eq!(node.status, PlanNodeStatus::Draft);
        assert_eq!(node.kind, PlanNodeKind::Coding);
        assert_eq!(node.kind, PlanNodeKind::default());
        assert_eq!(node.objective, "test objective");
        assert_eq!(node.write_scope, scopes(["src/t-1.rs"]));
        assert!(node.read_scope.is_empty());
        assert!(node.forbidden_scope.is_empty());
        assert!(node.dependencies.is_empty());
        assert_eq!(node.wave, None);
        assert_eq!(node.parent, None);
        assert_eq!(node.created_at, fixture_time());
        assert_eq!(node.updated_at, fixture_time());
    }

    /// Der id-abhängige Default-`write_scope` ist kein Zufall: zwei Knoten
    /// desselben Plans dürfen sich nicht gegenseitig blockieren.
    #[test]
    fn base_node_write_scopes_do_not_collide_between_nodes() {
        assert_ne!(base_node("t-1").write_scope, base_node("t-2").write_scope);
    }

    #[test]
    fn plan_node_macro_applies_every_named_field() {
        let node = plan_node!(
            "t-api",
            kind: Contract,
            status: Ready,
            objective: "Vertrag festziehen",
            deps: ["t-explore"],
            read: ["src/store.rs"],
            write: ["src/api.rs", "src/api/mod.rs"],
            forbidden: ["src/secret.rs"],
            wave: 2,
            parent: "t-root",
            at: fixture_time(),
        );

        assert_eq!(node.id, TaskId::new("t-api"));
        assert_eq!(node.kind, PlanNodeKind::Contract);
        assert_eq!(node.status, PlanNodeStatus::Ready);
        assert_eq!(node.objective, "Vertrag festziehen");
        assert_eq!(node.dependencies, task_ids(["t-explore"]));
        assert_eq!(node.read_scope, scopes(["src/store.rs"]));
        assert_eq!(node.write_scope, scopes(["src/api.rs", "src/api/mod.rs"]));
        assert_eq!(node.forbidden_scope, scopes(["src/secret.rs"]));
        assert_eq!(node.wave, Some(2));
        assert_eq!(node.parent, Some(TaskId::new("t-root")));
    }

    #[test]
    fn plan_node_macro_without_fields_equals_base_node() {
        let node = plan_node!("t-1");
        let expected = base_node("t-1");

        assert_eq!(node.id, expected.id);
        assert_eq!(node.status, expected.status);
        assert_eq!(node.kind, expected.kind);
        assert_eq!(node.write_scope, expected.write_scope);
        assert_eq!(node.objective, expected.objective);
    }

    /// `kind:`/`status:` müssen sowohl die unqualifizierte Variante als auch
    /// einen Ausdruck annehmen — sieben der abgelösten Fixtures reichen den
    /// Status als Variable durch.
    #[test]
    fn plan_node_macro_accepts_variants_and_expressions() {
        for (requested_status, requested_kind) in [
            (PlanNodeStatus::Draft, PlanNodeKind::Coding),
            (PlanNodeStatus::Blocked, PlanNodeKind::Docs),
        ] {
            let node = plan_node!("t-1", status: requested_status, kind: requested_kind);
            assert_eq!(node.status, requested_status);
            assert_eq!(node.kind, requested_kind);
        }

        let literal = plan_node!("t-2", status: Invalidated, kind: Verification);
        assert_eq!(literal.status, PlanNodeStatus::Invalidated);
        assert_eq!(literal.kind, PlanNodeKind::Verification);
    }

    #[test]
    fn plan_node_macro_accepts_empty_lists_via_constants() {
        let node = plan_node!("t-1", deps: NO_DEPS, write: NO_SCOPES);
        assert!(node.dependencies.is_empty());
        assert!(node.write_scope.is_empty());
    }

    /// Der Fall aus `graph.rs` / `mutation.rs`: die ID wird zur Laufzeit
    /// gebildet, das Makro muss `String` genauso nehmen wie `&str`.
    #[test]
    fn plan_node_macro_accepts_owned_ids() {
        let node = plan_node!(format!("t-{}", 7));
        assert_eq!(node.id, TaskId::new("t-7"));
        assert_eq!(node.write_scope, scopes(["src/t-7.rs"]));
    }

    #[test]
    fn plan_macro_applies_every_named_field() {
        let built = plan!(
            id: "p-graph-test",
            goal: "graph test",
            revision: 4,
            parent_revision: 3,
            nodes: [plan_node!("t-1"), plan_node!("t-2")],
            at: fixture_time(),
        );

        assert_eq!(built.id, PlanId::new("p-graph-test"));
        assert_eq!(built.goal_statement, "graph test");
        assert_eq!(built.revision, RevisionId::new(4));
        assert_eq!(built.parent_revision, Some(RevisionId::new(3)));
        assert_eq!(built.nodes.len(), 2);
        assert_eq!(
            built.nodes.first().map(|node| node.id.clone()),
            Some(TaskId::new("t-1"))
        );
        assert_eq!(built.created_at, fixture_time());
    }

    /// Der Fall aus `validate.rs` / `goal.rs`: `make_plan(nodes)` bekommt
    /// einen fertigen `Vec`.
    #[test]
    fn plan_macro_accepts_a_prepared_node_vec() {
        let nodes = vec![plan_node!("t-1"), plan_node!("t-2"), plan_node!("t-3")];
        let built = plan!(goal: "aus Vec", nodes: nodes);

        assert_eq!(built.nodes.len(), 3);
        assert_eq!(built.goal_statement, "aus Vec");
    }

    #[test]
    fn plan_macro_without_fields_equals_base_plan() {
        let built = plan!();
        let expected = base_plan();

        assert_eq!(built.id, expected.id);
        assert_eq!(built.revision, expected.revision);
        assert!(built.nodes.is_empty());
    }

    #[test]
    fn scopes_produces_path_variants_only() {
        let built = scopes(["src/lib.rs", "TraitName"]);

        assert_eq!(built.len(), 2);
        assert!(
            !built.iter().any(PathOrSymbol::is_symbol),
            "die Fixture darf keine Symbol-Heuristik einführen"
        );
    }

    #[test]
    fn expect_ok_returns_the_value_and_expect_err_the_error() {
        let ok: Result<u8, crate::error::PlanError> = Ok(7);
        assert_eq!(expect_ok(ok, "Beispiel"), 7);

        let err: Result<u8, crate::error::PlanError> = Err(crate::error::PlanError::PlanNotFound);
        assert!(matches!(
            expect_err(err, "Beispiel"),
            crate::error::PlanError::PlanNotFound
        ));
    }
}
