# Agent-IR v1 — Bestehende Pipeline als expliziter Compiler-Kanal

**Status:** Design-Anker (deskriptiv, kein neues Crate).
**Bindet an:** `agent-definition-dsl.md` §17, `coding-philosophy.md` §6, §7, §11.

Diese Doku macht die **Lowering-Stufen sichtbar**, die in `harw-agent-dsl` bereits latent existieren. Sie ersetzt keine Implementierung — sie benennt.

Absichtlich **kein** `harw-ir`/`harw-lowering`/`harw-codegen`-Crate. Alles bleibt in `harw-agent-dsl`. Der Wert liegt in expliziten Grenzen zwischen den Stufen, nicht in neuen Crates.

---

## 1. Die vier Stufen

```
TOML Source
    │  parse.rs (Frontend / Parser)
    ▼
Raw*Definition  (AST — Frontend-Output, syntax-nahe)
    │  resolve.rs (Semantic Analysis + Lowering)
    ▼
Resolved*Definition  (IR — normalisiert, monomorph, verifiziert)
    │  Consumer (Runtime / Job-Admission / Executor)
    ▼
Runtime Effect  (Backend — Jobs, Tool-Surface, Leases, Model Requests)
```

Alle Vorstufen sind bereits implementiert (Wave G):

- Frontend: `parse.rs::parse_toml`
- AST: `raw::RawAgentDefinition`
- Analysis + Lowering: `resolve.rs::resolve_definition`
- Semantic Verifier: `authority.rs::AuthorityCeiling::is_reduction_of`
- IR: `resolved::ResolvedAgentDefinition` mit `ResolutionTrace` als Provenance

Neu in Wave I:

- AST für Family: `family::RawFamilyDefinition`
- AST für Organization/Clan/Cell: `organization::RawOrganizationDefinition`
- IR für Family: `family::ResolvedFamily`
- IR für Organization: `organization::ResolvedOrganization`

---

## 2. Warum keine eigenen Backend-Crates

Ein Compiler produziert Maschinencode. Harwness produziert **Runtime-Effekte** — und die leben nicht in einem Backend-Crate, sondern in `harw-job-runtime`, `harw-plan`, `harw-tools`, `harw-provider`. Diese Crates sind das Backend.

Der Consumer-Kontrakt ist immer derselbe:

```rust
fn admit_agent(resolved: ResolvedAgentDefinition, org: &ResolvedOrganization) -> JobHandle;
```

Was der Compiler-Vergleich klar macht:

- **AST bleibt syntax-nah.** Keine Cross-Reference-Auflösung, keine Merge-Semantik.
- **IR ist normalisiert und verifiziert.** Nach `resolve_definition` gilt: keine Cycles, keine Authority-Elevation, alle Refs aufgelöst.
- **Consumer sehen nur IR.** Kein Runtime-Konsument darf `RawAgentDefinition` sehen — das ist der Grund, warum `raw` in `harw-agent-dsl` internal bleibt und nur `resolved` exportiert wird.

---

## 3. Optimierungs-Passes (spätere Waves, hier deklariert)

Die folgenden Passes sind sinnvoll — aber **keiner ist Voraussetzung** für ein funktionierendes System. Sie sind Kandidaten für Wave J+.

| Pass | Wirkung |
|---|---|
| `RemoveUnusedTools` | Tools, die keine Consumer im Plan-Graph haben, aus Tool-Surface entfernen |
| `MinimizeContext` | Context-Items, die von keinem Rendering-Pfad referenziert werden, weglassen |
| `CollapseRedundantPolicies` | zwei identische ContextPolicies → gemeinsam nutzen |
| `PartitionWriteSets` | WriteSets orthogonal machen, damit Fanout garantiert disjunkt ist |
| `InsertVerificationBarrier` | vor jedem Merge-Kandidaten einen `harw-plan`-Barrier-Node einsetzen |
| `EnforceToolChoiceReset` | nach jedem Terminal-Return `reset_tool_choice=true` erzwingen |
| `LowerGoalGraphToJobs` | ausdrucksstarke Goal-Graphen zu ausführbaren Job-Sequenzen abflachen |
| `InsertDurabilityBoundaries` | vor jedem State-Effekt einen `persist-before-rerun`-Marker einsetzen |

Der wichtigste Pass für die aktuelle Fanout-Diszplin: **`PartitionWriteSets`** — genau der, den ich beim Wave-Fanout händisch prüfe. Wenn er als Compiler-Pass existiert, verschwindet die manuelle Prüfung.

---

## 4. „Borrow Checker, Harwness Edition"

Wenn die IR ein Feld `write_scope: Vec<PathRule>` pro Job-Template trägt und ein Analyzer über den Job-Graph iteriert:

```
HARW-BORROW-017:
    mutable mutation lease for `harw-tui/src/app.rs`
    is already held by job `input-editor-wiring` (plan revision 42)
    job `chat-scroll-wiring` cannot acquire overlapping write scope
    hint: split plan node or serialize execution
```

Wir haben das bereits als Runtime-Check in `harw-plan/src/admission.rs`. Ein IR-Pass hebt es auf Plan-Compile-Zeit — genau wie ein echter Borrow Checker.

---

## 5. Was diese Doku *nicht* rechtfertigt

- Kein separates `harw-ir`-Crate.
- Kein Multi-Backend-Executor (nicht nötig — die einzelnen Runtime-Crates *sind* die Backends).
- Keine LLVM-Analogie. Kein MLIR. Kein pretty printer für IR.
- Keine SSA-Umformung, keine phi-Nodes, keine dominator tree. Das ist eine Agent-Runtime, kein Optimizer.

Die Klammer ist präzise: **Lowering-Grenzen benennen, damit die 22 DSL-Invarianten (§22 in `agent-definition-dsl.md`) an einer Stelle durchgesetzt werden können.** Nicht mehr, nicht weniger.
