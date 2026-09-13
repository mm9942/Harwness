# Contract-Master AW0 — die verbindliche `pub`-Fläche des Vokabulars

**Zweck.** Rund einundvierzig der neunundvierzig neuen Crates hängen an den
hier festgelegten Symbolen. Dieses Dokument ist die einzige Quelle für ihre
Form, solange die AW0-Crates noch nicht auf der Platte liegen. Sobald sie es
tun, gilt der Code, nicht dieses Dokument.

**Verbindlichkeit.** Die Rust-Blöcke sind **wörtlich zu übernehmen**, nicht zu
interpretieren. Wer eine Signatur ableitet statt sie zu lesen, erzeugt die
Vertragsdrift, die im Vorprogramm 26 Compile-Fehler auf einmal produziert hat.
Ergänzungen sind erlaubt (weitere Methoden, weitere `impl`-Blöcke, private
Helfer); Änderungen an einer hier gezeigten Signatur sind es nicht.

**Was jede neue Crate erbt.**

```toml
# Cargo.toml jeder neuen Crate
[package]
name = "…"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
# Geteilte Abhängigkeiten IMMER über den Workspace:
#   serde = { workspace = true }
#   jiff  = { workspace = true }
# Niemals eine Fassung hier hart schreiben.

[lints]
workspace = true
```

`#![forbid(unsafe_code)]` kommt aus `[workspace.lints]` und muss **nicht**
noch einmal in `lib.rs` stehen. Es ist `forbid`, nicht `deny`: `#[allow]`
überstimmt es nicht.

**Fehler.** Jede Crate hat genau einen Fehlertyp in `src/error.rs`, gebaut mit
`#[derive(harw_macros::HarwError)]`. Muster:

```rust
/// Fehler dieser Crate.
#[derive(Debug, harw_macros::HarwError)]
pub enum ObserveError {
    /// Der Feldname ist leer oder enthält Steuerzeichen.
    #[msg("field name '{name}' is empty or contains control characters")]
    InvalidFieldName { name: String },

    /// Ein Sink konnte nicht schreiben.
    ///
    /// Struct-Variante mit benannten Feldern: alle Felder werden im
    /// `#[msg]` über ihren Namen interpoliert.
    #[msg("telemetry sink '{sink}' failed to write: {source}")]
    SinkWrite {
        sink: &'static str,
        source: std::io::Error,
    },

    /// Durchgereichter I/O-Fehler.
    ///
    /// `#[from]` steht **auf der Variante**, nicht auf einem Feld, und die
    /// Variante ist ein **Ein-Feld-Tupel**. Nur so entsteht das `From`-Impl,
    /// das `?` braucht.
    #[msg("write failed: {0}")]
    #[from]
    Io(std::io::Error),
}
```

> **Zwei Fallen, die zwei Agenten unabhängig voneinander gefunden haben —
> hier korrigiert:**
>
> 1. **`#[from]` ist ein Attribut der Variante, nicht eines Feldes**, und die
>    Variante muss ein Ein-Feld-Tupel sein (`harw-macros/src/error.rs:47-52`
>    lehnt alles andere ab). Auf einem benannten Feld ist `#[from]`
>    wirkungslos — es ist als Helper-Attribut deklariert, also inert, und
>    erzeugt still **kein** `From`-Impl. `?` funktioniert dann nicht, und der
>    Compiler sagt nichts. Vorbild im Bestand: `harw-plan/src/error.rs:268`.
> 2. **`Debug` wird nicht erzeugt.** Die Ableitung liefert `Display`,
>    `std::error::Error` mit `source()` und die `From`-Impls — mehr nicht.
>    `Debug` steht überall im Bestand als eigenes Derive daneben:
>    `#[derive(Debug, HarwError)]`.
>
> **Korrektur einer früheren Fassung dieses Dokuments.** Hier stand: „jedes
> benannte Feld muss im `#[msg]` interpoliert werden, sonst
> `unused_variables` unter `-D warnings`". Das beschrieb eine Einschränkung
> des Makros und machte sie fälschlich zur Entwurfsregel — im direkten
> Widerspruch zur Sicherheitszusage mehrerer Teilbäume, dass
> **Fehlermeldungen keinen Inhalt tragen**. Unter dieser Zusage ist ein nicht
> interpoliertes Feld der Regelfall, nicht das Versehen.
>
> **Behoben an der Ursache:** die erzeugte `Display`-Implementierung trägt
> jetzt `#[allow(unused_variables)]`. Eine inhaltsfreie Meldung ist damit
> ausdrückbar, ohne unter `-D warnings` zu brechen.
>
> **Es gilt:** ein Feld gehört in die Variante, wenn `source()` oder ein
> bewusster Abholer es braucht — nicht, weil die Meldung es nennen muss. Und
> wo eine Crate Inhaltsfreiheit zusagt, **nennt die Meldung das Feld gerade
> nicht**. Der Detailwert bleibt über `std::error::Error::source()`
> erreichbar, für den, der ihn bewusst holt.

`anyhow` und `thiserror` sind verboten. `unwrap()` und `expect()` sind
außerhalb von Tests verboten.

**Zeit.** Ausnahmslos `jiff::Timestamp`. Kein `std::time::SystemTime` in einer
öffentlichen Signatur, kein `time`-Crate in neuen Crates. Reine Funktionen
bekommen `now: Timestamp` **injiziert** und lesen nie die Systemuhr.

---

## A · `harw-observe` (AW0-01)

Rund einundvierzig Konsumenten. Der Knoten baut **zwei** Crates:
`harw-observe` (Vokabular) und `harw-observe-file` (erster echter Sink).

### A.1 Feldnamen

```rust
/// Ein validierter Feldname für Metrik-Labels und Span-Felder.
///
/// Nur über [`field!`] konstruierbar: der Makro-Weg prüft zur Compile-Zeit,
/// was `try_new` erst zur Laufzeit prüfen könnte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldName(&'static str);

impl FieldName {
    /// Baut einen Feldnamen aus einem statischen String.
    ///
    /// Nicht öffentlich: der einzige vorgesehene Weg ist `field!`.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_static_unchecked(name: &'static str) -> Self {
        Self(name)
    }

    /// Der Name als Zeichenkette.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for FieldName { /* schreibt self.0 */ }

/// Ein Feldwert. Bewusst klein gehalten: Telemetrie trägt Zahlen und
/// geschlossene Aufzählungen, keine Inhalte.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Str(&'static str),
    Owned(String),
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
}
```

### A.2 Metrikschlüssel

```rust
/// Art einer Messgröße.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetricKind { Counter, Gauge, Histogram }

/// Basiseinheit einer Messgröße. Geschlossen — eine neue Einheit ist eine
/// Entscheidung, kein freier String.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit { Count, Bytes, Seconds, Ratio, Tokens, Celsius }

/// Obergrenze der Labelkombinationen. Verhindert Kardinalitätsexplosion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cardinality { Bounded(u32), Single }

/// Der vollständige Schlüssel einer Messgröße.
///
/// Nach AW3-04 (Prometheus-Sink mit Golden-Test) ist diese Form eingefroren:
/// jede Feldänderung wird danach zu einer Golden-Test-Migration über alle
/// bis dahin emittierten Metriken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MetricKey {
    pub name: &'static str,
    pub kind: MetricKind,
    pub unit: Unit,
    pub labels: &'static [FieldName],
    pub cardinality: Cardinality,
}

/// Der gemessene Wert.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetricValue { Count(u64), Gauge(f64), Observation(f64) }
```

### A.3 Der Sink-Trait — die drei Driftpunkte sind hiermit entschieden

```rust
/// Ein Ziel für Messwerte.
///
/// # Die drei Festlegungen
/// - **Receiver `&self`**: Sinks werden über `Arc<dyn TelemetrySink>` geteilt
///   und aus jedem Thread aufgerufen. `&mut self` verlangte einen Mutex auf
///   dem heißesten Pfad des Systems.
/// - **Rückgabe `()`**: Telemetrie darf die Operation, die sie beobachtet,
///   niemals scheitern lassen. Ein Sink behandelt seine Fehler selbst und
///   zählt sie über einen eigenen Zähler; ein `Result` hier bedeutete, dass
///   jeder Aufrufer eine Entscheidung treffen müsste, die er nicht treffen
///   kann.
/// - **Synchron**: der Kern hat Pfade ohne Runtime (`harw-sentinel` läuft
///   ohne `tokio`). Ein `async`-Trait schlösse sie aus.
pub trait TelemetrySink: Send + Sync + std::fmt::Debug {
    /// Nimmt einen Messwert entgegen.
    fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]);

    /// Erzwingt das Ausschreiben gepufferter Werte.
    fn flush(&self);

    /// Name des Sinks für Diagnosezwecke.
    fn name(&self) -> &'static str;
}

/// Ein Sink, der alles verwirft. Voreinstellung, damit Emission nie an einer
/// fehlenden Konfiguration scheitert.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl TelemetrySink for NullSink { /* leer; name() == "null" */ }
```

### A.4 Trace-Kontext — nach AW1-01 ein Wire-Typ

```rust
/// Verknüpft Arbeit über Prozess- und Sitzungsgrenzen hinweg.
///
/// # Warum die Serde-Form hier festgelegt wird
/// AW1-01 schreibt diesen Typ in `StoredJob` und `ChildLeaseRecord` — beides
/// bestehende Dateiformate mit der Anforderung „bestehende Dateien bleiben
/// lesbar". Nach AW1-01 bricht jede Formänderung Bestandsdateien. Der Typ
/// trägt daher dieselbe Härte wie ein Wire-Typ.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceContext {
    /// 32 Hexzeichen.
    pub trace_id: String,
    /// 16 Hexzeichen.
    pub span_id: String,
    /// Elternspanne, falls die Arbeit geerbt wurde.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
}
```

### A.5 Redaktion

```rust
/// Wie ein Wert in Diagnoseausgaben erscheint.
///
/// Die Voreinstellung des Ableitungsmakros ist [`Redacted::Omitted`]: ein
/// Feld, über das niemand nachgedacht hat, erscheint gar nicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redacted { Omitted, Shown(String), Hashed(String) }

/// Ein Typ, der weiß, wie er in Diagnoseausgaben erscheinen darf.
pub trait Redact {
    /// Die diagnosetaugliche Form dieses Wertes.
    fn redact(&self) -> Redacted;
}
```

### A.6 `harw-observe-file`

```rust
/// Ein anhängender JSONL-Sink unter dem harw-Home, mit Größenrotation.
#[derive(Debug)]
pub struct FileSink { /* privat */ }

impl FileSink {
    /// Öffnet oder legt den Sink unter `dir` an.
    ///
    /// # Errors
    /// [`ObserveFileError::Open`], wenn das Verzeichnis nicht beschreibbar ist.
    pub fn open(dir: &std::path::Path, max_bytes: u64) -> Result<Self, ObserveFileError>;
}

impl harw_observe::TelemetrySink for FileSink { /* name() == "file" */ }
```

Eine abgeschlossene (rotierte) Datei bekommt eine Prüfsumme als Beidatei.
Abhängigkeiten: nur `serde_json` über das hinaus, was `harw-observe` zieht.

---

## B · `harw-types` — Ergänzungen (AW0-03)

**Rein additiv.** Kein bestehendes Symbol wird geändert oder entfernt.

### B.1 Sechs neue Kennungen — GELANDET, Form nachgeführt

`FindingId`, `SensorId`, `ActionId`, `BaselineId`, `HostId`, `CgroupId`
liegen in `harw-types/src/ids.rs`.

> **Korrektur gegenüber dem ursprünglichen Entwurf dieses Dokuments.** Hier
> stand ein Beispiel mit `#[derive(harw_macros::HarwId)]`. **`harw-types`
> benutzt dieses Ableitungsmakro nicht.** Es hat ein eigenes lokales
> `macro_rules! newtype_id!` (`ids.rs` ab Zeile 25), und dessen Erzeugnis
> sieht anders aus als der Entwurf annahm. Verbindlich ist der Ist-Stand:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SensorId(pub String);
```

**Drei Dinge, die davon abweichen und Aufrufer betreffen:**

1. **Kein `PartialOrd`, kein `Ord`, kein `Copy`.** Ein `SensorId` gehört
   damit **nicht** in ein `BTreeSet` oder als `BTreeMap`-Schlüssel; nimm
   `HashSet`/`HashMap`. Und er wird bewegt oder geliehen, nie kopiert. Wer
   ein `#[derive(PartialOrd, Ord)]` auf einen Typ setzt, der eine dieser
   Kennungen enthält, bekommt einen Compile-Fehler.
2. **`SensorId::new()` nimmt kein Argument — es erzeugt eine zufällige
   UUID v4.** Der Konstruktor aus einer bekannten Zeichenkette heißt
   `SensorId::from_str("thermal")` (unfehlbar, für Bestandsaufrufer) oder
   `SensorId::try_from_str("thermal")` / `::parse(...)` (validierend, lehnt
   leere und nur aus Leerraum bestehende Werte ab). **Für alles, was aus
   Konfiguration, Wire-Daten oder einer Datei kommt, gilt der validierende
   Weg.**
3. **Das Feld ist öffentlich** (`pub String`), und die Serde-Form ist
   `transparent` — eine Kennung serialisiert als nackte Zeichenkette, nicht
   als Objekt.

`CgroupId` trägt in seiner Doku den Hinweis, dass **der Kernel cgroup-IDs
wiederverwendet**. Wer sie für dauerhaft eindeutig hält, verwechselt einen
alten Datensatz mit einem neuen — der Grund, warum `Freeze` über
`(CgroupId, FindingId, frozen_at)` geschlüsselt wird und nicht über
`CgroupId` allein.

### B.2 Inhaltsadressierung

```rust
/// Ein blake3-Digest über einen Inhalt.
///
/// # Warum ein eigener Typ und nicht `[u8; 32]`
/// Ein Digest wandert durch Nachweise, Chunks und Manifeste. Als nacktes
/// Array wäre er von jedem anderen 32-Byte-Wert nicht unterscheidbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    /// Berechnet den Digest über `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self;

    /// Die rohen 32 Bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32];
}

/// Als 64 Hexzeichen in Kleinschreibung.
impl std::fmt::Display for ContentDigest;

/// Aus 64 Hexzeichen; alles andere ist ein Fehler.
impl std::str::FromStr for ContentDigest { type Err = InvalidDigest; }

/// Serde als Hex-Zeichenkette, damit Bestandsdateien lesbar bleiben.
impl serde::Serialize for ContentDigest;
impl<'de> serde::Deserialize<'de> for ContentDigest;
```

`blake3` kommt über `{ workspace = true }`.

---

## C · `harw-lens-types` (AW0-08)

Trägt **auch** die Rangtypen. Sie liegen hier und nicht in `harw-context`,
weil `pack` aus `harw-lens-rank` sie nennt und `harw-context` `pack` aufruft —
lägen sie in `harw-context`, definierten Kontext und Lens einander gegenseitig.

```rust
/// Woher ein Chunk stammt. Geschlossen und ohne Interpretation: dieser Typ
/// sagt, wo etwas herkommt, nie was es bedeutet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum SourceRef {
    File { path: String },
    Artifact { id: String },
    PlanNode { plan: String, node: String },
    Diary { entry: String },
}

/// Ein Byte-Bereich in der Quelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteSpan { pub start: usize, pub end: usize }

/// Der Digest eines Chunks.
///
/// Ein Newtype über [`ContentDigest`] statt eines Alias: ein Chunk-Digest und
/// ein Nachweis-Digest sind beide 32 Bytes und dürfen trotzdem nie verwechselt
/// werden. Das Hashen selbst lebt an genau einer Stelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
pub struct ChunkDigest(pub harw_types::ContentDigest);

/// Ein Stück Text mit Herkunft.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub digest: ChunkDigest,
    pub source: SourceRef,
    pub span: ByteSpan,
    pub text: String,
}

/// Wo ein Embedding berechnet werden darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Locality { Local, Remote }

/// Abstandsmaß eines Index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Metric { Cosine, DotProduct, Euclidean }

/// Was ein Index über sich selbst behauptet.
///
/// Eine Abfrage gegen ein abweichendes `model` oder eine abweichende
/// `chunker_version` wird abgelehnt, nie stillschweigend beantwortet.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexManifest {
    pub model: String,
    pub locality: Locality,
    pub chunker_version: u32,
    pub visibility: String,
    pub metric: Metric,
    pub source_set_digest: harw_types::ContentDigest,
}

// ---- Rangtypen ----

/// Ein Kandidat mit Rangwert.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked { pub chunk: Chunk, pub score: f32 }

/// Was ein Kandidat kostet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CostEstimate(pub u32);

/// Schätzt die Kosten eines Textes.
pub trait CostEstimator: Send + Sync {
    /// Die geschätzten Kosten von `text`.
    fn estimate(&self, text: &str) -> CostEstimate;
}

/// Vier Bytes je Einheit. Die billige, deterministische Voreinstellung.
#[derive(Debug, Clone, Copy, Default)]
pub struct BytesOverFour;

impl CostEstimator for BytesOverFour { /* text.len().div_ceil(4) */ }

/// Ein Kostenbudget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSpec { pub total: u32 }

impl BudgetSpec {
    /// Punktweises Minimum. Es gibt bewusst kein `widen`.
    #[must_use]
    pub fn tighten(self, other: Self) -> Self;
}

/// Wie Duplikate zusammenfallen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapsePolicy { ByDigest, BySourceAndSpan }

/// Kanten zwischen Chunks, für die Relationsexpansion.
#[derive(Debug, Clone, Default)]
pub struct EdgeIndex { /* privat */ }

/// Das Ergebnis von `pack`.
#[derive(Debug, Clone, PartialEq)]
pub struct Packed {
    pub selected: Vec<Ranked>,
    pub spent: CostEstimate,
    pub dropped: usize,
}
```

---

## D · `harw-lens-rank` (AW0-09)

Vier reine Funktionen. **Kein I/O, keine Systemzeit, keine Zufallsquelle.**
Das ist ein CI-Gate, kein Vorsatz.

```rust
/// Verschmilzt mehrere Ranglisten über Reciprocal Rank Fusion.
///
/// Reihenfolgeunabhängig: die Permutation der Eingabelisten ändert das
/// Ergebnis nicht (Property-Test).
pub fn rrf_fuse(lists: &[Vec<Ranked>], k: f32) -> Vec<Ranked>;

/// Maximal Marginal Relevance: Relevanz gegen Vielfalt.
///
/// Deterministisch bei gleicher Eingabe.
pub fn mmr(candidates: &[Ranked], lambda: f32, limit: usize) -> Vec<Ranked>;

/// Lässt Duplikate zusammenfallen. Idempotent.
pub fn collapse(candidates: &[Ranked], policy: CollapsePolicy, edges: &EdgeIndex) -> Vec<Ranked>;

/// Füllt ein Budget mit den besten Kandidaten.
///
/// **Der einzige echte Vertrag zwischen Lens und Kontextmontage.** AW6-07
/// prüft ausdrücklich, dass die Montage dieselbe Funktion aufruft wie Lens.
pub fn pack(candidates: &[Ranked], cost: &dyn CostEstimator, budget: &BudgetSpec) -> Packed;
```

---

## E · `harw-context` (AW0-04)

```rust
/// Vertrauensklasse eines Fragments. **Drei Werte, geschlossen.**
///
/// Die Klasse entscheidet, in welchen Block ein Fragment gerendert wird.
/// `Instruction` ist die einzige, die im Instruktionsblock erscheinen darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustClass { Instruction, Evidence, Data }

/// Wie beständig ein Fragment über Turns hinweg ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stability { Pinned, Stable, Fresh, Volatile }

/// Wer ein Fragment wann geliefert hat.
///
/// **Heißt bewusst nicht `Provenance`:** diesen Namen besitzt bereits
/// `harw_memory::epistemic::Provenance` („woher weiß ich das"). Dieser Typ
/// sagt etwas anderes („welcher Provider hat das wann geliefert").
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentOrigin {
    pub provider: String,
    pub namespace: String,
    pub produced_at: jiff::Timestamp,
}

/// Ein Stück Kontext mit allem, was die Montage über es wissen muss.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    pub label: FragmentLabel,
    pub section: SectionName,
    pub trust: TrustClass,
    pub stability: Stability,
    pub origin: FragmentOrigin,
    pub cost: harw_lens_types::CostEstimate,
    pub digest: harw_types::ContentDigest,
    pub body: String,
}

/// Name einer Kontextsektion, etwa `history.tail` oder `plan.current`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
pub struct SectionName(String);

/// Kennung eines einzelnen Fragments innerhalb seiner Sektion.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FragmentLabel(String);

/// Wie ausführlich eine Sektion gerendert wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DetailMode { Full, Summary, References }

/// Warum ein Fragment nicht im Ergebnis steht. Eine Auslassung ohne Grund
/// gibt es nicht.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OmissionReason {
    OverBudget,
    BelowCeiling,
    ExcludedByProgram,
    Superseded,
}

/// Ein Glob über Sektions- und Fragmentnamen.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Selector(String);

impl Selector {
    /// Trifft dieser Selektor den genannten Namen?
    #[must_use]
    pub fn matches(&self, name: &str) -> bool;
}

/// Ein Budget je Sektion plus Gesamtbudget.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBudgetSpec {
    pub total: harw_lens_types::BudgetSpec,
    pub per_section: std::collections::BTreeMap<SectionName, u32>,
}

impl ContextBudgetSpec {
    /// Punktweises Minimum über Gesamt- und Sektionsbudgets.
    ///
    /// Fehlt eine Sektion in einem der beiden Budgets, gilt der vorhandene
    /// Wert. **Es gibt kein `widen`:** ein Budget kann auf keinem Weg wachsen,
    /// und das ist eine Typ-Eigenschaft, keine Konvention.
    #[must_use]
    pub fn tighten(&self, other: &Self) -> Self;
}

/// Die Obergrenze dessen, was ein Agent überhaupt sehen darf.
///
/// Wird beim Handoff an ein Kind **im selben Schritt geschnitten** wie die
/// Berechtigungen — ein Kind kann seine Decke nie anheben.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCeiling {
    pub sections: std::collections::BTreeSet<SectionName>,
    pub max_trust: TrustClass,
    pub budget: ContextBudgetSpec,
}

impl ContextCeiling {
    /// Schnitt zweier Decken. Es gibt bewusst keine Vereinigung.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self;

    /// Lässt diese Decke das Fragment zu?
    ///
    /// # Errors
    /// [`CeilingViolation`] mit dem verletzten Aspekt.
    pub fn admits(&self, fragment: &Fragment) -> Result<(), CeilingViolation>;
}
```

---

## F · `harw-dod-cap` (AW0-06) — höchste Konsequenz

Elf Crates implementieren gegen diese Typen **in einem parallelen Stapel**.

> **Abweichung vom Arbeitsplan, mit Begründung.** Der Plan legt `trait Sensor`
> hierher. Das geht nicht: der Trait müsste `HostSample` und `SecurityEvent`
> nennen, die in `harw-dod-signals` liegen — und `harw-dod-signals` hängt
> seinerseits an `harw-dod-cap`. Ein Zyklus. **`trait Sensor` liegt daher in
> `harw-dod-signals` (Abschnitt G).** `harw-dod-cap` besitzt das
> *Zugriffs*vokabular, `harw-dod-signals` das *Daten*vokabular und den Trait,
> der Daten erzeugt.

```rust
/// Was eine Crate lesen darf. Geschlossen: eine neue Fähigkeit ist eine
/// Entscheidung mit Eintrag in der Rechtematrix, kein freier String.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    ReadSysfsThermal,
    ReadProcStat,
    ReadProcMeminfo,
    ReadSysfsBlock,
    ReadProcNetDev,
    ReadSysfsDrm,
    ReadCgroupV2,
    ReadProcNet,
    ReadJournal,
    ReadAuditNetlink,
    ReadScanReports,
    ReadWorkspaceGraph,
    WatchFilesystem,
    LoadBpfProgram,
}

/// Die Berechtigungsklasse einer Fähigkeit. Bestimmt, in welches Binary eine
/// Crate gehören darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CapabilityClass { Unprivileged, Netlink, FileWatch, Bpf }

impl Capability {
    /// Die Klasse dieser Fähigkeit.
    #[must_use]
    pub const fn class(&self) -> CapabilityClass;

    /// Der Pfad oder die Ressource, die diese Fähigkeit erschließt.
    #[must_use]
    pub const fn probe(&self) -> &'static str;
}

/// Ein Lesebereich. Wie `NetworkScope` ein Halbverband: **kein `add`, keine
/// Vereinigung, nur Schnitt.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadScope { /* privat */ }

impl ReadScope {
    /// Baut einen Bereich aus Wurzelpfaden.
    #[must_use]
    pub fn from_roots(roots: impl IntoIterator<Item = std::path::PathBuf>) -> Self;

    /// Schnitt zweier Bereiche. Es gibt bewusst keine Vereinigung.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self;

    /// Liegt `path` im Bereich?
    #[must_use]
    pub fn allows(&self, path: &std::path::Path) -> bool;

    /// Öffnet `path` **nach** Auflösung aller Symlinks.
    ///
    /// # Description
    /// Die Reihenfolge ist der ganze Punkt: erst auflösen, dann prüfen. Wer
    /// zuerst prüft, prüft den Namen und öffnet das Ziel.
    ///
    /// # Errors
    /// [`SensorError::OutsideScope`], wenn das aufgelöste Ziel außerhalb
    /// liegt. Der Fehler nennt **nie** den Zielpfad.
    pub fn open(&self, path: &std::path::Path) -> Result<std::fs::File, SensorError>;

    /// Die Wurzeln dieses Bereichs.
    ///
    /// `allows` beantwortet „liegt dieser Pfad drin", nicht „welche Pfade
    /// gibt es". Wer das Dateisystem **durchlaufen** muss — etwa für einen
    /// Glob — braucht einen Startpunkt: ein Durchlauf ab `/`, der erst am
    /// Ende prüft, liest unterwegs Verzeichnisse, die ihn nichts angehen.
    pub fn roots(&self) -> impl Iterator<Item = &std::path::Path>;
}

/// Ob ein Fehler wiederholbar ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permanence { Transient, Permanent }

/// Fehler eines Sensors. **Inhaltsfrei:** kein Feldwert, kein Pfad, keine
/// gelesene Zeile erscheint hier. Ein Sensorfehler wird geloggt, und was
/// geloggt wird, verlässt den Host.
#[derive(Debug, harw_macros::HarwError)]
pub enum SensorError {
    #[msg("path resolves outside the sensor read scope")]
    OutsideScope,

    #[msg("sensor source is unavailable on this host")]
    SourceUnavailable,

    #[msg("sensor source has an unexpected shape")]
    MalformedSource,

    /// **Tupel-Variante, nicht Struct-Variante.** `#[from]` erzeugt das
    /// `From`-Impl nur auf einer Variante mit genau einem unbenannten Feld;
    /// auf einem benannten Feld ist es inert und `?` funktioniert nicht.
    #[msg("sensor read failed: {0}")]
    #[from]
    Io(std::io::Error),
}

impl SensorError {
    /// Ob ein erneuter Versuch sinnvoll ist. `Permanent` führt beim Sentinel
    /// zur Abmeldung des Sensors.
    #[must_use]
    pub const fn permanence(&self) -> Permanence;
}

/// Marker: der Sensor kennt seinen Bereich noch nicht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unbound;

/// Marker: der Sensor ist an einen Bereich gebunden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound;

/// Ein Sensor-Griff im Zustand `S`.
///
/// `poll` und `subscribe` gibt es **nur** auf `SensorHandle<Bound>`. Ein
/// ungebundener Griff kann nicht lesen, und das ist kein Laufzeitfehler,
/// sondern ein fehlender Methodenname.
#[derive(Debug)]
pub struct SensorHandle<S> { /* privat, enthält PhantomData<S> */ }

impl SensorHandle<Unbound> {
    /// Erzeugt einen ungebundenen Griff.
    #[must_use]
    pub fn new(id: harw_types::SensorId, capability: Capability) -> Self;

    /// Bindet den Griff an einen Lesebereich.
    #[must_use]
    pub fn bind(self, scope: ReadScope) -> SensorHandle<Bound>;
}

impl SensorHandle<Bound> {
    /// Der Lesebereich dieses Griffs.
    #[must_use]
    pub fn scope(&self) -> &ReadScope;

    /// Die Kennung dieses Sensors.
    #[must_use]
    pub fn id(&self) -> &harw_types::SensorId;

    /// Die Fähigkeit dieses Sensors.
    #[must_use]
    pub fn capability(&self) -> Capability;
}
```

---

## G · `harw-dod-signals` (AW0-07)

Hängt an `harw-dod-cap` und `harw-observe`.

```rust
/// Ein Messwert über den Hostzustand. Zahlen, keine Inhalte.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSample {
    pub sensor: harw_types::SensorId,
    pub observed_at: jiff::Timestamp,
    /// **`Cow`, nicht `&'static str`** — siehe Kasten unten.
    pub metric: std::borrow::Cow<'static, str>,
    pub value: f64,
}

/// Wer etwas ausgelöst hat.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub uid: u32,
    /// Die Anmelde-UID. Der Wert, den ein `sudo` **nicht** verändert — und
    /// damit der einzige, der sagt, wer wirklich angefangen hat.
    pub auid: Option<u32>,
    pub cgroup: Option<harw_types::CgroupId>,
}

/// Art eines Sicherheitsereignisses. Geschlossen.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
pub enum EventKind {
    ProcessExec { path: String, argv_digest: harw_types::ContentDigest },
    FileWrite { path: String },
    EgressFlow { destination: String, port: u16 },
    ListenerOpened { port: u16 },
    AuthEvent { outcome: AuthOutcome },
    StructureDrift { detail: String },
    SensorDegraded { sensor: harw_types::SensorId },
}

/// Ausgang eines Anmeldeversuchs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthOutcome { Success, Failure }

/// Ein sicherheitsrelevantes Ereignis. Getrennter Strom von [`HostSample`]:
/// Messwerte sind langweilig und häufig, Ereignisse sind selten und wichtig.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityEvent {
    pub sensor: harw_types::SensorId,
    pub observed_at: jiff::Timestamp,
    pub actor: Option<Actor>,
    pub kind: EventKind,
}

/// Wie hart ein Nachweis ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Hardness { Observed, Correlated, Inferred }

/// Wie schwer ein Befund wiegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity { Info, Low, Medium, High, Critical }

/// Eingefrorene Beobachtungen als Beleg.
///
/// Der Digest macht den Beleg zitierfähig: ohne ihn kann ein Befund auf
/// nichts zeigen, und `TrustClass::Evidence` bliebe leer.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityEvidence {
    pub digest: harw_types::ContentDigest,
    pub captured_at: jiff::Timestamp,
    pub samples: Vec<HostSample>,
    pub events: Vec<SecurityEvent>,
}

/// Was ein Sensor bei einem Abruf liefert.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SensorReading {
    pub samples: Vec<HostSample>,
    pub events: Vec<SecurityEvent>,
}

> **Korrektur, beim Bau gefunden.** Hier stand `metric: &'static str`. Ein
> `&'static str` in einem serde-Typ erzeugt `impl Deserialize<'static>` statt
> `impl<'de> Deserialize<'de>` — der Typ wäre dann nur aus einer wirklich
> statischen Quelle lesbar, nie aus einem zur Laufzeit gelesenen Puffer. Da
> `SecurityEvidence` `HostSample` enthält und auf Platte geschrieben **und
> zurückgelesen** wird, wäre das ein Typ gewesen, den man schreiben, aber
> nicht öffnen kann.
>
> `Cow<'static, str>` löst beides: der Erzeuger schreibt
> `Cow::Borrowed(KONSTANTE)` und alloziert nicht, das Zurücklesen ergibt
> `Cow::Owned`. **Sensoren konstruieren also `metric: Cow::Borrowed("…")`.**

/// Eine Quelle für Hostbeobachtungen.
///
/// # Der eine Trait, an dem elf Crates gleichzeitig hängen
/// Jede Sensor-Crate implementiert ihn genau einmal, für genau eine Quelle
/// und genau eine Fähigkeit. Sensoren erzeugen **niemals** einen Befund —
/// `Finding` entsteht ausschließlich in `harw-dod-rules`.
///
/// # Concurrency
/// `Send + Sync`: der Sentinel hält Sensoren hinter `Arc` und ruft sie aus
/// einem Sammelthread. `poll` nimmt `&self`, weil ein Sensor keinen
/// veränderlichen Zustand über den Abruf hinaus führen darf.
pub trait Sensor: Send + Sync + std::fmt::Debug {
    /// Der gebundene Griff dieses Sensors.
    fn handle(&self) -> &harw_dod_cap::SensorHandle<harw_dod_cap::Bound>;

    /// Liest die Quelle einmal aus.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit. Ein Sensor liest nie die
    ///   Systemuhr — sonst ist er nicht gegen ein Fixture prüfbar.
    ///
    /// # Errors
    /// [`harw_dod_cap::SensorError`], inhaltsfrei.
    fn poll(&self, now: jiff::Timestamp)
        -> Result<SensorReading, harw_dod_cap::SensorError>;
}
```

### G.1 Wo `Finding<S>` lebt — Korrektur gegenüber dem Arbeitsplan

Der Plan legt `Finding<S>` nach `harw-dod-signals` mit `pub(crate)`-
Konstruktoren. Von dort aus wären sie für die elf Sensor-Crates **nicht
erreichbar** — das Typestate-Muster funktioniert nur, wenn Typbesitzer und
Übergangsbesitzer dieselbe Crate sind.

**Verbindlich: `Finding<S>` und alle drei Übergänge leben in
`harw-dod-rules` (AW4-03).** Damit sind die Konstruktoren dort natürlich
`pub(crate)`, und `harw-dod-escalate` kommt an `Finding<Triaged>` nur, indem
es ein `Finding<RuleChecked>` **besitzt** — herstellbar allein durch
`harw-dod-rules`. Der Typ trägt die Regel; kein Reviewer muss sie tragen.

`harw-dod-signals` enthält **kein** `Finding`.

---

## H · Was jeder Knoten außerdem einhält

1. **Ein Fehlertyp je Crate**, `src/error.rs`, `#[derive(HarwError)]`.
2. **`deny_unknown_fields`** auf jedem Typ, der von außen kommendes JSON oder
   TOML entgegennimmt: Wire-Envelopes, Konfiguration, Definitionen, Fixtures.
   Nicht auf internen Rechenstrukturen — dort wäre es Lärm.
3. **Keine `unwrap()`/`expect()`** außerhalb von `#[cfg(test)]`.
4. **`&str`, `&Path`, `&[T]`** in Parametern; Eigentum nur, wo der Aufrufer
   es abgeben muss.
5. **`Arc::clone(&x)`**, nie `x.clone()` auf einem `Arc`.
6. **Modul- und Elementdokumentation** nach den Projektvorgaben: `//!` je
   Datei mit Zweck, Verantwortungsbereich, Nebenläufigkeit und Fehlern; `///`
   je öffentlichem Element mit `# Errors`, `# Panics` (falls zutreffend) und
   `# Examples`.
7. **Kein `cargo`-Aufruf** aus einem Arbeitsagenten heraus. Die Verifikation
   läuft sequenziell und an einer Stelle.
