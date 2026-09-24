# Installation

> Status: implemented · Last reviewed: 2026-09-24

This page covers installing Harwness (`harw`, the agent harness, and
`killer`, its process-termination helper) on a machine, from first run
through updating and uninstalling. Building from source is covered only as
one of three installation paths; for the full build toolchain (Rust version,
platform notes) see [`docs/setup/build-prerequisites.md`](build-prerequisites.md).

## Requirements

- **Platform:** Linux, x86_64 or aarch64. `scripts/install.sh` also runs on
  macOS in `--source` mode, but the sandbox (`bwrap`) is Linux-only.
- **Sandbox:** [bubblewrap](https://github.com/containers/bubblewrap)
  (`bwrap`) and util-linux, for the sandboxed execution the harness uses to
  run agent commands. Without `bwrap` on PATH, `harw` still runs but the
  sandbox is unavailable — the installer warns about this, it does not
  block installation.
- **Optional — PDF reports:** a TeX Live distribution with XeLaTeX, if you
  want `harw`-generated reports rendered to PDF.
- **Optional — `killer` pidfd path:** Linux kernel ≥ 5.3 for `pidfd`-based
  process identity. On older kernels `killer` falls back to PID-based
  termination.
- **Building from source:** rustup. The toolchain is pinned in
  [`rust-toolchain.toml`](../../rust-toolchain.toml) (currently `1.98.1`)
  and installs automatically on first `cargo`/`make` invocation — see
  [`docs/setup/build-prerequisites.md`](build-prerequisites.md) for details
  and Raspberry Pi notes.

## Installing

Pick one of three paths.

### (a) Release tarball

Download the tarball and `SHA256SUMS` for your platform from the
[releases page](https://github.com/mm9942/Harwness/releases), verify, and
copy the binaries yourself:

```sh
tag=vX.Y.Z   # the release tag you're installing
target=x86_64-unknown-linux-gnu   # or aarch64-unknown-linux-gnu
curl -fsSLO "https://github.com/mm9942/Harwness/releases/download/$tag/harw-$tag-$target.tar.gz"
curl -fsSLO "https://github.com/mm9942/Harwness/releases/download/$tag/SHA256SUMS"

sha256sum -c SHA256SUMS --ignore-missing

tar -xzf "harw-$tag-$target.tar.gz"
install -Dm755 harw ~/.local/bin/harw
install -Dm755 killer ~/.local/bin/killer
```

The tarball contains `harw`, `killer`, `LICENSE-MIT`, `LICENSE-APACHE` and
`README.md`.

### (b) `scripts/install.sh`

```sh
scripts/install.sh --binary          # download a release tarball (no Rust toolchain needed)
scripts/install.sh --source          # build from source (needs cargo)
scripts/install.sh                   # picks --source if cargo is on PATH, else --binary
```

Both modes install `harw` and `killer` into `$HARW_INSTALL_DIR` (default
`$HOME/.local/bin`) and add that directory to `PATH` in `~/.bashrc`/
`~/.zshrc` if it isn't already there, without duplicating existing entries.
`--source` mode uses `make install BINDIR=…` when `make` is available (the
same path as installing from source directly, below), and falls back to a
plain `cargo build --release --bin harw --bin killer` otherwise. See
`scripts/install.sh --help` for the full list of environment variables
(`HARW_REPO`, `HARW_VERSION`, …).

### (c) From source via `make install`

The root `Makefile` is the single build/install entry point. From a
checkout:

```sh
make install                 # release-build harw + killer, install into ~/.local/bin
make install BINDIR=/usr/local/bin   # or PREFIX=/usr/local (BINDIR wins if both are set)
```

`make help` lists every target:

| Target | What it does |
|---|---|
| `help` | Lists targets (default goal) |
| `build` | Release-build `harw` and `killer` |
| `install` | Build, then install both binaries into `BINDIR` |
| `uninstall` | Remove the two installed binaries (never touches `~/.harw`) |
| `service` | Install and enable the user systemd services (`harw serve`, `harw gateway`) |
| `check` | Fast workspace type check |
| `fmt` | Format check |
| `clippy` | Clippy, warnings as errors |
| `tests` | Test suite |
| `clippy-tests` | Canonical verification: clippy + tests + gates |
| `gates` | Write-scope/structure gates (`xtask gates`) |
| `dod-build` / `dod-install` / `dod-enable` / `dod-uninstall` | Delegate to `dod/Makefile` — see [`docs/setup/dod.md`](dod.md) |

## First run

Run `harw` once installed. On first start it creates `~/.harw` (the "root
space": config, agent definitions, logs, plugin/tool state) and walks
through the onboarding wizard (`harw onboard`) if it hasn't run before,
then drops into the interactive chat TUI. `harw init` re-runs the root-space
scaffolding step on its own (useful after a fresh checkout of config, or to
repair a damaged `~/.harw`) without also invoking onboarding.

During onboarding you pick and configure at least one model provider —
cloud (OpenAI, Anthropic, OpenRouter, …, via an API key or an imported CLI
credential) or local. For running against a local model server (vLLM, LM
Studio, Ollama), see
[`docs/setup/local-models.md`](local-models.md).

## Optional: user services

`harw` can run its MCP listener/job worker and its gateway as background
user services (systemd on Linux):

```sh
make service            # equivalent to: make install && harw service install
# or, once harw is already installed:
harw service install
```

`harw service status` / `harw service uninstall` manage the same two
services afterwards.

## Optional: DoD (Defense-on-Device)

Harwness ships a separate, privileged, opt-in observation subsystem ("DoD")
that watches host activity around agent execution. It is a distinct
system-level install (own workspace, own Makefile, root-owned service
accounts) and is never installed or enabled by `make install`/`make
service`. See [`docs/setup/dod.md`](dod.md) for what it is and how to set
it up; `make dod-build`, `sudo make dod-install` and `make dod-enable` are
thin delegations to `dod/Makefile` from the repository root.

## Updating

Re-running an install path overwrites `harw` and `killer` in place; it does
not touch `~/.harw`.

`~/.harw`'s bundled assets (agent definitions, prompts, and similar shipped
files under the root space) are updated according to the bundle manifest
(`harw-home/src/bundle_manifest.rs`, `harw-home/src/scaffold.rs`):

- A file you never modified, whose bundled contents changed, is updated in
  place.
- A file you modified yourself is **left alone**. If the bundled version
  also changed, the new version is written next to it with a
  `<file>.harw-neu` suffix instead of overwriting your edit, and this is
  reported so you can review and merge the diff by hand.
- The same `<file>.harw-neu` behavior applies to a file that predates the
  manifest (an old installation) and whose on-disk contents no longer match
  what the current bundle would write.

Nothing under `~/.harw` is ever silently overwritten once you've edited it.

## Uninstalling

```sh
make uninstall        # removes only $(BINDIR)/harw and $(BINDIR)/killer
# or, if installed via scripts/install.sh:
rm "$HARW_INSTALL_DIR/harw" "$HARW_INSTALL_DIR/killer"   # default $HOME/.local/bin
```

`~/.harw` (config, agent state, logs, credentials) is **kept** — uninstalling
the binaries does not touch it. To remove it as well (irreversible: this
deletes your configuration and any locally stored history/credentials):

```sh
rm -rf ~/.harw
```

If you installed the user services (`make service` / `harw service
install`), uninstall them first with `harw service uninstall` before
removing the binaries, so systemd doesn't keep unit files pointing at a
missing executable.

If you installed the DoD subsystem, it has its own uninstall path — see
[`docs/setup/dod.md`](dod.md#uninstall) — `make uninstall` never touches it.
