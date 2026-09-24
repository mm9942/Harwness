# Config scopes: GLOBAL vs. PROFILE

> Status: implemented · Last reviewed: 2026-09-24

An inventory of every `HarnessConfig` field, its merge behavior across the
config layers (root home → active profile → project), and the GLOBAL/PROFILE
scope each field should have. This analysis has since been implemented: the
`MergeRule` taxonomy in Section 6 is now `harw-config/src/scope.rs`'s
`FIELD_TABLE`/`MergeRule`, applied per layer by `harw-config/src/merge.rs`.
Section 0 and the "Merge today" column in Section 1 describe the behavior
*before* that implementation, kept here because Section 1 is still the
canonical per-field inventory; where behavior has since changed, the table
notes it.

Sources: `harw-config/src/harness_config.rs`, `harw-config/src/plan_toml.rs`,
`harw-config/src/mode_toml.rs`, `harw-config/src/research_toml.rs`,
`harw-config/src/permissions_toml.rs`, `harw-config/src/internal_models.rs`,
`harw-config/src/auth_toml.rs` (only the `SecretRef` type),
`harw-config/src/discovery.rs` (`discover_config_with_restricted`,
`apply_restricted_layer`, `merge_restricted_*`), `harw-config/src/scope.rs`,
`harw-config/src/merge.rs`, `harw-home/src/scaffold.rs`.

---

## 0. Merge model prior to this design (historical)

Before this design was implemented, `discover_config_with_restricted`
iterated over the trusted layers (`~/.harw` → `~/.harw/profiles/<name>`) in
ascending precedence. For each layer with its own `config.toml`:

1. `[network]`/`[browser]`/`[dod]`/`[web]` were parsed **independently** of
   `HarnessConfig` (`extract_section`, since `harness_config.rs` did not know
   these four tables — `NEW_SECTION_KEYS`) and **replaced wholesale only when
   present in the layer**; if the table was missing, the previous layer's
   value stayed ("home layer sets" behavior).
2. The rest of `config.toml` was deserialized as `HarnessConfig` (`cfg`).
   Serde filled in **every** field this layer did not itself set with the
   hardcoded section default (`#[serde(default)]`).
3. Only five values were explicitly written back from the accumulated state
   (`resolved.harness`) into `cfg` before `cfg` was adopted: `default_provider`,
   `default_model`, `active_uia_definition` (each only if `cfg.<field>.is_none()`),
   `onboarding` (only if the layer had no `[onboarding]` table at all), and
   `internal_models` (fine-grained per model slot via `merge_internal_models`).
4. `resolved.harness = cfg;` — **the entire `HarnessConfig` was replaced.**

**Consequence:** each of the **other ~80 fields** (see Section 4) silently
reset to its default as soon as a later trusted layer (typically the profile)
had a `config.toml` that did not itself set that field — regardless of
whether an earlier layer (root home) had explicitly set it. Since
`ensure_home` **always** creates the profile `config.toml` (template
`PROFILE_CONFIG_TEMPLATE` with a hardcoded `[mcp_listener]`), this hit
practically every installation as soon as the home layer set any field
outside the five exceptions differently from its default.

There was also already a **"restricted" logic** for an untrusted project
layer (`apply_restricted_layer`, run after all trusted layers are processed):
it reads `config.toml` of an untrusted repo `.harw` and applies **only
narrowing** rules on top of the already-merged trusted state, for a subset of
nine fields (`merge_restricted_harness`, plus `merge_restricted_network/browser/dod`).
Details in Section 3. This "restricted" logic is now generalized: it is the
direct precedent that `harw-config/src/scope.rs`/`merge.rs` extended to all
trusted layers, not just the untrusted project layer.

---

## 1. Complete field list

Legend for the "Merge today" column (as it stood before this implementation):
**RESET** = field reset to the section default when absent from a layer (the
bug); **CARRIED OVER** = explicit exception; **RESTRICTED PRECEDENT** =
additionally already part of the `merge_restricted_harness` narrowing for the
untrusted project layer. Where a field has since gained a real `MergeRule` in
`FIELD_TABLE`, the "Merge today" text is historical, not current behavior —
see Section 2 for the field's current, implemented rule.

### 1.1 Top-level fields (`harness_config.rs:13-103`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `config_version` | `u32` | `0` | `harness_config.rs:14-15` | RESET |
| `workspace_root` | `Option<String>` | `None` | `harness_config.rs:16-17` | RESET |
| `default_provider` | `Option<String>` | `None` | `harness_config.rs:18-19` | CARRIED OVER (only if the `cfg` value is `None`) |
| `default_model` | `Option<String>` | `None` | `harness_config.rs:20-21` | CARRIED OVER (only if the `cfg` value is `None`) |
| `active_agent_definition` | `Option<String>` | `None` | `harness_config.rs:22-23` | RESET |
| `active_uia_definition` | `Option<String>` | `None` | `harness_config.rs:24-28` | CARRIED OVER (only if the `cfg` value is `None`) |
| `uia_provider` | `Option<String>` | `None` | `harness_config.rs:29-35` | RESET |
| `uia_model` | `Option<String>` | `None` | `harness_config.rs:36-39` | RESET |
| `uia_worker_model` | `Option<String>` | `None` | `harness_config.rs:40-48` | RESET |
| `policy_profile` | `Option<String>` | `None` | `harness_config.rs:49-50` | RESET |
| `project_root_markers` | `Option<Vec<String>>` | `None` | `harness_config.rs:76-80` | RESET |
| `base_dir` | `Option<PathBuf>` | — | `harness_config.rs:101-102` | `#[serde(skip)]` — **not a TOML field**, set internally per layer to `Some(base.clone())`, pure bookkeeping |

### 1.2 `[logging]` (`harness_config.rs:262-284`, Defaults `420-422`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `logging.level` | `String` | `"info"` | `harness_config.rs:269` | RESET |
| `logging.target_module_paths` | `bool` | `false` | `harness_config.rs:271` | RESET |
| `logging.json` | `bool` | `false` | `harness_config.rs:273` | RESET |

### 1.3 `[tui]` (`harness_config.rs:286-304`, Defaults `423-428`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `tui.theme` | `String` | `"default-dark"` | `harness_config.rs:292` | RESET |
| `tui.keybindings_file` | `String` | `"keybindings.toml"` | `harness_config.rs:294` | RESET |
| `tui.child_stream` | `ChildStreamModeToml` (`orchestrators`/`all`/`none`) | `orchestrators` | `harness_config.rs` (`TuiSection`) | `ProfileReplaces` (`merge.rs`) |

### 1.4 `[session]` (`harness_config.rs:306-343`, Defaults `429-437`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `session.store_dir` | `String` | `"sessions"` | `harness_config.rs:311` | RESET |
| `session.journal_format` | `String` | `"jsonl"` | `harness_config.rs:313` | RESET |
| `session.retention_days` | `u32` | `90` | `harness_config.rs:315` | RESET |
| `session.title_generation` | `bool` | `true` | `harness_config.rs:321` | RESET |
| `session.title_model` | `Option<String>` | `None` | `harness_config.rs:325` | RESET |

### 1.5 `[policy]` (`harness_config.rs:345-353`, Default `411-418`, `default_visibility_scope` `438-440`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `policy.default_visibility_scope` | `String` | `"self"` | `harness_config.rs:350` | RESET |
| `policy.require_approval_for` | `Vec<String>` | `[]` | `harness_config.rs:352` | RESET — **RESTRICTED PRECEDENT**: already merged as a union for the untrusted project layer (`merge_restricted_harness:958-964`) |

### 1.6 `[mcp_listener]` (`harness_config.rs:355-409`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `mcp_listener.enabled` | `bool` | `false` | `harness_config.rs:364` | RESET |
| `mcp_listener.listen_addr` | `String` | `"127.0.0.1:1337"` | `harness_config.rs:368` | RESET |
| `mcp_listener.path` | `String` | `"/mcp"` | `harness_config.rs:372` | RESET |
| `mcp_listener.principals` | `Vec<McpPrincipalToml>` | `[]` | `harness_config.rs:376` | RESET |
| `mcp_listener.principals[].id` | `String` | — (required field) | `harness_config.rs:382` | Part of `principals`, see above |
| `mcp_listener.principals[].credential_ref` | `SecretRef` (`auth_toml.rs:23-40`) | — (required field) | `harness_config.rs:383` | Part of `principals`, see above |
| `mcp_listener.principals[].tenant` | `String` | — (required field) | `harness_config.rs:384` | Part of `principals`, see above |
| `mcp_listener.principals[].workspace` | `String` | — (required field) | `harness_config.rs:385` | Part of `principals`, see above |
| `mcp_listener.principals[].job_capabilities` | `Vec<McpJobCapabilityToml>` (Enum `ReadOwn`/`ReadWorkspace`/`SubmitOwn`/`CancelOwn`/`CancelWorkspace`, `harness_config.rs:390-398`) | `[]` | `harness_config.rs:387` | Part of `principals`, see above |

### 1.7 `[onboarding]` (`harness_config.rs:231-251`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `onboarding.seen` (table) | `OnboardingSeen` | see below | `harness_config.rs:238` | CARRIED OVER as a whole, only if the layer does **not** contain the top-level `[onboarding]` table at all (`discovery.rs:640-642`) — no per-field merge |
| `onboarding.seen.provider` | `bool` | `false` | `harness_config.rs:246` | see above |
| `onboarding.seen.model` | `bool` | `false` | `harness_config.rs:248` | see above |
| `onboarding.seen.channel` | `bool` | `false` | `harness_config.rs:250` | see above |

### 1.8 `[tools.plan]` (`plan_toml.rs:26-150`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `tools.plan.enabled` | `bool` | `true` | `plan_toml.rs:43` | RESET |
| `tools.plan.persist` | `bool` | `true` | `plan_toml.rs:47` | RESET |
| `tools.plan.require_for_complex_work` | `bool` | `false` | `plan_toml.rs:51` | RESET |
| `tools.plan.validate_dependency_cycles` | `bool` | `true` | `plan_toml.rs:55` | RESET — **RESTRICTED PRECEDENT**: OR narrowing (`merge_restricted_harness:990-992`) |
| `tools.plan.validate_write_conflicts` | `bool` | `true` | `plan_toml.rs:59` | RESET — **RESTRICTED PRECEDENT**: OR narrowing (`merge_restricted_harness:993-995`) |
| `tools.plan.max_nodes` | `usize` | `256` | `plan_toml.rs:63` | RESET — **RESTRICTED PRECEDENT**: Minimum (`merge_restricted_harness:996-998`) |
| `tools.plan.require_exploration_for` | `Vec<String>` | `["coding","integration"]` | `plan_toml.rs:67` | RESET |
| `tools.plan.exploration_ttl_secs` | `u64` | `86400` | `plan_toml.rs:71` | RESET |
| `tools.plan.max_expand_depth` | `u32` | `3` | `plan_toml.rs:75` | RESET — **RESTRICTED PRECEDENT**: Minimum (`merge_restricted_harness:999-1001`) |

### 1.8a `[tools.doc]` (`plan_toml.rs`, `DocSection`)

| TOML path | Type | Default | Allowed | Merge today |
|---|---|---|---|---|
| `tools.doc.remote_ocr` | `RemoteOcrMode` | `"ask"` | `off`/`ask`/`on` | `StricterOf` for the untrusted project (`merge_tools_doc`) — 🔒 |

Controls whether `doc.read_pdf` may send workspace PDFs to a remote OCR
service. Remote OCR is only possible when an enabled Mistral provider with a
resolvable credential is configured; the file then goes to that provider's
API host (normally `api.mistral.ai`), outside the `[network]` policy.

- `off`: no OCR client is installed; `doc.read_pdf` always extracts locally.
- `ask` (default): every `doc.read_pdf` call that would use remote OCR
  (any `backend` other than `"native"`) needs an explicit approval, in every
  approval mode including full access and regardless of allow rules. The
  approval dialog names the host. Entries where no one can answer (one-shot,
  jobs) deny the call with a hint to retry with `backend: "native"`.
- `on`: remote OCR without asking (the behavior before this setting existed).

```toml
[tools.doc]
remote_ocr = "ask"   # "off" | "ask" | "on"
```

### 1.9 `[mode]` (`mode_toml.rs:19-38`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `mode.default` | `String` (`chat`/`plan`/`explore`/`work`/`shell`) | `"chat"` | `mode_toml.rs:25` | RESET |

### 1.10 `[research]` (`research_toml.rs:16-105`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `research.network_allow_hosts` | `Vec<String>` | `["docs.rs","crates.io","doc.rust-lang.org","static.crates.io"]` | `research_toml.rs:22` | RESET — **RESTRICTED PRECEDENT**: intersection (`merge_restricted_harness:968-973`) |
| `research.cargo_registry_read` | `bool` | `true` | `research_toml.rs:26` | RESET — **RESTRICTED PRECEDENT**: AND (`merge_restricted_harness:974-976`) |
| `research.max_fetch_bytes` | `usize` | `1_048_576` | `research_toml.rs:29` | RESET — **RESTRICTED PRECEDENT**: Minimum (`merge_restricted_harness:977-980`) |
| `research.fetch_timeout_secs` | `u64` | `20` | `research_toml.rs:32` | RESET — **RESTRICTED PRECEDENT**: Minimum (`merge_restricted_harness:981-986`) |
| `research.cache_ttl_secs` | `u64` | `3600` | `research_toml.rs:36` | RESET — **not** covered by the restricted logic (an inconsistency within the same section, see Finding 3) |

### 1.11 `[permissions]` (`permissions_toml.rs:39-74`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `permissions.default_mode` | `Option<String>` (`ask`/`auto`/`full`) | `None` | `permissions_toml.rs:45` | RESET |
| `permissions.approval_timeout_secs` | `Option<u64>` (10–86400) | `None` | `permissions_toml.rs:49` | RESET |
| `permissions.allow` | `Vec<RuleToml>` | `[]` | `permissions_toml.rs:52` | RESET |
| `permissions.deny` | `Vec<RuleToml>` | `[]` | `permissions_toml.rs:57` | RESET |
| `permissions.extra_roots` | `Vec<PathBuf>` (≤8, absolut) | `[]` | `permissions_toml.rs:61` | RESET |
| `RuleToml.tool` (field of `allow[]`/`deny[]`) | `String` | — (required field) | `permissions_toml.rs:69` | Part of the respective list, see above |
| `RuleToml.pattern` (field of `allow[]`/`deny[]`) | `Option<String>` | `None` | `permissions_toml.rs:73` | Part of the respective list, see above |

### 1.12 `[sandbox]` (`harness_config.rs:180-229`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `sandbox.cargo` | `Option<CargoSandboxToml>` | `None` | `harness_config.rs:184` | RESET |
| `sandbox.cargo.mode` | `CargoSandboxModeToml` (`inspect`/`build_offline`/`fetch`) | — (required field, if `cargo` is set) | `harness_config.rs:198` | Part of `sandbox.cargo`, see above |
| `sandbox.cargo.cargo_bin` | `String` (absolute path) | — (required field) | `harness_config.rs:199` | Part of `sandbox.cargo`, see above |
| `sandbox.cargo.rustup_home` | `String` (absolute path) | — (required field) | `harness_config.rs:200` | Part of `sandbox.cargo`, see above |
| `sandbox.cargo.cargo_home` | `String` (absolute path) | — (required field) | `harness_config.rs:201` | Part of `sandbox.cargo`, see above |
| `sandbox.tmux` | `Option<TmuxSandboxToml>` | `None` | `harness_config.rs:186` | RESET |
| `sandbox.tmux.mode` | `TmuxOperationModeToml` (`inspect`/`write`) | — (required field) | `harness_config.rs:220` | Part of `sandbox.tmux`, see above |
| `sandbox.tmux.socket_path` | `String` (absolute path) | — (required field) | `harness_config.rs:221` | Part of `sandbox.tmux`, see above |

### 1.13 `[internal_models]` (`internal_models.rs:139-193`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `internal_models.use_openrouter_defaults` | `bool` | `true` | `internal_models.rs:160` | CARRIED OVER, fine-grained (`merge_internal_models:857-859`, only if the layer does not itself set the key) |
| `internal_models.session_title` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:162` | CARRIED OVER, fine-grained (`merge_internal_models:860-865`) |
| `internal_models.compaction_summary` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:164` | CARRIED OVER, fine-grained |
| `internal_models.memory_consolidation` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:166` | CARRIED OVER, fine-grained |
| `internal_models.dream_reflection` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:168` | CARRIED OVER, fine-grained |
| `internal_models.explorer` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:170` | CARRIED OVER, fine-grained |
| `internal_models.research` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:172` | CARRIED OVER, fine-grained |
| `internal_models.worker_simple` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:174` | CARRIED OVER, fine-grained |
| `internal_models.worker_complex` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:176` | CARRIED OVER, fine-grained |
| `internal_models.root_orchestrator` | `Option<InternalModelChoice>` | `None` (→ main model, never the OpenRouter default) | `internal_models.rs` | CARRIED OVER, fine-grained |
| `internal_models.sub_orchestrator` | `Option<InternalModelChoice>` | `None` (→ main model, never the OpenRouter default) | `internal_models.rs` | CARRIED OVER, fine-grained |
| `internal_models.auto_classifier` | `Option<InternalModelChoice>` | `None` (→ the active provider's fast model, `claude-haiku-4-5` for Anthropic; never the OpenRouter default) | `internal_models.rs` | CARRIED OVER, fine-grained |
| `InternalModelChoice.provider`/`.model` (field of each slot) | `Option<String>` je | `None` | `internal_models.rs:143-145` | Part of the respective slot, see above |

### 1.14 `[compaction]` (`harness_config.rs:105-117`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `compaction.absolute_ceiling_tokens` | `Option<u64>` | `None` | `harness_config.rs:116` | RESET |
| `compaction.max_history_bytes` | `Option<usize>` | `None` | `harness_config.rs:130` | RESET |

### 1.15 `[reasoning]` (`harness_config.rs:119-148`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `reasoning.uia` | `Option<String>` | `None` | `harness_config.rs:130` | RESET |
| `reasoning.root_orchestrator` | `Option<String>` | `None` | `harness_config.rs:134` | RESET |
| `reasoning.root_orchestrator_with_subs` | `Option<String>` | `None` | `harness_config.rs:138` | RESET |
| `reasoning.sub_orchestrator` | `Option<String>` | `None` | `harness_config.rs:141` | RESET |
| `reasoning.worker_complex` | `Option<String>` | `None` | `harness_config.rs:144` | RESET |
| `reasoning.worker_simple` | `Option<String>` | `None` | `harness_config.rs:147` | RESET |

### 1.16 `[guards]` (`harness_config.rs:150-175`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `guards.enabled` | `Option<bool>` | `None` → Laufzeit-Default `true` | `harness_config.rs:159` | RESET |
| `guards.repeated_failure_warn` | `Option<u32>` | `None` → `2` | `harness_config.rs:162` | RESET |
| `guards.repeated_failure_abort` | `Option<u32>` | `None` → `3` | `harness_config.rs:165` | RESET |
| `guards.no_progress_rounds_warn` | `Option<u32>` | `None` → `4` | `harness_config.rs:168` | RESET |
| `guards.no_progress_rounds_abort` | `Option<u32>` | `None` → `8` | `harness_config.rs:171` | RESET |
| `guards.plan_stale_rounds` | `Option<u32>` | `None` → `6` | `harness_config.rs:174` | RESET |

### 1.17 `[knowledge]` (`harness_config.rs`, `KnowledgeToml`/`DiaryToml`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `knowledge.diary.retention_days` | `Option<u32>` | `None` → `90` (`DEFAULT_DIARY_RETENTION_DAYS`) | `harness_config.rs:163` | `ProfileReplaces` (`merge.rs`, `merge_knowledge`) |

Days a diary day-file remains before maintenance
(`harw_knowledge::diary::maintain`, part of every dream run) rolls it into the
monthly rollup. Maintenance compacts, it never deletes content without a
rollup; hence no security relevance.

```toml
[knowledge.diary]
retention_days = 90
```

### 1.18 `[dream]` (`harness_config.rs`, `DreamToml`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `dream.enabled` | `Option<bool>` | `None` → `true` (`DEFAULT_DREAM_ENABLED`) | `harness_config.rs:199` | `ProfileReplaces` |
| `dream.budget` | `Option<u64>` | `None` → `16384` tokens per run (`DEFAULT_DREAM_BUDGET_TOKENS`, at least 1) | `harness_config.rs:202` | `ProfileReplaces` |
| `dream.idle_minutes` | `Option<u32>` | `None` → `15` | `harness_config.rs:205` | `ProfileReplaces` |
| `dream.cooldown_minutes` | `Option<u32>` | `None` → `60` | `harness_config.rs:208` | `ProfileReplaces` |
| `dream.schedule` | `Option<String>` | `None` (idle trigger) | `harness_config.rs:211` | `ProfileReplaces` |

- `enabled` only controls the autonomous gateway scheduler; `/dream run`
  (TUI, one-shot) runs independently of it.
- `schedule` is an optional 5-field cron expression in UTC. When set, it
  replaces the idle trigger (`idle_minutes`); the minimum spacing
  (`cooldown_minutes`) still applies. The expression is only validated at the
  scheduler (`harw_knowledge::context_steward::DreamSchedule`); an empty value
  counts as unset.
- Scheduler state (last run, running run) lives in
  `<profile>/knowledge/dreams/state.json` and survives a restart; `/dream
  status` shows it together with these values.
- No security relevance: a dream run only writes reports with suggestions;
  every adoption goes through `/dream review … accept`.

```toml
[dream]
enabled = true
budget = 16384
idle_minutes = 15
cooldown_minutes = 60
# schedule = "0 3 * * *"   # optional, UTC; replaces the idle trigger
```

### 1.19 `[host]` (`harness_config.rs`, `HostToml`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `host.sudo_session_minutes` | `Option<u32>` | `None` → `10` (`DEFAULT_SUDO_SESSION_MINUTES`), capped at `60` (`MAX_SUDO_SESSION_MINUTES`) | `harness_config.rs` | `MinBound`, Scope Global (`merge.rs`, Section 1.19) — 🔒 |

How long the TUI keeps a sudo password entered with "for this session" for
`host.sudo_exec` in memory. `0` disables session memory (leaving only
"once"). Every root command needs its own approval regardless. A profile can
only shorten this window.

```toml
[host]
sudo_session_minutes = 10
```

### 1.20 `[uia_worker_models]` (`uia_worker_models.rs`)

| TOML path | Type | Default | File:Line | Merge today |
|---|---|---|---|---|
| `uia_worker_models.uia_worker` | `Option<String>` (`"uia"` or `"provider/model"`) | `None` → legacy pin `uia_worker_model`, else "same as UIA" | `uia_worker_models.rs` | `ProfileReplaces` per role |
| `uia_worker_models.uia_shell_worker` | as above | as above | `uia_worker_models.rs` | `ProfileReplaces` |
| `uia_worker_models.uia_writer` | as above | as above | `uia_worker_models.rs` | `ProfileReplaces` |
| `uia_worker_models.uia_latex_writer` | as above | as above | `uia_worker_models.rs` | `ProfileReplaces` |
| `uia_worker_models.uia_explorer` | as above | as above | `uia_worker_models.rs` | `ProfileReplaces` |

`"uia"` follows the UIA model (including live switches); `"provider/model"`
is a fixed choice, split on the first `/`. A fixed choice only stays in
effect while its provider is signed in, otherwise it falls back to "same as
UIA" with a notice. Details: `tui-roles-models-modes.md` §1.1.

### 1.21 `[agents]` (`agent_limits.rs`, `AgentLimitsToml`)

| TOML path | Type | Default | Allowed | Merge today |
|---|---|---|---|---|
| `agents.max_root_orchestrators` | `Option<u32>` | `1` | 1–4 | `MinBound` for the untrusted project (`merge_agent_limits`) — 🔒 |
| `agents.max_sub_orchestrators` | `Option<u32>` | `2` | 1–6 | as above — 🔒 |
| `agents.max_sub_orchestrator_depth` | `Option<u32>` | `2` | 1–3 | as above — 🔒 |
| `agents.max_spawn_depth` | `Option<u32>` | `4` | 1–6 | as above — 🔒 |

- `max_root_orchestrators`: root orchestrators running concurrently under the
  UIA root (background and synchronous).
- `max_sub_orchestrators`: sub-orchestrators running concurrently per root
  orchestrator tree.
- `max_sub_orchestrator_depth`: nesting of sub-orchestrators (directly under
  the root orchestrator = 1).
- `max_spawn_depth`: general child depth under the root session
  (`ChildLimits::max_depth`).

Invalid values are **clamped** into the allowed range, never rejected.
Additionally, `max_sub_orchestrator_depth + 1 ≤ max_spawn_depth` holds (the
sub-depth is capped if needed, with a log warning). Home and profile set
freely, including upward; an untrusted project may only lower any number — an
attempt to raise one is ignored and reported as a `ScopeDiagnostic`.

```toml
[agents]
max_root_orchestrators = 1
max_sub_orchestrators = 2
max_sub_orchestrator_depth = 2
max_spawn_depth = 4
```

### 1.22 `[shell]` (`shell_limits.rs`, `ShellToml`)

| TOML path | Type | Default | Allowed | Merge today |
|---|---|---|---|---|
| `shell.max_timeout_secs` | `Option<u64>` | `900` | 30–3600 | `MinBound` for the untrusted project (`merge_shell_limits`) — 🔒 |

Upper bound on `timeout_secs` for a `shell.exec` call (sandbox, host and
host-mode requests). Without `timeout_secs`, 30s still applies, 600s for
build and test commands (`cargo`, `make`, `npm`, `pytest`, `go`, …) — both
capped by `max_timeout_secs`. The process's CPU limit scales with the time
limit. Merge behavior is the same as `[agents]`.

```toml
[shell]
max_timeout_secs = 900
```

**Total documented `HarnessConfig` fields at the time of the original
analysis: 111** — leaf fields including nested types such as
`McpPrincipalToml`, `RuleToml`, `CargoSandboxToml`/`TmuxSandboxToml`,
`InternalModelChoice`, `OnboardingSeen`; plus `base_dir` as an extra table row
in Section 1.1, which is **not a TOML field** (`#[serde(skip)]`, so not
counted). The current, authoritative count is `FIELD_TABLE.len()` in
`harw-config/src/scope.rs` (114 at last check; it grows as fields are added
and this document is not re-counted on every addition).

Parsed from the same `config.toml`, with the same layer mechanics, but
outside `HarnessConfig` itself: `[network]` (3 fields), `[browser]`
(5 fields), `[dod]` (4 fields), `[web]` (3 fields) — see Section 3. These four
sections are **not** `HarnessConfig` fields (`harness_config.rs` does not
reference them), but are extracted per layer from the same file
(`extract_section`) and have a **different, already sounder** merge behavior
(Section 0, point 1: sticky instead of reset). Provider/model/secret catalogs
(`providers/*.toml`, `models/*.toml`, `auth.toml`) are likewise **not**
`HarnessConfig` fields, but their own `ResolvedConfig` maps with file-based
merging (`discover_flat_dir`: the last layer file with the same name wins
outright; `auth.toml` is replaced wholesale per layer). The only actual
"provider/model fields" *inside* `HarnessConfig` are `default_provider`,
`default_model`, `uia_provider`, `uia_model`, `uia_worker_model`,
`active_agent_definition`, `active_uia_definition`, `policy_profile` (already
listed in 1.1).

---

## 2. Scope proposal per field

Legend: 🔒 = flagged as security-/restriction-relevant (approvals, sandbox,
network, secrets, listener, permissions).

**Status: table finalized and implemented (2026-09-21).** All rows previously
marked **OPEN** are resolved below and marked "(decided 2026-09-21)". The
"Merge rule" column refers to the exact `MergeRule` variants from Section 6;
the complete, field-exact mapping of all 89 fields is in Section 6.3 (the
grouping here still collapses structurally identical sub-fields into one row).
This table now matches `FIELD_TABLE` in `harw-config/src/scope.rs`.

| TOML path | Scope | Merge rule | Rationale | 🔒 |
|---|---|---|---|---|
| `config_version` | No scope — **validated per file, not merged** (decided 2026-09-21) | `PerFileValidated` | Every layer `config.toml` declares its own `config_version` for migration/compatibility control; it is not merged across layers, but checked per file against the supported schema version(s). `resolved.harness.config_version` then simply takes the value of the last-loaded trusted layer (today's `cfg`-assignment behavior is unchanged here), but is not a "merged" value in the sense of this model. | |
| `workspace_root` | **PROFILE** (decided 2026-09-21) | `ProfileReplaces` | Every profile needs its own working context. The path-traversal risk is not a merge-scope question — it is caught by the existing sandbox/root validation at runtime setup; this field only describes the *preferred* root, not a standalone permission boundary. | 🔒 |
| `default_provider` | PROFILE | last explicitly set value wins (already correctly implemented) | Active model choice is everyday profile business, not a restriction. | |
| `default_model` | PROFILE | last explicitly set value wins (already correctly implemented) | see above | |
| `active_agent_definition` | PROFILE | last explicitly set value wins | Choosing an agent definition is user preference. | |
| `active_uia_definition` | PROFILE | last explicitly set value wins (already correctly implemented) | see above | |
| `uia_provider` | PROFILE | last explicitly set value wins | Pinning is profile-specific. | |
| `uia_model` | PROFILE | last explicitly set value wins | see above | |
| `uia_worker_model` | PROFILE | last explicitly set value wins | Pinning the uia-worker role family is profile-specific, like `uia_model`. | |
| `policy_profile` | **GLOBAL** (decided 2026-09-21) | `GlobalOnly` | An admin enforces one policy family centrally; a profile cannot bypass it. No consumer found in code so far (only declared and settable via CLI, `harw-cli/src/settings.rs`) — the decision applies preemptively for when a consumer appears. | 🔒 |
| `project_root_markers` | **PROFILE** (decided 2026-09-21) | `ProfileReplaces` | Pure root-detection heuristic with no security effect of its own — the actual sandbox boundary is drawn by `sandbox.*` (GLOBAL exclusive) and `permissions.extra_roots` (GLOBAL upper bound), not root *detection*. A profile may freely adjust its own project detection. | |
| `logging.level` | PROFILE | last explicitly set value wins | Output verbosity, not a restriction. | |
| `logging.target_module_paths` | PROFILE | last explicitly set value wins | see above | |
| `logging.json` | PROFILE | last explicitly set value wins | see above | |
| `tui.theme` | PROFILE | last explicitly set value wins | Pure UI preference. | |
| `tui.keybindings_file` | PROFILE | last explicitly set value wins | see above | |
| `tui.child_stream` | PROFILE | `ProfileReplaces` | Purely presentational (live stream of child agents), not a restriction. | |
| `session.store_dir` | PROFILE | last explicitly set value wins | Storage path is profile-local. | |
| `session.journal_format` | PROFILE | last explicitly set value wins | Pure format choice. | |
| `session.retention_days` | **GLOBAL as an upper bound (minimum)** (decided 2026-09-21) | `MinBound` | An admin enforces a maximum retention as a compliance guardrail; the profile may only retain for less time, never longer than globally allowed. | |
| `session.title_generation` | PROFILE | last explicitly set value wins | Convenience feature. | |
| `session.title_model` | PROFILE | last explicitly set value wins | Legacy model choice, like `internal_models`. | |
| `policy.default_visibility_scope` | **GLOBAL, only a narrower value applies** (decided 2026-09-21) | `StricterOf(ordering)` — minimal ordering `"self"` < `"everyone"` (Section 6.2); any third/unknown string is rejected rather than ordered (decided 2026-09-21, risk R1, Section 8; regression test Section 7h #24) | Security-relevant (default visibility of new sessions), but as a free string with no `validate()` restriction in code, only partially orderable — only the two values actually used in code (`harness_config.rs:438-440`, `discovery.rs:1428`) are unambiguously orderable. | 🔒 |
| `policy.require_approval_for` | GLOBAL (baseline) + profile extends | **Union** | Directly the example given by the user; already implemented identically for the untrusted project layer (`merge_restricted_harness:958-964`) — the same pattern carried over to GLOBAL/PROFILE. | 🔒 |
| `mcp_listener.enabled` | GLOBAL (baseline) + profile may only narrow | **AND** (analogous to `browser.enabled`, `merge_restricted_browser:912-914`) | Opens a local network attack surface; a profile may not enable a globally disabled listener. | 🔒 |
| `mcp_listener.listen_addr` | GLOBAL | global wins | Bind address is attack surface; the profile should not be able to move it (analogous to `web.bind`, which a repo can never influence). | 🔒 |
| `mcp_listener.path` | GLOBAL | global wins | see above | 🔒 |
| `mcp_listener.principals` (incl. `id`/`credential_ref`/`tenant`/`workspace`/`job_capabilities`) | **GLOBAL, profile may only remove** (decided 2026-09-21) | `Intersection` keyed by `id` (decided 2026-09-21, risk R2, Section 8) — sub-fields `id`/`credential_ref`/`tenant`/`workspace`/`job_capabilities` are `CompositeMember` (Section 6.3) | Contains credential references and authorization capabilities; a profile may remove individual principals by leaving their `id` out of the list, but may never add a new `id` or change fields of an existing principal entry (e.g. extend `job_capabilities`) — only principals whose `id` matches exactly in the global list may appear in the profile result, and their fields come exclusively from the global version (no field-level merge within one principal entry). | 🔒 |
| `onboarding.seen.*` | PROFILE | last explicitly set value wins (already correctly implemented, but as a whole section rather than per field) | Pure first-run progress per profile. | |
| `tools.plan.enabled` | PROFILE | last explicitly set value wins | Feature toggle, not access protection. | |
| `tools.plan.persist` | PROFILE | last explicitly set value wins | Storage behavior. | |
| `tools.plan.require_for_complex_work` | PROFILE | last explicitly set value wins | Workflow setting, not access protection. | |
| `tools.plan.validate_dependency_cycles` | GLOBAL (baseline) + profile may only tighten | **OR** (precedent: `merge_restricted_harness:990-992`) | Correctness/safety check; turning it off would be a relaxation. | |
| `tools.plan.validate_write_conflicts` | GLOBAL (baseline) + profile may only tighten | **OR** (precedent: `merge_restricted_harness:993-995`) | see above | |
| `tools.plan.max_nodes` | GLOBAL (baseline) + profile may only narrow | **Minimum** (precedent: `merge_restricted_harness:996-998`) | Resource/blow-up limit. | |
| `tools.plan.require_exploration_for` | PROFILE | last explicitly set value wins | Workflow fine-tuning. | |
| `tools.plan.exploration_ttl_secs` | PROFILE | last explicitly set value wins | Workflow fine-tuning. | |
| `tools.plan.max_expand_depth` | GLOBAL (baseline) + profile may only narrow | **Minimum** (precedent: `merge_restricted_harness:999-1001`) | Resource/blow-up limit. | |
| `mode.default` | PROFILE | last explicitly set value wins | Interaction mode is pure preference. | |
| `research.network_allow_hosts` | GLOBAL (baseline) + profile may only narrow | **Intersection** (precedent: `merge_restricted_harness:968-973`) | Network egress allowlist. | 🔒 |
| `research.cargo_registry_read` | GLOBAL (baseline) + profile may only narrow | **AND** (precedent: `merge_restricted_harness:974-976`) | Filesystem read access to the cargo cache. | 🔒 |
| `research.max_fetch_bytes` | GLOBAL (baseline) + profile may only narrow | **Minimum** (precedent: `merge_restricted_harness:977-980`) | Resource limit for network fetches. | 🔒 |
| `research.fetch_timeout_secs` | GLOBAL (baseline) + profile may only narrow | **Minimum** (precedent: `merge_restricted_harness:981-986`) | see above | 🔒 |
| `research.cache_ttl_secs` | PROFILE | last explicitly set value wins | Pure cache lifetime, not access protection (unlike the other `[research]` fields, inconsistently untreated today — see Finding 3). | |
| `permissions.default_mode` | **GLOBAL as an upper bound** (decided 2026-09-21) | `StricterOf(ordering)` — `ask` > `auto` > `full` (Section 6.2) | `ask` always asks (safest), `full` never asks (most open). The ranking was not encoded anywhere in code before (`ALLOWED_MODES`, `permissions_toml.rs:17`, is only an unordered value list); this document establishes the strictness ordering for the first time. A profile may only tighten toward `ask`, never relax toward `full`. | 🔒 |
| `permissions.approval_timeout_secs` | GLOBAL (baseline) + profile may only narrow | **Minimum** | A shorter timeout is more conservative (auto-rejection kicks in sooner); analogous to `research.*_secs`. | 🔒 |
| `permissions.allow` | **GLOBAL, intersection** (decided 2026-09-21) | `Intersection` | `allow` rules bypass the approval prompt — the opposite of `require_approval_for`. A union would be a **relaxation**; the intersection is correct: a profile can open nothing new, it can only fail to reference globally allowed rules (effectively removing them). | 🔒 |
| `permissions.deny` | GLOBAL (baseline) + profile extends | **Union** | Counterpart to `allow`: more `deny` rules only mean more rejections, so safe to union — same pattern as `require_approval_for`. | 🔒 |
| `permissions.extra_roots` | **GLOBAL, intersection** (decided 2026-09-21) | `Intersection` | Extends allowed working roots — like `permissions.allow`, a potential rights expansion; same intersection logic: a profile can only reference a subset of the globally set roots, never open a new one. | 🔒 |
| `sandbox.cargo.*` | GLOBAL exclusive | profile may not set/override | Trust anchor for the cargo sandbox; module docs (`harness_config.rs:70-74`) explicitly require these values to be read only from the trusted configuration at runtime setup. Direct analogy to `browser.geckodriver_path`/`geckodriver_sha256` and `dod.proof_key_dir`, which are already never taken from a repo layer (`browser_toml.rs:18-25`, `dod_toml.rs:22-31`). | 🔒 |
| `sandbox.tmux.*` | GLOBAL exclusive | profile may not set/override | see above | 🔒 |
| `internal_models.*` (all 12 fields, including `auto_classifier`) | PROFILE | last explicitly set value wins (already correctly implemented) | Model routing for auxiliary tasks is user preference/cost control, not an access restriction. | |
| `compaction.absolute_ceiling_tokens` | **GLOBAL as an upper bound (minimum)** (decided 2026-09-21) | `MinBound` | An admin enforces a cost ceiling; the profile may only lower it, never exceed the global limit. | |
| `reasoning.*` (all 6 fields) | PROFILE | last explicitly set value wins | Reasoning effort is a cost/speed trade-off, not an access restriction. | |
| `guards.enabled` | GLOBAL (baseline) + profile may only tighten | **OR** (`true` wins) | Safety/stability guard; turning it off would be a relaxation, analogous to `tools.plan.validate_*`. | |
| `guards.repeated_failure_warn` | GLOBAL (baseline) + profile may only narrow | **Minimum** | A lower threshold is a more sensitive (stricter) guard. | |
| `guards.repeated_failure_abort` | GLOBAL (baseline) + profile may only narrow | **Minimum** | see above | |
| `guards.no_progress_rounds_warn` | GLOBAL (baseline) + profile may only narrow | **Minimum** | see above | |
| `guards.no_progress_rounds_abort` | GLOBAL (baseline) + profile may only narrow | **Minimum** | see above | |
| `guards.plan_stale_rounds` | GLOBAL (baseline) + profile may only narrow | **Minimum** | see above | |
| `host.sudo_session_minutes` | **GLOBAL as an upper bound (minimum)** | `MinBound` | How long a root password is remembered; a profile may only shorten it. | 🔒 |
| `uia_worker_models.*` (5 fields) | PROFILE | `ProfileReplaces` | Model choice per UIA-worker role is user preference, like `uia_worker_model`. | |
| `agents.*` (4 fields) | PROFILE, untrusted project may only lower | `MinBound` | Cost and blow-up limit for orchestration; home and profile set freely, including upward. | 🔒 |
| `shell.max_timeout_secs` | PROFILE, untrusted project may only lower | `MinBound` | Resource limit for shell commands; like `agents.*`. | 🔒 |
| `tools.doc.remote_ocr` | PROFILE, untrusted project may only tighten | `StricterOf` (`off` < `ask` < `on`) | Data egress to a remote OCR service; home and profile set freely, a project may never move from `ask` to `on`. | 🔒 |

---

## 3. Project layer

A third, **untrusted** layer already exists — `restricted_repo`
(`harw_home::LayerReport::untrusted_repo`, typically a `.harw/` inside a
cloned repository outside the user's control). It is processed **after** all
trusted layers and runs through a completely different code path
(`apply_restricted_layer`) than home/profile:

- Only `config.toml` is read, without following symlinks and with a 1 MiB
  cap (`read_restricted_file`, `MAX_RESTRICTED_CONFIG_BYTES`).
- `providers/`, `models/`, `auth.toml`, `.env`, `mcps/`, `plugins/`,
  `skills/`, `agents/`, `channels/`, `[mcp_listener]`,
  `default_provider`/`default_model`, `active_agent_definition`,
  `[policy].default_visibility_scope`, `[session]`, `[tui]`, `[logging]`,
  `[mode]`, and `[web]` are **never** carried over.
- For the remaining fields, strictly **monotone narrowing** applies: union
  for "more required" lists (`require_approval_for`), intersection for
  allowlists, minimum for positive numeric upper bounds (`min_positive`, `0`
  from the repo layer is never adopted), AND for "more permission needed"
  booleans, OR for "more checking" booleans.
- Only nine `HarnessConfig` fields are covered at all (Section 1: the rows
  marked **RESTRICTED PRECEDENT**), plus three/four fields each in
  `[network]`/`[browser]`/`[dod]`. `[mcp_listener]`, `[sandbox]`,
  `[permissions]`, `[guards]`, `[compaction]`, `[reasoning]` are **not
  covered at all** — not even against the untrusted project layer.

**Placement in the GLOBAL/PROFILE model:** the project layer sits
conceptually **below** PROFILE and gets, wholesale, the same "may only
narrow" contract GLOBAL gets against PROFILE (Section 2) — with the added
tightening that the (untrusted) project layer may never influence any
security-critical field (`permissions.allow`, `permissions.extra_roots`,
`sandbox.*`, `mcp_listener.*`), while (trusted) PROFILE can at least
reference a subset of those. This is implemented: the existing
`apply_restricted_layer` machinery was the direct precedent generalized into
`harw-config/src/scope.rs`/`merge.rs`, which now apply the full `FIELD_TABLE`
narrowing across all trusted layers, not just the untrusted project layer.

---

## 4. Settings lost under the pre-implementation merge model (historical)

Before this design was implemented, every field from Section 1 **except**
the five explicitly handled ones (`default_provider`, `default_model`,
`active_uia_definition`, `onboarding`, `internal_models`) was lost as soon as
a later trusted layer wrote a `config.toml` that did not itself contain that
field — the value fell back to the section default instead of the previous
layer's value. Since `ensure_home` **always** creates the profile
`config.toml` (with a hardcoded `[mcp_listener]` block), this affected
practically every installation as soon as the home layer
(`~/.harw/config.toml`) set any of the following fields differently from its
default:

- **All top-level scalars:** `config_version`, `workspace_root`,
  `active_agent_definition`, `uia_provider`, `uia_model`, `policy_profile`,
  `project_root_markers`.
- **`[logging]`, `[tui]`, `[session]`:** entirely (e.g. a globally set
  `logging.level = "debug"` would vanish again as soon as the profile wrote
  a `[mcp_listener]` without repeating `[logging]`).
- **`[policy]`:** `default_visibility_scope` **and** `require_approval_for`.
  A globally set approval requirement (e.g.
  `require_approval_for = ["shell.exec"]`) would fall back to `[]` as soon as
  the profile layer did not rewrite `[policy]`.
- **`[mcp_listener]`:** entirely, including `principals` (credential
  references!) — practically moot here since the profile template always
  sets the section itself anyway, but an admin adding extra `principals` at
  the home layer would lose them the moment the profile wrote its own
  `[mcp_listener]` without those principals.
- **`[tools.plan]`, `[mode]`, `[research]`, `[permissions]`, `[sandbox]`:**
  entirely — including security-critical fields like
  `permissions.allow`/`deny`/`extra_roots` and
  `sandbox.cargo`/`sandbox.tmux` (the sandbox's trust anchor).
- **`[compaction]`, `[reasoning]`, `[guards]`:** entirely.

Not affected (because fully or partially guarded against exactly this
problem): `default_provider`, `default_model`, `active_uia_definition`,
`onboarding.*` (as a whole), `internal_models.*` (per model slot), and —
structurally different, because parsed outside `HarnessConfig` — `[network]`,
`[browser]`, `[dod]`, `[web]` (these stayed "sticky": a layer that doesn't
write the section never overwrites the previous value).

---

## 5. Implementation sketch (carried out)

This is the plan that was implemented into `harw-config/src/scope.rs` and
`harw-config/src/merge.rs`:

1. Declare scope per field, not as a Rust attribute (scattered, hard to
   audit), but as **one central table**
   (`FieldScope { path: &str, scope: Global|Profile, merge: LastWins|Union|
   Intersection|Min|Max|And|Or|GlobalOnly }`) in `harw-config`, analogous to
   the `NEW_SECTION_KEYS`/`ALLOWED_MODES` constants — one review site for
   every future field addition.
2. `discover_config_with_restricted` replaces the old step 3
   (`resolved.harness = cfg`) with a generic
   `merge_layer_into(&mut resolved.harness, cfg, &fields, layer_kind)` that
   applies the matching rule per table entry instead of the old five-field
   special list — structure already present in `merge_restricted_harness`,
   just generalized and applied to **every** trusted layer, not only the
   untrusted project layer.
3. `apply_restricted_layer` becomes a special case of the same function with
   `layer_kind = Project` plus a stricter policy for the security-critical
   `OPEN` fields (Section 3).
4. Warn on load (`tracing::warn!`) when a PROFILE or project layer tries to
   relax a `GlobalOnly`/"may only tighten" field (value rejected by the merge
   rule instead of adopted) — a diagnostic analogous to `ConfigDiagnostic`,
   not fatal, but visible in `resolved.diagnostics`.

---

## 6. MergeRule taxonomy

### 6.1 The `MergeRule` enum

Ten variants cover all 89 fields from the original analysis exactly
(derivation and field count per variant: Section 6.3). No further variant
(e.g. a separate `MaxBound`) is needed — every "profile may only allow
*more*" case is already `Union`/`OrBool`, every "profile may only allow
*less*" case already `Intersection`/`MinBound`/`AndBool`. This enum is now
`harw_config::MergeRule` (`harw-config/src/scope.rs`).

```rust
/// Determines how a single `HarnessConfig` leaf field is merged across the
/// trusted layers (home → active profile), and — with the same variants but
/// restricted application (see Section 7c) — against an untrusted project
/// layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRule {
    /// The last **explicitly set** value wins. A layer that does not set the
    /// field leaves the previous layer's value unchanged (no reset to the
    /// section default — fixes the "RESET" regression from Section 4 for
    /// every field so classified). Never applied by the untrusted project
    /// layer.
    ProfileReplaces,
    /// Only the trusted home layer's value ever applies. A later layer
    /// (profile **or** untrusted project) that sets a *different* value is
    /// ignored and raises a [`ScopeDiagnostic`] warning; setting the same
    /// value again is a silent no-op.
    GlobalOnly,
    /// Union of every layer that sets the field (restricting lists: more
    /// entries are always safe — e.g. more approval requirements).
    Union,
    /// Intersection of every layer that sets the field, with the home
    /// value as the starting set (rights-expanding lists: a later layer can
    /// only remove entries, never add ones missing from the home layer).
    /// The comparison key for "is this the same entry" is fixed per field in
    /// `FIELD_TABLE`/the respective merge docs: for lists of primitive
    /// values (`String`, `PathBuf`) it is implicitly full value equality;
    /// for lists of structs it can be either full struct equality (e.g.
    /// `RuleToml` as a complete `(tool, pattern)` tuple, Section 6.3/1.11 —
    /// no partial match on `tool` alone) or a single identity field (e.g.
    /// `McpPrincipalToml` keyed on `id` alone, Section 6.3/1.6, decided
    /// 2026-09-21/R2). With a narrower identity field, the surviving
    /// entries always keep the full home version of the element — the
    /// other fields of a matching entry are never taken from a later layer,
    /// even if that layer sets the same key again with different values (no
    /// field-level merge within one list element). No separate `MergeRule`
    /// variant is needed — the same `Intersection` covers both variants,
    /// only the comparison key differs.
    Intersection,
    /// Numeric upper bound: the effective value is the minimum across every
    /// layer that sets the field (reuses the existing `min_positive`
    /// convention: `0`/the respective unset sentinel value from a later
    /// layer never lowers the bound further).
    MinBound,
    /// Boolean field where `true` is the **looser/permissive** value:
    /// effectively an AND across every layer that sets the field. A later
    /// layer may only turn it off (tighten), never on.
    AndBool,
    /// Boolean field where `true` is the **stricter/safer** value:
    /// effectively an OR across every layer that sets the field. A later
    /// layer may only turn it on (tighten), never off.
    OrBool,
    /// Ordinal value with an explicit strictness ordering recorded in
    /// `FieldScope::ordering` (strictest value first). Effectively the
    /// strictest value set by any layer that sets the field; an attempt to
    /// choose a looser value is ignored and warned about.
    StricterOf,
    /// No standalone merge: this field only travels as part of an enclosing
    /// atomic value (list element or point struct) that is itself merged
    /// under another field's rule. Exists so the exhaustiveness test
    /// (Section 7a) can demonstrably cover such fields too.
    CompositeMember,
    /// **Never** merged across layers: every layer validates its own value
    /// independently against the supported schema version(s) while loading
    /// that one file. Used only for `config_version`.
    PerFileValidated,
}

/// Where a field may come from, before `MergeRule` determines *how* several
/// layer values are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The home layer (`~/.harw`) sets the baseline; the profile is
    /// subordinate to it (details depend on `MergeRule`).
    Global,
    /// Applies only to the active profile; the profile layer replaces the
    /// global value there (`MergeRule::ProfileReplaces`).
    Profile,
    /// No scope in the GLOBAL/PROFILE sense — currently only
    /// `config_version` (`MergeRule::PerFileValidated`).
    NotScoped,
}

/// One entry of the central declaration table (Section 7a). The real
/// `FieldScope` in `harw-config/src/scope.rs` additionally carries
/// `intersection_key` and `security_critical`; both are omitted here for
/// brevity.
pub struct FieldScope {
    /// Dotted `HarnessConfig` path, exactly as in Section 1/6.3, e.g.
    /// `"mcp_listener.enabled"` or `"internal_models.session_title"`.
    pub path: &'static str,
    pub scope: Scope,
    pub merge: MergeRule,
    /// Strictness ordering for `MergeRule::StricterOf`, strictest value
    /// first. `None` for every other rule.
    pub ordering: Option<&'static [&'static str]>,
}
```

### 6.2 Explicit strictness orderings for `StricterOf`

Three fields use `StricterOf`; all orderings are deliberately **newly
established**, since they were not encoded anywhere in the code before this
design:

- **`permissions.default_mode`**: `["ask", "auto", "full"]` (index 0 =
  strictest value). `ask` asks on every action — no automatic approval
  possible, hence the safest setting. `full` never asks — the most open
  setting. `auto` sits between them (only asks where
  `policy.require_approval_for`/`permissions.deny` requires it). Source of
  the value set: `ALLOWED_MODES` (`permissions_toml.rs:17`); the order there
  is purely declarative and carried no meaning before — this document
  establishes the strictness ordering normatively for the first time.
- **`policy.default_visibility_scope`**: `["self", "everyone"]` (index 0 =
  strictest value), but **only a partial ordering**: these are the only two
  values actually used in code (`default_visibility_scope()` returns
  `"self"`; `"everyone"` appears as the only alternative value, in a test
  fixture). There is **no** `validate()` restriction to a fixed value set
  (unlike `permissions.default_mode`), so there is no complete ordering for
  an arbitrary third string. **Decided (2026-09-21, risk R1, Section 8):**
  the ordering stays exactly this two-value partial order; for any value
  outside `{"self", "everyone"}`, `stricter_of` falls back to `GlobalOnly`
  behavior (no comparison attempted — the deviating layer value is ignored
  and raises a `ScopeDiagnostic`), so a third value is never silently sorted
  into the ordering; covered by test #24 (Section 7h).
- **`tools.doc.remote_ocr`**: `["off", "ask", "on"]` (index 0 = strictest
  value, `TOOLS_DOC_REMOTE_OCR_ORDER`, same order as the `RemoteOcrMode`
  variants). Unlike the two fields above, trusted layers (home and profile)
  set it freely, including loosening it; only the untrusted project layer is
  restricted to stricter values (`merge_tools_doc`, same pattern as
  `merge_shell_limits`). The value set is closed by serde, so there is no
  unordered fallback case.

### 6.3 Complete field-by-field mapping (all fields from the original 111-field count)

One row per leaf field from Section 1, in the same order and with the same
subsection numbers, so the table can be checked 1:1 against Section 1.
"Scope" = `Scope` variant, "Rule" = `MergeRule` variant.

**1.1 Top level** (11 fields)

| Path | Scope | Rule |
|---|---|---|
| `config_version` | NotScoped | `PerFileValidated` |
| `workspace_root` | Profile | `ProfileReplaces` |
| `default_provider` | Profile | `ProfileReplaces` |
| `default_model` | Profile | `ProfileReplaces` |
| `active_agent_definition` | Profile | `ProfileReplaces` |
| `active_uia_definition` | Profile | `ProfileReplaces` |
| `uia_provider` | Profile | `ProfileReplaces` |
| `uia_model` | Profile | `ProfileReplaces` |
| `uia_worker_model` | Profile | `ProfileReplaces` |
| `policy_profile` | Global | `GlobalOnly` |
| `project_root_markers` | Profile | `ProfileReplaces` |

**1.2 `[logging]`** (3): `logging.level`, `logging.target_module_paths`,
`logging.json` — all Profile / `ProfileReplaces`.

**1.3 `[tui]`** (3): `tui.theme`, `tui.keybindings_file`, `tui.child_stream`
— all Profile / `ProfileReplaces`.

**1.4 `[session]`** (5 fields)

| Path | Scope | Rule |
|---|---|---|
| `session.store_dir` | Profile | `ProfileReplaces` |
| `session.journal_format` | Profile | `ProfileReplaces` |
| `session.retention_days` | Global | `MinBound` |
| `session.title_generation` | Profile | `ProfileReplaces` |
| `session.title_model` | Profile | `ProfileReplaces` |

**1.5 `[policy]`** (2 fields)

| Path | Scope | Rule |
|---|---|---|
| `policy.default_visibility_scope` | Global | `StricterOf` (`["self","everyone"]`, partial order, see 6.2) |
| `policy.require_approval_for` | Global | `Union` |

**1.6 `[mcp_listener]`** (9 fields)

| Path | Scope | Rule |
|---|---|---|
| `mcp_listener.enabled` | Global | `AndBool` (`true` = listener active = looser value) |
| `mcp_listener.listen_addr` | Global | `GlobalOnly` |
| `mcp_listener.path` | Global | `GlobalOnly` |
| `mcp_listener.principals` | Global | `Intersection` (comparison key: `id` alone, not the full struct — decided 2026-09-21, risk R2; `GlobalOnly` applied here before that) |
| `mcp_listener.principals[].id` | Global | `CompositeMember` (identity field of the `Intersection` comparison for `mcp_listener.principals`, see above) |
| `mcp_listener.principals[].credential_ref` | Global | `CompositeMember` (travels with the element; on a matching `id` the global version always wins, no field-level merge) |
| `mcp_listener.principals[].tenant` | Global | `CompositeMember` (see above) |
| `mcp_listener.principals[].workspace` | Global | `CompositeMember` (see above) |
| `mcp_listener.principals[].job_capabilities` | Global | `CompositeMember` (see above) |

**1.7 `[onboarding]`** (4 fields)

| Path | Scope | Rule |
|---|---|---|
| `onboarding.seen` (table) | Profile | `ProfileReplaces` — **atomic at the section level**, as today: not per flag, the whole `OnboardingSeen` struct is replaced together whenever the layer contains `[onboarding]` at all |
| `onboarding.seen.provider` | Profile | `CompositeMember` (part of the atomic `onboarding.seen` struct, see above) |
| `onboarding.seen.model` | Profile | `CompositeMember` |
| `onboarding.seen.channel` | Profile | `CompositeMember` |

**1.8 `[tools.plan]`** (9 fields)

| Path | Scope | Rule |
|---|---|---|
| `tools.plan.enabled` | Profile | `ProfileReplaces` |
| `tools.plan.persist` | Profile | `ProfileReplaces` |
| `tools.plan.require_for_complex_work` | Profile | `ProfileReplaces` |
| `tools.plan.validate_dependency_cycles` | Global | `OrBool` (`true` = check active = stricter value) |
| `tools.plan.validate_write_conflicts` | Global | `OrBool` |
| `tools.plan.max_nodes` | Global | `MinBound` |
| `tools.plan.require_exploration_for` | Profile | `ProfileReplaces` |
| `tools.plan.exploration_ttl_secs` | Profile | `ProfileReplaces` |
| `tools.plan.max_expand_depth` | Global | `MinBound` |

**1.8a `[tools.doc]`** (1): `tools.doc.remote_ocr` — Profile / `StricterOf`
(`off` < `ask` < `on`, untrusted project may only tighten), security-critical.

**1.9 `[mode]`** (1): `mode.default` — Profile / `ProfileReplaces`.

**1.10 `[research]`** (5 fields)

| Path | Scope | Rule |
|---|---|---|
| `research.network_allow_hosts` | Global | `Intersection` |
| `research.cargo_registry_read` | Global | `AndBool` (`true` = read access allowed = looser value) |
| `research.max_fetch_bytes` | Global | `MinBound` |
| `research.fetch_timeout_secs` | Global | `MinBound` |
| `research.cache_ttl_secs` | Profile | `ProfileReplaces` |

**1.11 `[permissions]`** (7 fields)

| Path | Scope | Rule |
|---|---|---|
| `permissions.default_mode` | Global | `StricterOf` (`["ask","auto","full"]`, see 6.2) |
| `permissions.approval_timeout_secs` | Global | `MinBound` |
| `permissions.allow` | Global | `Intersection` |
| `permissions.deny` | Global | `Union` |
| `permissions.extra_roots` | Global | `Intersection` |
| `RuleToml.tool` (field of `allow[]`/`deny[]`) | — | `CompositeMember` (travels as part of the `RuleToml` list element under the respective list's rule — `Intersection` for `allow`, `Union` for `deny`; the comparison key is the full `(tool, pattern)` tuple) |
| `RuleToml.pattern` | — | `CompositeMember` |

**1.12 `[sandbox]`** (8 fields) — all Global / `GlobalOnly`:
`sandbox.cargo`, `sandbox.cargo.mode`, `sandbox.cargo.cargo_bin`,
`sandbox.cargo.rustup_home`, `sandbox.cargo.cargo_home`, `sandbox.tmux`,
`sandbox.tmux.mode`, `sandbox.tmux.socket_path`.

**1.13 `[internal_models]`** (13 fields, including `auto_classifier`)

| Path | Scope | Rule |
|---|---|---|
| `internal_models.use_openrouter_defaults` | Profile | `ProfileReplaces` |
| `internal_models.session_title` | Profile | `ProfileReplaces` (atomic per slot, as today's `merge_internal_models`) |
| `internal_models.compaction_summary` | Profile | `ProfileReplaces` |
| `internal_models.memory_consolidation` | Profile | `ProfileReplaces` |
| `internal_models.dream_reflection` | Profile | `ProfileReplaces` |
| `internal_models.explorer` | Profile | `ProfileReplaces` |
| `internal_models.research` | Profile | `ProfileReplaces` |
| `internal_models.worker_simple` | Profile | `ProfileReplaces` |
| `internal_models.worker_complex` | Profile | `ProfileReplaces` |
| `internal_models.root_orchestrator` | Profile | `ProfileReplaces` |
| `internal_models.sub_orchestrator` | Profile | `ProfileReplaces` |
| `internal_models.auto_classifier` | Profile | `ProfileReplaces` |
| `InternalModelChoice.provider`/`.model` (field of each slot) | — | `CompositeMember` (travels as part of the atomic `Option<InternalModelChoice>` of that slot, see above; never merged individually) |

**1.14 `[compaction]`** (2): `compaction.absolute_ceiling_tokens` — Global /
`MinBound`; `compaction.max_history_bytes` — Profile / `ProfileReplaces`.

**1.15 `[reasoning]`** (6): `reasoning.uia`, `.root_orchestrator`,
`.root_orchestrator_with_subs`, `.sub_orchestrator`, `.worker_complex`,
`.worker_simple` — all Profile / `ProfileReplaces`.

**1.16 `[guards]`** (6 fields)

| Path | Scope | Rule |
|---|---|---|
| `guards.enabled` | Global | `OrBool` (`true` = guard active = stricter value) |
| `guards.repeated_failure_warn` | Global | `MinBound` |
| `guards.repeated_failure_abort` | Global | `MinBound` |
| `guards.no_progress_rounds_warn` | Global | `MinBound` |
| `guards.no_progress_rounds_abort` | Global | `MinBound` |
| `guards.plan_stale_rounds` | Global | `MinBound` |

**1.17 `[knowledge]`** (1): `knowledge.diary.retention_days` — Profile /
`ProfileReplaces`.

**1.18 `[dream]`** (5): `dream.enabled`, `.budget`, `.idle_minutes`,
`.cooldown_minutes`, `.schedule` — all Profile / `ProfileReplaces`, none
security-critical (`scope.rs`, `FIELD_TABLE`).

**1.19 `[host]`** (1): `host.sudo_session_minutes` — Global / `MinBound`,
security-critical.

**1.20 `[uia_worker_models]`** (5): `uia_worker_models.uia_worker`,
`.uia_shell_worker`, `.uia_writer`, `.uia_latex_writer`, `.uia_explorer` —
all Profile / `ProfileReplaces`.

**1.21 `[agents]`** (4): `agents.max_root_orchestrators`,
`.max_sub_orchestrators`, `.max_sub_orchestrator_depth`, `.max_spawn_depth` —
all Profile / `MinBound`, security-critical.

**1.22 `[shell]`** (1): `shell.max_timeout_secs` — Profile / `MinBound`,
security-critical.

**Distribution as implemented (`FIELD_TABLE`, `harw-config/src/scope.rs`,
verified by `scope.rs`'s own control-sum test):** `ProfileReplaces` and
`MinBound` are the two largest groups (the profile fields and the global
upper bounds respectively), followed by `GlobalOnly`, `CompositeMember`,
`Intersection`, `OrBool`, `Union`, `AndBool`, `StricterOf`, and exactly one
`PerFileValidated` (`config_version`). The exact per-variant counts are
whatever `FIELD_TABLE.len()` and its variant breakdown say at any given
time — the growth history through several rounds of field additions
(`uia_worker_model`, `compaction.max_history_bytes`,
`internal_models.root_orchestrator`/`.sub_orchestrator`, then
`tui.child_stream`, `internal_models.auto_classifier`,
`uia_worker_models.*`, `host.sudo_session_minutes`, `agents.*`,
`shell.max_timeout_secs`, `tools.doc.remote_ocr`) is not repeated here; `scope.rs`'s own test is the
source of truth, not this document's historical counts.

---

## 7. Implementation specification (binding, now implemented)

Fully operationalizes the sketch from Section 5; where the two disagree, this
section governs. This is the specification `harw-config/src/scope.rs` and
`harw-config/src/merge.rs` implement.

### 7a. Where the scope is declared

**One central, public table** `FIELD_TABLE: &[FieldScope]` in
`harw-config/src/scope.rs` (types: Section 6.1) — no Rust attribute scattered
per field, so there is one review site for every future field addition.

**Exhaustiveness guarantee:** a new `HarnessConfig` or section-struct field
without a `FIELD_TABLE` entry triggers a **compile error** (E0027 "pattern
does not mention field"), not just a runtime test failure. Mechanism: for
`HarnessConfig` itself, as well as for **every** section struct with named
fields (`LoggingSection`, `TuiSection`, `SessionSection`, `PolicySection`,
`McpListenerSection`, `McpPrincipalToml`, `OnboardingSection`,
`OnboardingSeen`, `ToolsSection` (i.e. `PlanToml`), `ModeSection`,
`ResearchSection`, `PermissionsSection`, `RuleToml`, `SandboxSection`,
`CargoSandboxToml`, `TmuxSandboxToml`, `InternalModelsToml`, per-slot
`InternalModelChoice`, `CompactionToml`, `ReasoningWeightsToml`,
`GuardsToml`), there is a test of this form:

```rust
#[test]
fn test_field_table_exhaustive_harness_config() {
    let HarnessConfig {
        config_version,
        workspace_root,
        default_provider,
        default_model,
        active_agent_definition,
        active_uia_definition,
        uia_provider,
        uia_model,
        uia_worker_model,
        policy_profile,
        logging: _,
        tui: _,
        session: _,
        policy: _,
        mcp_listener: _,
        onboarding: _,
        tools: _,
        mode: _,
        research: _,
        permissions: _,
        sandbox: _,
        project_root_markers,
        internal_models: _,
        compaction: _,
        reasoning: _,
        guards: _,
        base_dir: _, // #[serde(skip)], not a TOML field, no FIELD_TABLE row
    } = HarnessConfig::default();
    // No `..` — a new field on HarnessConfig not listed here is a compile
    // error (E0027), not a runtime probe.
    let _ = (
        config_version, workspace_root, default_provider, default_model,
        active_agent_definition, active_uia_definition, uia_provider,
        uia_model, uia_worker_model, policy_profile, project_root_markers,
    ); // avoid unused-variable warnings
    for path in [
        "config_version", "workspace_root", "default_provider",
        "default_model", "active_agent_definition", "active_uia_definition",
        "uia_provider", "uia_model", "uia_worker_model", "policy_profile",
        "project_root_markers",
    ] {
        assert!(
            FIELD_TABLE.iter().any(|f| f.path == path),
            "FIELD_TABLE is missing an entry for {path}"
        );
    }
}
```

The same pattern (struct destructuring without `..` plus a path-assert loop)
for each of the section structs listed above, each with its fully dotted
path (e.g. `"sandbox.cargo.mode"`, `"internal_models.session_title"`,
`"mcp_listener.principals"` — for `Vec`/`Option` fields only the container
path is destructured, not its elements; elements like
`RuleToml`/`McpPrincipalToml`/`InternalModelChoice` get their own, separate
destructuring test function). All these tests live in **package C**
(Section 7g), not scattered across the production files.

### 7b. New files, modules, types, function signatures

**New:** `harw-config/src/scope.rs` — `MergeRule`, `Scope`, `FieldScope`,
`FIELD_TABLE` (Abschnitt 6.1/6.3), rein deklarativ, keine Merge-Logik.

**New:** `harw-config/src/merge.rs` — the generic merge engine:

```rust
/// Coarse trust/layer context for this merge call (determines which
/// `MergeRule` variants take effect at all — table in Section 7c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRole {
    /// The first trusted layer (`~/.harw`, `layer_index == 0`). Every rule
    /// behaves identically to `ProfileReplaces` here — there is no GLOBAL
    /// prior state to narrow against yet.
    Baseline,
    /// Every further trusted layer (active profile, `layer_index >= 1`).
    Refinement,
    /// The untrusted project layer from `apply_restricted_layer`.
    UntrustedProject,
}

/// Modeled on [`ConfigDiagnostic`] (non-fatal, visible but does not block
/// startup), but carries the extra fields a scope violation needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDiagnostic {
    /// Dotted field path, e.g. `"mcp_listener.enabled"`.
    pub field: String,
    /// The `config.toml` that contained the relaxation attempt.
    pub file: String,
    /// The rejected value, `Debug`-formatted. Never a secret: every field
    /// that can trigger a `ScopeDiagnostic` is a non-secret
    /// scalar/enum/path/rule entry — `credential_ref` never participates,
    /// since `mcp_listener.principals` is `GlobalOnly`.
    pub rejected_value: String,
}

impl std::fmt::Display for ScopeDiagnostic { /* "{field} in {file}: …" */ }

/// Applies `incoming` (this layer's already fully deserialized
/// `HarnessConfig`, including its own section defaults for anything the
/// layer did not itself set) to `trusted` (the accumulated state so far)
/// according to `FIELD_TABLE` and `role`. `raw` is this layer's **raw**
/// `toml::Value`, not yet cleaned by `strip_new_sections`, and decides per
/// field, via a presence check (the `field_present` pattern, reused),
/// whether this layer actually set the field itself.
///
/// Replaces `resolved.harness = cfg` for trusted layers and
/// `merge_restricted_harness` for the untrusted project layer — both
/// callers differ only in the `role` they pass.
///
/// # Returns
/// Every `ScopeDiagnostic` produced by a rejected relaxation/expansion
/// attempt (Section 7c/7e). Empty if no layer value was rejected.
/// `tracing::warn!` also fires synchronously for each entry — the return
/// value is for `ResolvedConfig::scope_warnings` and tests, not the only
/// visibility channel.
pub fn merge_layer_into(
    trusted: &mut HarnessConfig,
    incoming: HarnessConfig,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) -> Vec<ScopeDiagnostic>;
```

Internally, `merge_layer_into` is **not** a reflective generic — Rust has no
runtime reflection over heterogeneous struct fields. It is a sequence of
hand-written section helpers (`merge_top_level`, `merge_logging`,
`merge_tui`, `merge_session`, `merge_policy`, `merge_mcp_listener`,
`merge_onboarding`, `merge_tools_plan`, `merge_mode`, `merge_research`,
`merge_permissions`, `merge_sandbox`, `merge_internal_models` (extends the
existing function, see below), `merge_compaction`, `merge_reasoning`,
`merge_guards`), each
`fn(trusted: &mut X, incoming: X, raw: &toml::Value, role: LayerRole) -> Vec<ScopeDiagnostic>`,
which for each of its fields calls the matching **generic rule
application** — defined once in `merge.rs`, reused by every section helper:

```rust
fn profile_replaces<T: Clone>(trusted: &mut T, incoming: T, present: bool, role: LayerRole);
fn global_only<T: Clone + PartialEq + std::fmt::Debug>(
    trusted: &mut T, incoming: T, present: bool, role: LayerRole,
    field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>,
);
fn union_list<T: Clone + PartialEq>(trusted: &mut Vec<T>, incoming: &[T], present: bool, role: LayerRole);
fn intersection_list<T: Clone + PartialEq + std::fmt::Debug>(
    trusted: &mut Vec<T>, incoming: &[T], present: bool, role: LayerRole,
    field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>,
);
fn min_bound<T: Ord + Default + Copy + std::fmt::Debug>(
    trusted: &mut T, incoming: T, present: bool, role: LayerRole,
    field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>,
); // internally reuses the existing `min_positive`
fn and_bool(trusted: &mut bool, incoming: bool, present: bool, role: LayerRole, field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>);
fn or_bool(trusted: &mut bool, incoming: bool, present: bool, role: LayerRole, field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>);
fn stricter_of(
    trusted: &mut String, incoming: String, present: bool, role: LayerRole,
    ordering: &[&str], field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>,
);
```

Each helper receives `present` already computed by its caller via
`field_present(raw, &[...])` (no helper reads `raw` itself). With
`role == LayerRole::Baseline`, **every** helper immediately returns
`profile_replaces` behavior (Section 7c) — implemented as an early
`if role == LayerRole::Baseline { ...; return; }` at the top of every helper
except `profile_replaces` itself (which is identical for `Baseline` anyway).

**Replacing the old `resolved.harness = cfg`:**

```rust
// before:
// cfg.base_dir = Some(base.clone());
// resolved.harness = cfg;

// after:
cfg.base_dir = Some(base.clone());
let role = if layer_index == 0 { LayerRole::Baseline } else { LayerRole::Refinement };
let mut warnings = merge_layer_into(&mut resolved.harness, cfg, &fields, role, base);
resolved.scope_warnings.append(&mut warnings);
```

The five special-case `if` blocks and `merge_internal_models` were **not**
simply dropped here — they became the concrete `ProfileReplaces`
implementations for exactly those five fields inside
`merge_top_level`/`merge_onboarding`/`merge_internal_models` (details:
Section 7f). `merge_internal_models` itself remains a function (signature
unchanged) and is called by `merge.rs`'s `merge_internal_models` section
helper instead of inline in the discovery loop.

`ResolvedConfig` gained a new public field:

```rust
/// Rejected scope-relaxation attempts from `merge_layer_into` (Section 7e),
/// collected across all layers including the untrusted project layer. Kept
/// separate from `diagnostics` (dangling model/provider references) since
/// they are semantically different — both are non-fatal.
pub scope_warnings: Vec<ScopeDiagnostic>,
```

### 7c. Relationship to `merge_restricted_*`

**`merge_restricted_network`/`_browser`/`_dod`** remain **unchanged** — they
handle `[network]`/`[browser]`/`[dod]`, which live outside `HarnessConfig`
(Section 1) and hence outside this specification's scope. Unifying them
under the same `MergeRule` engine in the future would be a reasonable
follow-up, but is **not** part of this specification's work packages (see
risk R3, Section 8).

**`merge_restricted_harness`** was **removed** and replaced with the same
`merge_layer_into` call the trusted layers use — `apply_restricted_layer`
calls it with `role = LayerRole::UntrustedProject` instead of maintaining
its own 62-line function. `min_positive` continues to be imported from its
original location by `merge.rs` (not duplicated).

To make sure no existing protection was lost, each `MergeRule` variant
applies to `LayerRole::UntrustedProject` exactly as it does to
`LayerRole::Refinement` (trusted profile), **with exactly two exceptions**,
both already identical for Refinement/Baseline:

| `MergeRule` | `Refinement` (profile) | `UntrustedProject` |
|---|---|---|
| `ProfileReplaces` | adopt the value if set, else keep `trusted` | **never applied** — field stays untouched (matches the former exclusion list: `default_provider`/`default_model`, `active_agent_definition`, `[session]`, `[tui]`, `[logging]`, `[mode]`, etc.) |
| `GlobalOnly` | deviating value ignored + warned | identical: deviating value ignored + warned |
| `Union`/`Intersection`/`MinBound`/`AndBool`/`OrBool`/`StricterOf` | applies the rule monotonically narrowing (by construction never relaxing) | **identical** — the same monotone rule; since each of these six rules can by construction only narrow, applying it from an untrusted layer is exactly as safe as from a trusted profile |
| `CompositeMember` | travels with the parent value | travels with the parent value |
| `PerFileValidated` | validated per file | validated per file (the project layer may not smuggle in an unsupported `config_version` value either) |

**Consequence (a deliberate, documented expansion versus before):** the
previous coverage of nine fields for the untrusted layer
(`policy.require_approval_for`, 4× `research.*`, 4× `tools.plan.*`) grew to
**all 25 fields** with `Union`/`Intersection`/`MinBound`/`AndBool`/`OrBool`/
`StricterOf` (16 newly protected fields, including `permissions.deny`,
`guards.*`, `session.retention_days`, `compaction.absolute_ceiling_tokens`,
`permissions.approval_timeout_secs`). This is a pure expansion of
protection (never a relaxation), consistent with "no existing protection
may be lost" — but it was flagged in review explicitly as an intended
behavior change, not a side effect.

### 7d. Placement of a trusted project layer

This model has **no separate** "trusted project layer" (unlike what the
original task wording suggested) — the implementation only knows (1)
trusted layers (`~/.harw` → active profile, iterated as `layers: &[PathBuf]`)
and (2) the one untrusted project layer (`restricted_repo`). A "trusted
project layer" in the sense of a third, additional `config.toml` (e.g. a
project overlay managed by the user themselves, not the repo) does not exist
as its own concept in the code; **if** such a layer is introduced in the
future, the proposed approach ("like a profile, so below global") is
directly applicable: it would be handled as an additional entry in `layers`
with `role = LayerRole::Refinement`, exactly like a second profile layer —
the engine does not distinguish "profile" from "trusted project" anyway,
both are `Refinement` in precedence order. This is a readiness statement for
a possible future layer, not an implementation delivered here.

### 7e. Warning format

`ScopeDiagnostic` (Section 7b) — modeled on the existing `ConfigDiagnostic`
pattern (non-fatal finding with `Display`), but with the three required
fields `field`, `file`, `rejected_value` instead of `site`/`kind`/`reference`.

**Trigger:** any `MergeRule` application other than `ProfileReplaces` /
`CompositeMember` / `PerFileValidated`, where (a) the current layer
**explicitly set** the field per `field_present`, **and** (b) the resulting
effective value does **not** match what this layer set (the rule rejected
or bounded its attempt — whether a `GlobalOnly` deviation, an
`Intersection`/`Union` entry outside the allowed direction, a `MinBound`
violation, an `AndBool`/`OrBool` in the wrong direction, or a `StricterOf`
value looser than the current one).

**Immediate visibility:** every finding fires **synchronously** via
`tracing::warn!` with structured fields (project convention, see the
`CLAUDE.md` tracing standards):

```rust
tracing::warn!(
    field = %diagnostic.field,
    file = %diagnostic.file,
    rejected_value = %diagnostic.rejected_value,
    "scope loosening attempt ignored"
);
```

**Collected visibility:** additionally, every finding lands in
`ResolvedConfig::scope_warnings` (Section 7b), inspectable independently of
the tracing log (e.g. for a future `harw doctor`-style diagnostic output or
for tests).

### 7f. The five special cases that were already handled before

`default_provider`, `default_model`, `active_uia_definition`, `onboarding`,
`internal_models` are all classified as `ProfileReplaces` (Section 6.3) —
their previous behavior was not allowed to change. Concretely:

- `default_provider`/`default_model`/`active_uia_definition`: the
  `profile_replaces` implementation for exactly these three top-level
  fields **reproduces the existing pattern verbatim**
  (`if cfg.<field>.is_none() { cfg.<field> = resolved.harness.<field>.clone() }`)
  rather than being replaced by a newly, independently written generic
  implementation whose behavior would need to be proven from scratch by a
  test. Simplest approach: `merge_top_level` calls exactly this existing
  code for these three fields (extracted into a small private helper, not
  rewritten).
- `onboarding`: `merge_onboarding` reproduces exactly the old
  `if fields.get("onboarding").is_none() { cfg.onboarding = resolved.harness.onboarding.clone(); }`
  — section level, not flag level (Section 6.3, 1.7).
- `internal_models`: `merge_internal_models` (the existing function) is
  **reused unchanged**, only the call site moved from the discovery loop
  into `merge.rs`'s `merge_internal_models` section helper.

All **other** ~35 `ProfileReplaces` fields, by contrast, got a **newly
written** generic `profile_replaces` implementation — for them there was
previously **no** correct code (they were marked "RESET", Section 4); that
is the actual bug-fix effect of this work.

### 7g. Work packages (3, disjoint file sets)

| Package | Files | Content | Depends on |
|---|---|---|---|
| **A — Scope & merge engine** | `harw-config/src/scope.rs` (new), `harw-config/src/merge.rs` (new), `harw-config/src/lib.rs` (2 lines: `pub mod scope; pub mod merge;` plus re-exports of `MergeRule`, `Scope`, `FieldScope`, `FIELD_TABLE`, `LayerRole`, `ScopeDiagnostic`, `merge_layer_into`) | `MergeRule`/`Scope`/`FieldScope`/`FIELD_TABLE` (Section 6.1/6.3) + `LayerRole`/`ScopeDiagnostic`/`merge_layer_into` + all section helpers + generic rule helpers (Section 7b) | none |
| **B — Discovery integration** | `harw-config/src/discovery.rs` (only this file: add `use` lines for `crate::scope::*`/`crate::merge::*`; replace `resolved.harness = cfg`, Section 7b; switch `apply_restricted_layer` to `merge_layer_into(..., LayerRole::UntrustedProject, ...)`; remove `merge_restricted_harness`; add the `ResolvedConfig::scope_warnings` field and its population, Section 7b) | Wiring the engine into the existing discovery flow | A |
| **C — Tests** | `harw-config/tests/config_scope_merge.rs` (new; plus `harw-config/tests/config_scope_exhaustive.rs` if needed) | All tests from Section 7a (exhaustiveness) and 7h (merge behavior, regression), exclusively via the public API (`FIELD_TABLE`, `merge_layer_into`, `discover_config_with_restricted`) — no change needed to the A/B files since all involved fields are `pub` | A, B |

Order strictly A → B → C (each package needs the previous one's finished
API). No package overlaps another in its file set.

### 7h. Test list

**At least one test per `MergeRule` variant** (in `config_scope_merge.rs`,
via `merge_layer_into` directly or via two synthetic layers):

1. `ProfileReplaces`: profile does not set the field → home value is
   preserved (not the default) — the actual core proof of the bug fix.
2. `ProfileReplaces` (special case `internal_models`): profile sets only
   `session_title` → `compaction_summary` from home is preserved (proof
   that `merge_internal_models` is wired in unchanged).
3. `GlobalOnly`: profile attempts to change `sandbox.cargo.cargo_bin` →
   home value stays effective, `ScopeDiagnostic` with
   `field == "sandbox.cargo.cargo_bin"` produced.
4. `Union`: home sets `require_approval_for = ["shell.exec"]`, profile sets
   `["fs.write"]` → effectively both entries present.
5. `Intersection`: home sets `permissions.allow = [A, B]`, profile sets
   `[A, C]` → effectively only `[A]` (C is rejected + `ScopeDiagnostic`).
6. `MinBound`: home sets `guards.repeated_failure_warn = 2`, profile sets
   `5` → effectively `2`, `ScopeDiagnostic` produced; profile sets `1` →
   effectively `1`, no diagnostic.
7. `AndBool`: home sets `mcp_listener.enabled = false`, profile sets `true`
   → effectively `false`, `ScopeDiagnostic` produced.
8. `OrBool`: home sets `guards.enabled = true`, profile sets `false` →
   effectively `true`, `ScopeDiagnostic` produced.
9. `StricterOf`: home sets `permissions.default_mode = "auto"`, profile
   sets `"full"` → effectively `"auto"`, `ScopeDiagnostic` produced;
   profile sets `"ask"` → effectively `"ask"`, no diagnostic.
10. `CompositeMember`: a `RuleToml` element is only compared as a whole —
    two `allow` rules with the same `tool` but different `pattern` count as
    **different** entries in the `Intersection` (no partial match on `tool`
    alone).
11. `PerFileValidated`: a layer's `config_version` is not "inherited" from
    the previous layer — two consecutive layers with different
    `config_version` each keep their own value until validated (no merge
    effect).

**At least one test per security-critical (🔒) field — a relaxation is
ignored and warned about** (at minimum the following, depending on the
`MergeRule` shape; the remaining 🔒 fields from Section 2/6.3 follow the same
pattern analogously):

12. `mcp_listener.enabled`: home `false` → profile `true` rejected (see 7).
13. `mcp_listener.listen_addr`: home `"127.0.0.1:1337"` → profile
    `"0.0.0.0:1337"` rejected + `ScopeDiagnostic`.
14. `mcp_listener.principals`: profile tries to add an extra principal →
    ignored + `ScopeDiagnostic`, home list unchanged (since R2, decided
    2026-09-21: the rule is `Intersection` keyed by `id`, no longer
    `GlobalOnly` — removal and override cases are separately covered in
    tests 25–27).
15. `permissions.allow`: see 5.
16. `permissions.extra_roots`: home `["/a"]`, profile `["/a", "/b"]` →
    effectively `["/a"]`, `ScopeDiagnostic` for `/b`.
17. `permissions.default_mode`: see 9.
18. `sandbox.cargo.*`/`sandbox.tmux.*`: see 3.
19. `research.network_allow_hosts`: home `["docs.rs"]`, profile
    `["docs.rs", "evil.example"]` → effectively `["docs.rs"]`,
    `ScopeDiagnostic` for `evil.example`.

**Regression test** (in `config_scope_merge.rs`, via
`discover_config_with_restricted` with two real temp layer directories):

20. "a global `require_approval_for` survives a profile `config.toml`":
    the home `config.toml` sets `[policy] require_approval_for = ["shell.exec"]`;
    the profile `config.toml` sets `[mcp_listener]` (or similar) but
    **not** `[policy]` → `resolved.harness.policy.require_approval_for`
    still contains `"shell.exec"` (the exact case described in
    Section 0/4).

**Protection from the untrusted project layer is preserved:**

21. All nine cases previously covered by `merge_restricted_harness`
    (`policy.require_approval_for` union, 4× `research.*`, 4×
    `tools.plan.*` — Section 3/6.3) are reproduced 1:1 as tests against
    `discover_config_with_restricted(..., Some(&restricted_repo))` and must
    keep passing (no regression from switching `merge_restricted_harness`
    to `merge_layer_into`).
22. A field newly protected for the project layer per Section 7c (e.g.
    `permissions.deny`, `guards.repeated_failure_warn`): a project
    `.harw/config.toml` with a relaxation attempt is rejected — proof of
    the deliberate protection **expansion**.
23. `[mcp_listener]`/`sandbox.*` remain completely unreachable for the
    project layer, same as before.

**Tests made necessary by the R1/R2 decision of 2026-09-21:**

24. `StricterOf` fallback for `policy.default_visibility_scope` (R1): home
    sets `default_visibility_scope = "self"`, profile sets a third/unknown
    value (e.g. `"team"`) → effectively stays `"self"` (home value); the
    profile value is **not** sorted into the ordering (treated neither as
    stricter nor looser than `"self"`/`"everyone"`), but rejected like a
    `GlobalOnly` deviation — a `ScopeDiagnostic` with
    `field == "policy.default_visibility_scope"` and
    `rejected_value == "team"` is produced. The test must fail if a future
    implementation instead silently sorts the unknown value as "looser" or
    "stricter" (e.g. via a string comparison instead of an explicit
    `ordering` lookup).
25. `mcp_listener.principals` — removal (R2): home sets
    `principals = [P1, P2]` (different `id`s), profile sets
    `principals = [P1]` → effectively `[P1]`; `P2` is missing from the
    result, **no** `ScopeDiagnostic` (pure removal is allowed and not a
    relaxation attempt).
26. `mcp_listener.principals` — addition rejected (R2): home sets
    `principals = [P1]`, profile sets `principals = [P1, P3]` (`P3` with an
    `id` not present in home) → effectively `[P1]`; `P3` is rejected and
    raises a `ScopeDiagnostic` (`field == "mcp_listener.principals"`,
    `rejected_value` names the rejected `id`).
27. `mcp_listener.principals` — rights expansion rejected (R2): home sets
    `P1` with `job_capabilities = ["ReadOwn"]`, profile sets an entry with
    the same `id` `P1` but expanded `job_capabilities =
    ["ReadOwn", "CancelWorkspace"]` (or a different `credential_ref`/
    `tenant`/`workspace`) → effectively the full home version of `P1` wins
    unchanged (no field-level merge within the principal entry); a
    `ScopeDiagnostic` is produced since the profile's value for `P1`
    deviates from the effective result.

---

## 8. Risks and open points

**R1 — `policy.default_visibility_scope`: incomplete ordering — decided
2026-09-21.** The ordering `["self", "everyone"]` established in Section 6.2
covers only the two values actually used in code.
`PolicySection.default_visibility_scope` has no `validate()` restricting the
value set (unlike `permissions.default_mode`/`ALLOWED_MODES`) — a layer
could in theory set any arbitrary string. **Decision:** the fixed order
`"self" < "everyone"` is locked in and stays `MergeRule::StricterOf` with
exactly this ordering (Section 6.2/6.3) — for any value outside
`{"self", "everyone"}`, `stricter_of` falls back to `GlobalOnly` behavior
(no comparison is attempted; the deviating profile/project value is ignored
and raises a `ScopeDiagnostic`), instead of silently sorting it into the
ordering. Beyond the plain behavior decision, this required an explicit
**regression test** (Section 7h, test #24) that proves exactly this
fallback and fails if a future implementation instead wrongly treats a
third value as "looser" or "stricter". Restricting
`default_visibility_scope` to a fixed value set via `validate()` was not
chosen as an alternative; the safeguard comes exclusively from the merge
fallback plus the test.

**R2 — `mcp_listener.principals`: removal proposal confirmed — decided
2026-09-21.** The original analysis flagged the question "may a profile
remove principals via intersection" as a proposal for this specification to
decide. **Decision:** yes — the rule changes from `GlobalOnly` to
`Intersection` keyed by `id` (Section 6.1/6.3, the `mcp_listener.principals`
row). A profile may narrow `principals` to a subset of the IDs set by home
(pure removal), but may **never** add a new `id` and may **never** change
`credential_ref`/`tenant`/`workspace`/`job_capabilities` for an existing
`id` (e.g. expand rights) — only principals whose `id` matches exactly in
the global list may appear in the profile result, and for them the full
global version always wins (no field-level merge within a principal entry).
This fully moved Section 6.3/7 to `Intersection` (no longer `GlobalOnly`);
the five principal sub-fields are `CompositeMember` accordingly. Regression
tests: Section 7h, tests #25 (removal), #26 (addition rejected), #27
(rights expansion rejected).

**R3 — `[network]`/`[browser]`/`[dod]`/`[web]` stay out of scope.** These
four sections live outside `HarnessConfig` (Section 1) and hence outside
this task's scope. Their merge behavior (sticky whole-section replacement
when present) is untouched; `merge_restricted_network/_browser/_dod` remain
separate, non-generalized functions. Unifying them under the same
`MergeRule` engine would be a reasonable follow-up, but is not part of the
work packages in Section 7g and would need to be commissioned separately.

**R4 — a "trusted project layer" is a term from the original task wording,
not from the code.** Section 7d explains that the code only knows "trusted
layers" (home + profile, undifferentiated) and the one untrusted project
layer. If "trusted project layer" meant something else (e.g. a third
configuration file managed by the user but not the repo, which does not
exist today), that could not be verified in the code and is deliberately not
guessed at further here — Section 7d only describes how such a layer, *if
introduced*, would fit into this model.

**R5 — `session.retention_days`/`compaction.absolute_ceiling_tokens` as
`MinBound`: the privacy-vs-audit tension remains.** The chosen "GLOBAL as an
upper bound (minimum)" for both fields was picked explicitly from the two
options originally offered — the tension itself (an admin might want a
*minimum* retention for audit reasons, which `MinBound` cannot express) is
thereby decided but not resolved; if an audit minimum-retention requirement
is needed in the future, it needs an additional, here unspecified field
(e.g. `session.min_retention_days` with a `MaxBound`-style rule), not a
repurposing of `retention_days` itself.
