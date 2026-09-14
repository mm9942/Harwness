# C-APPR – Approval-Modell: Serveruhr, TTL, Resolution-Leser, einheitlicher Actor (G-011, F-173, F-122)

Rolle: focused-coding-task (Opus), Welle W3 Teil B. Kein `cargo build/check/test/clippy/run/add`, kein
`make`/`rustc`/`rust-analyzer`, keine `git`-Schreibbefehle. Verifikation durch Lesen.

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`; Plan `eventual-wandering-pebble.md` Teil B (Abgleich,
gefrorene W3-Signaturen, W3-Tabelle C-APPR); Ledger `W1/W1-01.md` (O_NOFOLLOW), `W0b/W0B-05.md` (Principal);
Register `x-findings-register-w4.md:82` (G-011), `x-findings-register-w1-w3.md:230` (F-122), `:286` (F-173),
`00-SYNTHESE.md:237/364/406`.

Ablauf: erster Durchgang BLOCKED (kein Clock-Trait im Workspace, fremde Dateien error.rs/Cargo.toml nötig, kein
Anfragender im Datenmodell). Orchestrator-Entscheidungen (bindend) danach umgesetzt; Zuständigkeit erweitert.

## Geänderte / neue Dateien

| Datei | Änderung |
|---|---|
| `harw-types/src/clock.rs` (neu) | `pub trait Clock: Send + Sync { fn now(&self) -> jiff::Timestamp; }`, `SystemClock` (Debug, Clone, Copy, Default, PartialEq, Eq); `FixedClock` nur im Testmodul |
| `harw-types/src/lib.rs` | `pub mod clock;`, `pub use clock::{Clock, SystemClock};`, Crate-Doku |
| `harw-types/Cargo.toml` | `jiff = { workspace = true }` (Root-`Cargo.toml:121`, `0.2.32` + serde) |
| `harw-types/src/principal.rs` | `Principal::actor_id(&self) -> Option<ApprovalActor>` delegiert an `approval_actor()`; Tests |
| `harw-types/src/ids.rs` | **unverändert** – `ApprovalActor { Operator { id }, ChannelPeer { channel, peer } }` verifiziert (`ids.rs:175-183`), keine Änderung nötig |
| `harw-session-store/src/error.rs` | neu `ApprovalExpired { request, issued_at, expires_at }`, `ApprovalCorrupt { session, request, detail }` |
| `harw-session-store/Cargo.toml` | `tracing = { workspace = true }` (Root-`Cargo.toml:139`) |
| `harw-session-store/src/lib.rs` | Re-Export zusätzlich `DEFAULT_APPROVAL_TTL` |
| `harw-session-store/src/approval.rs` | TTL, Serveruhr in `resolve`, `resolution`, `pending_all`, Tests |

## API (Ist-Stand nach C-APPR)

```rust
// harw_types
pub trait Clock: Send + Sync { fn now(&self) -> jiff::Timestamp; }
pub struct SystemClock;
impl Principal { pub fn actor_id(&self) -> Option<ApprovalActor>; }

// harw_session_store::approval
pub const DEFAULT_APPROVAL_TTL: SignedDuration = SignedDuration::from_mins(30);
impl ApprovalStore {
    pub fn new(root: &Path) -> Self;                                  // TTL = DEFAULT_APPROVAL_TTL
    pub fn with_ttl(root: &Path, ttl: SignedDuration) -> Self;
    pub fn ttl(&self) -> SignedDuration;
    pub fn root(&self) -> &Path;                                      // unverändert
    pub fn issue(&self, record: &ApprovalRecord) -> SessionStoreResult<()>;               // unverändert
    pub fn pending(&self, session: &SessionId, request: &ItemId) -> SessionStoreResult<ApprovalRecord>; // unverändert
    pub fn resolve(&self, session: &SessionId, request: &ItemId, decision: ReviewDecision,
                   comment: Option<String>, actor: &ApprovalActor, clock: &dyn Clock)
        -> SessionStoreResult<ApprovalResolutionRecord>;
    pub fn resolution(&self, session: &SessionId, request: &ItemId)
        -> SessionStoreResult<Option<ApprovalResolutionRecord>>;
    pub fn pending_all(&self, limit: usize, clock: &dyn Clock) -> SessionStoreResult<Vec<ApprovalRecord>>;
}
```

`ApprovalRecord`/`ApprovalResolutionRecord`: **keine neuen Felder** (Wire-Form unverändert). `ApprovalStore` erhält
zusätzlich `#[derive(Debug, Clone)]`.

## Semantik

- **Serveruhr (F-122):** `resolved_at = clock.now()`; kein Zeitparameter vom Aufrufer mehr.
- **TTL:** abgelaufen ⇔ `clock.now() >= issued_at + ttl` (Grenze inklusiv, fail-closed). Überlauf von
  `issued_at + ttl` gilt als abgelaufen (`expires_at = Timestamp::MAX`). `ttl <= 0` ⇒ jede Anfrage sofort abgelaufen.
- **Prüfreihenfolge in `resolve` (unter Session-Lock):** Symlink auf `.resolved.json` → `ApprovalNotFound`;
  vorhanden → `ApprovalAlreadyResolved`; Pending fehlt/Symlink → `ApprovalNotFound`; Schlüssel passt nicht →
  `ApprovalNotFound`; Actor ≠ gebundener Actor → `ApprovalActorMismatch`; TTL → `ApprovalExpired`. Actor vor TTL,
  damit ein fremder Actor keinen TTL-Zustand erfährt. Bei `ApprovalExpired` wird nichts geschrieben.
- **`resolution`:** lock-frei (Datei entsteht nur per atomarem `persist()`), liest über `read_pending`
  (`harw_fsutil::open_nofollow`). Fehlt → `Ok(None)`. Symlink, Nicht-Datei, undekodierbar oder fremder
  Session-/Request-Schlüssel → `Err(ApprovalCorrupt)` (fail-closed; eigene Variante statt `CorruptRecord`, dessen
  Text „corrupt transcript record“ lautet).
- **`pending_all`:** scannt `<root>/approvals/<session>/*.pending.json`; offen = kein Eintrag (auch kein Symlink)
  an `<request>.resolved.json` und nicht abgelaufen. Sortierung `issued_at` aufsteigend, Gleichstand Session, dann
  Request; `truncate(limit)`; `limit == 0` → leer; fehlende Wurzel → leer; nur unlesbare Wurzel → `Err(Io)`.
  Übersprungen mit `tracing::warn!` (Felder `path`, `error`, ggf. `record_session`/`record_request`): unsichere
  Session-/Request-Namen, Session-Eintrag kein echtes Verzeichnis (Symlink), unlesbare Einträge, Pending-Symlink
  oder Nicht-Datei, undekodierbares JSON, Datensatz-Schlüssel ≠ Pfad. Abgelaufene/aufgelöste Einträge werden
  ohne Warnung ausgelassen (regulärer Zustand).
- **W1-01 unverändert:** `open_lock_file_without_following_symlinks`, `read_pending` über `open_nofollow`, alle
  drei Symlink-Regressionstests bleiben (nur auf neue `resolve`-Signatur umgestellt, `pending_all`-Assertion ergänzt).

## Actor (F-173, G-011)

`actor_id()` = `approval_actor()`; Tabelle unverändert (Human×Tui → `local-tui`, Human×Cli → `local-cli`,
Human×Web → `owner`, Channel×Mcp → `<id>`, sonst `None`). Namen bleiben laut Teil-A-Hinweis bis **P1.6** ungleich;
F-173 (Mismatch) ist damit bewusst **noch offen**. Hart kodierte Namen außerhalb meiner Dateien:
`harw-tui/src/app.rs:3215` (`local-tui`), `harw-runtime/src/spec.rs:588` (`local-tui`),
`harw-cli/src/web.rs:244` (`owner`, `StaticUidApprovalActorMap`).

## Selbstgenehmigung – nicht umgesetzt (Folgearbeit)

`ApprovalRecord.actor` ist der bei Ausstellung gebundene **Beantworter**, nicht der Anfragende
(`approval.rs` `ApprovalRecord`, Prüfung `record.actor != actor`). Der Anfragende (Model-/Child-Principal) wird
nirgends persistiert; ohne neues Feld ist „Actor == Anfragender“ nicht prüfbar. Entscheidung Orchestrator: **kein**
`requested_by`-Feld in C-APPR; daher auch **keine** Variante `ApprovalSelfApproval` angelegt.

- **Folgearbeit A-APPR (W4a) / WB-SRV (W5):** Anfragenden-Bindung entwerfen (z. B. `requested_by: Option<ApprovalActor>`
  bzw. Principal-Kennung mit `#[serde(default)]`), Aussteller in `harw-core/src/turn_loop.rs:1491` befüllen, Prüfung
  in `resolve` + Variante `ApprovalSelfApproval { request }`.
- **G-011-Hinweis:** Ein reiner Gleichheitsvergleich fängt G-011 nicht. Der Angriff ist ein Same-UID-Prozess (auch
  modellgestartet), der sich über den Unix-Socket per UID-Map als `owner` authentisiert; der modellnahe Principal
  hat `actor_id() == None`. Die eigentliche Gegenmaßnahme ist **Transport-Authentisierung**: Launch-Token/Bearer
  auch am Unix-Socket für `approval.resolve` (WB-SRV, F-029/F-030), Modellprozesse ohne Zugriff auf Token und
  `HARW_HOME` (bwrap/netns). Solange das fehlt, darf P1.6 die Actor-Namen nicht angleichen.

## Aufrufer der geänderten Signaturen (Folgearbeit, nicht geändert)

| Stelle | Heute | Nötig | Zuständig |
|---|---|---|---|
| `harw-core/src/turn_loop.rs:1139` | `approvals.resolve(session, req, &actor, decision, comment, jiff::Timestamp::now())` | `resolve(session, req, decision, comment, &actor, clock)`; Clock aus Runtime | A-LOOP (W4a) |
| `harw-core/src/turn_loop.rs:1491` | `issued_at: jiff::Timestamp::now()` | Serveruhr (`clock.now()`) statt Direktaufruf | A-LOOP (W4a) |
| `harw-core/tests/turn_loop.rs:802` | alte Reihenfolge + Zeitstempel | neue Signatur, `FixedClock`/`SystemClock` | A-LOOP (W4a) |
| `harw-web/src/security.rs:311-325` `resolve_approval(.., resolved_at: Timestamp)` | reicht Client-Zeit durch (F-122) | Parameter `clock: &dyn Clock`; Tests `:386/:416/:443/:455` | A-APPR (W4a) |
| `harw-ops/src/approval.rs:367/471-479` | `args.resolved_at` vom Client; Tests `:602/:661/:685/:716/:747` | Feld entfernen, Serveruhr aus `OpContext`-Service | A-APPR (W4a) |
| `harw-ops/src/approval.rs:205` `discover_pending_candidates`, `:275` `list_pending_records` | eigener Verzeichnis-Scan | durch `ApprovalStore::pending_all(limit, clock)` ersetzen | A-APPR (W4a) |
| `harw-tui/src/approval.rs:1110` | kein Store-Leser (G-011) | Approval-Pause pollt `ApprovalStore::resolution` | T-TUI (W4b) |
| `harw-cli/src/web.rs:219`, `harw-cli/src/runtime_web.rs:297`, `harw-runtime/src/assembly.rs:199/1513` | `ApprovalStore::new` | kompatibel; `SystemClock` in ServiceMap/Assembly bereitstellen | A-APPR / I-CONTRIB |

`ApprovalStore::new`, `issue`, `pending`, `root` sind unverändert; Literale von `ApprovalRecord`
(`turn_loop.rs:1491`, `harw-cli/src/web.rs:642`, `harw-ops/src/approval.rs:568`, `harw-web/src/security.rs:349`)
bleiben gültig.

## Tests

`harw-session-store/src/approval.rs`:
- `test_resolve_uses_server_clock_for_resolved_at` – `resolved_at == FixedClock.now()`, auch im durablen Datensatz.
- `test_resolve_rejects_expired_request_and_writes_nothing` – genau `issued_at + 30 min` → `ApprovalExpired` mit
  korrekten Feldern; `resolution` bleibt `None`.
- `test_resolve_accepts_request_just_before_ttl` – `TTL − 1 s` wird aufgelöst.
- `test_with_ttl_applies_custom_ttl_and_new_uses_default` – `new` = 30 min, `with_ttl(5 min)` läuft bei 6 min ab.
- `test_resolution_missing_returns_none`, `test_resolution_corrupt_file_is_error`,
  `test_resolution_record_keyed_to_other_request_is_error`, `test_resolution_symlink_is_error_not_none` (unix).
- `test_pending_all_sorts_by_issued_at_and_applies_limit` (sessionübergreifend, Limit 10/2/0),
  `test_pending_all_excludes_resolved_and_expired`, `test_pending_all_skips_corrupt_entries` (Müll-JSON,
  fehlabgelegter Datensatz, unsicherer Session-Name), `test_pending_all_without_root_is_empty`.
- Bestand angepasst: `resolution_is_durable_actor_bound_and_single_use`, drei Symlink-Tests (W1-01), Pfad-/Sync-Tests.

`harw-types/src/principal.rs`: `test_actor_id_table_per_principal_kind_and_surface` (4 Kinds × 8 Surfaces × 4 Tiers,
Gleichheit mit `approval_actor()`), `test_actor_id_is_none_for_model_and_operation_kinds`; Doctest an `actor_id`.
`harw-types/src/clock.rs`: `test_system_clock_now_is_not_before_a_prior_reading`, `test_clock_is_usable_as_trait_object`;
Doctests an Modul, `Clock`, `SystemClock`.

## Belegte fremde APIs

| API | Beleg |
|---|---|
| `SignedDuration::from_mins` (const fn), `from_secs` (const fn), `impl Sub for SignedDuration` | `jiff-0.2.32/src/signed_duration.rs:786, 497, 2615` |
| `Timestamp::checked_add<A: Into<TimestampArithmetic>>(self, A) -> Result<Timestamp, Error>`; `From<SignedDuration> for TimestampArithmetic` | `jiff-0.2.32/src/timestamp.rs:1483, 2922` |
| `Timestamp::MAX`, `Timestamp::constant` (const fn) | `timestamp.rs:348, 520` |
| `#[msg]` mit `jiff::Timestamp`-Feldern (Display) | Vorbild `harw-session-store/src/error.rs` `JobLeaseExpired` |
| `harw_fsutil::open_nofollow`, `OpenMode::read_only` | W1-01 (`harw-fsutil/src/open.rs:254-259`) |

## Compile-/Lint-Risiken (nicht kompiliert)

1. `cargo metadata --offline --no-deps --format-version 1` scheitert **derzeit workspaceweit** unabhängig von
   C-APPR: Root-`Cargo.toml` listet `harw-egress` als Member, `harw-egress/Cargo.toml` existiert noch nicht
   (C-EGRESS/X0 parallel in W3). Manifeste von harw-types/harw-session-store nur per Lesen geprüft.
2. `Cargo.lock`: `harw-types` erhält neue Kante `jiff`, `harw-session-store` neue Kante `tracing` (beide bereits im
   Lock vorhanden); ein `--locked`-Lauf braucht vorher einen Lock-Update-Lauf.
3. `tracing::warn!` mit `%`-Feldern und abschließendem Literal ohne Komma – Standardform.
4. `SessionId::from_str`/`ItemId::from_str` als inhärente Konstruktoren (nicht `FromStr`), wie im Bestand.
5. rustfmt nicht verfügbar (W0B-05); Formatierung von Hand (max_width 100, alle Zeilen ≤ 100 geprüft).

## Folgearbeit (zusammengefasst)

- Orchestrator: `cargo test -p harw-types -p harw-session-store`, `cargo clippy --all-targets -D warnings`, `cargo fmt`;
  danach sind `harw-core`, `harw-web`, `harw-ops` bis zur Umstellung (Tabelle oben) **nicht kompilierbar** –
  Sequenz: A-LOOP/A-APPR in W4a.
- A-APPR/WB-SRV: Selbstgenehmigungs-Bindung + Transport-Authentisierung (siehe oben).
- P1.6: Actor-Namen angleichen erst nach Transport-Authentisierung.
