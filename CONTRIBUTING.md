# Contributing to Harwness

Thanks for your interest in contributing. This document covers the practical
setup and the checks your change needs to pass. For the security model that
contributions must not weaken, see [SECURITY.md](SECURITY.md).

## Toolchain

The Rust toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
(currently `1.98.1`, with `rustfmt` and `clippy`). `rustup` picks this version
up automatically everywhere in the repository (including `dod/`), so your
local build uses the same compiler, `rustfmt` and `clippy` as CI.

```bash
rustup toolchain install   # reads rust-toolchain.toml
rustc --version
```

## Repository layout

This repository is one Cargo workspace (`Cargo.toml`, one `Cargo.lock`) with
two domains:

- the product crates at the repository root (`harw`, `harw-cli`, `harw-tui`,
  `harw-core`, and friends);
- the Defense-on-Device (DoD) crates under `dod/crates/`, listed explicitly
  in a separate block of the root `members`.

Until PL-60 `dod/` was a separate, nested workspace with its own lockfile; see
[`docs/architecture/dod-workspace-merge-plan.md`](docs/architecture/dod-workspace-merge-plan.md).
Workspace membership is build governance, not dependency permission: the
Warden/Sentinel/probe dependency and privilege budgets are enforced by
`cargo run -p xtask -- gates`, not by a workspace boundary.

### Architecture layers (`xtask gates arch`)

A crate being a member of the same Cargo workspace does not imply that
another crate may depend on it. Dependency permission comes from
[`xtask/arch-policy.toml`](xtask/arch-policy.toml) and is enforced by
`cargo run -q -p xtask -- gates arch` (part of the default `gates` run):

- Every package is classified into a layer: `F` foundation, `I` shared
  infrastructure, `C` compiler, `J` jobs, `D` DoD, `A` application/
  composition. A new crate that is not listed under `[packages]` fails the
  gate — add it with its layer in the same change.
- Every internal normal (non-dev, non-build) dependency edge must follow
  `[rules]` (edges only point inward, e.g. `J` may use `F`, `I`, `J`).
  Known inversions are listed as `[[exceptions]]` with `reason` and
  `until`. The list only shrinks: an exception whose edge is gone, or whose
  edge the rules already allow, fails the gate until it is removed.
- Packages with `tcb = true` (Warden and its protocol, `harw-dod-readfs`,
  `harw-dod-signals`) may only depend directly on the internal crates in
  `[tcb."<name>"].allowed_internal`.
- Layer `J` and every TCB package may not reach a `*-sys` crate in their
  transitive dependency closure (built from the workspace manifests and
  `Cargo.lock`), except the ones justified under `[[sys_crates.allow]]`.

See §6, §23, §40–§43, §47 and §55–§56 of
[`docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md`](docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md).

## Building and checking your change

There is no single "run everything" command for local iteration; use focused
commands while working, and the full check list below before you open a pull
request. Do not use `cargo run`/`cargo test` invocations against production
data — the runtime writes to `~/.harw` by default.

```bash
cargo check --workspace --all-features
cargo test -p <crate-you-changed>
```

`Makefile` provides shortcuts for the root workspace (`make fmt`, `make
clippy`, `make tests`, `make check`, `make build`); see the Makefile for the
full target list.

## What CI runs

CI runs the following (see
[`.github/workflows/ci.yml`](.github/workflows/ci.yml)); path filters skip
jobs that cannot be affected by a given change, but a full run covers all of
them. While GitHub Actions is unavailable for this repository, CI is started
by hand only (`workflow_dispatch`) and the local central build described in
`CLAUDE.md` is the gate; the push and pull-request triggers are commented out
in `ci.yml` and come back once Actions runs again:

- `cargo fmt --all -- --check` — formatting.
- `cargo clippy --workspace --tests -- -D warnings` — lints, warnings treated
  as errors.
- `cargo nextest run --workspace` — the test suite (all crates, DoD included).
- `cargo test --workspace --doc` — doc tests, which `nextest` does not run.
- `cargo run -q -p xtask -- gates` — dependency-edge, privilege and Warden
  budget gates, plus the `arch` layer gate (see below).
- DoD, package-scoped: the four DoD binaries built and the DoD crates tested
  with `-p` selections only (`cargo test --locked -p harw-dod-… -p …`), so
  the result is not masked by feature unification with product crates.
- `cargo deny check` (`EmbarkStudios/cargo-deny-action`) — license and
  advisory checks against [`deny.toml`](deny.toml).
- `actionlint` — lints on the GitHub Actions workflows themselves.

Run the closest equivalents locally before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --tests -- -D warnings
cargo nextest run --workspace   # or: cargo test --workspace
cargo test --workspace --doc
cargo run -q -p xtask -- gates
make -C dod test   # DoD crates only, -p selection against the root workspace
cargo deny check
```

## Code style

- **Edition 2024.** All crates target Rust 2024; keep new code idiomatic for
  that edition.
- **`#![forbid(unsafe_code)]`.** The workspace forbids `unsafe` at the lint
  level (`[workspace.lints.rust] unsafe_code = "forbid"`), not just via a
  per-crate attribute that could be overridden with `#[allow]`. Do not
  introduce `unsafe` blocks or weaken this lint.
- **No `unwrap`/`expect`/`panic!` in production code paths.** Fallible
  operations return `Result` with a typed or contextual error; panics are
  reserved for genuine programmer-error invariants, and even those should be
  rare. Tests are the exception: prefer `fn test_x() -> Result<...>` with `?`
  over `.unwrap()` so a failing assertion reports a normal test failure
  instead of a panic message with no context.
- **Errors carry context.** Wrap or annotate errors with what was being
  attempted (the path, the tool name, the identifier) rather than propagating
  a bare underlying error.
- **Documentation is in English**, including doc comments, `README.md` files,
  and everything under `docs/`.

## Security-sensitive changes

Harwness's value comes from a small set of invariants around approvals,
sandboxing and privilege boundaries (see [SECURITY.md](SECURITY.md) for the
current list). If your change touches tool permissions, the approval flow,
`shell.exec`, `process.kill`, sudo handling, the sandbox, or the
Defense-on-Device plane, explain in the pull request description:

- which trust boundary the change affects,
- what happens on failure (fail-open vs. fail-closed), and
- what is persisted, and where.

Do not move an authority decision into a prompt, a configuration string, or a
deserialized wire value, and do not add a path that turns a model proposal
into a host action without an explicit, typed governed transition.

## Pull requests

Keep changes narrow and scoped to one concern. Use the pull request template;
it asks for a summary, the exact change, how you tested it, and a checklist.
Update the relevant crate documentation and tests alongside behavioral
changes — a component described as "implemented" should be reachable from a
real entry point, not just from an internal test.

## Reporting a vulnerability

Do not open a public issue for a security vulnerability. See
[SECURITY.md](SECURITY.md) for how to report one privately.
