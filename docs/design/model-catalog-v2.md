# Model Catalog v2.1 — Vier-Schichten-Architektur

**Status:** Design-Anker (Vertrag für Fanout).
**Bindend:** `philosophy.md` §2, §6, §12, §16 (Invariante 6 „Provider-Adapter müssen semantisch verlustfrei statt nur syntaktisch einheitlich sein").
**Inspirationen:** codex-rs `model-provider`, Hermes `provider_models_cache.json`, OpenClaw `context-engine/registry.ts`.

**Grundprinzip:** Der Modellkatalog ist keine Provider-Liste, sondern vier klar getrennte Konzepte, die nur über stabile Interfaces zusammenhängen. Kein Modell wird auf den kleinsten gemeinsamen Nenner reduziert. Marketing-Behauptungen und gemessenes Verhalten leben in verschiedenen Typen.

---

## 1. Die vier Schichten

```
Layer 1  Provider              → Protokoll-Adapter (URL, Auth, API-Familie)
Layer 2  ModelDescriptor       → Deklarierte technische Fähigkeiten
Layer 3  ModelRuntimeProfile   → Wie das Harness das Modell betreibt (Policies)
Layer 4  ObservedModelBehavior → Empirisch gemessenes Verhalten (Evidenz)
```

Regeln:

- Layer 1 ist ein reiner **Protokoll-Adapter**. Kein Verhalten, keine Modellannahmen.
- Layer 2 sagt, was ein Provider **behauptet**.
- Layer 3 sagt, was das Harness **entscheidet**, unabhängig davon, was der Provider behauptet.
- Layer 4 wird nur aus **realen Harness-Runs** befüllt. Beim Bootstrap steht dort ein konservativer Default mit `updated_at = None`.

**Untrusted Provider-Metadaten dürfen Authority niemals erweitern** (philosophy.md §12). Layer-2-Werte sind Hinweise; Layer-3-Policies sind Runtime-Gesetz.

---

## 2. Layer 1 — Provider (bestehend, kleine Erweiterung)

**Datei:** `harw-model-catalog/src/spec.rs` (bestehend), `providers.toml` (bestehend).

**Fanout-Task E:** Erweitere `providers.toml` um die unten gelisteten Provider und aktualisiere Modell-Listen bestehender Provider. Keine Änderung an `spec.rs`-Typen. Neue Tests in `embedded.rs` (`#[cfg(test)] mod tests`).

Neue Provider (`openai-chat`-kompatibel, sofern nicht anders vermerkt):

| id | name | base_url | env_vars | default_model | models |
|---|---|---|---|---|---|
| `zai` | Z.AI (Zhipu) | `https://api.z.ai/api/paas/v4` | `ZAI_API_KEY`, `ZHIPU_API_KEY` | `glm-4.6` | `glm-4.6`, `glm-4.5`, `glm-4.5-air`, `glm-4-flash` |
| `moonshot` | Moonshot (Kimi) | `https://api.moonshot.ai/v1` | `MOONSHOT_API_KEY`, `KIMI_API_KEY` | `kimi-k2-turbo-preview` | `kimi-k2-turbo-preview`, `kimi-k2-0905-preview`, `moonshot-v1-128k` |
| `dashscope` | Alibaba DashScope (Qwen) | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | `DASHSCOPE_API_KEY`, `QWEN_API_KEY` | `qwen3-max` | `qwen3-max`, `qwen3-coder-plus`, `qwen2.5-max`, `qwen2.5-coder-32b-instruct` |
| `cerebras` | Cerebras | `https://api.cerebras.ai/v1` | `CEREBRAS_API_KEY` | — | `llama-4-scout-17b-16e-instruct`, `llama3.3-70b` |
| `gemini` | Google Gemini | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY`, `GOOGLE_API_KEY` | — | `gemini-2.5-pro`, `gemini-2.5-flash` |

Bestehende Provider aktualisieren (Modell-Listen und Defaults):

- `openai`: `models = ["gpt-5-fable", "gpt-5", "gpt-4o", "gpt-4o-mini", "o4-mini", "o3-mini"]`, `default_model = "gpt-5"`.
- `anthropic`: `models = ["claude-opus-4-8", "claude-sonnet-5", "claude-haiku-4-5-20251001", "claude-fable-5"]`.
- `xai`: `models = ["grok-4.5", "grok-4"]`, `default_model = "grok-4.5"`, `featured = true`.
- `mistral`: `models = ["mistral-large-2411", "mistral-small-2409", "codestral-2405"]`, `default_model = "mistral-large-2411"`, `featured = true`.
- `groq`: `models = ["llama-3.3-70b-versatile", "llama-3.1-70b-versatile", "moonshotai/kimi-k2-instruct", "mixtral-8x7b-32768"]`.
- `together`: `models = ["meta-llama/Llama-3.3-70B-Instruct-Turbo", "Qwen/Qwen2.5-72B-Instruct-Turbo", "deepseek-ai/DeepSeek-V3"]`.
- `fireworks`: `models = ["accounts/fireworks/models/llama-v3p3-70b-instruct", "accounts/fireworks/models/qwen2p5-72b-instruct", "accounts/fireworks/models/deepseek-v3"]`.
- `deepseek`: `models = ["deepseek-chat", "deepseek-reasoner"]`, `default_model = "deepseek-chat"`, `featured = true`.
- `deepinfra`: `models = ["meta-llama/Meta-Llama-3.3-70B-Instruct", "Qwen/Qwen2.5-72B-Instruct", "deepseek-ai/DeepSeek-V3"]`.

Test-Pflicht: Für **jeden neuen Provider** eine Assertion `assert!(cat.iter().any(|p| p.id == "<id>"))`.

---

## 3. Layer 2 — `ModelDescriptor`

**Datei:** `harw-model-catalog/src/descriptor.rs` (**neu**, Fanout-Task F).

Deklarierte technische Fähigkeiten. Alles, was ein Provider über sein Modell *sagt*.

```rust
use serde::{Deserialize, Serialize};

pub type TokenCount = u32;
pub type ProviderId = String;
pub type ModelId    = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality { Text, Image, Audio, Video, Pdf }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModalitySet(pub Vec<Modality>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallingSupport { None, Basic, Parallel, Native }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputSupport { None, JsonMode, JsonSchema }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSupport { None, Effort, Trace }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCachingSupport { None, Implicit, Explicit }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamingSupport { None, ServerSent }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentFeatureSet {
    pub computer_use: bool,
    pub code_execution: bool,
    pub built_in_search: bool,
    pub file_search: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub tool_calling: ToolCallingSupport,
    pub parallel_tools: bool,
    pub structured_output: StructuredOutputSupport,
    pub reasoning: ReasoningSupport,
    pub prompt_caching: PromptCachingSupport,
    pub streaming: StreamingSupport,
    pub image_input: bool,
    pub native_agent_features: AgentFeatureSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelLifecycle { Preview, Ga, Deprecated, Retired }

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_mtoken_usd:  f32,
    pub output_per_mtoken_usd: f32,
    pub cached_input_per_mtoken_usd: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub provider: ProviderId,
    pub model:    ModelId,
    pub context_window: TokenCount,
    pub max_output_tokens: Option<TokenCount>,
    pub modalities: ModalitySet,
    pub capabilities: ModelCapabilities,
    pub pricing: Option<Pricing>,
    pub lifecycle: ModelLifecycle,
}
```

Zusätzlich: `pub fn bootstrap_descriptors() -> Vec<ModelDescriptor>` mit mindestens 15 kuratierten Einträgen für:

- `openai:gpt-5`, `openai:gpt-4o`, `openai:o4-mini`,
- `anthropic:claude-opus-4-8`, `anthropic:claude-sonnet-5`, `anthropic:claude-haiku-4-5-20251001`,
- `zai:glm-4.6`,
- `moonshot:kimi-k2-turbo-preview`,
- `dashscope:qwen3-max`, `dashscope:qwen3-coder-plus`,
- `mistral:mistral-large-2411`,
- `xai:grok-4.5`,
- `deepseek:deepseek-chat`, `deepseek:deepseek-reasoner`,
- `groq:llama-3.3-70b-versatile`.

Werte konservativ; jede Zeile mit `///`-Quellenkommentar (Datum, Herkunftsdomäne).

Tests (mind. 6): Serde-Roundtrip, `bootstrap_descriptors().len() >= 15`, jeder Descriptor hat `!provider.is_empty() && !model.is_empty()`, Modalities enthalten mindestens `Text`, Enum-kebab-serde für alle Enums, Context-Window > 0.

---

## 4. Layer 3 — `ModelRuntimeProfile`

**Datei:** `harw-model-catalog/src/runtime.rs` (**neu**, Fanout-Task G).

Wie das Harness ein Modell **betreiben** soll. Unabhängig von Provider-Behauptungen.

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskShape { Micro, Small, Medium, Large, RepositoryScale }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPolicy {
    /// Aggressive Selektion, häufige Kompaktierung, kleine Turn-Pakete.
    TightSelect,
    /// Ausgewogenes Fenster mit thematischem Bündeln.
    Balanced,
    /// Große kohärente Fenster, seltene Kompaktierung.
    BroadContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPolicy { Never, OnPressure, Periodic, Aggressive }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationPolicy { Forbidden, Cautious, Standard, Bold }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_retries: u8,
    pub backoff_ms:  u32,
    pub retry_on_tool_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRuntimeProfile {
    pub context_policy:      ContextPolicy,
    pub compaction_policy:   CompactionPolicy,
    pub delegation_policy:   DelegationPolicy,
    pub retry_policy:        RetryPolicy,
    pub preferred_task_shape: TaskShape,
    pub max_parallel_tools:  u8,
    pub max_child_fanout:    u8,
}

/// Konservativer Standard, wenn kein Profil bekannt ist.
pub const DEFAULT_PROFILE: ModelRuntimeProfile = ModelRuntimeProfile {
    context_policy: ContextPolicy::TightSelect,
    compaction_policy: CompactionPolicy::OnPressure,
    delegation_policy: DelegationPolicy::Cautious,
    retry_policy: RetryPolicy { max_retries: 2, backoff_ms: 500, retry_on_tool_error: false },
    preferred_task_shape: TaskShape::Small,
    max_parallel_tools: 2,
    max_child_fanout: 2,
};

pub fn profile_for(model: &str) -> ModelRuntimeProfile { /* match, siehe unten */ }
```

`profile_for` hardcoded für dieselben 15 Modelle wie `bootstrap_descriptors`. Werte begründen sich aus philosophy.md §2 (Rolle, Granularität, Parallelität).

Beispiele (Auszug — verbindlich für den Agent):

- `claude-opus-4-8`: `Balanced` / `OnPressure` / `Standard` / retry 3/500ms/false / `Medium` / 4 / 4.
- `claude-haiku-4-5-20251001`: `TightSelect` / `Aggressive` / `Cautious` / retry 2/300ms/true / `Small` / 3 / 3.
- `gpt-5`: `BroadContext` / `OnPressure` / `Standard` / retry 3/500ms/false / `Large` / 4 / 4.
- `glm-4.6`: `BroadContext` / `Periodic` / `Standard` / retry 2/500ms/false / `RepositoryScale` / 3 / 3 (1M-Kontext, philosophy.md §2).
- `kimi-k2-turbo-preview`: `Balanced` / `OnPressure` / `Bold` / retry 3/400ms/true / `Medium` / 6 / 4 (agentisch, philosophy.md §2).
- `deepseek-reasoner`: `TightSelect` / `Never` / `Forbidden` / retry 1/1000ms/false / `Small` / 1 / 0 (Reasoning-Mode).

Tests (mind. 5): `profile_for("unknown") == DEFAULT_PROFILE`, alle 15 kuratierten Modelle liefern != DEFAULT, Serde-Roundtrip, Enum-kebab-serde, RetryPolicy sinnvoll (max_retries ≤ 10, backoff_ms ≤ 60_000).

---

## 5. Layer 4 — `ObservedModelBehavior`

**Datei:** `harw-model-catalog/src/observed.rs` (**neu**, Fanout-Task H).

Empirische Messwerte aus Harness-Runs. Beim Bootstrap: konservative Defaults, `updated_at = None`, `evidence = []`.

```rust
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub type EvaluationRunId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Score(u8);

impl Score {
    pub const fn new(v: u8) -> Option<Self> { if v <= 100 { Some(Self(v)) } else { None } }
    pub const fn clamp(v: u8) -> Self { Self(if v > 100 { 100 } else { v }) }
    pub const fn get(self) -> u8 { self.0 }
    pub const ZERO: Self = Self(0);
    pub const HALF: Self = Self(50);
    pub const FULL: Self = Self(100);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedModelBehavior {
    pub provider: String,
    pub model: String,
    pub tool_schema_reliability:  Score,
    pub long_context_retention:   Score,
    pub delegation_discipline:    Score,
    pub recovery_after_tool_error: Score,
    pub completion_calibration:   Score,
    pub compaction_resilience:    Score,
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
    pub evidence: Vec<EvaluationRunId>,
}

impl ObservedModelBehavior {
    /// Konservativer Bootstrap ohne Evidenz.
    pub fn bootstrap(provider: &str, model: &str) -> Self { /* alle Scores = Score::HALF */ }
}

pub fn bootstrap_observations() -> Vec<ObservedModelBehavior> { /* dieselben 15 IDs */ }
```

Tests (mind. 5): Score-Clamp, Score::new bei 101 = None, Serde-Roundtrip, bootstrap ohne evidence, alle 15 IDs vorhanden.

---

## 6. Router — Rollenbasierte Auswahl

**Datei:** `harw-model-catalog/src/router.rs` (**neu**, Fanout-Task I).

Kombiniert Descriptor + RuntimeProfile + Observation für Rolle-basierte Modellauswahl.

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Orchestrator,
    RepositoryWorker,
    FocusedCodingWorker,
    Verifier,
    Scout,
}

/// Kandidat mit vollständigem Kontext.
#[derive(Debug, Clone)]
pub struct Candidate<'a> {
    pub descriptor: &'a crate::descriptor::ModelDescriptor,
    pub profile:    &'a crate::runtime::ModelRuntimeProfile,
    pub observed:   &'a crate::observed::ObservedModelBehavior,
}

/// Rangfolge deterministisch (stabil bei Gleichstand: alphabetisch nach model_id).
pub fn rank<'a>(role: ModelRole, candidates: &'a [Candidate<'a>]) -> Vec<&'a Candidate<'a>> { /* ... */ }

/// Bequemer Auswahlpfad — bester Kandidat oder None.
pub fn pick<'a>(role: ModelRole, candidates: &'a [Candidate<'a>]) -> Option<&'a Candidate<'a>> {
    rank(role, candidates).into_iter().next()
}
```

Score-Formel (in 0..=100, verbindlich):

- `Orchestrator` = 0.4·delegation_discipline + 0.3·long_context_retention + 0.3·tool_schema_reliability
- `RepositoryWorker` = 0.5·long_context_retention + 0.3·compaction_resilience + 0.2·tool_schema_reliability
- `FocusedCodingWorker` = 0.5·tool_schema_reliability + 0.3·completion_calibration + 0.2·recovery_after_tool_error
- `Verifier` = 0.5·completion_calibration + 0.5·tool_schema_reliability
- `Scout` = 0.5·tool_schema_reliability + 0.5·recovery_after_tool_error

Tests (mind. 5): leere Liste → None, deterministische Order bei Gleichstand, jede Rolle liefert nicht-leere Rangfolge bei nicht-leerer Kandidatenliste, Ordering monoton, Serde-Roundtrip von `ModelRole`.

---

## 7. Fanout — WriteSet-Ownership (verbindlich)

| Task | Owner-Agent | Datei | WriteSet-Disjunkt? |
|---|---|---|---|
| A | focused-coding-task | `harw-memory/src/short_term.rs` (neu) | ja |
| B | focused-coding-task | `harw-memory/src/heartbeat.rs` (neu) | ja |
| C | focused-coding-task | `harw-memory/src/learning.rs` (neu) | ja |
| E | focused-coding-task | `harw-model-catalog/src/providers.toml` + range in `embedded.rs` (nur `#[cfg(test)] mod tests`-Block ergänzen) | ja |
| F | focused-coding-task | `harw-model-catalog/src/descriptor.rs` (neu) | ja |
| G | focused-coding-task | `harw-model-catalog/src/runtime.rs` (neu) | ja |
| H | focused-coding-task | `harw-model-catalog/src/observed.rs` (neu) | ja |
| I | focused-coding-task | `harw-model-catalog/src/router.rs` (neu) | ja |

**Barrier (Orchestrator):** Nach Rückkehr aller Worker fügt der Main-Agent `pub mod <neu>;` in `harw-memory/src/lib.rs` und `harw-model-catalog/src/lib.rs` ein und läuft workspace-weite Builds.

**Verbotene Pfade für alle Worker:** jede Datei außerhalb der eigenen Zeile in der Tabelle. Insbesondere `lib.rs` beider Crates. `error.rs` beider Crates (falls neue Fehlerklassen nötig sind: BLOCKED-JSON zurückgeben).

**Acceptance-Command je Worker:** `cargo test -p <crate> --lib <modul>` (nur eigener Modul-Test, nicht das ganze Crate — vermeidet False Positives durch unfertige Sibling-Module). Der Orchestrator läuft danach `cargo test -p <crate>` und `cargo clippy -p <crate> --tests -- -D warnings`.

---

## 8. Nicht-Ziele dieses Fanouts

- Kein `pub mod`-Wiring in `lib.rs` durch Worker.
- Keine cross-crate-Integration (`harw-provider`, `harw-core`) — kommt in einer späteren Welle.
- Kein Netzwerkzugriff, kein `models-dev`-Enrichment in diesem Fanout.
- Keine Änderung an `error.rs` beider Crates.
- Keine Live-Benchmarks — Bootstrap-Werte sind kuratiert und dokumentiert.
