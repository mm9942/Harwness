# Memory v2 — STM/LTM, Self-Learning, Continuous Improvement

**Status:** Design-Anker (Vertrag für Fanout).
**Bindend:** `philosophy.md` §3, §4, §5, §7 sowie §16 (Invarianten 7, 8, 10, 15).
**Inspirationen:** codex-rs `context-fragments`/`models-manager`, Hermes `~/.hermes/memories/` mit `MEMORY.md` + Lock-File, OpenClaw `context-engine`/self-improving Skill-Bundle (HOT/WARM/COLD, Heartbeat).

---

## 1. Ziel

Eine tokensparsame Memory-Schicht, die

1. **Short-Term Memory (STM)** — flüchtigen Turn-Kontext im Prozess hält (kein I/O),
2. **Long-Term Memory (LTM)** — persistente Tiers HOT/WARM/COLD nach philosophy.md §3 verwaltet,
3. **Self-Learning** — Korrekturen, Reflexionen und Muster-Kandidaten aus Signalen extrahiert,
4. **Continuous Improvement (Heartbeat)** — deterministisch promotiert/demotiert/archiviert,

und dabei die **kleinste hoch-signifikante Tokenmenge pro Turn** liefert (philosophy.md §3, „Kontext muss konstruiert werden, nicht akkumulieren").

Keine LLM-Aufrufe im Hot-Path. Alle Regeln sind pure Rust.

---

## 2. Layer-Übersicht

```
+---------------------------------------------------------------+
|  Turn Context Assembly (harw-core, außerhalb dieses Crates)   |
|      ↑ pulls (bounded)                                        |
+---------------------------------------------------------------+
   |                                    |
   |  hot() + recall(namespace,kw)      |  stm.snapshot()
   ↓                                    ↓
+-------------------------+    +----------------------------+
|  LTM (FileMemoryStore)  |    |  STM (in-process)          |
|  HOT ≤100 lines         |    |  Ring-Buffer, N=32 default |
|  WARM per namespace     |    |  Salience-Score, TTL       |
|  COLD archive           |    |  Send + Sync via RwLock    |
+-------------------------+    +----------------------------+
        ↑                             ↑
        |  maintain() (heartbeat)     |  push(...) pro Turn
+---------------------------------------------------------------+
|  Signals (append-only JSONL)                                  |
|  correction | reflection | pattern_hint                        |
+---------------------------------------------------------------+
        ↑
        |  detect + record aus dem laufenden Turn
```

---

## 3. Short-Term Memory (STM)

**Datei:** `harw-memory/src/short_term.rs` (neu).

- Reiner In-Process-Speicher — kein Filesystem-I/O.
- `struct ShortTermMemory { inner: RwLock<Inner> }` (`Send + Sync`).
- `Inner` enthält:
  - `VecDeque<StmEntry>` mit `capacity` (Default 32),
  - `token_budget: usize` (Default 2048 Token, Schätzung `content.len() / 4`),
  - `session_id: String`.
- `StmEntry { at: Timestamp, role: StmRole, salience: u8 /*0..=100*/, content: String }`.
- `StmRole { User, Assistant, Tool, System }`.
- API:
  - `pub fn new(session_id: impl Into<String>, capacity: usize, token_budget: usize) -> Self`
  - `pub fn push(&self, role: StmRole, salience: u8, content: impl Into<String>)`
  - `pub fn snapshot(&self) -> Vec<StmEntry>` (klont).
  - `pub fn render(&self, max_tokens: usize) -> String` — komprimiert von hinten (jüngste zuerst), fällt unter `max_tokens` durch Weglassen niedrigster Salience.
  - `pub fn clear(&self)`.
- **Verdrängung:** Wenn `capacity` überschritten → pop_front nach `salience` (kleinster raus), bei Gleichstand ältestes. Wenn Token-Budget überschritten → gleicher Algorithmus bis unter Budget.
- **Kein Alloc-Sturm:** `render` benutzt `String::with_capacity(max_tokens * 4)`.

Tests (mind. 6):
- Cap-Verhalten,
- Budget-Verhalten,
- Verdrängung nach Salience,
- Send+Sync-Compile-Test,
- `render` respektiert `max_tokens`,
- `clear` leert alles.

**Bindung an philosophy.md §3:** STM ist genau der „Working Context — Daten für den unmittelbar aktuellen Turn".

---

## 4. Long-Term Memory (LTM)

**Bereits vorhanden:** `harw-memory/src/{store.rs,file_store.rs,types.rs,workflow.rs}`.
Contract bleibt stabil. `Tier::Hot::max_lines() = 100`, `Warm = 200`, `Cold = None`.

Erweiterung dieser Doku: STM speist LTM **nicht** direkt. Signale werden weiterhin explizit via `store.record(Signal::…)` erzeugt (aus TUI/Core), oder aus STM-Reflection promoviert.

---

## 5. Self-Learning

**Datei:** `harw-memory/src/learning.rs` (neu). Ergänzt `detect.rs`.

- `pub fn score_correction(text: &str) -> u8` — 0..=100 Score anhand Regex-loser Keyword-Menge (nutze `detect::detect_correction`).
- `struct PatternCounter` — flache HashMap-Wrapper `key -> (count, first_seen, last_seen)`. Persistenz in `signals/patterns.json`.
  - `pub fn observe(&mut self, key: &str, now: Timestamp)`
  - `pub fn is_promotable(&self, key: &str, now: Timestamp, window_days: i64, threshold: u32) -> bool`
    - Default `window_days = 7`, `threshold = 3`.
  - `pub fn prune(&mut self, now: Timestamp, max_age_days: i64)`
  - Serialisierbar via serde (JSON).
- `pub struct LessonRule { pub keyword: &'static str, pub weight: u8 }` — statische Tabelle deutscher + englischer Korrektur-Trigger (mind. 20 Einträge, siehe self-improving/SKILL.md „Learning Signals").
- Integration: `FileMemoryStore::maintain()` ruft `learning::PatternCounter::load_or_default(root)` und promoviert `PatternHint`-Signale nach WARM, wenn `is_promotable == true`.

Tests (mind. 6):
- Zähler-Increment,
- Fenster-Pruning,
- Promotion-Schwelle exakt bei 3,
- JSON-Roundtrip,
- Score-Werte für Korrektur-Beispiele,
- Store-Roundtrip (leerer Zustand).

**Bindung an philosophy.md §16 Invariante 8:** „Memory ist eine Promotion-Pipeline, kein unkontrolliertes Langzeit-Transcript."

---

## 6. Continuous Improvement — Heartbeat

**Datei:** `harw-memory/src/heartbeat.rs` (neu).

- `pub struct Heartbeat<'a, M: Memory> { store: &'a M, clock: fn() -> Timestamp }`.
- Regeln (aus self-improving/SKILL.md „Automatic Promotion/Demotion"):
  - PatternHint 3× in 7d → nach WARM (`domain/<key>.md`).
  - HOT-Eintrag ungenutzt seit 30d → nach WARM (`domain/inactive.md`).
  - WARM-Eintrag ungenutzt seit 90d → nach COLD.
  - HOT-Overflow (>100 Zeilen) → älteste per `last_used` → WARM.
- Wrapper-Funktion `pub fn tick<M: Memory>(store: &M, now: Timestamp) -> MemoryResult<HeartbeatReport>` — idempotent.
- `HeartbeatReport { promoted: usize, demoted: usize, archived: usize, hot_lines_after: usize }`.
- Nutzt bestehende `WorkflowMarker`/`WorkflowStep`-Statemachine als atomaren Rahmen (vgl. philosophy.md §4).

Tests (mind. 5):
- Promotion bei 3×,
- Demotion HOT→WARM bei Alter,
- Archivierung WARM→COLD bei Alter,
- HOT-Overflow-Trimming,
- Idempotenz zweier aufeinander folgender `tick`s bei stabiler Uhr.

---

## 7. Kosten- und Ressourcenbudget

| Aspekt | Wert | Herkunft |
|---|---|---|
| HOT-Tokens/Turn | ≤ ~1 500 (≈100 Zeilen × 15 Tokens) | philosophy.md §3 |
| STM-Tokens/Turn | ≤ 2 048 (Default konfigurierbar) | design |
| WARM-Load/Turn | genau 1 Namespace (Namespace + Keyword-Match) | philosophy.md §3 |
| I/O pro Turn | 1× `hot()` + 1× `recall()` | design |
| I/O pro Heartbeat | O(HOT+WARM), append-only | philosophy.md §4 |
| Lock-Contention | RwLock (STM), Datei-Locks (LTM) | bestehend |
| LLM-Aufrufe | 0 in Memory-Layer | bindend |

---

## 8. Datei-Layout LTM

```
<root>/
  HOT.md
  workflow.json
  signals/
    corrections.jsonl
    reflections.jsonl
    patterns.jsonl
    patterns.json       # PatternCounter-State (learning.rs)
  warm/
    domain/<key>.md
    project/<name>.md
  cold/
    <namespace>.md
```

---

## 9. Fanout-Verantwortungen (verbindlich)

| Datei | Owner-Agent | Acceptance-Command |
|---|---|---|
| `harw-memory/src/short_term.rs` (neu) | Agent-A | `cargo test -p harw-memory --lib short_term` |
| `harw-memory/src/heartbeat.rs` (neu) | Agent-B | `cargo test -p harw-memory --lib heartbeat` |
| `harw-memory/src/learning.rs` (neu) | Agent-C | `cargo test -p harw-memory --lib learning` |
| `harw-model-catalog/src/providers.toml` + `embedded.rs` (edit) | Agent-D | `cargo test -p harw-model-catalog` |
| `harw-model-catalog/src/behavior.rs` (neu) | Agent-E | `cargo test -p harw-model-catalog --lib behavior` |

Jeder Agent muss:
- `lib.rs` um `pub mod <neuer_modul>;` erweitern (soweit vorhanden),
- volle `///` und `//!` Doku,
- keine `unwrap()`/`expect()` außerhalb Tests,
- keine `anyhow`/`thiserror`,
- Fehler in bestehende `MemoryError`/`CatalogError` einreihen,
- keine externen Netzwerkaufrufe.
