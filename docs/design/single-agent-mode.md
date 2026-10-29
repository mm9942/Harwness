# Single-agent execution mode

For a **non-embedded runtime**, set this top-level key in trusted home/profile
`config.toml`:

```toml
single_agent_mode = true
```

Default: `false` (existing multi-agent behavior). This is an explicit topology
choice, not automatic model routing. Trusted configuration layers can override
it; absent keys inherit and untrusted project configuration cannot change it.
Restart/rebuild the runtime to apply a change. Set it to `false` to restore
managed delegation before asking the assistant to use parallel agents.

## Embedded-build limitation

Compiled agents and native personalized builds carrying an embedded UIA do not
read home/profile configuration, including this key. They retain their bundled
execution topology; setting this key in their home directory has no effect.
This preserves the existing hermetic embedded-config contract. This release
does not add an embedded manifest field or CLI override for single-agent mode.
The config-boundary regression test covers embedded `CompiledAgent`, `Tui`
and `OneShot` entries explicitly.

## Runtime behavior

Production assembly retains the existing durable root session/model and its
normal iterative model/tool loop, context compaction, cancellation and turn
budgets. It suppresses the managed child spawner, durable child-job submitter
and background-agent status/cancel tools. No unmanaged child fallback is added.
This avoids repeated task-agent setup and handoffs when one capable model can
work through the whole task. It trades away parallel child-agent execution.

The root keeps its existing tool registry, workspace sandbox, permission
narrowing, approval policy, authenticated principal and budget limits. This
switch grants no new tools or rights, never auto-approves writes or execution,
and does not turn a limited entry point into a full-access one. Remote tools
and externally managed jobs still follow their own governance; this setting
is not a global ban on other processes or remote model services.

The name intentionally says *single agent*, not *only one model inference*:
configured internal approval-classifier, compaction and title helpers retain
their existing behavior. The root model choice is still controlled by the
existing default/UIA model settings and explicit overrides.

## Cycles and long-running work

Existing plan/cycle tools remain available only where the ordinary entry profile
and plan services already mount them. Their admissions, approvals, budgets,
leases and recovery rules are unchanged. This switch neither starts a cycle
nor provides an autonomous forever-loop. Use the normal durable session and
bounded tool/cycle mechanisms; no new sleeping background model is created.

## Regression coverage

`test_single_agent_mode_trusted_precedence` checks defaults, inheritance,
trusted overrides and both directions of untrusted rejection.
`test_single_agent_mode_keeps_governed_tools_without_child_spawner` exercises
production TUI and one-shot assembly without a child event sender, verifies
no spawner/background tools, retains filesystem/shell tools, and preserves
explicit `AlwaysAsk` approval. Existing default-mode spawner and rights-matrix
tests continue to cover the unchanged opt-out path.
