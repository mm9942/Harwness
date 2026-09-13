# harw-plan v1 — First-Class Planning-Tool

**Status:** Design-Anker.
**Bindend:** `coding-philosophy.md` §6 (Planning ist First-Class), §7 (Plan ist Executable Dependency Graph), §8 (WriteSet-Disjunktheit), §15 (Failure/Recovery), §19 (Authority nicht durch Prosa). Zusätzlich `philosophy.md` §5 (Goal ≠ Plan), §6 (Modell besitzt keine Prozesse), §12 (monotone Authority-Reduktion).

Diese Version ist ein **Vertical Slice**: ein neues Crate `harw-plan` mit Kern-Datentypen, In-Memory- und File-Store, Validation und einer minimalen synchronen API. Adapter (Command / Model-Tool / Channel), MCP-Bridge und TUI-Projektion kommen in Folge-Waves — nicht in diesem Slice (coding-philosophy §11).

---

## 1. Verantwortungsbereich

Genau die Semantik aus coding-philosophy.md §6.1:

```
create plan | inspect plan | add or update node | declare dependency |
declare read/write scope | declare contracts | declare acceptance criteria |
mark node ready | mark node blocked | invalidate stale nodes |
record evidence | close or supersede plan revision
```

Ausdrücklich nicht: Job-Admission, Executor-Aufruf, LLM-Interaktion. Das Planning-Tool **beschreibt** Arbeit — es **führt** keine aus.

---

## 2. Kern-Typen (verbindlich)

```rust
// harw-plan/src/types.rs

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use harw_types::{PlanId, TaskId, RevisionId, PathOrSymbol, ContractRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNodeStatus {
    Draft, Ready, InProgress, Blocked, Completed, Superseded, Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    pub description: String,
    pub verification: Vec<VerificationStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerificationStep {
    Command  { cmd: String, expect_exit: i32 },
    Artifact { path: String },
    TraceEvent { name: String },
    Manual   { note: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvalidationCondition {
    ContractChanged(ContractRef),
    UpstreamNodeReopened(TaskId),
    RepoRevisionMoved,
    ManualInvalidate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: String,          // "cargo-test", "clippy", "diff", "trace-span"
    pub locator: String,       // path, url, id
    #[serde(with = "time::serde::rfc3339")]
    pub attached_at: OffsetDateTime,
    pub actor: String,         // "worker-abc", "orchestrator", "human:mia"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanNode {
    pub id: TaskId,
    pub objective: String,
    pub dependencies: Vec<TaskId>,
    pub input_contracts: Vec<ContractRef>,
    pub output_contracts: Vec<ContractRef>,
    pub read_scope: Vec<PathOrSymbol>,
    pub write_scope: Vec<PathOrSymbol>,
    pub forbidden_scope: Vec<PathOrSymbol>,
    pub acceptance_criteria: Vec<Criterion>,
    pub invalidation_conditions: Vec<InvalidationCondition>,
    pub status: PlanNodeStatus,
    pub evidence: Vec<EvidenceRef>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    pub revision: RevisionId,
    pub parent_revision: Option<RevisionId>,
    pub goal_statement: String,
    pub nodes: Vec<PlanNode>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}
```

`PlanId`, `TaskId`, `RevisionId`, `PathOrSymbol`, `ContractRef` leben in `harw-types`. Wenn dort nicht vorhanden: als Newtype in `harw-plan/src/ids.rs` deklarieren mit TODO-Kommentar für spätere Umsiedlung. **Kein Cross-Crate-Editier-Impuls im Skeleton-Fanout.**

---

## 3. Actions

```rust
// harw-plan/src/actions.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PlanAction {
    Create { plan_id: PlanId, goal: String },
    AddNode { node: PlanNode },
    UpdateNode { id: TaskId, objective: Option<String>, /* delta fields */ },
    AddDependency { child: TaskId, parent: TaskId },
    SetStatus { id: TaskId, status: PlanNodeStatus, reason: Option<String> },
    AttachEvidence { id: TaskId, evidence: EvidenceRef },
    Invalidate { ids: Vec<TaskId>, condition: InvalidationCondition },
    Supersede { new_parent_revision: RevisionId },
    Inspect,   // read-only
}
```

Jede Mutation wird über `PlanStore::apply(action) -> PlanResult<PlanEvent>` gefahren. Reine Wert-Actions; kein `&mut PlanNode` in der API-Signatur.

---

## 4. Store-Trait und Referenz-Implementierungen

```rust
// harw-plan/src/store.rs

pub trait PlanStore: Send + Sync {
    fn current(&self) -> PlanResult<Plan>;
    fn revision(&self) -> RevisionId;
    fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent>;
    fn history(&self, since: Option<RevisionId>) -> PlanResult<Vec<PlanEvent>>;
}
```

Zwei Implementierungen im Slice:

- `InMemoryPlanStore` — für Tests, RwLock-gebunden.
- `FilePlanStore` — persistent unter `<root>/plans/<plan_id>/rev-<n>.json` plus `history.jsonl` (append-only Event-Log).

Beide sind `Send + Sync`.

---

## 5. Validation (coding-philosophy §6.3)

**Datei:** `harw-plan/src/validate.rs`.

Vor `apply` muss `validate(&plan, &action)` bestehen. Verbindliche Regeln:

- Referenzierte Nodes existieren.
- `AddDependency` erzeugt keinen Zyklus (DFS im Dependency-Graph).
- `SetStatus` folgt einer legalen Übergangs-Matrix (Draft→Ready→InProgress→Completed/Blocked; Superseded ist terminal).
- `SetStatus(Completed)` verlangt mindestens ein `EvidenceRef`.
- `AddNode` erzwingt: `forbidden_scope ∩ write_scope == ∅`.
- `AddNode` erzwingt: `write_scope` disjunkt zu allen aktiven `write_scope` von Nodes im Status `Ready | InProgress` (coding-philosophy §8).
- `Supersede` erhöht `revision` monoton.
- `Invalidate` legaler nur für Nodes ≠ `Completed` (bereits committete Arbeit wird nur via Supersede ungültig).
- `AttachEvidence` ist idempotent auf `(id, kind, locator)`.

Fehler in `PlanError` mit Varianten `CycleDetected`, `IllegalTransition`, `ScopeConflict`, `NodeMissing`, `RevisionRegressed`, `EvidenceMissing`, `ForbiddenScopeOverlap`, `Io`, `Serde`.

---

## 6. Konfiguration (coding-philosophy §6.4)

```toml
[tools.plan]
enabled = true
persist = true
require_for_complex_work = true
validate_dependency_cycles = true
validate_write_conflicts = true
max_nodes = 256
```

`enabled = false` muss die Registrierung **komplett** entfernen. Im Slice:

- `PlanToolConfig` struct mit `#[serde(default)]` auf allen Feldern; Default = disabled außer für Tests.
- `PlanStore` wird nur konstruiert, wenn `PlanToolConfig::is_enabled()`.
- Wenn deaktiviert: **kein** `pub use` und **kein** Registry-Eintrag. Der Consumer bekommt zur Compile-Zeit einen `Option<Arc<dyn PlanStore>>` oder gar keine Referenz.

---

## 7. Authority (coding-philosophy §19)

Untrusted `PlanAction`-Inputs dürfen nie:

- `actor` selbst wählen (der Runtime setzt den Actor auf Basis der Session).
- `plan_id` außerhalb der eigenen Session ändern.
- `revision` direkt setzen (nur `apply` erhöht sie).
- `created_at`/`updated_at` überschreiben (Runtime-Timestamps).

Diese Regeln werden im `apply` durch das Verwerfen entsprechender Payload-Felder erzwungen — nicht durch Prompt-Vertrauen.

---

## 8. Vertical-Slice-Grenzen (coding-philosophy §11)

**Im Slice enthalten:**

- Alle Typen aus §2.
- `PlanAction` (§3).
- `PlanStore`-Trait mit `InMemoryPlanStore` + `FilePlanStore`.
- Vollständige Validation (§5) mit Tests pro Regel.
- `PlanToolConfig` mit `enabled = false`-Default (§6).

**Nicht im Slice:**

- Adapter (Command / Model-Tool / Channel) — Folge-Wave.
- MCP-Bridging — Folge-Wave.
- TUI-Projektion — Folge-Wave.
- Cross-Crate-Konsumenten (`harw-job-runtime`, `harw-tools`) — Folge-Wave.

Der Slice ist selbst-verifizierend über `cargo test -p harw-plan`.

---

## 9. Fanout-Aufteilung

Ein einzelner Focused-Coding-Task-Agent bekommt das komplette Slice-Crate. Kein Sub-Fanout, weil:

- die Module (`types.rs`, `actions.rs`, `store.rs`, `file_store.rs`, `validate.rs`, `error.rs`, `config.rs`, `lib.rs`) sich gegenseitig referenzieren — Interface-Drift zwischen parallelen Workern wäre teuer;
- der ganze Slice ~800 LOC — bounded genug für einen Worker;
- coding-philosophy §13: „Tightly coupled work should remain under one atomic owner".

**Der Worker bekommt diesen Design-Doc als Vertrag und hat keine architektonische Freiheit.**
