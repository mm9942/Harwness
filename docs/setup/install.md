# Installation

> Status: implemented · Last reviewed: 2026-09-24

This page covers installing Harwness (`harw`, the agent harness, and
`killer`, its process-termination helper) on a machine, from first run
through updating and uninstalling. Building from source is covered only as
one of three installation paths; for the full build toolchain (Rust version,
platform notes) see [`docs/setup/build-prerequisites.md`](build-prerequisites.md).

## Requirements

- **Platform:** Linux, and Android under Termux (arm64, see
  [Android (Termux, no root)](#android-termux-no-root)). The source
  installer works on the host architecture supported by the repository's
  pinned Rust toolchain.
- **Sandbox:** [bubblewrap](https://github.com/containers/bubblewrap)
  (`bwrap`) and util-linux, for the sandboxed execution the harness uses to
  run agent commands. The source installer installs the distribution package
  when `bwrap` is missing; it requires root privileges or sudo/doas for that
  step.
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
install -Dm755 "harw-$tag-$target/harw" ~/.local/bin/harw
install -Dm755 "harw-$tag-$target/killer" ~/.local/bin/killer
install -Dm755 "harw-$tag-$target/harw-agent-runner" ~/.local/bin/harw-agent-runner
```

The tarball holds one directory `harw-<tag>-<target>/` with `harw`,
`killer` (not in Android builds), `harw-agent-runner`, `LICENSE-MIT`,
`LICENSE-APACHE` and `README.md`. The same steps, checksum included, in one
line:

```sh
curl -fsSL https://get.harw.dev/harw/install.sh | bash -s -- --binary
```

`--binary` picks the latest release (`HARW_RELEASE_TAG=vX.Y.Z` pins one) and
the target of this machine: `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, or `aarch64-linux-android` under Termux. It
reads the mirror at `get.harw.dev/harw` (the tag from `version.json`, else
`latest`, then `<tag>/harw-<tag>-<target>.tar.gz` and `<tag>/SHA256SUMS`); with
`HARW_RELEASES_URL` set it reads a GitHub-style release layout instead. It
never falls back to a source build: without a release for this machine it
stops with an error and installs nothing.
Releases are built by `.github/workflows/release.yml` when a `v*` tag is
pushed (or by hand for an existing tag); `make release` builds the same
tarball locally.

### (b) Source installer

```sh
curl -fsSL https://get.harw.dev/harw/install.sh | bash
# from an existing checkout:
bash scripts/install.sh --source
```

Without an argument the piped script first asks the mirror for a prebuilt
release for this machine (`latest` names a tag whose `SHA256SUMS` lists
`harw-<tag>-<target>.tar.gz`) and installs it like `--binary`, so no Rust
toolchain is needed. Only when there is none it logs "building from source"
and continues as below.

`version.json` is the release manifest of the mirror, written by
`scripts/release-manifest.sh` from the release's `SHA256SUMS`: `version`,
`tag`, `published_at`, the `source` tarball and one entry per target, each
with its `file` (relative to the mirror base) and `sha256`. After a
successful install the script seeds `~/.harw/version.json` (the update
notice's state, see below) with the installed version, unless that file
already exists.

The source path takes the versioned source tarball of that release,
`<tag>/harwness-<version>-source.tar.gz`, checked against the release's
`SHA256SUMS` like a binary tarball. Only a mirror without one falls back to
`Harwness-main.zip` (no checksum). It extracts the source under
`$HARW_SOURCES_DIR` (default `~/.local/share/harw/sources`) and leaves the
source in place for later agent builds. It installs Rustup from
`https://sh.rustup.rs` if needed, then installs missing dependencies such as
Bubblewrap with apt, dnf, yum, pacman, zypper, apk or xbps. Only `unzip` may
be installed earlier to unpack the archive. Rustup starts with stable and
its default profile; `rust-toolchain.toml` selects the pinned version for the build.
Finally the script runs `make install BINDIR=…`. Unknown package managers
produce a manual-install error listing what is missing. The source archive
is fetched over HTTPS; a separate checksum is not published for it yet.
`HARW_INSTALL_DIR` defaults to `~/.local/bin`. See `--help` for overrides.

#### Android (Termux, no root)

The same one-liner works in [Termux](https://termux.dev) on an arm64
phone. The installer detects Termux (`uname -o` = `Android`) and then:

- installs missing build tools with `pkg install` (no root, no sudo):
  `rust clang make cmake pkg-config git binutils`. `rust` comes from
  Termux because rustup has no Android host toolchain; the pinned
  `rust-toolchain.toml` is therefore not applied, any Rust ≥ 1.85 builds.
- does **not** require `bwrap`, `prlimit` or the D-Bus development files.
  Android has no bubblewrap and no user namespaces, and the keyring does not
  use D-Bus there; secrets live in the encrypted SecretStore.
- builds and installs `harw` and `harw-agent-runner` only. `killer` needs
  Linux procfs and pidfd and is skipped (`make install` detects Android).

Without a sandbox, harw runs shell commands on the host **only after your
approval**: once per command, or a host phase that you end with Ctrl+H.
Without an approval channel (non-interactive use) commands are refused.
In FullAccess mode an Android build runs them on the host without asking.
Linux builds are unaffected: there FullAccess stays sandboxed.

```sh
pkg install -y curl
curl -fsSL https://get.harw.dev/harw/install.sh | bash
harw doctor
```

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
| `gates` | Dependency-edge, privilege, Warden structure and architecture-layer (`arch`, policy in `xtask/arch-policy.toml`) gates (`xtask gates`, reads the root workspace including the DoD crates under `dod/crates/`) |
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

Remote OCR for `doc.read_pdf` is off unless you turn it on. With a Mistral
provider configured (provider key `mistral` or base URL host
`api.mistral.ai`), set `[tools.doc].remote_ocr` in `config.toml` to upload
workspace PDFs to the Mistral API for text extraction: `"ask"` asks for
approval before each upload, `"on"` uploads without asking. Without the
setting (`"off"`, the default) PDFs are always extracted locally. See
[`docs/design/config-scopes.md`](../design/config-scopes.md) §1.8a.

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
system-level install (own crates under `dod/crates/` in the root workspace,
own Makefile, root-owned service accounts) and is never installed or enabled by `make install`/`make
service`. See [`docs/setup/dod.md`](dod.md) for what it is and how to set
it up; `make dod-build`, `sudo make dod-install` and `make dod-enable` are
thin delegations to `dod/Makefile` from the repository root.

All system units (DoD and the infrastructure daemons), the `sysusers.d`
accounts and the `tmpfiles.d` directories live in one canonical tree,
`deploy/`, which is also compiled into `harw`. `harw install
--print-systemd [UNIT]` prints the shipped units; see
[DoD setup](dod.md#systemd-units-and-the-deploy-tree).

## Updating

`harw update` checks for a newer version and installs it:

```sh
harw update            # check, ask, then install
harw update --yes      # install without asking (scripts, no terminal)
harw update --check    # only check; exit code 10 = a newer release exists
harw update --dismiss  # hide the start notice for the known newest version
```

- **Release first.** It asks the GitHub API for the latest release
  (`HARW_UPDATE_API` overrides the endpoint). If that release is newer, it
  downloads `harw-<tag>-<target>.tar.gz` and `SHA256SUMS`, refuses the
  tarball unless its checksum matches, and replaces `harw`, `killer` and
  (if included) `harw-agent-runner` in the directory recorded in
  `~/.harw/install.toml` (else next to the running `harw`). The previous
  binaries stay as `<name>.old`.
- **Otherwise from source.** With no published release it updates the
  source directory `make install` recorded: a Git checkout with
  `git pull --ff-only` and `make install`, a source archive by re-running
  its `scripts/install.sh`.
- **Start notice.** When the chat starts, `harw` reads `~/.harw/version.json`
  and names a newer, not dismissed version. If the last check is older than
  20 hours, it runs `harw update --check` detached in the background; the
  chat start never waits for the network. `HARW_NO_UPDATE_CHECK=1` turns the
  notice and the background check off.

Re-running an install path by hand still works the same way: it overwrites
`harw` and `killer` in place and does not touch `~/.harw`.

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
