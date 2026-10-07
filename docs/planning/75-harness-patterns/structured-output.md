---
id: STRUCTURED-OUTPUT
title: Wire-level structured output (ModelRequest.output_schema) and Codex cap (DEC-003)
status: accepted
date: 2026-09-27
tags: [provider, structured-output, codex, dec-003, contract]
related:
  - README.md
  - codex.md
  - ../70-decisions/DEC-003-provider-limits.md
  - ../85-gap-hunt/patterns.md
---

> **Assignment (Mia):** The wire level is missing for all providers; harw only validates
> afterwards. `ModelRequest.output_schema` is mapped by every adapter onto its
> mechanism (OpenAI `text.format`, Anthropic `output_config` or
> tool forcing). The existing validator stays behind it as a fail-closed
> safeguard. On the Codex route, the configured cap (DEC-003) is the
> actual control, not header pacing.
>
> This sheet is the implementation contract: fixed signatures, one agent per
> file, then one central build.

# Implementation contract: wire-level `output_schema` and Codex cap

## 0. Answer, and corrections to the mappings

Both gaps are real. Five findings change the design:

- **`ModelRequest` has no `Default` and no `#[non_exhaustive]`** (harw-core/src/model.rs:116-172). Adding a field breaks 39 struct literals in 7 files: `harw-provider-http/src/anthropic.rs` (17), `lib.rs` (16), `local_model_tests.rs:151`, `routing.rs:356`, `tool_names.rs:203,334`, `tests/anthropic_effort.rs:21`, and `harw-cli/src/onboarding.rs:1032`. Every other caller uses the constructors and stays source-compatible.
- **`ProviderToml` has no `Default` either** and is built literally in about 22 files. So this contract adds no `ProviderToml` field. The capability flag goes on `ModelCapabilitiesToml`, which derives `Default` (`model_toml.rs:87`) and has only one literal site (`harw-cli/src/models.rs:928`).
- **DEC-003 never mentions Codex.** `grep -ci codex` on the file returns 0, so the research claim that it "already states" the Codex conclusion is wrong. It needs an addendum.
- **The work driver reads the cap straight from TOML** (`harw-cli/src/job_worker_work_driver.rs:2260-2273`). A Codex default placed only in harw-provider-http would leave wave width uncapped. The default must therefore be a `harw-config` `effective_*` method used at both sites. `effective_*` methods are the existing pattern (`provider_toml.rs:296-310`).
- **Only one consumer exists this wave:**
  - No gap-hunt agent exists in code; the name only appears under `docs/planning/85-gap-hunt/`.
  - No JSON schema exists for any `ReturnPipeline` contract; `harw-research` and `harw-dod-signals` contain no `JsonSchema`.
  - So the judge is the only consumer now. Return contracts get a documented seam, not code.

## 1. Public types

**New file `harw-core/src/output_schema.rs`:**

```rust
pub use harw_tools::{AdditionalProperties, JsonSchema, JsonSchemaType};

/// Provider-neutral JSON-Schema contract for the model's final text answer.
/// Not serde: the only serialization is an adapter's `serde_json::to_value(&s.wire_schema())`.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSchema { name: String, schema: JsonSchema, strict: bool }

#[derive(Debug, Clone, PartialEq, Eq, HarwError)]
pub enum OutputSchemaError {
    #[msg("output schema name '{0}' must be 1-64 chars of A-Z a-z 0-9 _ -")] InvalidName(String),
    #[msg("output schema root must be an object schema")] RootNotObject,
    #[msg("output schema uses unsupported keyword: {0}")] UnsupportedKeyword(String),
}

impl OutputSchema {
    pub fn new(name: impl Into<String>, schema: JsonSchema) -> Result<Self, OutputSchemaError>; // strict = true
    #[must_use] pub fn non_strict(self) -> Self;
    #[must_use] pub fn name(&self) -> &str;
    #[must_use] pub fn schema(&self) -> &JsonSchema;
    #[must_use] pub fn is_strict(&self) -> bool;
    #[must_use] pub fn wire_schema(&self) -> JsonSchema; // strict ? schema.clone().into_strict() : schema.clone()
}

/// Decorator in the style of PinnedModelProvider (pinned_model.rs:36-70).
pub struct OutputSchemaProvider { inner: Arc<dyn ModelProvider>, schema: OutputSchema }
impl OutputSchemaProvider { #[must_use] pub fn new(inner: Arc<dyn ModelProvider>, schema: OutputSchema) -> Self; }
// ModelProvider impl: sets request.output_schema only if None (the caller wins);
// forwards pinned_model_id / pinned_provider_id / pacing_wait (model.rs:604-650).
```

**What `new` validates, recursively, failing closed:**
- The name matches `^[A-Za-z0-9_-]{1,64}$`.
- The root has `schema_type == Some(Object)`.
- `default` is rejected.
- `AdditionalProperties::Schema(_)` and `Bool(true)` are rejected. Anthropic only accepts `false` (Anthropic structured-outputs docs).

Why `JsonSchema` and not `serde_json::Value`: `JsonSchema` is the crate's existing harw-owned type for tool parameters (`harw-tools/src/schema.rs:23-46`, `spec.rs`). Its `into_strict()` (schema.rs:65-107) already does the OpenAI strict transform.

**`harw-core/src/model.rs`:**
- Add a field after `stream` (line 171), with docs: "`None` = today's behaviour; adapters map it or ignore it with a debug log; never replaces post-hoc validation".
  ```rust
  pub output_schema: Option<OutputSchema>,
  ```
- Set it to `None` in both `Self { .. }` literals (`with_context_budget` about line 221, `with_context_program` about line 340). `new` delegates, so it needs nothing.
- Add a builder next to line 426:
  ```rust
  #[must_use] pub fn with_output_schema(mut self, output_schema: Option<OutputSchema>) -> Self
  ```
- **`harw-core/src/lib.rs`:** add `pub mod output_schema;` and `pub use output_schema::{OutputSchema, OutputSchemaError, OutputSchemaProvider};`.

**`harw-config/src/model_toml.rs` (`ModelCapabilitiesToml`):**

```rust
/// Wire-level JSON-Schema output: Some(true) send, Some(false) never, None = transport default.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub structured_output: Option<bool>,
```

**`harw-config/src/provider_toml.rs`:**

```rust
pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
/// RESERVED_PROVIDER_SLOTS (1, job_worker_work_driver.rs:164) + one worker.
pub const CODEX_DEFAULT_MAX_CONCURRENCY: usize = 2;
impl ProviderToml {
    /// api == "openai-responses" && (base_url == CODEX_BASE_URL, trailing '/' ignored
    ///   || auth is FileJson{pointer "/tokens/access_token", path ends_with ".codex/auth.json"})
    #[must_use] pub fn is_codex_route(&self) -> bool;
    #[must_use] pub fn effective_max_concurrency(&self) -> Option<usize>; // max_concurrency.or_else(codex default)
}
```

## 2. Per-adapter mapping

**New file `harw-provider-http/src/structured_output.rs`** (declared with `mod structured_output;` in lib.rs):

```rust
pub(crate) const FORCED_TOOL_NAME: &str = "harw_structured_output";
#[derive(Debug, Clone, Default)]
pub(crate) struct StructuredOutputSupport { transport_default: bool, overrides: BTreeMap<String, bool> }
impl StructuredOutputSupport {
    pub(crate) fn from_config(provider_name: &str, config: &harw_config::ResolvedConfig, transport_default: bool) -> Self; // ids + aliases, like toolless_models lib.rs:3476
    pub(crate) fn override_for(&self, model: &str) -> Option<bool>;
    pub(crate) fn enabled(&self, model: &str) -> bool; // override, else transport_default
}
pub(crate) fn apply_responses(body: &mut Value, s: &OutputSchema) -> Result<(), serde_json::Error>;
pub(crate) fn apply_chat(body: &mut Value, s: &OutputSchema) -> Result<(), serde_json::Error>;
pub(crate) fn anthropic_format(s: &OutputSchema) -> Result<Value, serde_json::Error>;
pub(crate) fn anthropic_forced_tool(s: &OutputSchema) -> Result<Value, serde_json::Error>;
pub(crate) fn log_skipped(provider_id: &str, model: &str, s: &OutputSchema, reason: &'static str); // tracing::debug!
```

| Transport | Default | Body key (top level only) | Answer comes back as |
|---|---|---|---|
| Responses, OpenAI and Codex (`lib.rs:4114`) | `!provider.is_local()` | `"text":{"format":{"type":"json_schema","name":N,"schema":W,"strict":S}}` | `output_text` (lib.rs:3400); refusal is already mapped (3040, 3113) |
| Codex extra | same as Responses | `codex::prepare_body` (codex.rs:308-318) keeps `text` | streamed terminal body |
| Chat, including DashScope, Cloudflare, Ollama (`lib.rs:4128`) | `false`; opt in per model with `structured_output = true` | `"response_format":{"type":"json_schema","json_schema":{"name":N,"schema":W,"strict":S}}` | `content`; refusal is already mapped (3319) |
| Anthropic, native | caps column, overridden by the config value | merge into `output_config` (keep `effort`): `"output_config":{"format":{"type":"json_schema","schema":W}}` | first `text` block (anthropic.rs:660); refusal at :825 |
| Anthropic, fallback | only if caps `structured_output=false`, caps `forced_tool_choice=true`, `request.tools` empty, and no `thinking` in the body | `"tools":[{"name":FORCED_TOOL_NAME,"input_schema":W,...}]`, `"tool_choice":{"type":"tool","name":FORCED_TOOL_NAME,"disable_parallel_tool_use":true}` | `interpret_body` (anthropic.rs:1288): the forced call's `arguments` become `message` (`serde_json::to_string`), the call is removed from `tool_calls`, and `stop` goes from `ToolUse` to `EndTurn`. Any other `tool_use` block returns `ModelError::RequestFailed`. |

Here `W = s.wire_schema()` and `S = s.is_strict()`.

Why the fallback is so tightly gated:
- Forced `tool_choice` returns HTTP 400 on the newest Anthropic models (Anthropic tool-use docs; the rows are marked in `anthropic_caps.rs`).
- Bedrock requires thinking to be disabled for forced `tool_choice`.

**When a schema is unsupported:** leave the body unchanged, call `log_skipped`, and return the response normally. The post-hoc validator stays the gate.

**Serialization:** use `?` into `ModelError::SerdeJson` (model.rs:674) everywhere. Do not copy the fail-open `unwrap_or_else(|_| json!({}))` at lib.rs:2809-2810. The only exception is `build_messages_body`, which is a public infallible function: on a serialization error it omits the field and logs `warn`.

**Keeping `build_messages_body` stable:** its public signature (anthropic.rs:564) does not change. It delegates to a new `pub(crate) fn build_messages_body_with(model, max_tokens, request, override_: Option<bool>) -> Value`, the same pattern as `build_chat_body`/`build_chat_body_with`.

**Prompt cache:**
- The schema only ever goes into top-level keys (`text`, `response_format`, `output_config`, `tool_choice`), never into system, instructions or messages.
- `apply_chat_cache_control` and `apply_messages_cache_control` only touch system, tools and messages (cache_strategy.rs:1-22), so they need no change.
- `JsonSchema` stores properties in a `BTreeMap` (schema.rs:31), so the bytes are stable.
- Changing `tool_choice` invalidates Anthropic's system and messages caches (Anthropic prompt-caching docs). `output_config.format` is not listed in that table. Rule: keep the schema constant for the whole session, which the decorator guarantees.

## 3. Consumers

- **The judge.**
  - `harw-plan-bridge/src/work_driver.rs`: add `pub fn judge_output_schema() -> Result<OutputSchema, OutputSchemaError>` with name `harw_judge_verdict`.
  - The schema: an object with `passed: boolean`, `comment: string`, `missing: string[]`, and all three in `required`.
  - Why all three are required: `into_strict` makes optional fields nullable (schema.rs:90-94). `JudgeVerdict.comment: String` with `#[serde(default)]` (work_driver.rs:249) rejects `null`, so an optional field would break parsing.
  - `harw-cli/src/job_worker_work_driver.rs:2818`: wrap `Arc::clone(&self.provider)` in `OutputSchemaProvider` before `MeteredModelProvider::new`. Map a schema error to `JudgeError::Failed`.
  - `parse_verdict` (:1857) stays unchanged as the backstop.
- **Return contracts:** no change this wave. `evaluate_child_return`, `return_validators.rs:64-83`, `agent_tool.rs:667/2730` and `delegate_wave.rs:1040-1053` stay as they are. The future seam is `harw-runtime/src/children.rs:1855` (`model_for`): wrap it once a contract has a schema.

## 4. Codex cap

- **Default:** `effective_max_concurrency()` is used at `lib.rs:2082` (OpenAI and Codex), `lib.rs:760` (Anthropic; no behaviour change there) and `job_worker_work_driver.rs:2269`. An explicit value always wins; to lift the cap, set a larger number.
- **Why 2:** it is the smallest value where `parallel_ceiling` (:374-379) still gives one worker plus the reserved orchestrator/judge slot.
- **Pacing:** `observe_headers` stays (lib.rs:4231). Add a comment and a codex.rs module doc saying header pacing is best-effort on this route and that `effective_max_concurrency` plus `[rate_limit]` budgets are the real control (DEC-003).
- **Provenance marker:** `pub(crate) const WIRE_VERIFIED_AGAINST: &str` in `codex.rs`, and a private copy in `harw-oauth/src/codex_refresh.rs` (replacing the "verified in this session …" comment at :47-53). Its value is `"openai/codex@<40-hex sha> (YYYY-MM-DD)"` only if the agent actually checks against a fetched tree, otherwise `"unpinned"`. Do not invent a hash: the `../codex` tree is absent here, and the `88235f8` in codex.md:86 is a research-batch commit, not a verification. The `PROVISIONAL` comment (codex.rs:245-256) stays.

## 5. Files, edits, tests

No agent builds. All agents can run in parallel because every signature is fixed above. Each file has exactly one owner.

| Agent | File | Edit | Tests (all `-> TestResult`) |
|---|---|---|---|
| A | harw-core/src/output_schema.rs (new), model.rs, lib.rs | §1 | `new_rejects_invalid_name`, `new_rejects_non_object_root`, `new_rejects_default_and_open_additional_properties`, `wire_schema_is_strict_by_default`, `non_strict_wire_schema_is_unchanged`, `provider_sets_schema_only_when_absent`, `provider_forwards_pins_and_pacing`, `constructors_leave_output_schema_none`, `with_output_schema_sets_and_clears` |
| B | harw-config/src/model_toml.rs, provider_toml.rs | §1, §4 | `capabilities_parse_structured_output`, `capabilities_omit_structured_output_when_none`, `is_codex_route_matches_base_url_and_login_reference`, `effective_max_concurrency_defaults_only_for_codex`, `explicit_max_concurrency_wins_on_codex_route` |
| C | harw-provider-http/src/structured_output.rs (new) | §2 | `responses_text_format_shape`, `chat_response_format_shape`, `anthropic_format_shape`, `forced_tool_shape`, `overrides_include_aliases`, `enabled_prefers_model_override`, `serialization_is_byte_stable` |
| D | harw-provider-http/src/lib.rs | `mod`; a provider field initialised at about :1838 and set at :2091; apply in the Responses and Chat arms (4114/4128); `configure_structured_output` call at about :760; the effective cap at :760 and :2082; the :4231 comment; 16 literals | `responses_body_carries_text_format_when_supported`, `responses_body_omits_text_format_for_local_provider`, `chat_body_omits_response_format_by_default`, `chat_body_carries_response_format_with_model_override`, `codex_base_url_provider_gets_default_concurrency_cap`, `chat_cache_control_leaves_response_format_untouched` |
| E | harw-provider-http/src/anthropic.rs | `build_messages_body_with`; merge `output_config`; fallback; `interpret_body` translation; an overrides field plus `configure_structured_output`; 17 literals | `native_output_config_format_merges_with_effort`, `unknown_model_gets_no_output_format`, `config_override_enables_unknown_model`, `config_override_false_suppresses`, `forced_tool_fallback_only_without_tools_and_thinking`, `interpret_body_turns_forced_tool_into_message`, `interpret_body_rejects_foreign_tool_with_forced_fallback` |
| F | harw-provider-http/src/anthropic_caps.rs | add the columns `structured_output` and `forced_tool_choice` (details below) | `structured_output_column_matches_documented_models`, `forced_tool_choice_rejected_on_newest_models` |
| G | harw-provider-http/src/codex.rs | marker and module doc | `wire_verified_marker_is_well_formed`, `config_predicate_agrees_with_route_predicates`, `prepare_body_preserves_text_format` |
| H | harw-oauth/src/codex_refresh.rs | marker | `wire_verified_marker_is_well_formed` |
| I | local_model_tests.rs, routing.rs, tool_names.rs, tests/anthropic_effort.rs, harw-cli/src/onboarding.rs, harw-cli/src/models.rs | add `output_schema: None,` / `structured_output: None,` | none |
| J | harw-plan-bridge/src/work_driver.rs | `judge_output_schema` | `judge_output_schema_is_strict_object`, `judge_schema_instance_deserializes_to_verdict` |
| K | harw-cli/src/job_worker_work_driver.rs | judge wrap; extract `provider_cap_for(&ProviderToml)` using the effective cap | `judge_model_attaches_verdict_schema`, `provider_cap_for_uses_codex_default` |
| L | docs/planning/70-decisions/DEC-003-provider-limits.md, docs/planning/75-harness-patterns/codex.md | Codex addendum; update the risk bullets at :64, :84, :86 | none |

**Agent F detail:**
- `structured_output = true` only for the models the Anthropic structured-outputs docs list as supported. Every other row is `false` until checked against that page.
- `forced_tool_choice = false` for the newest models that reject forced `tool_choice` with HTTP 400; `true` for the rest.

**Re-check only, no edit:** `cache_strategy.rs`, `sse.rs` (it feeds `interpret_body`), `rate_limiter.rs`, `harw-provider/src/openai.rs` (`TextFormatConfig` stays unused), `retry.rs`, `return_validators.rs`, `agent_tool.rs`, `delegate_wave.rs`, `harw-runtime/src/children.rs`.

## 6. Central build and risks

The main session runs this once, after every agent has finished. In the cloud container, prefix every step with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`.
```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
cargo deny check
make -C dod clippy test
actionlint
```

**Risks:**
1. **Missed literals.** The compiler will flag any `ModelRequest` literal that lacks the new field; the list in §0 is complete per grep.
2. **Let-chains versus MSRV.** Existing code already uses let-chains (lib.rs:4054, 4221, 3651; anthropic.rs:568) although `rust-version = "1.85"` (Cargo.toml:165). New code must not add more.
3. **The Anthropic model list is not verified for every row.** The fallback when unsure is to skip sending the schema, which still fails closed.
4. **Unconfirmed Codex backend support for `text.format`.** The only mitigation is the model-level `structured_output = false`. Only the judge is exposed.
5. **The cap is per backend instance, not per process.** The concurrency limiter is per backend (lib.rs:2081), while budgets are process-global (budget.rs:1057).
6. **Unverified OpenAI details.** The name pattern and the strict rejection of `default` come from OpenAI docs (developers.openai.com/api/docs/guides/structured-outputs, per the research); I did not re-fetch them in this session.

**Tests to add:** listed per file in §5. **Central build:** the commands above.
