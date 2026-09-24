# Testing

Run tests through the workspace like any other crate:

```sh
cargo test -p harw-killer          # unit tests only
cargo test -p harw-killer -- --ignored   # opt-in Linux system tests
```

Pure unit tests live inline in each module (`src/cli.rs`, `src/process.rs`,
`src/pidfd.rs`, `src/engine.rs`, `src/privilege.rs`, `src/typestate/`) and
run by default. The Linux system tests in `tests/cli.rs` are explicitly
marked `#[ignore]` and only run when opted into.

System prerequisites for the ignored tests: Linux with procfs and pidfd
support, Python 3, the ability to spawn/signal owned child processes, and
visibility of those children in procfs. Each fixture installs its `TERM`
handler before announcing readiness; a child guard kills and reaps its owned
process even when an assertion fails. Selector tests use per-test
command-line markers so unrelated Python instances on the machine are never
accidentally selected.

## Covered system contracts

- Dry-run leaves the child alive and exposes the selected PID in JSON.
- The first signal is KILL; the kernel-observed exit signal is checked.
- An installed `TERM` handler does not alter the mandatory first KILL.
- Pure engine stubs prove a still-live target receives exactly
  `[KILL, KILL]`.
- Exit after the retry is classified separately; an unresolved target is
  not treated as success.
- Non-interactive termination requires `--yes`.
- PID 1 and the invoking ancestry are excluded (tested with dry-run).
- Invalid selectors/timeouts are rejected.
- Reaped children are not selectable.
- Multiple exact executable names and explicit PIDs form a deduplicated
  union.
- Duplicate `--pid` flags yield one target.
- An effective-UID mismatch excludes an explicitly requested PID.

## Remaining environment-dependent coverage

Foreign-owner `--no-sudo` behavior, sudo authentication/denial, helper
validation and descriptor inheritance under real sudo, an inaccessible
procfs mount, cross-architecture execution, and a kernel lacking pidfd
support. A foreign-owner integration fixture cannot be created portably
without additional credentials; tests never signal arbitrary host
processes to manufacture that scenario. Root/capability semantics also
differ from an ordinary unprivileged caller. These cases need an isolated
test VM or explicit test-account setup and stay separately opt-in outside
this crate's default test run.

## What pure tests should cover

Proc-stat delimiter/field parsing, malformed start ticks, exact
multi-name/PID union and UID selection, CLI duration bounds/non-finite
inputs, protected-ID policy, `Error`'s `Display`/`source` contract, and
outcome success rules. Race behavior and sudo invocation are exercised
through replaceable boundaries (`engine`'s test seam, `privilege`'s
`CommandRunner`) — source inspection alone is not proof of race safety.
