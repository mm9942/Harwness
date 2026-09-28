#!/usr/bin/env bash
# Install Harwness from the source archive published at get.harw.dev.
# Usage: curl -fsSL https://get.harw.dev/harw/install.sh | bash
set -euo pipefail

base_url="${HARW_BASE_URL:-https://get.harw.dev/harw}"
archive_name="Harwness-main.zip"
install_dir="${HARW_INSTALL_DIR:-$HOME/.local/bin}"
sources_dir="${HARW_SOURCES_DIR:-$HOME/.local/share/harw/sources}"

log() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

case "${1:-}" in
  -h|--help)
    cat <<'EOF'
Usage: curl -fsSL https://get.harw.dev/harw/install.sh | bash
       curl -fsSL https://get.harw.dev/harw/install.sh | bash -s -- --binary
       bash scripts/install.sh [--source | --binary]

The piped installer downloads and extracts Harwness-main.zip, installs
Rustup with the stable default toolchain when needed, installs missing
Linux build dependencies including Bubblewrap, then runs make install.

--binary installs harw, killer and harw-agent-runner from a GitHub release
instead (no Rust toolchain): it downloads harw-<tag>-<target>.tar.gz and
SHA256SUMS, refuses a tarball whose checksum does not match or that lacks a
binary, and only then replaces an existing installation.

Environment: HARW_BASE_URL, HARW_INSTALL_DIR, HARW_SOURCES_DIR, HARW_HOME,
HARW_RELEASE_TAG (default: the latest release), HARW_RELEASES_URL.
EOF
    exit 0 ;;
  ''|--source|--binary) ;;
  *) die "unknown argument: $1 (see --help)" ;;
esac
[ "$#" -le 1 ] || die "too many arguments (see --help)"
[ "$(uname -s)" = Linux ] || die "this source installer currently supports Linux only"

# `sudo` and `doas` use /dev/tty for a password when this script is piped.
as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  elif command -v doas >/dev/null 2>&1; then
    doas "$@"
  else
    die "missing system packages; install them as root or provide sudo/doas"
  fi
}

# Termux on Android: uname -s says Linux, uname -o says Android. Termux
# installs packages without root (`pkg`), has no bubblewrap, no user
# namespaces and no prlimit, and links no D-Bus (the keyring uses its
# Android fallback, secrets live in the encrypted SecretStore).
is_termux() {
  [ "$(uname -o 2>/dev/null)" = Android ]
}

install_packages() {
  local phase="$1"
  local packages=()
  if is_termux; then
    if [ "$phase" = unzip ]; then packages=(unzip)
    else packages=(rust clang make cmake pkg-config git binutils); fi
    pkg install -y "${packages[@]}"
  elif command -v apt-get >/dev/null 2>&1; then
    if [ "$phase" = unzip ]; then
      packages=(unzip)
    else
      packages=(bubblewrap util-linux build-essential pkg-config cmake libdbus-1-dev git)
    fi
    as_root apt-get update
    as_root apt-get install -y "${packages[@]}"
  elif command -v dnf >/dev/null 2>&1; then
    if [ "$phase" = unzip ]; then packages=(unzip)
    else packages=(bubblewrap util-linux gcc gcc-c++ make pkgconf-pkg-config cmake dbus-devel git); fi
    as_root dnf install -y "${packages[@]}"
  elif command -v yum >/dev/null 2>&1; then
    if [ "$phase" = unzip ]; then packages=(unzip)
    else packages=(bubblewrap util-linux gcc gcc-c++ make pkgconfig cmake dbus-devel git); fi
    as_root yum install -y "${packages[@]}"
  elif command -v pacman >/dev/null 2>&1; then
    if [ "$phase" = unzip ]; then packages=(unzip)
    else packages=(bubblewrap util-linux base-devel pkgconf cmake dbus git); fi
    as_root pacman -S --noconfirm --needed "${packages[@]}"
  elif command -v zypper >/dev/null 2>&1; then
    if [ "$phase" = unzip ]; then packages=(unzip)
    else packages=(bubblewrap util-linux gcc gcc-c++ make pkg-config cmake dbus-1-devel git); fi
    as_root zypper --non-interactive install "${packages[@]}"
  elif command -v apk >/dev/null 2>&1; then
    if [ "$phase" = unzip ]; then packages=(unzip)
    else packages=(bubblewrap util-linux build-base pkgconf cmake dbus-dev git); fi
    as_root apk add "${packages[@]}"
  elif command -v xbps-install >/dev/null 2>&1; then
    if [ "$phase" = unzip ]; then packages=(unzip)
    else packages=(bubblewrap util-linux base-devel pkg-config cmake dbus-devel git); fi
    as_root xbps-install -Sy "${packages[@]}"
  else
    die "unsupported package manager; install missing packages manually ($phase)"
  fi
}

ensure_unzip() {
  if ! command -v unzip >/dev/null 2>&1; then
    log "Installing unzip to extract the downloaded archive"
    install_packages unzip
  fi
  command -v unzip >/dev/null 2>&1 || die "unzip is still missing after package installation"
}

install_linux_dependencies() {
  local missing=() tool
  for tool in make cc pkg-config cmake bwrap prlimit git; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
  done
  if command -v pkg-config >/dev/null 2>&1 && ! pkg-config --exists dbus-1; then
    missing+=("dbus-1 development files")
  fi
  [ "${#missing[@]}" -eq 0 ] && return 0

  log "Installing missing Linux dependencies: ${missing[*]}"
  install_packages build
  for tool in make cc pkg-config cmake bwrap prlimit git; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is still missing after package installation"
  done
  pkg-config --exists dbus-1 || die "dbus-1 development files are still missing"
}

# Build dependencies on Termux: the Rust toolchain from Termux (rustup has
# no Android host toolchain), a C toolchain and cmake for aws-lc-sys. Not
# needed: bwrap, prlimit, dbus-1 (no sandbox and no D-Bus on Android).
install_termux_dependencies() {
  local missing=() tool
  for tool in cargo rustc make cc pkg-config cmake git; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "Installing missing Termux packages for: ${missing[*]}"
    install_packages build
    for tool in cargo rustc make cc pkg-config cmake git; do
      command -v "$tool" >/dev/null 2>&1 || die "$tool is still missing after pkg install"
    done
  fi
  log "Android/Termux: no sandbox (no bubblewrap, no user namespaces)."
  log "  Shell commands run on the host only after your approval (once, or a"
  log "  host phase you end with Ctrl+H). In FullAccess mode they run without asking."
}

fetch() {
  local url="$1" output="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 "$url" -o "$output"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$output"
  else
    die "curl or wget is required to download $url"
  fi
}

ensure_rustup() {
  if ! command -v rustup >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/rustup" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
  fi
  if ! command -v rustup >/dev/null 2>&1; then
    local rustup_script="$1/rustup-init.sh"
    log "Installing Rustup and the stable Rust toolchain"
    fetch https://sh.rustup.rs "$rustup_script"
    sh "$rustup_script" -y --default-toolchain stable --profile default
    export PATH="$HOME/.cargo/bin:$PATH"
  fi
  command -v cargo >/dev/null 2>&1 || die "Rustup was installed, but cargo is unavailable"
  command -v rustc >/dev/null 2>&1 || die "Rustup was installed, but rustc is unavailable"
}

archive_hash() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    die "sha256sum or shasum is required to identify the source archive"
  fi
}

# Release target of this machine: the Rust target triple the release
# workflow builds for (Termux on Android reports "Android" as its OS).
release_target() {
  local arch
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) die "no release build for architecture $arch; use the source installer" ;;
  esac
  if [ "$(uname -o 2>/dev/null)" = Android ]; then
    printf '%s-linux-android\n' "$arch"
  else
    printf '%s-unknown-linux-gnu\n' "$arch"
  fi
}

# Tag of the latest release, read from the redirect of /releases/latest.
latest_release_tag() {
  local url
  command -v curl >/dev/null 2>&1 || die "curl is required to find the latest release (or set HARW_RELEASE_TAG)"
  url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$releases_url/latest")" \
    || die "no release found at $releases_url (or set HARW_RELEASE_TAG)"
  case "$url" in
    */tag/v*) printf '%s\n' "${url##*/tag/}" ;;
    *) die "no published release yet at $releases_url; use the source installer" ;;
  esac
}

install_binary_release() {
  local target tag name work_dir unpacked binary runner_dir
  target="$(release_target)"
  tag="${HARW_RELEASE_TAG:-$(latest_release_tag)}"
  name="harw-$tag-$target"
  work_dir="$(mktemp -d)"
  trap 'rm -rf "$work_dir"' EXIT
  log "Downloading $name.tar.gz"
  fetch "$releases_url/download/$tag/$name.tar.gz" "$work_dir/$name.tar.gz"
  fetch "$releases_url/download/$tag/SHA256SUMS" "$work_dir/SHA256SUMS"
  (
    cd "$work_dir"
    grep -E "^[0-9a-fA-F]{64}[[:space:]]+\*?(\./)?$name\.tar\.gz\$" SHA256SUMS > "$name.sha256" \
      || die "SHA256SUMS of $tag has no entry for $name.tar.gz"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c "$name.sha256" >/dev/null
    else
      shasum -a 256 -c "$name.sha256" >/dev/null
    fi
  ) || die "checksum of $name.tar.gz does not match SHA256SUMS; nothing installed"
  tar -xzf "$work_dir/$name.tar.gz" -C "$work_dir"
  unpacked="$work_dir/$name"
  [ -d "$unpacked" ] || unpacked="$work_dir"
  # killer is Linux-only (procfs, pidfd); Android releases ship without it.
  binaries="harw harw-agent-runner"
  case "$target" in *-android) ;; *) binaries="$binaries killer" ;; esac
  for binary in $binaries; do
    [ -f "$unpacked/$binary" ] || die "$name.tar.gz has no $binary; nothing installed"
  done
  mkdir -p "$install_dir"
  for binary in $binaries; do
    install -m 0755 "$unpacked/$binary" "$install_dir/$binary"
  done
  runner_dir="${HARW_HOME:-$HOME/.harw}/bin/.runners/$target/${tag#v}"
  mkdir -p "$runner_dir"
  install -m 0755 "$unpacked/harw-agent-runner" "$runner_dir/harw-agent-runner"
  "$install_dir/harw" agent install-record --bindir "$install_dir" \
    || log "could not record the installation; harw update falls back to the running binary's directory"
  log "Harwness $tag installed into $install_dir"
  "$install_dir/harw" --version
  printf '%s\n' 'Next: harw doctor; then harw to start onboarding.'
}

releases_url="${HARW_RELEASES_URL:-https://github.com/mm9942/Harwness/releases}"
if [ "${1:-}" = --binary ]; then
  install_binary_release
  exit 0
fi

checkout=""
if [ "${1:-}" = --source ]; then
  [ -f scripts/install.sh ] && [ -f Cargo.toml ] && [ -f Makefile ] \
    || die "--source must be run from the Harwness repository root"
  checkout="$PWD"
  work_dir="$(mktemp -d)"
  trap 'rm -rf "$work_dir"' EXIT
else
  # A script piped to bash has no checkout. Keep the source after installation:
  # `make install` records this path for later agent builds.
  mkdir -p "$sources_dir"
  work_dir="$(mktemp -d "$sources_dir/.download-XXXXXX")"
  trap 'rm -rf "$work_dir"' EXIT
  log "Downloading $base_url/$archive_name"
  fetch "$base_url/$archive_name" "$work_dir/$archive_name"
  ensure_unzip
  hash="$(archive_hash "$work_dir/$archive_name")"
  checkout="$sources_dir/$hash"
  if [ ! -f "$checkout/Makefile" ]; then
    mkdir -p "$work_dir/unpacked"
    unzip -q "$work_dir/$archive_name" -d "$work_dir/unpacked"
    extracted="$work_dir/unpacked/Harwness-main"
    [ -f "$extracted/Cargo.toml" ] && [ -f "$extracted/Makefile" ] \
      || die "$archive_name has no Harwness source root"
    [ ! -e "$checkout" ] || die "incomplete source directory exists: $checkout"
    mv "$extracted" "$checkout"
  fi
fi

if is_termux; then
  install_termux_dependencies
else
  ensure_rustup "$work_dir"
  install_linux_dependencies
fi
mkdir -p "$install_dir"
if is_termux; then
  log "Building in $checkout with the Termux Rust toolchain (killer is Linux-only and skipped)"
else
  log "Building in $checkout with its pinned Rust toolchain"
fi
(cd "$checkout" && make install BINDIR="$install_dir")

if [ "$install_dir" = "$HOME/.local/bin" ]; then
  for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
    [ -f "$rc" ] || continue
    if ! grep -q 'added by harw installer' "$rc"; then
      printf '\n%s\n' 'export PATH="$HOME/.local/bin:$PATH"  # added by harw installer' >> "$rc"
    fi
  done
fi

log "Harwness installed from $checkout"
"$install_dir/harw" --version
printf '%s\n' 'Next: harw doctor; then harw to start onboarding.'
