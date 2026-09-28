# Packaging

## From source (recommended today)

See [docs/setup/install.md](../docs/setup/install.md) for the full guide.

```sh
scripts/install.sh            # builds release + installs to ~/.local/bin
HARW_INSTALL_DIR=/opt/bin scripts/install.sh   # custom target
```

The installer checks prerequisites (cargo, optional bwrap), builds
`harw`, `killer` and `harw-agent-runner` in release mode (through `make
install` when `make` is available; the runner builds under the
`release-runner` profile — fat LTO, one codegen unit, stripped — see the
root `Cargo.toml`), installs all three binaries (mode 0755) plus a runner
copy under `~/.harw/bin/.runners/<target>/<version>/` for `harw agent
build`, and wires `~/.local/bin` into `~/.bashrc`/`~/.zshrc` idempotently.
It never touches an existing `~/.harw` beyond that — first `harw` run
performs onboarding.

`scripts/install.sh --binary` installs the same three binaries from the
release archive and records their installation directory. It requires no
Rust toolchain. An archive missing the agent runner is rejected before an
existing installation is replaced.

## Homebrew

`harw.rb` is a formula template. Before publishing, fill in the `url`,
`sha256`, and version tag (marked `TODO`), then:

```sh
brew install --build-from-source ./packaging/harw.rb
```

## Podman / Quadlet

`podman/` holds a container image (`Containerfile`) and a Quadlet user unit
for `harw web` (the control plane); see [podman/README.md](podman/README.md).
They live here rather than under `deploy/`, which carries only the systemd,
`sysusers.d` and `tmpfiles.d` files that `harw-install` embeds byte-identically.

## Shell completions

After install:

```sh
harw completions --install              # shell detected from $SHELL
harw completions zsh --install          # zsh (oh-my-zsh custom/completions or ~/.local/share/zsh/site-functions)
harw completions bash --install         # bash (~/.local/share/bash-completion/completions/harw)
harw completions fish --install         # fish (~/.config/fish/completions/harw.fish)
harw completions zsh --install --dry-run   # show what would change, touch nothing
harw completions zsh --uninstall        # remove harw-managed scripts, rc blocks and caches
harw completions zsh --install --all-binaries   # also the DoD binaries found on $PATH
harw completions zsh > _harw            # still prints the script to stdout
```

Re-running `--install` replaces older harw-managed installations (legacy
paths, stale `.zcompdump*` caches, old rc blocks) instead of stacking them.
Open a new shell afterwards (e.g. `exec zsh`). `elvish` and `powershell` are
supported too. `harw completion` remains as a hidden alias.
