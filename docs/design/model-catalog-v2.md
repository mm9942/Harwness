# Model Catalog — Four-Layer Architecture

> Status: implemented · Last reviewed: 2026-09-24

**Base principle:** the model catalog is not a provider list but four clearly
separated concepts, connected only through stable interfaces. No model is
reduced to a lowest common denominator; marketing claims and measured
behavior live in different types.

## 1. The four layers

```
Layer 1  Provider              -> protocol adapter (URL, auth, API family)
Layer 2  ModelDescriptor       -> declared technical capabilities
Layer 3  ModelRuntimeProfile   -> how the harness operates the model (policy)
Layer 4  ObservedModelBehavior -> empirically measured behavior (evidence)
```

Rules:

- Layer 1 is a pure **protocol adapter**. No behavior, no model assumptions.
- Layer 2 says what a provider **claims**.
- Layer 3 says what the harness **decides**, independent of what the
  provider claims.
- Layer 4 is only ever populated from **real harness runs**. At bootstrap it
  holds a conservative default with `updated_at = None`.

Untrusted provider metadata must never widen authority. Layer-2 values are
hints; Layer-3 policies are runtime law.

## 2. Layer 1 — Provider

**File:** `harw-model-catalog/src/spec.rs`, `providers.toml`.

The provider table now carries 24 providers, including `zai` (Zhipu GLM),
`moonshot` (Kimi), `dashscope` (Qwen), `cerebras`, and `gemini` alongside the
earlier OpenAI-compatible, Anthropic, xAI, Mistral, Groq, Together, Fireworks,
DeepSeek, and DeepInfra entries, plus local/custom endpoints (`ollama`,
`lmstudio`, `vllm`, `custom`, `foundry`).

## 3. Layer 2 — `ModelDescriptor`

**File:** `harw-model-catalog/src/descriptor.rs`.

Declared technical capabilities — everything a provider *states* about its
model.

```rust
pub enum Modality { Text, Image, Audio, Video, Pdf }
pub struct ModalitySet(pub Vec<Modality>);

pub enum ToolCallingSupport { None, Basic, Parallel, Native }
pub enum StructuredOutputSupport { None, JsonMode, JsonSchema }
pub enum ReasoningSupport { None, Effort, Trace }
pub enum PromptCachingSupport { None, Implicit, Explicit }
pub enum StreamingSupport { None, ServerSent }

pub struct AgentFeatureSet {
    pub computer_use: bool,
    pub code_execution: bool,
    pub built_in_search: bool,
    pub file_search: bool,
}

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

pub enum ModelLifecycle { Preview, Ga, Deprecated, Retired }

pub struct Pricing {
    pub input_per_mtoken_usd: f32,
    pub output_per_mtoken_usd: f32,
    pub cached_input_per_mtoken_usd: Option<f32>,
}

pub struct ModelDescriptor {
    pub provider: ProviderId,
    pub model: ModelId,
    pub context_window: TokenCount,
    pub max_output_tokens: Option<TokenCount>,
    pub modalities: ModalitySet,
    pub capabilities: ModelCapabilities,
    pub pricing: Option<Pricing>,
    pub lifecycle: ModelLifecycle,
}
```

A curated set of `bootstrap_descriptors()` seeds the catalog for the major
models across providers; each entry carries a source comment (date, origin
domain).

## 4. Layer 3 — `ModelRuntimeProfile`

**File:** `harw-model-catalog/src/runtime.rs`.

How the harness should **operate** a model, independent of what the provider
claims.

```rust
pub enum TaskShape { Micro, Small, Medium, Large, RepositoryScale }

pub enum ContextPolicy {
    /// Aggressive selection, frequent compaction, small turn packets.
    TightSelect,
    /// Balanced window with topical bundling.
    Balanced,
    /// Large coherent windows, rare compaction.
    BroadContext,
}

pub enum CompactionPolicy { Never, OnPressure, Periodic, Aggressive }
pub enum DelegationPolicy { Forbidden, Cautious, Standard, Bold }

pub struct RetryPolicy {
    pub max_retries: u8,
    pub backoff_ms: u32,
    pub retry_on_tool_error: bool,
}

pub struct ModelRuntimeProfile {
    pub context_policy: ContextPolicy,
    pub compaction_policy: CompactionPolicy,
    pub delegation_policy: DelegationPolicy,
    pub retry_policy: RetryPolicy,
    pub preferred_task_shape: TaskShape,
    pub max_parallel_tools: u8,
    pub max_child_fanout: u8,
}

/// Conservative default when no profile is known.
pub const DEFAULT_PROFILE: ModelRuntimeProfile = /* TightSelect / OnPressure / Cautious / ... */;

pub fn profile_for(model: &str) -> ModelRuntimeProfile { /* curated per-model table */ }
```

`profile_for` is hardcoded for the curated model set: reasoning-focused
models get `TightSelect`/`Forbidden`/single-threaded retries, large-context
agentic models get `BroadContext`/`Bold` with higher fanout, and so on —
tuned per model, not per provider.

## 5. Layer 4 — `ObservedModelBehavior`

**File:** `harw-model-catalog/src/observed.rs`.

Empirical measurements from harness runs. At bootstrap: conservative
defaults, `updated_at = None`, `evidence = []`.

```rust
pub struct Score(u8); // clamped 0..=100

pub struct ObservedModelBehavior {
    pub provider: String,
    pub model: String,
    pub tool_schema_reliability: Score,
    pub long_context_retention: Score,
    pub delegation_discipline: Score,
    pub recovery_after_tool_error: Score,
    pub completion_calibration: Score,
    pub compaction_resilience: Score,
    pub updated_at: Option<OffsetDateTime>,
    pub evidence: Vec<EvaluationRunId>,
}
```

`ObservedModelBehavior::bootstrap(provider, model)` returns all scores at
`Score::HALF` with no evidence.

## 6. Router — role-based selection

**File:** `harw-model-catalog/src/router.rs`.

Combines descriptor + runtime profile + observation for role-based model
selection.

```rust
pub enum ModelRole { Orchestrator, RepositoryWorker, FocusedCodingWorker, Verifier, Scout }

pub struct Candidate<'a> {
    pub descriptor: &'a ModelDescriptor,
    pub profile: &'a ModelRuntimeProfile,
    pub observed: &'a ObservedModelBehavior,
}

/// Deterministic ranking (stable on ties: alphabetical by model_id).
pub fn rank<'a>(role: ModelRole, candidates: &'a [Candidate<'a>]) -> Vec<&'a Candidate<'a>>;
pub fn pick<'a>(role: ModelRole, candidates: &'a [Candidate<'a>]) -> Option<&'a Candidate<'a>>;
```

Score formula (weighted sum in 0..=100):

- `Orchestrator` = 0.4 x delegation_discipline + 0.3 x long_context_retention + 0.3 x tool_schema_reliability
- `RepositoryWorker` = 0.5 x long_context_retention + 0.3 x compaction_resilience + 0.2 x tool_schema_reliability
- `FocusedCodingWorker` = 0.5 x tool_schema_reliability + 0.3 x completion_calibration + 0.2 x recovery_after_tool_error
- `Verifier` = 0.5 x completion_calibration + 0.5 x tool_schema_reliability
- `Scout` = 0.5 x tool_schema_reliability + 0.5 x recovery_after_tool_error

## 7. Scope notes

- No live benchmarking — bootstrap values are curated and documented, not
  measured at build time.
- Cross-crate integration into `harw-provider`/`harw-core` model selection is
  a separate concern from the catalog types themselves.
- No `models.dev` enrichment logic lives in this layer — see
  `docs/design/model-catalog-refresh.md` for the runtime refresh path.

## Implementation

Implemented: `harw-model-catalog/src/descriptor.rs`, `runtime.rs`,
`observed.rs`, `router.rs`, `family.rs`, `providers.toml`.
