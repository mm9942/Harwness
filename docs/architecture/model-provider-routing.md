# Architecture: Model and Provider Routing

Describes the atomic validation semantics introduced in 0.2.0 for
`/provider switch` and `/model switch`, credential resolution, and the
`ReasoningEffort::Minimal` mapping on the Anthropic adapter.

---

## Atomic fail-close semantics

Both `/provider switch` and `/model switch` apply an "all checks must pass or
nothing changes" contract. If any validation gate fails, the session is left
exactly as it was before the command ran. No partial state is written.

```
/provider switch <id>
        |
        v
  [Gate 1] Registry lookup — does <id> exist?
        | fail -> return InvalidArguments, session unchanged
        v
  [Gate 2] Credential resolvability — can credentials be resolved now?
        | fail -> return AuthError, session unchanged
        v
  [Gate 3] Active-model compatibility — is current active_model valid for <id>?
        | fail -> return Incompatible, session unchanged
        v
  All gates passed -> queue provider change on TuiSessionController
```

```
/model switch <id>
        |
        v
  [Gate 1] harw_model_catalog::resolve(<id>) — does the model exist?
        | fail -> return InvalidArguments, session unchanged
        v
  [Gate 2] Active-provider compatibility — does the resolved model support it?
        | fail -> return Incompatible, session unchanged
        v
  All gates passed -> queue model change on TuiSessionController
```

Queued changes are not applied until `apply_pending_controller_state` fires at
the next safe turn boundary. See
[docs/architecture/session-controller.md](session-controller.md).

---

## Three-gate validation for `/provider switch`

### Gate 1 — Registry lookup

The provider registry (`harw-provider`) holds a map of `ProviderId` →
`ProviderDescriptor`. The command handler calls `registry.get(&id)`. If absent,
`RegistryError::UnknownProvider` is returned immediately.

### Gate 2 — Credential resolvability

`ProviderDescriptor` carries one or more `SghAuth` variants:

| Variant              | Resolution strategy                              |
|----------------------|--------------------------------------------------|
| `ApiKey(env_var)`    | `std::env::var(env_var)` at command time         |
| `OAuth2(config)`     | Token cache lookup; refresh if expired           |
| `None`               | Always resolves (local / unauthenticated)        |

Resolution is attempted eagerly at switch time, not deferred to turn start. This
surfaces missing credentials before the user submits a prompt, avoiding a silent
failure mid-turn.

If resolution fails (env var absent, token cache empty, refresh rejected), the
command returns `AuthError` and the provider is not switched.

### Gate 3 — Active-model compatibility

If the session already has an `active_model`, the handler calls
`harw_model_catalog::resolve(active_model)` and checks whether the resolved
model's supported-provider set includes the requested provider. An incompatible
combination returns `Incompatible` without mutating state.

If `active_model` is `None`, Gate 3 is skipped — there is nothing to conflict
with.

---

## `/model switch` validation

Gate 1 calls `harw_model_catalog::resolve(id)` which returns a `ModelDescriptor`
or `CatalogError::UnknownModel`. The catalog lookup is synchronous and does not
touch the network.

Gate 2 checks whether `ModelDescriptor.supported_providers` contains the current
`active_provider`. If the session has no `active_provider` set, the check is
skipped.

---

## `/provider show` and `/provider list` output

`/provider show` reads `AgentSession::active_provider` (the already-applied
field, not the queue). `/provider list` iterates the provider registry and
annotates each entry:

| Marker        | Meaning                                               |
|---------------|-------------------------------------------------------|
| `[active]`    | Matches `AgentSession::active_provider`               |
| `[auth-ok]`   | Credentials resolved at list time                     |
| `[auth-miss]` | Credentials not available                             |

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
