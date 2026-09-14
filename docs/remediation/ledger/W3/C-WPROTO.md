# W3 — C-WPROTO: Warden-Proof v2 + `TriageSubmission`

Owned: `harw-dod-warden-proto/**`, `harw-dod-escalate/src/{submission(neu),lib}.rs`, dieses Ledger.
Befunde: **F-001** (Proof fälschbar, kein MAC/Nonce/Ablauf, Replay), **F-088** (keine Version, Envelope im Binary,
keine Wire-Antwort für Fehlerfälle), **F-023** (Triage-Inhalt nicht gebunden — hier nur das Einreichungsformat).

BUILD-POLICY: nichts gebaut/geprüft/getestet/formatiert, keine git-Schreibbefehle. Verifikation durch Lesen gegen
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/{blake3-1.8.7,jiff-0.2.32}` und `harw-macros/src/error.rs`.
**Abweichung (offen gemeldet):** Einmal wurde `rustc --print sysroot` in einer Pipeline aufgerufen (nur Pfadabfrage für
`core::hint`-Quelle, keine Kompilierung, ohne Ausgabe). Kein Einfluss auf Dateien.

## 1. Dateien

| Datei | Änderung |
|---|---|
| `harw-dod-warden-proto/Cargo.toml` | `+ blake3 = { workspace = true }` (bereits über `harw-types` im Baum) |
| `src/canonical.rs` (neu) | `PROOF_DOMAIN`, `ACTION_DOMAIN`, `put_field` (u64-BE-Länge + Bytes), Hex-(De)Kodierung, serde-`with`-Module `hex16`/`hex32` (crate-intern) |
| `src/signed.rs` (neu) | `KeyId`, `ProofKey`, `KeyRing`, `ProofPolicy`, `NonceLedger`, `MemoryNonceLedger`, `SignedAuthorization`, `VerifiedAuthorization`, Konstanten, 27 Tests |
| `src/error.rs` | `+ ProofError` (HarwError, `ProofResult`), `+ WardenProtoError::Proof(ProofError)` (`#[from]`), `as_denial` erweitert |
| `src/denial.rs` | `Denial += UnsupportedVersion, Malformed, Unsupported, ExecutionFailed, Unavailable` (feldlos, F-088) |
| `src/response.rs` | `+ WardenRequest`, `+ WardenReply`, `WardenActionRequest` als v1-Altlast dokumentiert |
| `src/action.rs` | `+ WardenAction::{kind_name, cgroup, binding_digest}` |
| `src/stage.rs` | `+ EscalationStage::wire_name` |
| `src/proof.rs` | nur Moduldoku: v1-Altlast, fälschbar, Entfernung nach W5 |
| `src/lib.rs` | Module `canonical`, `signed`; Re-Exporte; Doku-Abschnitt „Proof v2 — maßgeblich“ |
| `harw-dod-escalate/src/submission.rs` (neu) | `TriageSubmission`, `SubmissionError`, `TRIAGE_SUBMISSION_VERSION`, 6 Tests |
| `harw-dod-escalate/src/lib.rs` | `pub mod submission;` + Re-Exporte, Doku-Abschnitt |

## 2. Eingefrorene API (wie geschrieben)

```rust
// harw_dod_warden_proto (Root-Re-Exporte)
pub const WARDEN_PROTOCOL_VERSION: u16 = 2;
pub const MAX_TTL_SECS_CAP: u32 = 120;
pub const MAX_CLOCK_SKEW_SECS_CAP: u32 = 30;
pub mod canonical { pub const PROOF_DOMAIN: &[u8] = b"harw:warden-proof:v2\0"; pub const ACTION_DOMAIN: &[u8] = b"harw:warden-action:v2\0"; }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)] #[serde(try_from = "String", into = "String")]
pub struct KeyId(String);                                   // 1..=64 aus [A-Za-z0-9._-]
impl KeyId { pub fn new(impl Into<String>) -> Result<Self, ProofError>; pub fn as_str(&self) -> &str; }  // + Display, TryFrom<String>, From<KeyId> for String

pub struct ProofKey([u8; 32]);                              // kein Clone/PartialEq/Serialize; Debug = "ProofKey(<redacted>)"; Drop überschreibt
impl ProofKey { pub const fn from_bytes([u8; 32]) -> Self; pub fn try_from_slice(&[u8]) -> Result<Self, ProofError>; }

#[derive(Default)] pub struct KeyRing { /* HashMap<KeyId, ProofKey> */ }   // Debug listet nur Key-IDs
impl KeyRing { pub fn new() -> Self; pub fn insert(&mut self, KeyId, ProofKey) -> Result<(), ProofError>;
               pub fn remove(&mut self, &KeyId) -> bool; pub fn contains(&self, &KeyId) -> bool; pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool; }

#[derive(Debug, Clone, PartialEq, Eq)] #[non_exhaustive]
pub struct ProofPolicy { pub max_ttl_secs: u32, pub max_clock_skew_secs: u32, pub allowed_cgroup_prefixes: Vec<String> }
impl ProofPolicy { pub fn new(u32, u32, Vec<String>) -> Result<Self, ProofError>;   // TTL 1..=120, Skew ≤ 30, Präfixe wohlgeformt
                   pub fn allows_cgroup(&self, &CgroupId) -> bool; }

pub trait NonceLedger { fn check_and_record(&self, nonce: &[u8; 16], expires_at: jiff::Timestamp) -> Result<(), ProofError>; }
#[derive(Debug, Default)] pub struct MemoryNonceLedger { .. }   // NICHT dauerhaft; Tests/Referenz
impl MemoryNonceLedger { pub fn new() -> Self; pub fn prune_expired(&self, Timestamp) -> Result<usize, ProofError>; pub fn recorded_count(&self) -> Result<usize, ProofError>; }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct SignedAuthorization { version: u16, key_id: KeyId, action_digest: ContentDigest, stage: EscalationStage,
    cgroup: CgroupId, nonce: [u8;16] /*hex32 chars*/, issued_at: Timestamp, ttl_secs: u32, mac: [u8;32] /*hex64 chars*/ }
impl SignedAuthorization {
    pub fn sign(key: &ProofKey, key_id: &KeyId, action: &WardenAction, stage: EscalationStage, cgroup: &CgroupId,
                nonce: [u8; 16], issued_at: Timestamp, ttl_secs: u32) -> Self;
    pub fn verify(&self, ring: &KeyRing, action: &WardenAction, now: Timestamp, policy: &ProofPolicy,
                  ledger: &dyn NonceLedger) -> Result<VerifiedAuthorization, ProofError>;
    pub fn version/key_id/action_digest/stage/cgroup/nonce/issued_at/ttl_secs(&self);
}
#[derive(Debug, Clone, PartialEq, Eq)] pub struct VerifiedAuthorization { .. }  // kein pub-Konstruktor, kein Deserialize
impl VerifiedAuthorization { pub fn key_id/action_digest/stage/cgroup/nonce/issued_at/expires_at(&self); }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct WardenRequest { pub version: u16, pub action: WardenAction, pub authorization: SignedAuthorization }
impl WardenRequest { pub fn new(WardenAction, SignedAuthorization) -> Self;
                     pub fn peek_version(json: &[u8]) -> Result<u16, serde_json::Error>;
                     pub fn verify(&self, &KeyRing, Timestamp, &ProofPolicy, &dyn NonceLedger) -> Result<VerifiedAuthorization, ProofError>; }
#[derive(Debug, Clone, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct WardenReply { pub version: u16, pub outcome: WardenResponse }
impl WardenReply { pub fn new(WardenResponse) -> Self; }

impl WardenAction { pub const fn kind_name(&self) -> &'static str; pub const fn cgroup(&self) -> &CgroupId; pub fn binding_digest(&self) -> ContentDigest; }
impl EscalationStage { pub const fn wire_name(self) -> &'static str; }

pub enum Denial { ProofMismatch, NotAdmissibleAtStage, UnsupportedVersion, Malformed, Unsupported, ExecutionFailed, Unavailable }

#[derive(harw_macros::HarwError)]   // + Debug→Display, ProofResult<T>
pub enum ProofError { InvalidKeyId, InvalidKeyMaterial, DuplicateKeyId, InvalidPolicy, UnsupportedVersion(u16), UnknownKeyId,
    BadMac, TtlOutOfPolicy, NotYetValid, Expired, TimestampOutOfRange, ActionMismatch, NotAdmissibleAtStage,
    CgroupMismatch, CgroupNotAllowed, NonceReplayed, LedgerIo(std::io::Error) /*#[from]*/ }
impl ProofError { pub fn as_denial(&self) -> Denial; }
// WardenProtoError += Proof(ProofError) /*#[from]*/
```

### 2.1 Kanonische Bytes / MAC

- Aktion: `binding_digest = blake3(ACTION_DOMAIN ‖ F(kind_name) ‖ F(cgroup))`, `F(x) = u64_be(len) ‖ x`. Kein serde.
- MAC: `blake3::keyed_hash(key, PROOF_DOMAIN ‖ F(version_be16) ‖ F(key_id) ‖ F(action_digest32) ‖ F(stage.wire_name)
  ‖ F(cgroup) ‖ F(nonce16) ‖ F(i64_be(issued_at.as_second) ‖ i32_be(subsec_nanosecond)) ‖ F(ttl_secs_be32))`.
- Vergleich: `blake3::Hash::from_bytes(mac) != expected` → `constant_time_eq_32` (`blake3-1.8.7/src/lib.rs:351-355`).

### 2.2 Prüfreihenfolge `verify` (verbindlich)

1 Version (`!= 2` → `UnsupportedVersion`) → 2 Key-ID (`UnknownKeyId`) → 3 MAC (`BadMac`) → 4 Zeitfenster
(`ttl == 0 || ttl > min(policy.max_ttl, 120)` → `TtlOutOfPolicy`; `issued_at > now + min(skew,30)` → `NotYetValid`;
`now >= issued_at + ttl` → `Expired`; Überlauf → `TimestampOutOfRange`) → 5 `action.binding_digest() == action_digest`
(`ActionMismatch`) → 6 `is_admissible_from(stage)` (`NotAdmissibleAtStage`) → 7 `action.cgroup() == cgroup`
(`CgroupMismatch`), `policy.allows_cgroup` (`CgroupNotAllowed`) → 8 `ledger.check_and_record(nonce, issued_at+ttl)`.
Test `test_verify_forged_request_does_not_reach_ledger` belegt: Fälschungen verbrauchen keine Nonce.

### 2.3 `TriageSubmission` (eingefroren)

```rust
// harw_dod_escalate::submission (Root-Re-Export: TriageSubmission, SubmissionError, SubmissionResult, TRIAGE_SUBMISSION_VERSION)
pub const TRIAGE_SUBMISSION_VERSION: u16 = 1;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct TriageSubmission { version: u16, finding: FindingId, finding_digest: ContentDigest, verdict: harw_dod_signals::SecurityVerdict }
impl TriageSubmission {
    pub fn new(FindingId, ContentDigest, SecurityVerdict) -> Result<Self, SubmissionError>;   // erzwingt verdict.binds(finding, digest)
    pub fn validate(&self) -> Result<(), SubmissionError>;             // Version, contract == CONTRACT_ID, validate_verdict, binds
    pub fn binds(&self, record_finding: &FindingId, record_digest: ContentDigest) -> Result<(), SubmissionError>;  // + Record aus eigenem Spool-Lesen
    pub fn version(&self) -> u16; pub fn finding(&self) -> &FindingId; pub fn finding_digest(&self) -> ContentDigest; pub fn verdict(&self) -> &SecurityVerdict;
}
#[derive(harw_macros::HarwError)] pub enum SubmissionError { UnsupportedVersion(u16), BindingMismatch, InvalidVerdict }
```
Wire: `{"version":1,"finding":"<id>","finding_digest":"<64 hex>","verdict":{…harwness.security-verdict/v1…}}`.
Keine Aktion/Stufe/cgroup — der Escalator leitet sie aus dem selbst gelesenen `FindingRecord` ab. `finding_digest` ist der
Digest, den C-FIND für `FindingRecord` festlegt (D-ESC berechnet ihn selbst und ruft `binds`).

## 3. Entscheidungen / Abweichungen vom Brief

1. **`ttl`-Parameter heißt `ttl_secs: u32`** (Sekunden; serde-freundlich, passt zu `max_ttl_secs`).
2. **`key_id`/`cgroup` als `&KeyId`/`&CgroupId`**; `KeyId` ist ein neuer validierter Newtype.
3. **`SignedAuthorization` trägt selbst `version`** (sonst wäre Schritt 1 in `verify(&self, …)` nicht prüfbar);
   `WardenRequest.version` wird zusätzlich in `WardenRequest::verify` geprüft.
4. **cgroup-Präfix strikt:** Ziel muss *unterhalb* eines Präfixes liegen (Präfix selbst nicht erlaubt); leere Präfixliste
   erlaubt nichts; `.`/`..`/leere Komponenten/Steuerzeichen passen nie. Signierte cgroup muss der Aktions-cgroup gleichen.
5. **`as_denial` faltet** alle Authentisierungs-/Bindungsfehler auf `Denial::ProofMismatch`; getrennt nur
   `UnsupportedVersion`, `NotAdmissibleAtStage` (erst nach gültigem MAC), `Unavailable` (Ledger-/Konfigurationsfehler).
6. **v1 bleibt kompilierbar** (`AuthorizationProof`, `WardenActionRequest`, `WardenAction::content_digest`), weil
   `harw-dod-warden`, `harw-warden`, `harw-dod-escalate/{action,freeze_ops,error}.rs` außerhalb meiner Zuständigkeit
   liegen. Kein `#[deprecated]` (würde unter `-D warnings` diese Crates brechen). Nur Doku-Kennzeichnung. Akzeptanz von v1
   endet mit D-WARDEN; **Folgeauftrag nötig:** Entfernen von `proof.rs`, `WardenActionRequest`, `content_digest`,
   `MismatchAspect`, `WardenProtoError::{ProofMismatch,NotAdmissibleAtStage,ActionEncoding}` nach W5 (Owner zuweisen).
7. **`#[allow(clippy::too_many_arguments)]`** an `SignedAuthorization::sign` (8 Parameter durch gefrorene Signatur
   erzwungen; gleiches Muster wie `SecurityVerdict::new`, `harw-dod-signals/src/verdict.rs:250`). Einzige neue `allow`.
8. **Zeroize:** nicht im Workspace → `Drop` überschreibt best effort (`self.0 = [0; 32]` + `black_box`). dep-request unten.
9. **Zusätzlich (F-088):** `Denial`-Varianten, `WardenReply`, `WardenRequest::peek_version` (Antwort auf fremde Versionen
   trotz `deny_unknown_fields`). `WardenResponse` selbst unverändert (Struct-Literale in `harw-dod-warden`).
10. `SubmissionError` lebt in `submission.rs` (nicht `error.rs`, nicht owned); D-ESC darf in `EscalateError` einfalten.

## 4. dep-requests

```
dep-request: harw-dod-escalate/Cargo.toml [dependencies] serde = { workspace = true }      # PFLICHT: submission.rs nutzt derive(Serialize, Deserialize); serde 1.0.228 (Cargo.lock), Features derive via Workspace
dep-request: cargo add -p harw-dod-warden-proto zeroize@1.8.2                              # optional: ProofKey via Zeroizing/ZeroizeOnDrop statt best-effort Drop (1.8.2 bereits in Cargo.lock)
```
Ohne den `serde`-Eintrag kompiliert `harw-dod-escalate` **nicht** (Datei nicht in meiner Zuständigkeit).
`blake3 = { workspace = true }` für `harw-dod-warden-proto` ist direkt eingetragen (workspace=true-Zeile, owned).

## 5. Nutzer alter Proof-Typen (nicht geändert → W5)

| Datei:Zeile | Nutzung | Folgearbeit |
|---|---|---|
| `harw-dod-warden/src/warden.rs:69,230-233` | `WardenActionRequest`, `request.proof.verify(finding, &action)`, `err.as_denial()` | **D-WARDEN**: `WardenRequest::verify(&ring, now, &policy, &ledger)` + persistenter `NonceLedger`; Executor an `VerifiedAuthorization` binden; `finding`-Parameter entfällt |
| `harw-dod-warden/src/warden.rs:96,117-125` | `WardenOutcome::to_wire -> Option<WardenResponse>` (`ExecutionFailed`/`VerificationFailed` → `None`) | **D-WARDEN**: `WardenReply::new(..)`, `Denial::{ExecutionFailed,Unavailable,Malformed,UnsupportedVersion,Unsupported}` |
| `harw-dod-warden/src/warden.rs:295-622` (Tests) | `AuthorizationProof::new`, `content_digest`, `WardenActionRequest::new`/serde | D-WARDEN |
| `harw-dod-warden/src/audit.rs:41,73,153,177` | `Denial`, `WardenActionAudit` (unverändert gültig) | D-WARDEN: `VerifiedAuthorization::{key_id,nonce}` + Peer-UID ins Audit |
| `harw-dod-warden/src/executor.rs` (6 Treffer), `lib.rs` (9), `error.rs` (5) | `WardenAction`/Re-Exporte | D-WARDEN: cgroup-Präfix nur noch aus `ProofPolicy` |
| `harw-warden/src/protocol.rs:32,42-51,57-75` | `WardenRequestEnvelope { finding, request: WardenActionRequest }` im Binary | **D-WARDEN**: Envelope löschen, `WardenRequest` + `peek_version` verwenden |
| `harw-warden/src/ipc.rs:292` | schreibt `WardenResponse` | D-WARDEN: `WardenReply` |
| `harw-warden/src/warden_factory.rs:61-140` (Tests) | `AuthorizationProof::new`, `content_digest`, `WardenActionRequest::new` | D-WARDEN: `KeyRing` aus `harw dod install`-Keys, `ProofPolicy` |
| `harw-warden/src/audit.rs:96,120`, `main.rs` (1) | `Denial::ProofMismatch` | D-WARDEN |
| `harw-dod-escalate/src/action.rs:52,117,273-298,412-437` | `Action::authorize` baut `AuthorizationProof` + `WardenActionRequest`; Tests `proof.verify/authorizes` | **D-ESC**: `SignedAuthorization::sign` mit Escalator-Key, CSPRNG-Nonce, TTL ≤ 120, `WardenRequest::new` |
| `harw-dod-escalate/src/freeze_ops.rs:326,408,437` | `.verify(finding.id(), &authorized.request().action)` | **D-ESC** |
| `harw-dod-escalate/src/error.rs:43,90,117,153` | `ActionEncoding(WardenProtoError)` | D-ESC (ggf. `Submission(SubmissionError)`/`Proof(ProofError)`) |
| `harw-dod-escalate/src/ladder.rs` (2) | `EscalationStage`/`WardenAction` (unverändert gültig) | — |

Exhaustive `match` auf `Denial`/`WardenProtoError` außerhalb der Proto-Crate: per grep keine → neue Varianten brechen nichts.

## 6. Tests (geschrieben, nicht ausgeführt)

`signed.rs`: Roundtrip (+ Accessoren, `expires_at`), serde-Roundtrip mit Nanosekunden, `deny_unknown_fields`/Großbuchstaben-Hex,
falscher Key → `BadMac`, unbekannte Key-ID, manipulierte Action → `ActionMismatch` ohne Nonce-Verbrauch, manipulierte
Stufe/cgroup im JSON → `BadMac`, Stufe zu niedrig → `NotAdmissibleAtStage`, signierte ≠ Aktions-cgroup → `CgroupMismatch`,
cgroup außerhalb Präfix (inkl. Präfix selbst, `harw.slice2`, `..`) → `CgroupNotAllowed`, abgelaufen (Grenze `ttl` exakt),
Zukunft jenseits Skew, TTL 0/121/`u32::MAX`/über enger Policy, mutierte Policy gekappt, Replay → `NonceReplayed`,
Fälschung erreicht Ledger nicht, Version 1/3 → `UnsupportedVersion`, Key-Rotation (alt+neu gültig, nach `remove` alt
abgelehnt, alter Key unter neuer ID → `BadMac`), `KeyId`-Grammatik, `ProofKey`-Redaktion/Slice-Länge, `KeyRing`-Duplikat/Debug,
`ProofPolicy::new`-Grenzen, komponentenweiser Präfixvergleich, `MemoryNonceLedger::prune_expired`.
`canonical.rs` (4), `action.rs` (+3), `stage.rs` (+1), `error.rs` (+2, 1 erweitert), `denial.rs` (Liste auf 7 erweitert),
`response.rs` (+5: `WardenRequest` Roundtrip/verify, Envelope-Version, `peek_version`, unknown field, `WardenReply`).
`submission.rs` (6): Bau/Bindung, fremder Befund/Digest, leere Begründung, `binds` gegen Record, serde + Revalidierung, Display.

## 7. Prüfhinweise für den Review

- `harw_macros::HarwError` auf Tupelvariante mit `#[msg]` ohne `{0}` (`UnsupportedVersion(u16)`): Display-Arm bindet `f0`,
  `#[allow(unused_variables)]` im generierten Code (`harw-macros/src/error.rs:77-90`) — wie `ProofMismatch(MismatchAspect)`.
- `#[serde(with = "crate::canonical::hex16")]` → Module `pub(crate)` in `pub mod canonical`, erzeugt per `macro_rules!`.
- `jiff::SignedDuration` (Root-Re-Export `jiff-0.2.32/src/lib.rs:760`), `Timestamp::checked_add(SignedDuration)`
  (`timestamp.rs:1483`, `From<SignedDuration> for TimestampArithmetic` `:2922`), `as_second`/`subsec_nanosecond` (`:965,:1161`).
- `blake3::{keyed_hash, KEY_LEN, Hash::from_bytes}` (`lib.rs:949,153,252`).
- `const fn cgroup(&self) -> &CgroupId` mit Or-Pattern-Bindung — stabil in const fn.
