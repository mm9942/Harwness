# Inventory: every Claude branch and every preserved state

Taken after `git fetch` on 2026-09-28. `dev` = `79b471d`, which has the same
tree as the pin `1ad9021`. Integration = `a856dea`. Nothing Claude produced
for R16 lives only in a container any more.

## Already in `dev`
| Branch | Head |
|---|---|
| `claude/r11-harw-ecosystem` | `140ab6a` |
| `claude/r12-execution-crypto` | `01763fc` |
| `claude/r14-work-driver` | `02401aa` |
| `claude/r15-verify-pacing` | `7b96833` |
| `claude/r15-wip-snapshot` | `b3fced0` |

## In `claude/r16-integration` (not in `dev`)
| Branch | Head | Content |
|---|---|---|
| `claude/r16/kit`, `kit-2`, `kit-3`, `kit-4` | `d112317`, `06d17f0`, `1102513`, `662ccd3` | gap-hunt kit and gates |
| `claude/r16/wa-egress` | `57bb5e7` | wave with manifest |
| `claude/r16/wa-authz` | `e8eace4` | wave with manifest |
| `claude/r16/wa-web` | `f48013e` | wave with manifest |
| `claude/r16/contract-a` | `72ea67e` | wave with manifest |
| `claude/r16/contract-b` | `f5fde82` | wave with manifest |
| `claude/r16/ripple-authz` | `3f8071c` | wave with manifest (fixes review item C) |
| `claude/r16/ripple-egress` | `d36212f` | wave with manifest |

## Open: not merged anywhere
| Branch | Head | State |
|---|---|---|
| `claude/r16/contract-c` | `6893f33` | Reviewed contract wave with manifest. It overlaps the w-waves on `harw-runtime/src/assembly.rs` and `harw-core-bridge/src/agent_tool.rs`. |
| `claude/r16/wip-snapshot` | `bbd603b` | w1–w8 one-file waves: 95 files, **unreviewed, unbuilt**. |
| `claude/r16/wip-ripple-web` | `b3c3947` | ripple-web fixer output: 14 files, **unreviewed, unbuilt**. Its review and repairs died. |
| `claude/r16/wip-contract-a` | `72bff60` | Stray P14 writes, 2 files. The intended copies are in `wip-snapshot`. |
| `claude/r16/wip-kit-3` | `4162324` | Stray P14 writes, 4 files. The intended copies are in `wip-snapshot`. |
| `claude/r16/coordination` | this branch | Coordination notes (this folder). |
| `claude/laughing-hypatia-6yzvvz` | `3701b3a` | Older session branch: planning tree, logo and icon drafts. 2 commits not in `dev`, 25 behind. Not R16. |

`wip-*` branches are input for a re-review against the base. They are not
results. Nothing on them may be marked CURRENT or merged without review and
the central build.

## Notes in this folder
| File | Content |
|---|---|
| `claude-handoff.md` | Open R16 state, sorted by `r16-dev-integration` plan node |
| `claude-inventory.md` | This list |
| `claude-patterns-pending.md` | Catalog entries P15–P20 |
| `claude-field-report.md` | Live harw session failures mapped to code: 10 verdicts, 16 confirmed findings |
| `w11/DEC-048-draft.md`, `w11/critic.md`, `w11/maps.json` | W00/W11 WebSocket control plane: design draft, critic (11 corrections, 5 operator decisions), code maps. Draft only. |
| `tools/` | The wave scripts used for R16: commit, worktree commit, reconcile, follow-up builder, PR ledger. Paths come from `R16_REPO` / `R16_SCRATCH` / `R16_SUBAGENTS`. |
