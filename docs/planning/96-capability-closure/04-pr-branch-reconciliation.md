# PL-96 / 04 — PR and stale-branch reconciliation protocol

> **Planning only.** No PRs/branches are automatically closed, rebased, merged, force-updated or deleted. All links below are review targets; state refers to October 9 inspection. [Evidence register](01-evidence-register.md) is authoritative for what code does.

## Why merge-status cleanup is not sufficient

An open PR can be **older than implemented code** (#91). A merged PR can itself say it is **not merge-ready** (#131). A docs-labeled PR may carry hundreds of source file changes (#95). A cleanup PR with five files (#142) cannot stand as proof that all stale branches have been rescued.

Before deciding, calculate `merge-base(dev, head)`, exact missing commits, and semantic hunk differences. Compare **actual behavior** on both sides and the title/body's declared scope. With large divergence, a `compare/dev...head` view includes unrelated historic context; classify truly exclusive changes, not only number of changed files.

## Candidate matrix (verified GitHub metadata, branch heads pinned as reviewed)

| PR / branch | State | Finding / exclusive candidate | Proposed treatment |
|---|---|---|---|
| [#88](https://github.com/mm9942/Harwness/pull/88) `coop/tools-gap@006afe0040b4` | Draft | Tunnel policy, tool-gap document, CloudHub wiring plan; executable tunnel scaffold | Rescue pure policy and spec after comparing current source, separately implement runtime; no pretend tunnel |
| [#89](https://github.com/mm9942/Harwness/pull/89) `coop/tool-index@586cebaf6ee3` | Draft | `tool_index.rs` static catalog view | Salvage pure search index; complement with effective runtime admission; do not make static catalog policy |
| [#90](https://github.com/mm9942/Harwness/pull/90) `coop/deployment-profile@3ec23e56cda3` | Draft | Pure DeploymentProfile tables | Recheck real service mount call sites; integrate only after contract tests |
| [#91](https://github.com/mm9942/Harwness/pull/91) `coop/hub-daemon@4d129830725a` | Draft | Old local daemon over UDS; current dev already has real CoreTurnDriver composition | Mark partly superseded, inspect residual unique hunks; do not replace newer `dev` |
| [#95](https://github.com/mm9942/Harwness/pull/95) `ccr-fe134f37-xdezhn@1d7c4e8b5630` | Open, base main | Provider-lifecycle research but 433-file diff, 298 Rust | Recreate minimal docs-only PR from current dev or correct base intentionally, verify no scope contamination |
| [#124](https://github.com/mm9942/Harwness/pull/124) | Merged dev | PL-95 S1–S9 documentation | Keep as documentation source; S3–S9 not assumed deployed |
| [#126](https://github.com/mm9942/Harwness/pull/126) | Merged dev | Skill triggers, pure selector | Implement missing turn injector in separate wave |
| [#127](https://github.com/mm9942/Harwness/pull/127) | Merged dev | 42 native tool providers | No-Registry status; add admission and model-visible tests |
| [#131](https://github.com/mm9942/Harwness/pull/131) | Merged dev | Async-first job-backed delegation | Re-run exact-dev fault/portability gates; compare with #141 |
| [#132](https://github.com/mm9942/Harwness/pull/132) | Merged dev | Fixed dev Cargo.lock, worker diagnostics | Port only minimal release hotfix to main; verify branch independence |
| [#134](https://github.com/mm9942/Harwness/pull/134) | Draft | Cycle-composed agents; prompt edits were flagged by #142 as too large/regressive | Keep planning reference, avoid unreviewed prompt overwrite |
| [#135](https://github.com/mm9942/Harwness/pull/135) | Merged dev | Intent/composition planning | Source is PL-90 design, not full generic cycle runtime |
| [#136](https://github.com/mm9942/Harwness/pull/136) | Merged dev | W01-W03 durable intent cycle; no production W03 caller | Source-level modules exist; verify clean build without private cloud stubs |
| [#137](https://github.com/mm9942/Harwness/pull/137) | Merged dev | Mission Control / Progress Observer plan | No live progress observer implied |
| [#138](https://github.com/mm9942/Harwness/pull/138) | Merged dev | Cloudflare adaptive model lanes | No worker deployment; test and rollout separately |
| [#139](https://github.com/mm9942/Harwness/pull/139) | Merged dev | Qwen micro-expert dataset/training research scaffolding | No weights or performance claim |
| [#140](https://github.com/mm9942/Harwness/pull/140) | Merged dev | Research paper baseline main Oct 7 | Preserve original baseline, append verified dev delta |
| [#141](https://github.com/mm9942/Harwness/pull/141) `fix/pl93-test-portability@ab33dedd5458` | Draft | Targeted nextest/macOS/doc-lint fixes | Independent diff/CI check against dev before cherry-picking |
| [#142](https://github.com/mm9942/Harwness/pull/142) `ccr-d0359234-af3m9p@04d4ed847d9b` | Draft | Five rescued files including pure AgentTree model; pending stale branches | Use as rescue *work ledger*, not closure proof |

### Additional stale branch candidates to inspect without deleting

- `feature/telegram-bot-api-10-3-command-registry@91db7b5904`: eight exclusive commits at audit comparison; policy-expression gap requires a fix prior to any exposure.
- `coop/proxy-plan@918b5c8564`: pure proxy policy and Pingora dependency discussion; no live proxy.
- `arch/unify-process-runtime@b7bcb91d1`: divergent newer job/command runtime work with portability fixes; compare vs merged #131, not blindly overwrite.
- `chatgpt/semantic-activity-patterns@2cc179b9c7`: extensive planning/migration seams, no runtime matcher.
- `chatgpt/on-demand-worker-boot@b3d6a7794` and worker builder branch: planning + experimental Podman; owner decision required for runtime dependencies.
- `chatgpt/tui-control-deck-v2@b3d649137`, `chatgpt/cloud-home-hub-profiles-v2@6c256cfcf`: docs may be superseded by merged review patches, compare conceptually and preserve exclusive decisions.
- `copilot/fix-blocking-handoff@ffcd8d09d`: admission/fail-closed change candidate; cross-check against current turn-loop rather than cherry-pick blindly.

## Required rescue ledger schema

For **each** branch/PR record:

```yaml
id: PR-XYZ
reviewed_head: <40-hex-sha>
compared_dev: <40-hex-sha>
merge_base: <40-hex-sha>
scope: <what the branch actually changes>
unique_commits: []
unique_hunks: []
behavioral_delta: <verified>
contract_conflicts: []
tests_existing: []
tests_to_run: []
classification: superseded | integrate_minimal | defer | reject
replacement_pr: <PR URL or null>
owner_approval: pending
safe_to_delete: false
```

A branch can be marked `safe_to_delete: true` only after its final commit is pinned and exclusive changes are demonstrably integrated or explicitly rejected with rationale. If integration happens, its new PR/commit and verification log must be linked. A partially superseded PR should receive an explanatory comment before closure, not a misleading “merged”.

## Merge policy

1. Freeze dev head and branch heads at triage start.
2. Derive merge-bases and semantically diff **producer + consumer**. When a branch is stacked, inspect its parent chain to avoid dragging unrelated changes.
3. Assign exactly one writer per file and worktree per wave; reviewers should be isolated from writer branch and evaluate the same pinned SHA.
4. Produce a small, purpose-bound PR for each integration; do not merge old broad history to recover one file.
5. Require clean-checkout metadata/fmt, focused tests and architecture gates; do central builds sequentially to avoid Cargo target contention.
6. For security-critical changes require deny-path tests and explicit human signoff; no autonomous `main` fast-forward.
7. Record merge result and rollback instructions; after all branch-specific work is accounted for, ask owner approval for deletion.

## Specific follow-up decisions

- #91 should not be assumed the canonical local daemon; `dev` has the production driver path.
- #95 base=main with broad diff requires new branch or corrected target, not a perfunctory “docs review”.
- #142 should be extended to explicit staged rescue records and no-delete default.
- #141's latest CI status at review time was failure; isolate its failing job and reproduce before marking verified.
- “Merged PR” may still contain a checklist that says **not merge ready** (#131/#136). Do not erase this evidence; record a post-merge verification debt rather than a fabricated release pass.
