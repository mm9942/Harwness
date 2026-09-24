# Contributing to Harwness

Thanks for your interest in contributing. This document covers the practical
setup and the checks your change needs to pass. For the security model that
contributions must not weaken, see [SECURITY.md](SECURITY.md).

## Toolchain

The Rust toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
(currently `1.98.1`, with `rustfmt` and `clippy`). `rustup` picks this version
up automatically in the repository root and in the separate `dod/` workspace,
so your local build uses the same compiler, `rustfmt` and `clippy` as CI.

```bash
rustup toolchain install   # reads rust-toolchain.toml
rustc --version
```

## Repository layout

This repository holds two independent Cargo workspaces:

- the root workspace (`Cargo.toml`), with the product crates (`harw`,
  `harw-cli`, `harw-tui`, `harw-core`, and friends);
- `dod/`, the Defense-on-Device workspace, with its own `Cargo.toml` and
  `Cargo.lock`.

Path dependencies cross from the root workspace into `dod/crates/...`, but
`dod/` is never a member of the root workspace (`exclude = ["dod"]`), so each
workspace builds and locks independently.

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

Every pull request runs the following (see
[`.github/workflows/ci.yml`](.github/workflows/ci.yml)); path filters skip
jobs that cannot be affected by a given change, but a full run covers all of
them:

- `cargo fmt --all -- --check` — formatting.
- `cargo clippy --workspace --tests -- -D warnings` — lints, warnings treated
  as errors.
- `cargo nextest run --workspace` — the test suite (root workspace).
- `cargo test --workspace --doc` — doc tests, which `nextest` does not run.
- `cargo test --workspace --locked` in `dod/` — the separate Defense-on-Device
  workspace, run with its own lockfile.
- `cargo deny check` (`EmbarkStudios/cargo-deny-action`) — license and
  advisory checks against [`deny.toml`](deny.toml).
- `actionlint` — lints on the GitHub Actions workflows themselves.

Run the closest equivalents locally before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --tests -- -D warnings
cargo nextest run --workspace   # or: cargo test --workspace
cargo test --workspace --doc
(cd dod && cargo test --workspace --locked)
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
