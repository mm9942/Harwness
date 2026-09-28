# github-pr-reviewer — least-privilege PR-Diff-Review-Beispiel
#
# Builds with `harw agent build examples/agents/github-pr-reviewer`; see
# ./definition.toml for the rights surface and ./system.md for the working
# rules. The diff is untrusted input; publication happens only in the runner
# (`harw pr-review`, R3) behind explicit interactive approval.

harw agent check examples/agents/github-pr-reviewer   # validate definition
harw agent build examples/agents/github-pr-reviewer   # compile to ~/.harw/bin
harw pr-review --repo mm9942/Harwness-dev --pr 41 --fixture scratch/pr-reviewer/fixture.diff
