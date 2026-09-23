# Validation plan

Status: the final authored tests were executed through Make.
Exact counts, results and remaining environment limits are in VALIDATION.md.

Use the Make entrypoints, with `make clippy-tests` first. `make test` runs pure
unit tests. Linux system tests in `tests/cli.rs` have explicit `#[ignore]`
annotations under rule R183; opt in with `make test-system`.

System prerequisites: Linux with procfs and pidfd support, Python 3, ability to
spawn/signal owned child processes, and visibility of those children in procfs.
The fixture installs its TERM handler before announcing readiness. A child guard
kills and reaps its owned process even when an assertion fails. Selector tests use
per-test command-line markers so unrelated Python instances are not targets.

Covered system contracts:

- Dry-run leaves the child alive and exposes the selected PID in JSON.
- The first signal is KILL; the kernel-observed exit signal is checked.
- An installed TERM handler does not alter the mandatory first KILL.
- Pure engine stubs prove a still-live target receives exactly [KILL, KILL].
- Exit after the retry is classified separately; an unresolved target is not success.
- Noninteractive termination requires `--yes`.
- PID 1 and invoking ancestry are excluded (tested with dry-run).
- Invalid selectors/timeouts are rejected.
- Reaped children are not selectable.
- Multiple exact executable names and explicit PIDs form a deduplicated union.
- Duplicate PID flags yield one target.
- An effective-UID mismatch excludes an explicitly requested PID.

Remaining environment-dependent coverage: foreign-owner `--no-sudo` behavior,
sudo authentication/denial, helper validation and descriptor inheritance under
real sudo, an inaccessible procfs mount, cross-architecture execution on a Pi,
and a kernel lacking pidfd support. A foreign-owner integration fixture cannot
be created portably without additional credentials; do not signal arbitrary host
processes to manufacture that scenario. Root/capability semantics also differ
from an ordinary unprivileged caller. These cases require an isolated test VM or
explicit test account setup and must stay separately opt-in.

Pure tests should cover proc-stat delimiter/field parsing, malformed start ticks,
exact multi-name/PID union and UID selection, CLI duration bounds/non-finite inputs,
protected-ID policy, error Display/source contracts and outcome success rules.
Race behavior and sudo invocation should be exercised via replaceable boundaries
where implemented (R184); source inspection alone is not proof of race safety.
