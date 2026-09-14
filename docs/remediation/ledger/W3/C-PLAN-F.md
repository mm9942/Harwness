# C-PLAN-F — Folgearbeit `harw-plan/src/lib.rs` (Re-Export) + `harw-cli/src/job_worker.rs` (Test-Fixture)

Rolle: focused-coding-task (Sonnet). BUILD-POLICY eingehalten: nichts gebaut/getestet/gelintet, keine
`git`-Schreibbefehle. `cargo metadata --offline --no-deps --format-version 1` nicht erneut nötig (reine
Lese-/Grep-Verifikation gegen den bereits vorhandenen C-PLAN-Quellcode). Verifikation ausschließlich durch
Lesen.

Owned: `harw-plan/src/lib.rs` (nur Re-Exporte), `harw-cli/src/job_worker.rs` (nur Testmodul), diese
Ledger-Datei.

## Auftrag 1 — `harw-plan/src/lib.rs`: `PlanRevision` re-exportieren

Laut `docs/remediation/ledger/W3/C-PLAN.md`, Abschnitt „Folgearbeit“ (lib.rs-Owner): `PlanStore::apply_batch`
gibt `PlanRevision` zurück (`harw-plan/src/store.rs:46` `pub struct PlanRevision { revision: RevisionId,
events: Vec<PlanEvent> }`), das Crate-Root re-exportierte bisher nur `PlanStore` selbst
(`pub use crate::store::PlanStore;`, Zeile 87 vor der Änderung) — `harw_plan::PlanRevision` existierte nicht,
obwohl `RevisionId` und `PlanEvent` (die Feldtypen) bereits am Crate-Root stehen (`pub use crate::ids::{…,
RevisionId, …};`, `pub use crate::actions::{…, PlanEvent};`).

### Änderung

```rust
pub use crate::memory_store::InMemoryPlanStore;
pub use crate::store::{PlanRevision, PlanStore};
```

(vorher: `pub use crate::store::PlanStore;`). Eine Zeile geändert, sonst nichts in `lib.rs` angefasst.

### Geprüft: weitere neue pub-Items aus C-PLAN, die einen Re-Export bräuchten

Gegen `docs/remediation/ledger/W3/C-PLAN.md` §„Öffentliche Signaturen“ Modul für Modul geprüft:

- `ids.rs`: `PlanId`-API unverändert in der Form (nur verschärfte Semantik), bereits re-exportiert. Neu:
  `pub const PLAN_ID_MAX_LEN: usize = 64` (`ids.rs:25`) — **kein** Konsument außerhalb `harw-plan/src/ids.rs`
  selbst (`grep -rn PLAN_ID_MAX_LEN` über den gesamten Workspace: nur Definition + zwei interne Nutzungen +
  zwei ids.rs-Tests). Bleibt unter `harw_plan::ids::PLAN_ID_MAX_LEN` — kein Re-Export nötig, da kein externer
  Aufrufer und die Fassade laut Modulkopf (Zeilen 76-98) kuratierte *Kerntypen* führt, nicht jede Konstante.
- `store.rs`: `PlanRevision` (neu) → re-exportiert (dieser Auftrag). `stage_actions`/`check_batch_target`
  sind `pub(crate)` — nicht re-exportierbar (Sichtbarkeit reicht nicht für `pub use`) und nicht dafür
  vorgesehen (Crate-interne Batch-Mechanik).
- `error.rs`: drei neue Varianten (`RevisionConflict`, `BatchActionRejected`, `SealMismatch`) liegen im
  bereits re-exportierten `PlanError`-Enum (`pub use crate::error::{PlanError, …}`) — automatisch mit
  abgedeckt, kein zusätzlicher Re-Export möglich oder nötig (Enum-Varianten werden nicht einzeln re-exportiert).
- `validate.rs`: öffentliche API (`validate`, `validate_with`) unverändert; neue Items (`EXPLORATION_KINDS`,
  `is_fresh_finding`) sind `pub(crate)` — nicht re-exportierbar.
- `graph.rs`: Signaturen unverändert.
- `mutation.rs`: crate-privat (`mod mutation;`, nicht `pub mod`) — unverändert, keine Fassade betroffen.
- `file_store.rs`: öffentliche API unverändert bis auf `apply_batch` (Trait-Methode, kein separates
  Re-Export-Ziel); `pub(crate) mod seal` ist crate-privat.
- `goal_store.rs`, `memory_store.rs`: öffentliche API unverändert (`apply_batch` ist Trait-Methode).

Ergebnis: **einzig** `PlanRevision` fehlte in der Fassade; ergänzt. Keine weiteren Re-Exports nötig.

## Auftrag 2 — `harw-cli/src/job_worker.rs`: Testfixture auf Draft-Einfügung umgestellt

`grep -n 'AddNode' harw-cli/src/job_worker.rs` (neu verifiziert): genau ein Treffer, in
`test_plan_node_under_a_marked_parent_directory_binds_the_workspace_root` (Zeile 2926 vor dieser Änderung).

### Befund

```rust
let mut node = harw_plan::testing::base_node("t-1");
node.kind = PlanNodeKind::Research;
node.status = PlanNodeStatus::Ready;                       // <- verletzt seit C-PLAN
if let Err(error) = plan.apply(PlanAction::AddNode { node }, "test") {
    panic!("add node: {error}");
}
```

`harw-plan/src/validate.rs::validate_add_node` (~:396-410, C-PLAN F-013 §5.1 Punkt 1) lehnt `AddNode` mit
`node.status != PlanNodeStatus::Draft` jetzt mit `PlanError::IllegalTransition { from: "(neu)", .. }` ab.
`harw_plan::testing::base_node` liefert bereits `status: PlanNodeStatus::Draft` (`testing.rs:172`); die
Fixture überschrieb das explizit auf `Ready`, bevor sie den Knoten per `AddNode` einfügte — genau der Fall,
den C-PLAN jetzt verbietet.

### Fix

Knoten als `Draft` einfügen (Fixture-Default, kein expliziter Overwrite mehr) und per erlaubtem
`Draft → Ready`-Übergang (`STATUS_MATRIX`, `validate.rs:102`) auf `Ready` bringen — atomar in einem Batch
gegen die aktuelle Revision des Stores, wie im Brief bevorzugt:

```rust
let mut node = harw_plan::testing::base_node("t-1");
node.kind = PlanNodeKind::Research;
let node_id = node.id.clone();
let expected_rev = plan.revision();
if let Err(error) = plan.apply_batch(
    &PlanId::new("p-test"),
    vec![
        PlanAction::AddNode { node },
        PlanAction::SetStatus {
            id: node_id,
            status: PlanNodeStatus::Ready,
            reason: None,
        },
    ],
    "test",
    expected_rev,
) {
    panic!("add node: {error}");
}
```

`plan` ist zu diesem Zeitpunkt noch `InMemoryPlanStore` (nicht `Arc<dyn PlanStore>` — das Wrapping erfolgt
erst danach), `PlanStore` (inkl. `apply_batch`/`revision`) ist über `use super::*;` (`job_worker.rs:1894`) aus
dem Top-Level-Import `harw_plan::PlanStore` (`job_worker.rs:53`) im Testmodul sichtbar; `PlanAction`, `PlanId`,
`PlanNodeStatus`, `InMemoryPlanStore` und `RevisionId` sind bereits testmodul-lokal importiert
(`job_worker.rs:1897,1899`) — keine neuen `use`-Zeilen nötig.

### Verifikation der Übergangs-Erlaubnis (durch Lesen)

- `Draft → Ready` steht in `STATUS_MATRIX` (`validate.rs:102`).
- Regel 12 (Explore-before-implement) greift nur, wenn `cfg.require_exploration_for` den Knotentyp enthält
  (`validate.rs::ensure_exploration_fresh`, ~:469). `InMemoryPlanStore::new()` (im Test verwendet) baut über
  `PlanToolConfig::enabled_defaults()` (`memory_store.rs:75`), deren `require_exploration_for: Vec::new()`
  ist (`config.rs:192`, bewusst leer für diesen Bootstrap-Helfer) — die Prüfung ist für diesen Store immer
  ein No-Op, unabhängig vom `kind` des Knotens (hier `Research`).
- Regel 6 (`write_scope`-Disjunktheit aktiver Knoten, auch beim Übergang: `validate.rs:830-833`
  `ensure_write_scope_free`): der Plan enthält zu diesem Zeitpunkt nur diesen einen Knoten (`Create` direkt
  davor, keine weiteren `AddNode`-Aufrufe im Test) — kein Konflikt möglich.
- Abhängigkeiten (`ensure_dependencies_completed`, `validate.rs:828`): `base_node` liefert
  `dependencies: Vec::new()` (`testing.rs:164`) — nichts zu prüfen.
- `apply_batch`-Batch-Semantik (`store.rs`-Doku, C-PLAN.md §„Semantik apply_batch“ Punkt 5): jede Aktion sieht
  den Zustand nach ihren Vorgängern — `SetStatus` im selben Batch sieht den gerade per `AddNode` eingefügten
  Knoten `t-1`.

Testaussage unverändert: der Knoten erreicht denselben Endzustand (`t-1`, `kind = Research`, `status = Ready`)
wie vorher, nur über einen laut aktueller Validierung zulässigen Pfad. Die nachfolgenden Assertions
(`completed == 1`, `provider.recorded()` nicht leer, `narrowing.workspace_root`, …) greifen unverändert auf
denselben Store/Plan zu und sind von dieser Umstellung nicht betroffen — keine Revisionsnummer wird im Test
sonst geprüft (`grep -n "revision" harw-cli/src/job_worker.rs` im Testkörper: keine Treffer außerhalb dieser
Stelle).

## Nicht angefasst (bewusst außerhalb des Auftrags)

- `harw-plan/src/lib.rs`-Modulbeispiel (`PlanId::new("p-1")`, Zeilen 32-48): C-PLAN.md nennt zusätzlich
  „lib.rs-Doku-Beispiel auf `PlanId::parse`" als Folgearbeit; mein Auftrag beschränkte sich explizit auf den
  `PlanRevision`-Re-Export. `PlanId::new` bleibt gültig (laut `ids.rs`-Signaturliste UNCHECKED, Entfernung
  erst W12) — keine Kompilierfehler, aber weiterhin offen. Empfehlung: eigener Kurzauftrag.
- `harw-cli/src/job_worker.rs:2916` `PlanId::new("p-test")` in derselben Testfunktion (im `Create`-Aufruf) —
  C-PLAN.md listet separat `job_worker.rs:2825 PlanId::new` unter Folgearbeit für A-MAIN/A-JOBW; mein Auftrag
  nannte nur die `AddNode`/Status-Verletzung. `PlanId::new` bleibt gültig (s.o.) — nicht geändert, um den Diff
  auf den angefragten Befund zu beschränken.
- Sonstige Testfunktionen in `job_worker.rs` mit `PlanId::new`/`PlanAction::Create` (z. B. weitere Fixtures)
  — keine davon nutzt `AddNode` (bestätigt durch den eingangs zitierten `grep`), also nicht vom C-PLAN-Befund
  betroffen.

## BLOCKED-Bewertung

Nicht blockiert. Brief war vollständig: Task 1 deckungsgleich mit C-PLAN.md-Folgearbeit („lib.rs-Owner"),
Task 2 deckungsgleich mit C-PLAN.md-Folgearbeit („A-MAIN/A-JOBW", Testzeile per Grep neu verifiziert und
inhaltlich bestätigt). Fix hält die geforderte Testaussage exakt bei, nutzt den bevorzugten `apply_batch`-Pfad
mit `expected_rev` aus dem Store.
