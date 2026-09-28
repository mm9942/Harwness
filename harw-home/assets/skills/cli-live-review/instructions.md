# Harw CLI live review

**Rule:** Review the implementation that is actually present. Current source, parser tests and executable gates outrank prose docs and planning files.

## Workflow

1. **Pin the state.** Record the branch/ref and exact HEAD when available. If the ref is not known, say so instead of calling the result current.
2. **Name the invocation exactly.** Preserve whether the user means `harw ...`, a slash command such as `/goal`, a model tool, or a daemon/service entrypoint.
3. **Trace the live chain.** For a process-CLI command, follow:
   `spelling -> clap type -> root dispatch -> adapter/handler -> runtime/service composition -> authority/approval -> stores/state -> output/exit contract`.
4. **Inspect the real domain module.** Do not stop at `harw-cli/src/cli/mod.rs`; the enum is grammar, not implementation.
5. **Check reuse before calling code duplicate.** Some root CLI commands intentionally bridge into registered Operations via `harw-cli/src/op_bridge.rs`; provider/config/model paths also converge behind adapters.
6. **Check special paths explicitly.**
   - `harw kill`: raw argv passthrough before normal clap/tracing; inspect `harw-killer`.
   - session flags: syntactically global but semantically limited by `reject_misplaced_session_flags`.
   - `--json`: root family gate plus per-handler support; inspect both.
   - `gateway` without action vs `gateway ACTION`: foreground runtime vs service management.
   - `serve`, `web`, `gateway`, `service`: related operationally, not interchangeable.
7. **Check command-plane overlap.** If the same word exists as root CLI and slash/model operation (for example `analyze`), prove whether they share a handler. Never assume parity from naming.
8. **Check tests and docs last.** Compare parser tests, authority tests, `docs/cli.md`, and relevant design docs against the traced implementation.
9. **Separate static evidence from live verification.** Do not claim that a daemon started, a provider round-trip worked, or a command executed unless it was actually run.

## Review output

Return a compact reviewer report:

```markdown
## <invocation> — live review
Pinned ref: <ref@sha>

### CURRENT
<what the implementation does>

### Execution path
<spelling -> parser -> dispatch -> handler -> runtime/state>

### Authority and state
<principal/approval/store ownership>

### Findings
| ID | severity | finding | evidence | direction |
|---|---|---|---|---|

### Verification
<tests/commands actually run, or "static only">

### Unknown / not claimed
<live or external behavior not verified>
```

Do not turn a review into a redesign unless the task explicitly asks for a target architecture.
