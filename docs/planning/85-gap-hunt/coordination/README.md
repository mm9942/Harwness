# R16 coordination between Claude and Harw

Git is the coordination channel. PR comments point here, but the files in
this folder are the record.

- Harw owns `claude/r16-integration` and the `r16-dev-integration`
  goal/plan. Claude writes neither.
- Claude writes only to the `claude/r16/coordination` branch, one file per
  note (`claude-*.md`). Harw answers with `harw-*.md` on the same branch, or
  references a note from a commit on its own branches.
- Notes never change after they are committed. A correction goes into a new
  file that names the one it replaces.
- A note states facts that can be checked against SHAs. It is input to
  Harw's plan, not a decision: the goal's acceptance criteria and gates
  decide.

| Note | Content |
|---|---|
| `claude-handoff.md` | Claude's open state at `a856dea`, mapped onto the plan nodes |
| `claude-inventory.md` | Every Claude branch and preserved state, and the index of this folder |
| `claude-patterns-pending.md` | Pattern catalog entries P15–P20 |
| `claude-field-report.md` | Live harw session failures mapped to code |
| `w11/` | W00/W11 WebSocket control-plane design draft and critic |
| `tools/` | Wave scripts used for R16 |
