# Instructions for GitHub Copilot (coding agent) in Harwness

These rules are binding for every task in this repository.

## Branches and PRs
- Base branch is **`dev`**, never `main`. Open one PR per issue against `dev`.
- One task = one PR = the smallest scope that solves the issue. Don't refactor
  beyond it. If you notice something else, mention it in the PR, don't fix it.

## Rust rules
- Edition 2024, **MSRV 1.85**: no let-chains (`if let … && …`), no newer std APIs.
- `unsafe` is forbidden workspace-wide (`#![forbid(unsafe_code)]`).
- No `unwrap()`, `expect()` or `panic!` in library code **or tests**. Tests
  return `TestResult` and use the crate's existing test helpers (`ctx(...)`,
  `TestError`); follow the neighbouring tests.
- No third-party types in public APIs; keep error types hand-written like the
  surrounding code.
- No `*-sys` or C-building crates in the J (jobs) or TCB rings. The `arch`
  gate enforces this, and new dependencies need a line in
  `docs/architecture/dependency-review.md`.
- Match the surrounding comment language and density (German comments where
  the file is German).

## Required checks before you mark a PR ready
Run exactly these, in this order, and fix everything they report:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
cargo deny check
```

If the runner is short on disk, build with
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`.

## Docs and records
- Design decisions live in `docs/planning/70-decisions/` (Obsidian-compatible:
  YAML frontmatter, relative Markdown links, no `[[wikilinks]]`).
- If your change resolves an open item named in
  `docs/planning/90-migration-ledger/MIGRATION_LEDGER.md`, say so in the PR;
  don't edit the ledger yourself.
- Never add book titles, authors or quotes anywhere.
