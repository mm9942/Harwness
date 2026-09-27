# Harw CLI documentation drift review

**Rule:** Documentation is the object being checked, not the source of truth.

Use this evidence order:

1. current parser and handler source;
2. parser/runtime/authority tests and executable gates;
3. implemented architecture/design documentation;
4. planning material.

## Procedure

1. Pin the reviewed ref/HEAD.
2. Identify the documentation claim precisely: command inventory, syntax, flag, alias, behavior, output, exit status, security/authority statement, or example.
3. Find the live grammar:
   - root: `harw-cli/src/cli/mod.rs` plus the domain module;
   - slash: Operation metadata/TUI registry;
   - model tool: Operation/model adapter;
   - delegated grammar: the owning crate (for example `harw-killer`).
4. Trace root dispatch and the concrete handler. A parser entry alone does not prove documented behavior.
5. Check adapter convergence. Do not report duplicate behavior when two CLI spellings intentionally route to one implementation.
6. Compare exact semantics, including hidden/visible aliases, global-vs-session flag rules, JSON support, default behavior, service role, and security boundary.
7. Use live `--help`/focused parser tests when execution is available; record what was actually run.
8. Classify every mismatch:
   - **omitted-live** — implementation exists but docs omit it;
   - **phantom** — docs claim a command/flag that is absent;
   - **grammar** — syntax/required/default/alias mismatch;
   - **behavior** — parser exists but described handler behavior is wrong;
   - **surface** — docs confuse process CLI, slash command, model tool or service role;
   - **output** — text/JSON/TUI/exit behavior mismatch;
   - **authority** — docs misstate identity, permission, approval or trust boundary.
9. Do not silently "fix" a contradiction by combining docs and code. State the delta.
10. If the task includes editing docs, make the smallest correction that matches CURRENT and preserve separate PLANNED material as planned.

## Finding format

```markdown
| ID | class | severity | documented claim | CURRENT evidence | correction |
|---|---|---|---|---|---|
| D1 | omitted-live | medium | ... | ... | ... |
```

Severity guidance:
- **high** — security/authority/side-effect or destructive-operation misunderstanding;
- **medium** — materially wrong command discovery, syntax, runtime role or automation behavior;
- **low** — wording, stale example, non-blocking alias/detail.

End with:
- docs/files checked;
- implementation files checked;
- tests/help commands actually run;
- remaining unverified claims.

Do not claim all CLI docs are stale from one mismatch.
