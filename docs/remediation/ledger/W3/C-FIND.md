# W3 — Agent C-FIND: Finding-Typestate, `FindingRecord`, `triage_record`, `FindingSpool`

Befunde: F-023, F-074 (Vorbedingung), G-037. BUILD-POLICY eingehalten: nichts gebaut, geprüft oder getestet,
keine git-Schreibbefehle. Nur `cargo metadata --offline --no-deps` für die Zyklusprüfung. Verifikation erfolgte
durch Lesen.

Owned und geändert:
- `harw-dod-rules/src/finding.rs` (neu geschrieben)
- `harw-dod-rules/src/engine.rs` (nur Moduldoku)
- `harw-dod-rules/src/rules/baseline_deviation.rs` (Doctest Z. 75: `.kind` → `.kind()`)
- `harw-dod-sentinel/src/spool.rs` (neu)
- `harw-dod-sentinel/src/lib.rs` (`pub mod spool`, Re-Exporte, Doku)
- dieses Ledger

## 1. Signaturen (wie geschrieben)

### `harw_dod_rules::finding`
```rust
pub struct Finding<S: FindingState> { pub(crate) rule_id, kind, severity, hardness, summary, observed_at; identity; outcome }
impl<S: FindingState> Finding<S> {
    pub fn rule_id(&self) -> &'static str;  pub fn kind(&self) -> FindingKind;
    pub fn severity(&self) -> Severity;     pub fn hardness(&self) -> Hardness;
    pub fn summary(&self) -> &str;          pub fn observed_at(&self) -> Timestamp;
}
impl Finding<Raw> { pub(crate) fn raw(..) -> Self; pub(crate) fn check(self, FindingId) -> Finding<RuleChecked>; } // unverändert
impl Finding<RuleChecked> {
    pub fn id(&self) -> &FindingId;
    pub fn record(&self, evidence: SecurityEvidence) -> FindingRecord;   // NEU, total
}
impl Finding<Triaged> {
    pub fn id(&self) -> &FindingId;  pub fn verdict(&self) -> &Verdict;
    pub fn record_digest(&self) -> Option<ContentDigest>;               // NEU: Some nur über triage_record
}
impl FindingState for Triaged { type Outcome = (Verdict, Option<ContentDigest>); }  // war: Verdict

#[derive(Serialize, Deserialize)] #[serde(rename_all = "kebab-case")] pub enum FindingKind { .. }  // serde NEU
impl Verdict { pub fn from_classification(VerdictClassification) -> Self; }  // Confirmed→Confirmed, Benign→FalsePositive, Suspicious→NeedsReview

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(try_from = "FindingRecordWire")]
pub struct FindingRecord { version, finding_id, rule_id: String, kind, severity, hardness, summary, observed_at,
                           evidence: SecurityEvidence, digest: ContentDigest }   // alle Felder privat
impl FindingRecord {
    pub const VERSION: u32 = 1;
    pub fn version/finding_id/rule_id/kind/severity/hardness/summary/observed_at/evidence(&self);
    pub fn digest(&self) -> ContentDigest;
}
pub enum RecordError { UnsupportedVersion, UnknownRule, EvidenceEncoding(SignalsError), EvidenceDigestMismatch, RecordDigestMismatch }
pub enum TriageError { Record(RecordError), VerdictContract, VerdictInvalid(SignalsError), VerdictUnbound(SignalsError) }
impl From<RecordError> for TriageError;
pub fn triage_record(record: &FindingRecord, verdict: &SecurityVerdict) -> Result<Finding<Triaged>, TriageError>;
pub fn triage(finding: Finding<RuleChecked>, verdict: Verdict) -> Finding<Triaged>;   // unverändert pub, siehe §4 O-1
```
Fehlertypen sind handgeschrieben (`Display`, `std::error::Error::source`, `derive(Debug)`). `harw-dod-rules` hat
kein `harw-macros`. Die Meldungen enthalten keine Werte aus Record oder Verdikt.

### `harw_dod_sentinel::spool` (Re-Export an der Crate-Wurzel)
```rust
pub const RECORD_FILE_MODE: u32 = 0o640;   pub const SPOOL_DIR_MODE: u32 = 0o750;
pub const DEFAULT_MAX_RECORD_BYTES: u64 = 1 MiB;  pub const DEFAULT_MAX_ENTRIES: usize = 65_536;  pub const MAX_PAGE_LIMIT: usize = 1_000;
pub struct SpoolLimits { pub max_record_bytes: u64, pub max_entries: usize }  // Default
pub struct SpoolId { seq: u64, digest: ContentDigest }  // Ord, Display "<seq:020>-<hex>", parse(&str)->Option, seq(), digest()
pub struct SpoolCursor { after: SpoolId }               // Display, parse(&str)->Option, after()
pub struct SpoolEntry { pub id: SpoolId, pub record: Result<FindingRecord, SpoolError> }
pub enum SpoolError { Io{op,path,source}, SymlinkRejected{path}, NotADirectory{path}, InsecurePermissions{path,mode},
    DirectoryReplaced{path}, NotARegularFile{id}, RecordTooLarge{len,limit}, SpoolFull{limit}, TooManyEntries{limit},
    IdCollision{id}, SequenceExhausted, Encode(serde_json::Error), InvalidRecord(serde_json::Error),
    Decode{id,source}, DigestMismatch{id} }
pub type SpoolResult<T> = Result<T, SpoolError>;
impl FindingSpool {
    pub fn open(dir: &Path) -> SpoolResult<Self>;
    pub fn open_with_limits(dir: &Path, limits: SpoolLimits) -> SpoolResult<Self>;
    pub fn dir(&self) -> &Path;  pub fn limits(&self) -> SpoolLimits;
    pub fn put(&self, record: &FindingRecord) -> SpoolResult<SpoolId>;
    pub fn get(&self, id: &SpoolId) -> SpoolResult<Option<FindingRecord>>;
    pub fn page(&self, cursor: Option<&SpoolCursor>, limit: usize) -> SpoolResult<(Vec<SpoolEntry>, Option<SpoolCursor>)>;
}
```
`SpoolError` ist handgeschrieben, weil `HarwError` `source()` nur für `#[from]`-Tupelvarianten erzeugt und damit
keine Varianten mit Kontext und Ursache abbilden kann. `error.rs` gehört nicht zu C-FIND.

## 2. Design-Entscheidungen

1. **Private Felder als `pub(crate)`.** Ganz private Felder würden `advisory.rs:297-300` (Tests, nicht owned) und
   die Regeltests brechen. Nach außen sind die Felder damit so privat wie verlangt, das belegt ein
   `compile_fail`-Doctest am Typ.
2. **Digest = BLAKE3 über kanonische Bytes, kein serde.** Die Bytes sind: Domain `harw:finding-record:v1\0` ‖
   version u32 LE ‖ längenpräfixierte Felder (u64 LE) `finding_id, rule_id, kind, severity, hardness, summary` ‖
   `observed_at` in ns (i128 LE) ‖ `evidence.digest` (32 B) ‖ `evidence.captured_at` in ns (i128 LE). Die
   Enum-Kennungen sind feste Strings aus erschöpfenden `match`es und nicht `Debug`. Den Inhalt von
   `samples`/`events` deckt `evidence.digest` ab.
3. **Prüfung bei jedem Eintritt.** `Deserialize` läuft über `try_from` und prüft Version, bekannte Regel,
   Beleg-Digest und Record-Digest neu. Den Beleg-Digest berechnet `SecurityEvidence::capture` neu, das ist die
   öffentliche API. `triage_record` prüft den Record **nochmals** vollständig, auch wenn er in-process über
   `record()` entstand (`SecurityEvidence` hat öffentliche Felder). `binds` erhält den selbst berechneten Digest
   (G-037/K43). Außerdem prüft `triage_record` die Vertragsfassung und `validate_verdict`.
4. **Schwere und Härte kommen aus dem Record, nicht aus dem Verdikt.** Ein Agent kann einen Befund also nicht
   hochstufen.
5. **`rule_id: &'static str`** wird nicht geleakt, sondern gegen die bekannten Regeln aufgelöst
   (`EgressFlowRule`, `StructureDriftRule`, `BaselineDeviationRule`, `advisory::RULE_ID`). Unbekannte Regeln
   werden abgelehnt. **Jede neue Regel muss in `finding.rs::known_rule_id` eingetragen werden**, der Test
   `test_known_rule_id_covers_every_rule` hält die Liste aktuell.
6. **Spool-Benennung `<seq:020>-<digest>.json`, keine reinen Digest-Namen.** Bei Namen nur aus dem Digest könnte
   ein neuer Record mit kleinerem Digest hinter einem bereits gelesenen Cursor landen und ginge der Triage
   verloren. `seq` wird beim `open` aus dem Verzeichnis bestimmt (Maximum + 1) und danach atomar hochgezählt.
7. **Symlink- und Ersetzungsschutz:**
   - `symlink_metadata`, dann `open_dir_nofollow`, dann `(dev, ino)` merken und vor jeder Operation und nach
     jedem Scan neu vergleichen.
   - Records werden über `open_beneath` gelesen, Symlink-Einträge ausdrücklich abgelehnt.
   - `o+w`-Verzeichnisse werden abgelehnt.
   - Geschrieben wird über `write_atomic(.., with_mode(0o640))`.
8. **Größenlimit** gilt bei `put` (serialisierte Länge) und bei `get` (`fstat` plus `take(limit+1)`). `put` prüft
   den Record per Rundreise vorab: Ein inkonsistenter oder nicht verlustfrei kodierbarer Record landet nicht als
   Giftpille im Spool.
9. **`page`:** Defekte Einträge erscheinen als `record: Err(..)`, sie blockieren die Seite nicht und werden
   auch nicht still übersprungen. `next` ist `Some`, sobald mindestens ein Eintrag betrachtet wurde. Bei leerer
   Seite ist `next` `None`, der Konsument behält dann seinen Cursor.
10. **Scan** über `walk_beneath` (sortiert, symlinkfest) mit Budget `2·max_entries+64`. Wird es überschritten,
    kommt `TooManyEntries`. Erreicht `put` die Grenze `SpoolFull`, zählt es vorher neu, falls extern gelöscht
    wurde.

## 3. dep-requests (Manifeste nicht owned, **ohne sie kompiliert nichts davon**)

```
# harw-dod-rules/Cargo.toml [dependencies]
serde = { workspace = true }                       # derive für FindingRecord/FindingKind
# harw-dod-sentinel/Cargo.toml [dependencies]
harw-dod-rules = { path = "../harw-dod-rules" }    # FindingRecord
harw-fsutil = { path = "../harw-fsutil" }          # open_nofollow-Familie, write_atomic, walk_beneath
serde_json = { workspace = true }                  # Spool-Format (bisher nur dev-dep → in [dependencies] verschieben)
# harw-dod-sentinel/Cargo.toml [dev-dependencies]
harw-sandbox = { path = "../harw-sandbox" }        # NetworkScope für RuleContext in spool-Tests
```
Zyklusprüfung per `cargo metadata`: `harw-dod-rules` und `harw-fsutil` erreichen `harw-dod-sentinel` nicht. Keine
neue externe Crate. `serde`, `blake3` (über `harw-types`) und `jiff` mit dem Feature `serde` sind bereits im
Workspace.

Empfehlung, nicht zwingend: `serde_json` im Workspace mit `features = ["float_roundtrip"]`. Ohne das Feature
können einzelne `HostSample.value` (f64) die JSON-Rundreise nicht bitgenau überstehen. Dann scheitert die
Beleg-Digest-Prüfung (fail-closed), und `put` meldet `InvalidRecord`. NaN/±∞ sind in JSON grundsätzlich nicht
darstellbar.

## 4. Offene Punkte / Folgearbeit

- **O-1 (F-023, Rest).** `pub fn triage` (freies Verdict) bleibt `pub`, weil `harw-dod-rules/src/lib.rs:120`
  sie re-exportiert und `lib.rs` nicht owned ist. Zum Schließen braucht es:
  - In `lib.rs` `triage` aus dem `pub use` streichen und `FindingRecord, RecordError, TriageError,
    triage_record` aufnehmen.
  - Den lib.rs-Doctest (Z. 68-100) auf `record`/`triage_record` umstellen und die Fehler-Moduldoku („definiert
    keinen eigenen Fehlertyp“) aktualisieren.
  - Danach `triage` in `finding.rs` auf `pub(crate)` setzen oder löschen.
  Voraussetzung ist die Migration aller Aufrufer (siehe unten).
- **O-2.** `advisory::correlate_advisories` bleibt eine zweite Prägestelle innerhalb der Crate (F-023). Sie läuft
  über `Finding::check` und ist nicht von außen fälschbar. `advisory.rs` ist nicht owned.
- **O-3 (xtask, X-PI).** `harw-fsutil` fehlt in `xtask/src/gate_privileges.rs::CRATE_PRIVILEGE`. Über
  `harw-sentinel → harw-dod-sentinel → harw-fsutil` meldet das Gate sonst einen Verstoß. Eintrag:
  `("harw-fsutil", RequiredPrivilege::Unprivileged)` (nur `rustix` fs/process, keine Capability).
- **O-4.** Die Doku in `harw-dod-sentinel/src/lib.rs` erklärt, warum der Spool keine „Parselogik“ ist. Ob
  `gate_edges` die neue Kante `harw-dod-sentinel → harw-dod-rules` akzeptiert, wurde gelesen: Verboten sind nur
  `harw-dod-warden*`, also ok.

### Bisherige Aufrufer geänderter APIs (brechen ohne Migration, W5 D-SENTLIB / D-SENTBIN / D-ESC / D-TRIAGE / A-PLANB)

Feldzugriffe auf `Finding` sind jetzt crate-privat. Ersatz: Lesemethoden. Schreibzugriffe sind entfallen; Tests
müssen Befunde mit der gewünschten Schwere über eine Regel erzeugen:

| Datei:Zeile | Zugriff | Art | Ersatz |
|---|---|---|---|
| `harw-sentinel/src/findings.rs:213-218,225-226` | `finding.rule_id/.kind/.severity/.hardness/.summary` | prod | `finding.rule_id()` usw. |
| `harw-dod-escalate/src/ladder.rs:147,153` | `finding.kind/.severity/.hardness` | prod | Lesemethoden |
| `harw-dod-escalate/src/ladder.rs:229-230` | `finding.severity = ..; finding.hardness = ..` | Test | Regel/Record mit passender Schwere, dann `triage_record` |
| `harw-dod-escalate/src/freeze_ops.rs:283-284` | Schreibzugriff wie oben | Test | wie oben |
| `harw-plan-bridge/src/security_bridge.rs:118,167` | `finding.rule_id/.observed_at` | prod | Lesemethoden |
| `harw-plan-bridge/src/security_bridge.rs:315-316,353,365,380` | Schreib-/Lesezugriffe | Test | wie oben; `observed_at` über `RuleContext.now` setzen |

Aufrufer von `triage` (weiter kompilierbar, nach O-1 auf `triage_record` umstellen):
`harw-dod-escalate/src/action.rs:334,375`, `ladder.rs:231`, `freeze_ops.rs:285`, `lib.rs:101` (Re-Export,
C-WPROTO/D-ESC), `harw-plan-bridge/src/security_bridge.rs:317`, `harw-dod/src/lib.rs:304` (Doctest), `:333`
(Re-Export), `harw-dod/tests/facade.rs:74`, `harw-dod-rules/src/lib.rs:98,120`.

`Triaged::Outcome` ist jetzt ein Tupel. Das ist nur über `verdict()`/`record_digest()` beobachtbar, kein externer
Aufrufer ist betroffen.

Neue Nutzer:
- `harw-sentinel` (D-SENTBIN) schreibt `finding.record(sentinel.freeze(now)?)` per `FindingSpool::put`.
- D-TRIAGE liest `page` und lässt ein Verdikt erzeugen.
- Der Escalator (D-ESC) liest `get(id)` selbst und ruft dann `triage_record(&record, &verdict)` auf. Den Wert
  `record_digest()` sollte er für Audit und Action-Bindung übernehmen und `None` ablehnen.

## 5. Tests (geschrieben, nicht ausgeführt)

`finding.rs`:
- Record: Felder übernommen, deterministisch, Digest je Identität verschieden.
- `triage_record` passend → `Triaged` (Schwere aus dem Record, `record_digest` gesetzt).
- Falscher Digest oder falsche Befund-ID → `VerdictUnbound`.
- Manipulierter Beleg → `Record(EvidenceDigestMismatch)`; leere Begründung → `VerdictInvalid`.
- JSON-Rundreise mit anschließender Triage.
- Deserialize lehnt manipulierte `summary`/`severity`, unbekannte Regel und unbekanntes Feld ab.
- Version ≠ 1 → `UnsupportedVersion`.
- Mapping `from_classification`, `known_rule_id`-Abdeckung, inhaltsfreie Fehlermeldungen.
- Bestandstests auf Lesemethoden umgestellt.

`spool.rs`:
- `put`/`get`-Rundreise; Dateimodus 0640; fehlende ID → `None`.
- `page` in Schreibreihenfolge mit Cursor über 3 Seiten; Cursor sieht später geschriebene Records; `limit` 0.
- Reopen setzt die Folgenummer fort.
- **Symlink-Spool-Verzeichnis → `SymlinkRejected`**, `o+w` → `InsecurePermissions`.
- **Symlink statt Record-Datei → `get`/`page` `SymlinkRejected`**.
- **Record zu groß bei `put` und bei `get` → `RecordTooLarge`**.
- Manipulierter Inhalt → `Decode`; fremder Record unter falschem Namen → `DigestMismatch`; volles Spool →
  `SpoolFull`.
- `SpoolId`/`SpoolCursor`-Parser, Fehlermeldung.

## 6. Prüfung durch Lesen

- `SecurityVerdict::binds(&FindingId, ContentDigest) -> Result<(), SignalsError>`, `contract()`,
  `classification()` und `CONTRACT_ID` gibt es (`harw-dod-signals/src/verdict.rs:201-393`). `validate_verdict`
  wird re-exportiert (`lib.rs:99-102`).
- `SecurityEvidence::capture(Vec, Vec, Timestamp) -> Result<Self, SignalsError>`; die Felder sind `pub`, auch
  `digest`/`captured_at` (`evidence.rs:163-225`).
- `ContentDigest::{of, as_bytes}`, `FromStr`, `Display` (Hex klein), serde (`harw-types/src/digest.rs`).
  `FindingId::{as_str, try_from_str}`; das validierende `Deserialize` lehnt leere Werte ab.
- `jiff::Timestamp::as_nanosecond(self) -> i128` (jiff 0.2.32, `timestamp.rs:1069`).
- `harw_fsutil::{open_dir_nofollow -> io::Result<OwnedFd>, open_beneath(BorrowedFd, &Path, OpenMode) -> io::Result<File>,
  write_atomic(&Path, &[u8], AtomicWriteOptions), AtomicWriteOptions::with_mode, OpenMode::read_only, walk_beneath,
  WalkLimits{max_depth,max_entries,deadline}, WalkEntry{rel_path,entry_type,len}, EntryType, WalkStop}` sind
  alle an der Crate-Wurzel exportiert.
- `DriftSeverity`, `EventKind::StructureDrift{severity, detail}` und `StructureDriftRule` (erzeugt je Event
  einen Befund) gibt es. Die Tests nutzen `NetworkScope::empty()`.
- `Option::is_none_or` (1.82) liegt unter MSRV 1.85. Kein `unwrap`/`expect` außerhalb von Tests, kein `unsafe`,
  keine neuen `#[allow]`.
