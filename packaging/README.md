# Packaging

## From source (recommended today)

```sh
scripts/install.sh            # builds release + installs to ~/.local/bin
HARW_INSTALL_DIR=/opt/bin scripts/install.sh   # custom target
```

The installer checks prerequisites (cargo, optional bwrap), builds
`harw-cli --release`, installs the `harw` binary (mode 0755), and wires
`~/.local/bin` into `~/.bashrc`/`~/.zshrc` idempotently. It never touches an
existing `~/.harw` — first `harw` run performs onboarding.

## Homebrew

`harw.rb` is a formula template. Before publishing, fill in the `url`,
`sha256`, and version tag (marked `TODO`), then:

```sh
brew install --build-from-source ./packaging/harw.rb
```

## Shell completions

After install:

```sh
harw completion zsh  > "${fpath[1]}/_harw"     # zsh
harw completion bash > /etc/bash_completion.d/harw
```
