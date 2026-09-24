# Model Capabilities & Behavior-Profile Registry

> Status: implemented · Last reviewed: 2026-09-24

An early design for splitting model metadata into two kinds: declared
provider capabilities versus empirically measured runtime behavior. The
shapes below are the original proposal; see `docs/design/model-catalog-v2.md`
for the four-layer architecture that the code actually implements
(`ModelDescriptor` for declared capabilities, `ObservedModelBehavior` for
measured behavior, `ModelRuntimeProfile` for harness policy). Names differ
from this draft but the separation principle is the same and still holds.

## Core statement

A provider abstraction like `async fn respond(messages, tools) ->
ModelResponse` is not enough. A capable harness needs two separate model
properties:

- **Declared provider capabilities** — the technical API surface.
- **Measured runtime profile** — how the model actually behaves under this
  harness.

Design principle: keep the separation strict. Capabilities are contracted
(provider documentation); the behavior profile is empirical (versioned,
scorable, overridable).

## Original proposed types (`harw-model-catalog/src/behavior.rs`)

```rust
/// Declared technical capabilities (from provider docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub context_limit: usize,
    pub max_output_tokens: Option<usize>,
    pub supports_tools: bool,
    pub supports_parallel_tools: bool,
    pub supports_streaming: bool,
    pub supports_reasoning_effort: bool,
    pub supports_prompt_caching: bool,
    pub supports_images: bool,
    pub supports_audio: bool,
    pub supports_json_mode: bool,
}

/// Empirical behavior under this runtime.
/// All scores are integers 0..=100 (calibratable, JSON-friendly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBehaviorProfile {
    pub tool_call_reliability: u8,
    pub long_context_retention: u8,
    pub delegation_discipline: u8,
    pub schema_strictness: u8,
    pub compaction_tolerance: u8,
    pub retry_sensitivity: u8,
    pub preferred_task_granularity: TaskGranularity,
    pub safe_parallelism: u8,
    pub last_calibrated: Option<OffsetDateTime>,
}

pub enum TaskGranularity { Focused, Balanced, Sweeping }

pub enum ModelRole { Orchestrator, RepositoryWorker, FocusedCoding, Verifier, Scout }
```

## Router (`harw-model-catalog/src/router.rs`)

```rust
pub struct ModelRouter { catalog: Arc<Catalog> }

impl ModelRouter {
    pub fn select(&self, role: ModelRole, hints: &RoutingHints) -> Option<ModelSelection>;
}

pub struct RoutingHints {
    pub prefers_families: Vec<ModelFamily>,
    pub min_context: Option<usize>,
    pub needs_tools: bool,
    pub max_cost_tier: CostTier,
}
```

Selection algorithm:

1. Candidates = models meeting the hard constraints (context, tools).
2. A behavior-profile score favoring the requested role gives a priority
   bonus (e.g. `long_context_retention >= 80` for `RepositoryWorker`).
3. If `prefers_families` is set, family match breaks ties.
4. Deterministic: on a further tie, sort alphabetically by `model_id`.

## Binding principles

- Capabilities (declared) vs. behavior profile (empirical) stay structurally
  separate.
- The model may propose work but never owns a process: the router decides
  server-side; a model tool call may never overwrite `model_id`, only
  suggest a `ModelRole`.
- Authority monotonicity: `RoutingHints` can only narrow, never widen
  (`min_context`, `max_cost_tier`).
- Provider adapters must be semantically lossless: `ModelCapabilities`
  prevents model-specific features (reasoning effort, prompt caching) from
  disappearing behind a lowest common denominator.

## Implementation

Implemented, under different type names than this original draft — see
`docs/design/model-catalog-v2.md` for the current four-layer design
(`harw-model-catalog/src/descriptor.rs`, `family.rs`, `observed.rs`,
`router.rs`, `runtime.rs`).
