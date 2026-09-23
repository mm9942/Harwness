//! Lokale Newtype-IDs für `harw-plan`.
//!
//! Verantwortungsbereich: Typisierte Identitäten für Pläne, Tasks und Revisionen,
//! sowie Pfad/Symbol-Referenzen und Vertragsreferenzen.
//!
//! TODO: Diese Typen in `harw-types` überführen, sobald der Cross-Crate-Vertrag
//! in einem Folge-Wave vereinheitlicht wird. (Design-Doc §2)
//!
//! Alle Typen sind `Send + Sync`: Newtypes über `String` / `u64`, dazu
//! [`PathOrSymbol`] als zweivariantiges Enum über `String`-Feldern.
//!
//! `PlanId` trägt eine feste Grammatik (Pfadsegment-sicher, F-013/G-032);
//! Fehler: [`PlanError::InvalidId`].
//!
//! Exportierte Typen: [`PlanId`], [`TaskId`], [`RevisionId`], [`PathOrSymbol`], [`ContractRef`].

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::PlanError;

/// Maximale Länge einer [`PlanId`] in Bytes (= Zeichen, da nur ASCII).
pub const PLAN_ID_MAX_LEN: usize = 64;

/// Eindeutiger Bezeichner eines Plans.
///
/// # Description
/// Newtype über `String` mit fester Grammatik `^[a-z0-9][a-z0-9-]{0,63}$`
/// (F-013, G-032). Die ID wird von [`crate::file_store::FilePlanStore`] als
/// Pfadsegment unter `<root>/plans/` verwendet; die Grammatik schließt deshalb
/// `/`, `\`, `.`/`..`, Steuerzeichen, Leerraum und jedes Nicht-ASCII-Zeichen
/// (Unicode-Homoglyphen, Bidi-Steuerzeichen) aus.
///
/// Durchgesetzt wird die Grammatik von [`PlanId::parse`], [`PlanId::try_new`],
/// [`FromStr`], `Deserialize` (über `#[serde(try_from = "String")]`),
/// `validate::validate_with` (`Create`) und `FilePlanStore` vor jedem
/// Pfad-`join`. [`PlanId::new`] prüft **nicht** (siehe dort).
///
/// # Concurrency
/// `Send + Sync` (Newtype über `String`).
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::ids::PlanId;
/// let id = PlanId::parse("plan-001").unwrap();
/// assert_eq!(id.as_str(), "plan-001");
/// assert!(PlanId::parse("../etc").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct PlanId(String);

impl PlanId {
    /// Parst und validiert eine Plan-ID gegen die feste Grammatik.
    ///
    /// # Description
    /// Erlaubt sind 1 bis [`PLAN_ID_MAX_LEN`] Zeichen aus `a-z`, `0-9` und `-`;
    /// das erste Zeichen ist kein `-`. Damit sind Pfadtrenner, `.`/`..`,
    /// Steuerzeichen, Leerraum, Großbuchstaben und Nicht-ASCII ausgeschlossen.
    ///
    /// # Arguments
    /// - `raw` (`&str`): der ungeprüfte Rohwert.
    ///
    /// # Returns
    /// Die validierte `PlanId`.
    ///
    /// # Errors
    /// - [`PlanError::InvalidId`] mit `field = "PlanId"` und dem Rohwert, wenn
    ///   die Grammatik verletzt ist.
    ///
    /// # Concurrency
    /// Rein, keine Sperren.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::ids::PlanId;
    /// assert!(PlanId::parse("p-1").is_ok());
    /// assert!(PlanId::parse("a/b").is_err());
    /// ```
    pub fn parse(raw: &str) -> Result<PlanId, PlanError> {
        if is_valid_plan_id(raw) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(PlanError::InvalidId {
                field: "PlanId",
                value: raw.to_owned(),
            })
        }
    }

    /// Fallibler Konstruktor; identisch zu [`PlanId::parse`].
    ///
    /// # Errors
    /// - [`PlanError::InvalidId`] bei Grammatikverletzung.
    pub fn try_new(value: impl Into<String>) -> Result<PlanId, PlanError> {
        let value = value.into();
        Self::parse(&value)
    }

    /// Ungeprüfter Kompatibilitätskonstruktor.
    ///
    /// # Description
    /// **Unchecked — nur für vertrauenswürdige Konstanten und Tests.** Der Wert
    /// wird nicht gegen die Grammatik geprüft. Die Store-Grenze
    /// (`validate_with` bei `Create`, `FilePlanStore` vor jedem Pfad-`join`)
    /// weist ungültige IDs trotzdem ab. Entfernung geplant in W12; neue
    /// Aufrufer verwenden [`PlanId::parse`].
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowt den inneren String-Slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Gibt den inneren `String` konsumierend zurück.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    /// Prüft, ob diese ID die Grammatik erfüllt (relevant für über
    /// [`PlanId::new`] erzeugte Werte).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        is_valid_plan_id(&self.0)
    }
}

/// Grammatik `^[a-z0-9][a-z0-9-]{0,63}$` (siehe [`PlanId`]).
fn is_valid_plan_id(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    bytes.len() <= PLAN_ID_MAX_LEN
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

impl TryFrom<String> for PlanId {
    type Error = PlanError;

    /// Validierende Konvertierung; Grundlage von `Deserialize`.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl fmt::Display for PlanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PlanId {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl AsRef<str> for PlanId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for PlanId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for PlanId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for PlanId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for PlanId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

/// Eindeutiger Bezeichner eines Plan-Knotens (Task).
///
/// # Description
/// Newtype über `String`. Innerhalb eines Plans eindeutig.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::ids::TaskId;
/// let id: TaskId = "task-42".parse().unwrap();
/// assert_eq!(id.as_str(), "task-42");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, harw_macros::HarwId)]
// `infallible` erhaelt den unvalidierten `new(impl Into<String>)`, den der
// Bestand ueberall benutzt. `try_new` kommt zusaetzlich dazu und weist leere
// oder nur aus Steuerzeichen bestehende Bezeichner ab — das Derive liefert
// damit mehr als die abgeloeste Handschrift, ohne bestehende Aufrufer zu
// brechen.
#[harw_id(infallible, error = "crate::error::PlanError", ctor = "empty_id")]
pub struct TaskId(String);

/// Monoton steigende Revisionskennung eines Plans.
///
/// # Description
/// Newtype über `u64`. Revisionsnummer 0 ist die Anfangsrevision;
/// jede `Supersede`-Aktion muss eine höhere Zahl setzen.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::ids::RevisionId;
/// let r = RevisionId::new(1);
/// assert!(r > RevisionId::new(0));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RevisionId(u64);

impl RevisionId {
    /// Erstellt eine `RevisionId` aus einem `u64`.
    pub fn new(n: u64) -> Self {
        Self(n)
    }

    /// Gibt den numerischen Wert zurück.
    pub fn value(self) -> u64 {
        self.0
    }

    /// Liefert die nächste Revision (inkrementiert um 1).
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for RevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for RevisionId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let n: u64 = s.parse()?;
        Ok(Self(n))
    }
}

/// Referenz auf einen Dateipfad oder ein Symbol im Repository.
///
/// # Description
/// Wird für `read_scope`, `write_scope` und `forbidden_scope` eines
/// [`crate::types::PlanNode`] verwendet. Zwei Varianten statt des vormaligen
/// String-Newtypes (Design-Doc §2):
///
/// - [`PathOrSymbol::Path`] — ein dateiweiter Bereich (Datei, Verzeichnis
///   oder Glob-Muster).
/// - [`PathOrSymbol::Symbol`] — ein benanntes Symbol *innerhalb* einer Datei.
///   Der Pfad ist Pflichtbestandteil: ein Symbol ohne Datei wäre für das
///   Scope-Enforcement nicht auflösbar.
///
/// Die Datei bleibt in **beiden** Fällen die Einheit des Enforcements:
/// `admission::path_or_symbol_to_rules` bildet Scope-Einträge auf dateiweise
/// [`crate::admission::ScopeMatcher`]-Regeln ab, Symbol-Granularität ist
/// Gegenstand einer Folge-Welle (Design-Doc §5). Deshalb liefert
/// [`PathOrSymbol::as_str`] auch bei `Symbol` den **Pfad**-Anteil und nicht
/// eine zusammengesetzte Form wie `path::symbol` — ein zusammengesetzter
/// String würde dort zu einer `Exact`-Regel führen, die auf keinen realen
/// Patch-Pfad passt und den Scope damit stillschweigend wirkungslos machte.
///
/// # Serde-Kompatibilität
/// Das Enum ist `#[serde(untagged)]`:
///
/// - `Path` serialisiert als **schlichter String** (`"src/lib.rs"`) — exakt
///   die Form, die der frühere Newtype erzeugt hat.
/// - `Symbol` serialisiert als Objekt (`{"path": …, "symbol": …}`).
///
/// Bestehende Pläne (`plan.json`) und `history.jsonl`-Einträge führen
/// Scope-Einträge als schlichte Strings und deserialisieren unverändert nach
/// `Path(_)`. Die Variantenreihenfolge ist dabei nicht kritisch — ein
/// JSON-String kann nie auf `Symbol` passen und ein JSON-Objekt nie auf
/// `Path` —, die Regression wird trotzdem durch
/// `legacy_plan_json_with_plain_string_scopes_stays_readable` festgehalten.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::ids::PathOrSymbol;
///
/// let path = PathOrSymbol::new("src/lib.rs");
/// assert_eq!(path.as_str(), "src/lib.rs");
///
/// let symbol = PathOrSymbol::symbol("src/lib.rs", "PlanStore");
/// // Enforcement bleibt dateiweise:
/// assert_eq!(symbol.as_str(), "src/lib.rs");
/// assert_eq!(symbol.symbol_name(), Some("PlanStore"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathOrSymbol {
    /// Ein dateiweiter Bereich: Datei, Verzeichnis oder Glob-Muster.
    ///
    /// Serialisiert als schlichter String — die Wire-Form des früheren
    /// Newtypes.
    Path(String),
    /// Ein benanntes Symbol innerhalb einer Datei.
    ///
    /// Serialisiert als Objekt mit den Feldern `path` und `symbol`.
    Symbol {
        /// Datei, in der das Symbol liegt. Trägt das Enforcement.
        path: String,
        /// Name des Symbols innerhalb der Datei (z. B. `PlanStore`).
        symbol: String,
    },
}

impl PathOrSymbol {
    /// Erstellt einen dateiweiten Scope-Eintrag ([`PathOrSymbol::Path`]).
    ///
    /// # Description
    /// Semantisch unverändert gegenüber dem früheren String-Newtype: jeder
    /// über `new` erzeugte Eintrag ist ein `Path`. Symbol-Einträge entstehen
    /// ausschließlich über [`PathOrSymbol::symbol`] oder über
    /// Deserialisierung der Objekt-Form.
    ///
    /// # Arguments
    /// - `s` (`impl Into<String>`): Pfad, Verzeichnis oder Glob-Muster.
    ///
    /// # Returns
    /// `PathOrSymbol::Path(s)`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::ids::PathOrSymbol;
    /// assert_eq!(PathOrSymbol::new("src/lib.rs").as_str(), "src/lib.rs");
    /// ```
    pub fn new(s: impl Into<String>) -> Self {
        Self::Path(s.into())
    }

    /// Erstellt einen Symbol-Scope ([`PathOrSymbol::Symbol`]).
    ///
    /// # Arguments
    /// - `path` (`impl Into<String>`): Datei, in der das Symbol liegt.
    /// - `symbol` (`impl Into<String>`): Name des Symbols.
    ///
    /// # Returns
    /// `PathOrSymbol::Symbol { path, symbol }`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::ids::PathOrSymbol;
    /// let s = PathOrSymbol::symbol("src/store.rs", "PlanStore");
    /// assert_eq!(s.symbol_name(), Some("PlanStore"));
    /// ```
    pub fn symbol(path: impl Into<String>, symbol: impl Into<String>) -> Self {
        Self::Symbol {
            path: path.into(),
            symbol: symbol.into(),
        }
    }

    /// Gibt den Pfad-Anteil zurück — die Einheit des Scope-Enforcements.
    ///
    /// # Description
    /// Bei [`PathOrSymbol::Path`] der Pfad selbst, bei
    /// [`PathOrSymbol::Symbol`] das Feld `path`. Damit sehen alle
    /// dateiweise arbeitenden Aufrufer (`admission::path_or_symbol_to_rules`,
    /// `graph::scopes_overlap`, Renderer) für einen Symbol-Eintrag genau die
    /// Datei, in der das Symbol liegt.
    ///
    /// # Returns
    /// `&str` — Pfad, Verzeichnis oder Glob-Muster; nie der Symbolname.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Path(path) => path,
            Self::Symbol { path, .. } => path,
        }
    }

    /// Gibt den Symbolnamen zurück, falls dieser Eintrag ein Symbol ist.
    ///
    /// # Returns
    /// `Some(&str)` mit dem Symbolnamen bei [`PathOrSymbol::Symbol`],
    /// sonst `None`.
    pub fn symbol_name(&self) -> Option<&str> {
        match self {
            Self::Path(_) => None,
            Self::Symbol { symbol, .. } => Some(symbol),
        }
    }

    /// Prüft, ob dieser Eintrag ein Symbol innerhalb einer Datei bezeichnet.
    ///
    /// # Returns
    /// `true` genau dann, wenn die Variante [`PathOrSymbol::Symbol`] ist.
    pub fn is_symbol(&self) -> bool {
        matches!(self, Self::Symbol { .. })
    }
}

impl fmt::Display for PathOrSymbol {
    /// Menschenlesbare Form: `src/lib.rs` bzw. `src/lib.rs::PlanStore`.
    ///
    /// Bewusst **nicht** identisch mit [`PathOrSymbol::as_str`]: Display ist
    /// für Diagnose und Anzeige gedacht, `as_str` für das dateiweise
    /// Enforcement.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => f.write_str(path),
            Self::Symbol { path, symbol } => write!(f, "{path}::{symbol}"),
        }
    }
}

impl FromStr for PathOrSymbol {
    type Err = std::convert::Infallible;

    /// Parst jede Eingabe als [`PathOrSymbol::Path`] — verlustfrei und ohne
    /// Heuristik.
    ///
    /// Ein `::` im String wird bewusst **nicht** als Symbol-Trenner gedeutet:
    /// Alt-Daten dürfen ihre Bedeutung durch den Typwechsel nicht ändern.
    /// Symbol-Einträge entstehen über [`PathOrSymbol::symbol`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::Path(s.to_owned()))
    }
}

/// Referenz auf einen Interface-Vertrag (z. B. Trait, Schema, Protokoll).
///
/// # Description
/// Newtype über `String`. Wird für `input_contracts` und `output_contracts`
/// in `PlanNode` verwendet.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::ids::ContractRef;
/// let c = ContractRef::new("PlanStore::apply");
/// assert_eq!(c.as_str(), "PlanStore::apply");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, harw_macros::HarwId)]
// `infallible` erhaelt den unvalidierten `new(impl Into<String>)`, den der
// Bestand ueberall benutzt. `try_new` kommt zusaetzlich dazu und weist leere
// oder nur aus Steuerzeichen bestehende Bezeichner ab — das Derive liefert
// damit mehr als die abgeloeste Handschrift, ohne bestehende Aufrufer zu
// brechen.
#[harw_id(infallible, error = "crate::error::PlanError", ctor = "empty_id")]
pub struct ContractRef(String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use crate::types::PlanNode;

    /// Ein `PlanNode`, wie ihn ein Plan aus der Newtype-Ära auf Platte hat:
    /// Scope-Einträge sind schlichte Strings, `kind`/`wave`/`assignment`/
    /// `parent` fehlen noch ganz. Bewusst als Literal und nicht über
    /// `serde_json::to_value` erzeugt — nur so pinnt der Test die tatsächliche
    /// Wire-Form und nicht bloß den aktuellen Serializer gegen sich selbst.
    const LEGACY_PLAN_NODE_JSON: &str = r#"{
        "id": "t-legacy",
        "objective": "Alt-Plan aus der Newtype-Ära",
        "dependencies": [],
        "input_contracts": [],
        "output_contracts": [],
        "read_scope": ["src/store.rs"],
        "write_scope": ["src/lib.rs"],
        "forbidden_scope": ["src/secret.rs"],
        "acceptance_criteria": [],
        "invalidation_conditions": [],
        "status": "draft",
        "evidence": [],
        "created_at": "1970-01-01T00:00:00Z",
        "updated_at": "1970-01-01T00:00:00Z"
    }"#;

    /// Hilft den Tests ohne `unwrap()`/`expect()` aus (Bible R087/R165).
    fn ok_or_err<T, E: fmt::Display>(result: Result<T, E>, context: &'static str) -> TestResult<T> {
        result.map_err(|error| TestError::Context {
            context,
            source: error.to_string(),
        })
    }

    #[test]
    fn test_plan_id_newtype_roundtrip() -> TestResult {
        let id = PlanId::new("plan-abc");
        let json = ok_or_err(serde_json::to_string(&id), "PlanId serialisieren")?;
        let back: PlanId = ok_or_err(serde_json::from_str(&json), "PlanId deserialisieren")?;
        assert_eq!(id, back);
        assert_eq!(back.as_str(), "plan-abc");
        Ok(())
    }

    #[test]
    fn test_revision_id_from_str() -> TestResult {
        let r: RevisionId = ok_or_err("42".parse(), "RevisionId parsen")?;
        assert_eq!(r.value(), 42);
        assert_eq!(r.next().value(), 43);
        assert!(r > RevisionId::new(0));
        Ok(())
    }

    // ── PathOrSymbol: Serde-Kompatibilität ──────────────────────────────────

    /// Der wichtigste Test des Typwechsels: ein auf Platte liegender Alt-Plan
    /// mit schlichten Strings in allen drei Scope-Listen muss unverändert
    /// lesbar bleiben und als `Path(_)` ankommen. Schlägt er fehl, ist jede
    /// bestehende `plan.json` und jede `history.jsonl` unlesbar geworden.
    #[test]
    fn legacy_plan_json_with_plain_string_scopes_stays_readable() -> TestResult {
        let node: PlanNode = ok_or_err(
            serde_json::from_str(LEGACY_PLAN_NODE_JSON),
            "Alt-Plan mit String-Scopes deserialisieren",
        )?;

        assert_eq!(
            node.write_scope,
            vec![PathOrSymbol::Path("src/lib.rs".to_owned())],
            "write_scope muss als Path-Variante ankommen"
        );
        assert_eq!(
            node.read_scope,
            vec![PathOrSymbol::Path("src/store.rs".to_owned())]
        );
        assert_eq!(
            node.forbidden_scope,
            vec![PathOrSymbol::Path("src/secret.rs".to_owned())]
        );

        // Kein Eintrag darf durch den Typwechsel zum Symbol umgedeutet werden.
        assert!(
            !node.write_scope.iter().any(PathOrSymbol::is_symbol),
            "Alt-Einträge dürfen nicht heuristisch zu Symbolen werden"
        );
        assert_eq!(
            node.write_scope.first().map(PathOrSymbol::as_str),
            Some("src/lib.rs"),
            "as_str muss den unveränderten Alt-String liefern"
        );

        // Rückrichtung: erneut serialisiert steht dort wieder ein schlichter
        // String — ein Alt-Leser bleibt lesefähig.
        let value = ok_or_err(serde_json::to_value(&node), "PlanNode serialisieren")?;
        assert_eq!(
            value.get("write_scope"),
            Some(&serde_json::json!(["src/lib.rs"])),
            "Path-Einträge müssen als schlichte Strings zurückgeschrieben werden"
        );
        Ok(())
    }

    #[test]
    fn legacy_scope_list_of_plain_strings_deserializes_as_path() -> TestResult {
        let scopes: Vec<PathOrSymbol> = ok_or_err(
            serde_json::from_str(r#"["src/lib.rs", "src/", "src/**/*.rs", "TraitName"]"#),
            "Scope-Liste deserialisieren",
        )?;

        assert_eq!(scopes.len(), 4);
        for scope in &scopes {
            assert!(
                matches!(scope, PathOrSymbol::Path(_)),
                "{scope:?} muss Path sein — auch ein Eintrag ohne / und ."
            );
        }
        Ok(())
    }

    #[test]
    fn path_variant_serializes_as_plain_string() -> TestResult {
        let json = ok_or_err(
            serde_json::to_string(&PathOrSymbol::new("src/lib.rs")),
            "Path serialisieren",
        )?;
        assert_eq!(json, "\"src/lib.rs\"", "Wire-Form des Newtypes bleibt");
        Ok(())
    }

    #[test]
    fn symbol_roundtrip_is_lossless() -> TestResult {
        let original = PathOrSymbol::symbol("src/store.rs", "PlanStore");
        let json = ok_or_err(serde_json::to_string(&original), "Symbol serialisieren")?;
        let back: PathOrSymbol = ok_or_err(serde_json::from_str(&json), "Symbol deserialisieren")?;

        assert_eq!(back, original, "Symbol-Roundtrip muss verlustfrei sein");
        assert_eq!(back.symbol_name(), Some("PlanStore"));
        assert_eq!(back.as_str(), "src/store.rs");

        let value = ok_or_err(serde_json::to_value(&original), "Symbol als Value")?;
        assert_eq!(
            value,
            serde_json::json!({"path": "src/store.rs", "symbol": "PlanStore"}),
            "Symbol serialisiert als Objekt, nicht als String"
        );
        Ok(())
    }

    // ── PathOrSymbol: unveränderte Semantik der Bestandsmethoden ────────────

    #[test]
    fn new_produces_path_and_as_str_is_unchanged() {
        let scope = PathOrSymbol::new("src/lib.rs");
        assert_eq!(scope, PathOrSymbol::Path("src/lib.rs".to_owned()));
        assert_eq!(scope.as_str(), "src/lib.rs");
        assert!(!scope.is_symbol());
        assert_eq!(scope.symbol_name(), None);
    }

    /// `as_str` trägt das dateiweise Enforcement: für ein Symbol muss der
    /// Pfad herauskommen, sonst erzeugte `path_or_symbol_to_rules` eine
    /// `Exact`-Regel, die auf keinen realen Patch-Pfad passt.
    #[test]
    fn symbol_as_str_yields_the_enclosing_path() {
        let scope = PathOrSymbol::symbol("src/lib.rs", "PlanStore");
        assert_eq!(scope.as_str(), "src/lib.rs");
        assert!(scope.is_symbol());
    }

    #[test]
    fn display_shows_the_symbol_but_as_str_does_not() {
        let path = PathOrSymbol::new("src/lib.rs");
        let symbol = PathOrSymbol::symbol("src/lib.rs", "PlanStore");

        assert_eq!(path.to_string(), "src/lib.rs");
        assert_eq!(symbol.to_string(), "src/lib.rs::PlanStore");
        assert_eq!(symbol.as_str(), "src/lib.rs");
    }

    #[test]
    fn from_str_never_infers_a_symbol() -> TestResult {
        let parsed: PathOrSymbol = ok_or_err("src/lib.rs::PlanStore".parse(), "parse")?;
        assert_eq!(
            parsed,
            PathOrSymbol::Path("src/lib.rs::PlanStore".to_owned()),
            "`::` darf nicht als Symbol-Trenner gedeutet werden"
        );
        Ok(())
    }

    // ── PlanId-Grammatik (F-013, G-032) ─────────────────────────────────────

    #[test]
    fn test_plan_id_parse_accepts_existing_ids() -> TestResult {
        for raw in [
            "p-1",
            "plan-abc",
            "p-test",
            "restart",
            "update-rollback",
            "plan-cli",
            "plan-analyze",
            "0",
            "a",
        ] {
            let id = ok_or_err(PlanId::parse(raw), raw)?;
            assert_eq!(id.as_str(), raw);
            assert!(id.is_valid());
        }
        let max = "a".repeat(PLAN_ID_MAX_LEN);
        assert!(PlanId::parse(&max).is_ok(), "64 Zeichen sind erlaubt");
        Ok(())
    }

    #[test]
    fn test_plan_id_parse_rejects_traversal_and_separators() {
        for raw in [
            "",
            "..",
            ".",
            "../x",
            "../../etc",
            "a/b",
            "/abs",
            "a\\b",
            "a.b",
            "-lead",
            "a b",
            "a\tb",
            "a\nb",
            "a\0b",
            "p_1",
            "P-1",
        ] {
            let result = PlanId::parse(raw);
            assert!(
                matches!(
                    &result,
                    Err(PlanError::InvalidId { field: "PlanId", value }) if value == raw
                ),
                "{raw:?} muss abgewiesen werden, war: {result:?}"
            );
        }
        let too_long = "a".repeat(PLAN_ID_MAX_LEN + 1);
        assert!(PlanId::parse(&too_long).is_err(), "65 Zeichen sind zu lang");
    }

    #[test]
    fn test_plan_id_parse_rejects_unicode() {
        // Kyrillisches а (Homoglyph), Bidi-Override, Zeilentrenner, Umlaut,
        // Fullwidth-Solidus.
        for raw in [
            "p-\u{0430}",
            "p\u{202E}1",
            "p\u{2028}1",
            "plän",
            "a\u{FF0F}b",
        ] {
            assert!(
                PlanId::parse(raw).is_err(),
                "{raw:?} muss abgewiesen werden"
            );
        }
    }

    #[test]
    fn test_plan_id_deserialize_enforces_grammar() {
        let ok: Result<PlanId, _> = serde_json::from_str("\"p-ok\"");
        assert!(ok.is_ok());
        let bad: Result<PlanId, _> = serde_json::from_str("\"../escape\"");
        assert!(bad.is_err(), "Deserialize muss Traversal abweisen");
    }

    #[test]
    fn test_plan_id_from_str_and_try_new_enforce_grammar() {
        assert!("a/b".parse::<PlanId>().is_err());
        assert!(PlanId::try_new("..").is_err());
        assert!(PlanId::try_new("p-2").is_ok());
    }

    #[test]
    fn test_plan_id_new_is_unchecked() {
        let unchecked = PlanId::new("../x");
        assert_eq!(unchecked.as_str(), "../x");
        assert!(!unchecked.is_valid(), "new prüft nicht, is_valid meldet es");
    }
}
