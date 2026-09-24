# Architecture: Model and Provider Routing

> Status: implemented · Last reviewed: 2026-09-24

Describes the atomic validation semantics of `/model switch`, credential
resolution, and the `ReasoningEffort::Minimal` mapping on the Anthropic
adapter.

`/provider switch` and `/uia-provider switch` do not exist as subcommands.
A provider+model switch runs exclusively, atomically, through `/model
switch <id>` (or `/uia-model switch <id>`), so a provider/model pair never
passes through a moment of incompatibility. Both resolve the target model's
configured provider and delegate to the same core function,
`handle_switch_core` (`harw-ops/src/provider.rs`), even when the target
model belongs to a different provider than the one currently active.
`/provider` itself is read-only (`show | list | test`); its `switch`
subcommand falls into the unknown-subcommand branch and points the caller at
`/model`.

---

## Atomic fail-close semantics

`/model switch` applies an "all checks must pass or nothing changes"
contract. If any validation step fails, the session is left exactly as it
was before the command ran. No partial state is written.

```
/model switch <id>
        |
        v
  [Step 1] Provider catalog lookup — does the model's provider exist and resolve?
        | fail -> return InvalidArguments, session unchanged
        v
  [Step 2] Credential resolvability — is the provider enabled and does it have configured auth?
        | fail -> return InvalidArguments, session unchanged
        v
  [Step 3] Model/provider compatibility — does <id> resolve on that provider
        |  (or, with no explicit model, is the current active_model still
        |  compatible with the target provider)?
        | fail -> return InvalidArguments, session unchanged
        v
  All steps passed -> queue provider+model change on TuiSessionController
```

Queued changes are not applied until `apply_pending_controller_state` fires at
the next safe turn boundary. See
[docs/architecture/session-controller.md](session-controller.md).

---

## `handle_switch_core` validation steps

### Step 1 — Provider catalog lookup

`configured_provider(&config, &target)` resolves the target against the
configured provider catalog and returns its canonical ID. If absent,
`OpError::InvalidArguments("unknown provider: …")` is returned immediately.

### Step 2 — Credential resolvability

The resolved `ProviderConfig` must have `enabled = true` and configured auth
present (`configured_auth_is_present`, checking the provider's
`ProviderAuth` value — see `harw-provider/src/auth.rs`: `ApiKey`,
`StaticBearer`, `ChatGptOAuth` (feature-gated), or `None`). If either check
fails, the command returns `InvalidArguments` and the provider is not
switched.

Resolution is attempted eagerly at switch time, not deferred to turn start.
This surfaces missing credentials before the user submits a prompt, avoiding
a silent failure mid-turn.

### Step 3 — Model/provider compatibility

- With an explicit model argument: the model is resolved against the
  catalog (`configured_model`) and its own configured provider must match
  the switch target; a mismatch returns `InvalidArguments` naming both
  providers.
- With no explicit model argument: if the session already has an
  `active_model`, that model's configured provider must match the switch
  target, or the switch is rejected the same way. With no `active_model`,
  this check is skipped — there is nothing to conflict with.

---

## `/provider show` and `/provider list` output

`/provider show` reads `AgentSession::active_provider` (the already-applied
field, not the queue). `/provider list` iterates the provider registry and
annotates each entry:

| Marker        | Meaning                                               |
|---------------|-------------------------------------------------------|
| `[active]`    | Matches `AgentSession::active_provider`               |
| `[auth-ok]`   | Credentials resolved at list time                     |
| `[auth-missing]` | Credentials not available                          |

`/model show` and `/model list` follow the same pattern; `/model list` filters
the catalog to models supported by the current `active_provider`.

Unknown subcommands to either `/model` or `/provider` return `InvalidArguments`
with the supported subcommand set. The old silent fallback to `show` is removed.

---

## `ReasoningEffort::Minimal` on the Anthropic adapter

The Anthropic API does not expose a discrete "low reasoning" setting. The mapping
table:

| `ReasoningEffort` variant | Anthropic `output_config.effort` |
|---------------------------|----------------------------------|
| `Full`                    | `"high"`                         |
| `Standard`                | `"medium"` (default)             |
| `Minimal`                 | omitted (field absent in payload) |

When `effort` is omitted, the Anthropic backend applies its own adaptive
reasoning heuristics. Sending `"low"` (the previous incorrect mapping) caused
the backend to disable reasoning tokens entirely, which was not the intended
behaviour for `Minimal`. The correct fix is to omit the field.

The relevant adapter code lives in the Anthropic provider crate under the
`build_request_payload` function.
