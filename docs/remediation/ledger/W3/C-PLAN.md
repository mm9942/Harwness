# W3 / C-PLAN — Plan-Store-Invarianten, PlanId-Grammatik, atomarer Batch, Siegel

Owned: `harw-plan/src/{ids,store,memory_store,file_store,graph,validate,mutation,goal_store,error}.rs`
(`error.rs` per Orchestrator-Entscheid Q1 ergänzt), dieses Ledger.
Build-Policy eingehalten: nichts gebaut/getestet, keine git-Schreibbefehle. `cargo fmt` ist auf dem Host nicht
installiert (`error: no such command: fmt`) → Formatierung von Hand an rustfmt-Stil angelehnt (keine Codezeile
> 100 Zeichen); Parser-Gate des Orchestrators bitte beachten. Verifikation durch Lesen.

Orchestrator-Entscheide Q1–Q7 umgesetzt (siehe Nachricht an C-PLAN).

## Geschlossene Register-IDs

| ID | Wie |
|---|---|
| F-013 (Traversal + §5.1 Punkte 1–5) | `PlanId`-Grammatik; nur-`Draft`-Einfügen; `Superseded` terminal; Zukunfts-Findings nicht frisch + Kappung; `AddDependency`/Patch-Deps mit Siegel-/Statusprüfung; Regel 6 bei `SetStatus→Ready/InProgress` |
| G-032 | `PlanId::parse` an `validate_create`, beiden Store-`Create`-Pfaden und vor jedem `join` in `FilePlanStore` |
| G-014 | `missing_explorations` filtert auf `Draft|Blocked`; Frist über gemeinsame `is_fresh_finding`; `AttachEvidence`-Duplikat erneuert `attached_at` |
| F-130 | `graph` zählt `Explore|Research` (gemeinsame Konstante `validate::EXPLORATION_KINDS`) |
| F-126 | `BindGoal` setzt `plan.goal_id = Some(goal_id)` |
| w1-plan §5.2 „Tempname/Siegel“ | n/a; neu: Integritätssiegel `rev-<n>.seal` (Plan- und Goal-Store) |
| w1-plan §5.5 `.expect` in `InMemoryPlanStore::new` | entfernt (`from_parts`) |

## Öffentliche Signaturen (exakt)

### `harw-plan/src/ids.rs`
```rust
pub const PLAN_ID_MAX_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct PlanId(String);                        // HarwId-Derive ersetzt durch Handimpl

impl PlanId {
    pub fn parse(raw: &str) -> Result<PlanId, PlanError>;          // Grammatik ^[a-z0-9][a-z0-9-]{0,63}$
    pub fn try_new(value: impl Into<String>) -> Result<PlanId, PlanError>;   // = parse
    #[must_use] pub fn new(value: impl Into<String>) -> Self;       // UNCHECKED, nur Konstanten/Tests, Entfernung W12
    #[must_use] pub fn as_str(&self) -> &str;
    #[must_use] pub fn into_inner(self) -> String;
    #[must_use] pub fn is_valid(&self) -> bool;
}
impl TryFrom<String> for PlanId { type Error = PlanError; }
impl FromStr for PlanId { type Err = PlanError; }               // = parse
impl Display, AsRef<str>, Borrow<str>, PartialEq<str>, PartialEq<&str>, PartialEq<String> for PlanId
```
Fehler bei Grammatikverletzung: `PlanError::InvalidId { field: "PlanId", value: <Rohwert> }`.
Die vom Derive bisher gelieferte API (`try_new`, `new`, `as_str`, `into_inner`, `Display`, `FromStr`, `AsRef`,
`Borrow`, 3× `PartialEq`) bleibt vollständig erhalten (Nachweis `harw-macros/src/id.rs:273-365`); verschärft sind
nur `try_new`/`FromStr` (vorher: nur „nicht leer“) und `Deserialize` (vorher: ungeprüft).

### `harw-plan/src/store.rs`
```rust
#[derive(Debug, Clone)]
pub struct PlanRevision { pub revision: RevisionId, pub events: Vec<PlanEvent> }

pub trait PlanStore: Send + Sync {
    // … current / revision / apply / history / ready_nodes / waves unverändert …
    fn apply_batch(
        &self,
        plan: &PlanId,
        actions: Vec<PlanAction>,
        actor: &str,
        expected_rev: RevisionId,
    ) -> PlanResult<PlanRevision>;                                // PFLICHT-Methode (Q3)
}

pub(crate) fn stage_actions(base: &Plan, actions: Vec<PlanAction>, actor: &str, cfg: &PlanToolConfig,
    first_revision: RevisionId, now: OffsetDateTime) -> Result<(Plan, Vec<PlanEvent>), (usize, PlanError)>;
pub(crate) fn check_batch_target<'a>(current: Option<&'a Plan>, plan: &PlanId, expected_rev: RevisionId)
    -> PlanResult<&'a Plan>;
```
Semantik `apply_batch` (beide Stores identisch):
1. Schreib-Lock; `config.require_enabled()`.
2. Kein Plan **oder** `current.id != *plan` → `PlanNotFound`.
3. `current.revision != expected_rev` → `RevisionConflict { plan, expected, actual }`.
4. Leerer Batch → `Ok(PlanRevision { revision: current.revision, events: [] })`, kein Write.
5. Je Aktion auf dem **laufenden Kandidaten** (Klon): `Create` → `PlanExists`; `cfg.validate_action(action,
   laufende Knotenzahl)`; `validate_with`; `apply_mutation`; eigene Revision (lückenlos `next_revision…`).
   Erster Fehler → `BatchActionRejected { index, source: Box<PlanError> }`, nichts veröffentlicht.
6. Memory: Kandidat + Events + `next_revision` übernehmen. File: **ein** Snapshot `rev-<letzte>.json` + Siegel,
   **alle** N History-Zeilen in einem `write_all` + `fsync` (Fehler → `set_len(original)`), dann Siegel-, dann
   Snapshot-`rename`, dann Cache.
`apply` nutzt für Nicht-`Create` dieselbe Mechanik (`stage_actions` mit einer Aktion).

### `harw-plan/src/error.rs` (neue Varianten)
```rust
#[msg("Revisionskonflikt für Plan '{plan}': erwartet={expected}, aktuell={actual}")]
RevisionConflict { plan: PlanId, expected: RevisionId, actual: RevisionId },
#[msg("Batch verworfen: Aktion #{index} abgewiesen: {source}")]
BatchActionRejected { index: usize, source: Box<PlanError> },
#[msg("Siegelprüfung fehlgeschlagen für '{path}': erwartet={expected}, gefunden={actual}")]
SealMismatch { path: String, expected: String, actual: String },
```
Geändert: `InvalidId`-Text `"Ungültiger Wert für '{field}': {value:?} (leer, nur Leerzeichen oder unzulässige
Zeichen)"` (Debug-Form → Steuerzeichen escaped, keine Log-Injektion); `NodeSealed`-Text nennt alle drei
versiegelten Status. Hinweis: `HarwError` liefert für benannte Varianten `source() == None`
(`harw-macros/src/error.rs:173-192`) — die Ursache von `BatchActionRejected` ist über das Feld `source` erreichbar,
nicht über `Error::source`.

### `harw-plan/src/validate.rs`
Öffentliche API unverändert (`validate`, `validate_with`). Neu crate-privat:
`pub(crate) const EXPLORATION_KINDS`, `pub(crate) fn is_fresh_finding(&EvidenceRef, OffsetDateTime, u64) -> bool`.
Verhaltensänderungen:
- `STATUS_MATRIX` + `(Draft, Invalidated)`.
- `Create`: `ensure_not_blank` + `PlanId::parse`.
- `AddNode`/`Expand`-Kinder/`Condense`-Ersatz: Status ≠ `Draft` → `IllegalTransition { from: "(neu)", .. }`.
- `Invalidate`: `Completed` → `InvalidateCompleted`; sonst Matrix (`Superseded`/`Invalidated` → `IllegalTransition`).
- `AddDependency` und `UpdateNode.dependencies` und `AddNode.dependencies`: Kind versiegelt → `NodeSealed`;
  Parent `Superseded` → `IllegalTransition`; Kind `Ready|InProgress` und Parent ≠ `Completed` →
  `DependencyNotCompleted`.
- `SetStatus → Ready|InProgress`: zusätzlich `ensure_write_scope_free(plan, &node.write_scope, Some(id))`.
- Regel 12 nur beim Übergang nach `Ready` (nicht mehr bei `Ready → InProgress`; `InProgress` ist nur aus `Ready`
  erreichbar). Grund: mit Statusfilter in `graph` hinge ein `Ready`-Knoten nach Fristablauf sonst fest.
- `is_fresh_finding`: `attached_at > now` → nicht frisch.

### `harw-plan/src/graph.rs`
Signaturen unverändert. `missing_explorations`: nur `Draft|Blocked`, Deckung durch `Explore|Research`,
Frist über `validate::is_fresh_finding`.

### `harw-plan/src/mutation.rs` (crate-privat)
`BindGoal` → `plan.goal_id = Some(goal_id.clone())`; `materialize_node` kappt Payload-`attached_at` auf `now`;
`push_evidence_unique` erneuert bei Duplikat `(kind, locator)` den `attached_at` auf `now`.

### `harw-plan/src/file_store.rs`
Öffentlich unverändert bis auf `apply_batch`. Crate-privat `pub(crate) mod seal`:
```rust
pub(crate) const SEAL_VERSION: u32 = 1;
pub(crate) struct SnapshotSeal { version, revision: u64, digest: String, prev_revision: Option<u64>,
                                  prev_chain: Option<String>, chain: String }   // JSON in rev-<n>.seal
pub(crate) struct SealLink { revision: u64, chain: String }
pub(crate) fn seal_path(snapshot: &Path) -> PathBuf;                 // rev-<n>.json → rev-<n>.seal
pub(crate) fn chain_value(prev_chain: Option<&str>, revision: u64, digest: &str) -> String;
pub(crate) fn build(prev: Option<&SealLink>, revision: u64, snapshot: &[u8]) -> PlanResult<(Vec<u8>, SealLink)>;
pub(crate) fn directory_is_sealed(dir: &Path) -> PlanResult<bool>;
pub(crate) fn verify(snapshot_path: &Path, revision: u64, bytes: &[u8], dir_sealed: bool)
    -> PlanResult<Option<SealLink>>;
pub(crate) fn stage(target: &Path, bytes: &[u8]) -> PlanResult<StagedSeal>;   // commit()/discard()
```
- `digest` = `ContentDigest::of(snapshot_bytes)` (BLAKE3), Hex über `Display`.
- `chain` = `ContentDigest::of("harw-plan-seal:v1\0" ‖ prev_chain ‖ "\0" ‖ n ‖ "\0" ‖ digest)`.
- Laden: nur der neueste Snapshot wird gelesen (vorher: alle), Siegel **vor** Deserialisierung geprüft: Revision,
  Digest, Kettenwert, bei `prev_*` das Vorgängersiegel (`chain` gleich). Fehlt das Siegel in einem Verzeichnis
  mit mindestens einem `rev-*.seal` → `SealMismatch`. Verzeichnis ganz ohne Siegel = Legacy → einmal `warn!`,
  nächster Write versiegelt (`prev = None`).
- Veröffentlichungsreihenfolge: Snapshot+Siegel stagen → History-Append → Siegel-`rename` → Snapshot-`rename`
  (Absturz dazwischen ⇒ verwaistes Siegel, nie unversiegelter neuester Snapshot).
- **Ungeschlüsselt**: erkennt Beschädigung/naive Manipulation, nicht einen Angreifer mit Schreibrecht auf
  `HARW_HOME` (dokumentiert in Modulkopf und `SealMismatch`).
- Laden: Verzeichnisse mit ungültigem `PlanId`-Namen werden mit `warn!` übersprungen; Snapshot-`id` ≠
  Verzeichnisname → `InvalidId { field: "plan.id" }`; `plan_dir/plan_path/history_path` → `PlanResult<PathBuf>`
  mit `PlanId::parse` vor `join`.

### `harw-plan/src/goal_store.rs`
`FileGoalStore` bekommt dieselben Siegel (Mechanik direkt übertragbar: flaches `<root>/rev-<n>.json`), geteilter
Code `crate::file_store::seal`; `load` liest nur noch den neuesten Snapshot. Öffentliche API unverändert.

### `harw-plan/src/memory_store.rs`
`apply_batch`; `Create` prüft `PlanId::parse`; `new()` ohne `expect` (`fn from_parts`).

## Q5 — Nachweis Plan-ID-Herkunft (Grammatik `^[a-z0-9][a-z0-9-]{0,63}$`)
Keine Großbuchstaben/`_`/`.` in erzeugten IDs gefunden:
- `harw-ops/src/analyze.rs:136` `const ANALYSIS_PLAN_ID: &str = "plan-analyze"`; `:417`, `:659`.
- `harw-cli/src/main.rs:843` `const STARTUP_PLAN_ID: &str = "plan-cli"`; `:1328`.
- `harw-ops/src/plan.rs:507-512` `plan create <plan-id>` → `PlanId::new(plan_id.as_str())` (Nutzereingabe, frei).
- `harw-ops/src/goal.rs:462-468` `goal bind <plan-id>` → `PlanId::new(...)` (Nutzereingabe).
- `harw-cli/src/job_worker.rs:1228,1258` `plan_id: String` aus Job-Payload (Tests `"p-test"`).
- `harw-plan-bridge` nur Tests (`"p-test"`, `"p-apply"`, `"p-empty"`); `harw-web/src`: keine Treffer;
  `harw-tui` Tests `"p-test"`, `"plan-7"`; `harw-protocol/src/events.rs:303,325` `"plan-1"`.
- Keine Ableitung aus Goal-/Session-IDs oder UUIDs gefunden.

## API-Nachweise
- `harw_types::ContentDigest::of(&[u8]) -> Self` `harw-types/src/digest.rs:112`; `impl Display` (Hex)
  `:173`; `harw-types` ist bereits Dep (`harw-plan/Cargo.toml:8`).
- `PlanToolConfig::require_enabled` `harw-plan/src/config.rs:125`, `validate_action` `:146`.
- `HarwError`-Derive: `#[msg]` interpoliert benannte Felder inkl. `{x:?}` (`harw-macros/src/error.rs:229-260`),
  `source()` für benannte Varianten `None` (`:173-192`).
- `HarwId`-Derive-API (ersetzt durch Handimpl): `harw-macros/src/id.rs:273-365`.
- serde `try_from`-Container-Attribut, `TryFrom<String>` mit `Error: Display` (`PlanError` via `HarwError`).
- `serde_json::from_slice`, `to_vec_pretty`, `Value` IndexMut (Tests) — serde_json 1.0.150 (Cargo.toml:10).

## Tests (neu / angepasst)
- `ids.rs`: `test_plan_id_parse_accepts_existing_ids`, `test_plan_id_parse_rejects_traversal_and_separators`,
  `test_plan_id_parse_rejects_unicode`, `test_plan_id_deserialize_enforces_grammar`,
  `test_plan_id_from_str_and_try_new_enforce_grammar`, `test_plan_id_new_is_unchecked`.
- `error.rs`: `test_revision_conflict_display`, `test_batch_action_rejected_display_names_index_and_cause`,
  `test_seal_mismatch_display`, `test_invalid_id_display_escapes_control_characters`; `test_invalid_id_display`
  an Debug-Form angepasst.
- `memory_store.rs`: `test_apply_batch_applies_all_actions_with_sequential_revisions`,
  `test_apply_batch_failure_in_third_action_changes_nothing`, `test_apply_batch_revision_conflict_changes_nothing`,
  `test_apply_batch_other_plan_id_is_plan_not_found`, `test_apply_batch_rejects_create_inside_batch`,
  `test_apply_batch_enforces_node_limit_with_running_count`, `test_apply_batch_empty_is_noop`,
  `test_create_rejects_traversal_plan_id`, `test_bind_goal_sets_goal_id_through_store`.
- `file_store.rs`: `test_create_with_traversal_id_writes_nothing_outside_store`,
  `test_load_ignores_directories_with_invalid_plan_id`, `test_seal_is_written_and_reload_verifies`,
  `test_manipulated_snapshot_is_rejected_on_load`, `test_manipulated_seal_chain_is_rejected_on_load`,
  `test_deleted_seal_in_sealed_directory_is_rejected`, `test_legacy_unsealed_plan_loads_and_is_sealed_on_next_write`,
  `test_apply_batch_persists_one_snapshot_and_all_events`, `test_apply_batch_failure_in_third_action_writes_nothing`,
  `test_apply_batch_revision_conflict_writes_nothing`.
- `goal_store.rs`: `test_file_goal_store_rejects_manipulated_snapshot_on_open`,
  `test_file_goal_store_legacy_without_seal_opens_and_seals_next_write`.
- `validate.rs` (je §5.1-Lücke): `test_gap1_add_node_completed_without_evidence_is_rejected`,
  `test_gap1_expand_child_and_condense_replacement_must_be_draft`,
  `test_gap2_invalidate_superseded_and_invalidated_is_rejected`, `test_gap2_invalidate_draft_follows_matrix`,
  `test_gap3_future_finding_is_not_fresh`, `test_gap4_set_status_ready_checks_write_scope_conflict`,
  `test_gap5_add_dependency_checks_seal_and_status`, `test_gap5_patch_dependencies_on_active_node_requires_completed`,
  `test_rule12_in_progress_after_ready_does_not_recheck_freshness`, `test_create_rejects_traversal_plan_id`.
  Angepasst (Verhalten bewusst geändert): `test_add_ready_node_is_rejected_as_non_draft` (vorher
  `…rejects_incomplete_dependency`), `test_add_ready_coding_node_is_rejected_before_exploration_check`.
- `graph.rs`: `test_missing_explorations_filters_by_status`,
  `test_missing_explorations_respects_ttl_and_rejects_future_timestamps`,
  `test_missing_explorations_counts_completed_research_dependency`.
- `mutation.rs`: `test_bind_goal_sets_goal_id` (ersetzt `…is_noop_until_plan_has_goal_id`),
  `test_add_node_clamps_future_evidence_timestamp`, `test_attach_evidence_duplicate_refreshes_attached_at`.

## Offene Annahmen / Risiken
- **Strikte Deserialisierung**: vorhandene `rev-*.json`/`history.jsonl`/Goal-Snapshots mit nicht konformer
  `PlanId` (z. B. per `plan create My_Plan` angelegt) sind nicht mehr ladbar (`Serde`-Fehler beim Start). Im Code
  keine solchen IDs; Nutzerdaten unbekannt.
- Regel 12 nicht mehr bei `Ready → InProgress` (siehe oben) — bewusste Semantikänderung.
- Nicht behoben (außerhalb Q7): Absturz/Fehler **nach** History-Append beim Snapshot-/Siegel-`rename` hinterlässt
  Events ohne Snapshot (w1-plan §5.2, vorbestehend); kein Prozess-Lock; `Expand`/`Condense` ohne `max_nodes`
  (§5.1 Punkt 7 — `stage_actions` prüft nur, was `config.rs::validate_action` prüft: `AddNode`).
- Siegel prüft nur den neuesten Snapshot + Kettenwert des direkten Vorgängers, nicht die History.

## Folgearbeit
- **Alle Aufrufer-Agenten (Trait)**: `PlanStore::apply_batch` ist Pflicht-Methode; Implementierer außerhalb
  `harw-plan`: keine (`grep "impl.*PlanStore for"` → nur `file_store.rs`, `memory_store.rs`). Test-Doubles
  außerhalb bei künftigen Impls ergänzen.
- **lib.rs-Owner (harw-plan)**: `pub use crate::store::{PlanRevision, PlanStore};` im Crate-Root;
  lib.rs-Doku-Beispiel auf `PlanId::parse`.
- **testing.rs-Owner**: `impl_store_conformance_tests!` um `apply_batch` (Atomarität, Konflikt) erweitern und für
  `FilePlanStore` instanziieren; `PlanId::new`-Nutzung (`testing.rs:202,532,632,675,689,985`) → `parse`.
- **A-PLANB (harw-plan-bridge)**:
  - `controller.rs:614-629` `InsertExplore` (AddNode + AddDependency) und `:582-603` (AttachEvidence + SetStatus)
    auf `apply_batch(plan.id, …, plan.revision)` umstellen.
  - Test-Fixture `lib.rs:283-301` `seeded_plan_store` fügt Knoten mit `Ready`/`Completed` per `AddNode` ein
    (`lib.rs:370,418-419`, `job_bridge.rs:577,609,668,699,732,751`) → schlägt jetzt mit `IllegalTransition` fehl;
    auf Draft + `SetStatus`-Pfad (bzw. Evidenz + Completed) oder `apply_batch` umbauen.
  - `controller.rs:1331/1403/1459/1605/1656/1690`, `lib.rs:271,287,360`, `job_bridge.rs:682` `PlanId::new` → `parse`.
  - `finding_store.rs:150,266` nutzt `plan_id: &str` als Pfadsegment ungeprüft → `PlanId::parse`.
  - `controller.rs:324` verlässt sich auf `missing_explorations`: liefert jetzt nur `Draft|Blocked`.
  - `job_bridge.rs:447` `Invalidate` auf bereits `Invalidated`/`Superseded` Knoten wird jetzt abgewiesen.
- **A-OPSPLAN (harw-ops)**: `plan.rs:512`, `goal.rs:468` Nutzereingabe mit `PlanId::parse` validieren und
  Fehlermeldung zeigen (statt `PlanId::new`); `analyze.rs:417,659` Konstante → `parse`; Tests `analyze.rs:1416`,
  `goal.rs:1056,1521`, `plan.rs:1685,1766,1830`; `plan bind-goal` meldet jetzt wahrheitsgemäß (goal_id gesetzt).
- **A-MAIN/A-JOBW (harw-cli)**: `main.rs:1328` → `parse`; Test `job_worker.rs:2926` fügt `Ready`-Knoten per
  `AddNode` ein → Draft + `SetStatus`; `job_worker.rs:2825` `PlanId::new`.
- **harw-tui**: `history_cell.rs:1635` `PlanId::new` (Test).
- **W12**: `PlanId::new` entfernen (nach Migration aller oben gelisteten Aufrufer).
- **match auf geänderte Enums**: `PlanError` +3 Varianten. Treffer außerhalb `harw-plan`:
  `harw-ops/src/plan.rs:772-777,783-787` und `harw-ops/src/goal.rs` — alle mit `other =>`-Arm, also nicht
  exhaustiv betroffen. `PlanNodeStatus`/`PlanAction` unverändert.
