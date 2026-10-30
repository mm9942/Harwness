# Local branch consolidation

User request: inspect every online GitHub branch, recover missing code, and
prepare a final local main/dev state before any push or remote cleanup.

## Scope and frozen inputs

- Repository: mm9942/Harwness; 79 GitHub heads enumerated through paginated gh API
  and independently checked with git ls-remote.
- Original local dev: 23cd2e4e1b3fb629ecd9cb18aa09f493befe4d28.
- Online dev: 3929f159a57c1df65c60861d5ff206d63a769740.
- Refreshed online dev: 287777ba50d1ea02b51951d75823d0ca5c23d6a5. PR163
  merged externally during the audit; its rename was taken by fast-forward.
- Online main: 843054a35b7eb0297ca26973a4c58cad2ff624af.
- Also inspect the 158 retrievable pull-request heads, including 19 not reachable
  from any current online branch.
- A verified pre-consolidation Git bundle and source inventory live outside the
  checkout at /root/harw-release/branch-consolidation-20261030/.

## Approach

Use current online dev as the content baseline. Compare ancestry, patch identity,
and actual files; unmatched commit hashes alone do not establish missing code.
Restore useful independent additions and adapt lost fixes to current interfaces.
Keep newer implementations where old prototypes were intentionally replaced.

Blindly replaying all branch diffs would reintroduce removed executors, stale
lockfiles, and older authorization behavior. Merely deleting branches would lose
provenance. Instead, preserve every reviewed head in the final Git ancestry after
content reconciliation, with explicit per-head dispositions. A history-only
merge records reconciliation; it does not claim every old implementation is active.

## Planned content

1. Restore atomic pending-start accounting from PR116, adapted to async command
   ports and dynamic job limits, with concurrency/failure/cancellation tests.
2. Restore the pure DeploymentProfile table and its existing tests, explicitly a
   composition contract rather than proof of runtime enforcement.
3. Preserve standalone historical paper automation and planning artifacts with
   clear pointers to the newer canonical manuscript; activate no automation.
4. Recover factual PL92 review corrections found in closed PR118.
5. Apply the dod-to-harw-dod directory rename, preserving newer paper edits and
   checking workspace paths, packaging, architecture gates, and scripts.
6. Commit complete branch/PR inventories and a proposed remote-cleanup manifest.
7. Repair the pre-existing all-features Cloud Gateway compilation failure:
   activate the Pingora proxy trait feature and retain explicit HTTP501 refusal,
   with an in-memory regression test. No forwarding is implemented or activated.

## Constraints and decisions

No push, remote branch deletion, PR closing, service restart, or deployment.
No credentials or environment dumps. Do not enable obsolete Podman/worker or
blocking-handoff prototypes. Preserve their source history and reasons instead.
Only main and dev are planned as final local branches, following the user's
last explicit desired names. After ancestry verification, remove only the local
imported tracking/audit refs and retain normal origin/main and origin/dev
tracking. The actual remote branches remain untouched.

## Verification

Require targeted tests for restored behavior, pure script tests, workspace
formatting, canonical clippy/tests/gates where supported, and final proof that
all inventoried heads are ancestors of the consolidated commit. Compare remote
heads before/after to detect external changes without modifying them.
