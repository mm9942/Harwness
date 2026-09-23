#!/usr/bin/env bash
# Harwness installer — installs `harw` either by building from source or by
# downloading a prebuilt release binary.
#
# Usage:
#   scripts/install.sh [--help]
#   scripts/install.sh [--source]              # build from source (default when cargo is present)
#   scripts/install.sh --binary [--version TAG] # download a prebuilt release tarball
#
# Modes:
#   --source   Build `harw` with `cargo build --release` (needs a Rust toolchain).
#              This is the default when `cargo` is on PATH.
#   --binary   Download a release tarball instead of building. This is the
#              default when `cargo` is NOT on PATH. Requires HARW_REPO (see
#              below) unless run inside a git checkout of the Harwness repo,
#              in which case the origin remote is used to find it.
#
# Environment:
#   HARW_INSTALL_DIR   Target bin directory (default: $HOME/.local/bin)
#   HARW_REPO          GitHub repo to fetch release binaries from, as
#                       "owner/repo" or a full git/https remote URL.
#                       Required for --binary mode unless this script is run
#                       from inside a checkout with an "origin" remote.
#   HARW_VERSION       Release tag to install in --binary mode
#                       (default: the latest GitHub release).
#
# The installer is idempotent: it never duplicates PATH entries, never
# overwrites an existing ~/.harw configuration (that is `harw`'s own job on
# first run), and re-running it simply re-installs the same binary.
set -euo pipefail

usage() {
  sed -n '2,29p' "$0"
  exit 0
}

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
install_dir="${HARW_INSTALL_DIR:-$HOME/.local/bin}"
mode=""
version="${HARW_VERSION:-}"

log() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarn:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# --- Argument parsing --------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) usage ;;
    --binary) mode="binary" ;;
    --source) mode="source" ;;
    --version)
      shift
      [ $# -gt 0 ] || die "--version requires an argument"
      version="$1"
      ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
  shift
done

# --- Prerequisites ------------------------------------------------------------
os="$(uname -s)"
case "$os" in
  Linux|Darwin) ;;
  *) die "unsupported OS: $os (Linux/macOS only)";;
esac

if [ -z "$mode" ]; then
  if command -v cargo >/dev/null 2>&1; then
    mode="source"
  else
    mode="binary"
    warn "cargo not found — falling back to --binary install"
  fi
fi

if [ "$os" = "Linux" ] && ! command -v bwrap >/dev/null 2>&1; then
  warn "bubblewrap (bwrap) not found — the sandbox will be unavailable until it is installed"
fi

# --- Mode: source -------------------------------------------------------------
install_from_source() {
  command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust via https://rustup.rs and re-run, or use --binary"

  log "Building harw (release)…"
  ( cd "$repo_root" && cargo build -p harw-cli --release )

  built="$repo_root/target/release/harw"
  [ -x "$built" ] || die "build did not produce $built"

  mkdir -p "$install_dir"
  install -m 0755 "$built" "$install_dir/harw"
  log "Installed $install_dir/harw"
}

# --- Mode: binary --------------------------------------------------------------
# Normalizes an "owner/repo" value out of a raw HARW_REPO setting, which may
# already be "owner/repo" or a git/https remote URL.
normalize_repo() {
  raw="$1"
  case "$raw" in
    git@github.com:*)
      raw="${raw#git@github.com:}"
      raw="${raw%.git}"
      ;;
    https://github.com/*|http://github.com/*)
      raw="${raw#*github.com/}"
      raw="${raw%.git}"
      ;;
    */*)
      : # already looks like owner/repo
      ;;
    *)
      die "cannot parse HARW_REPO value: $1 (expected owner/repo or a GitHub remote URL)"
      ;;
  esac
  printf '%s\n' "$raw"
}

resolve_repo() {
  if [ -n "${HARW_REPO:-}" ]; then
    normalize_repo "$HARW_REPO"
    return 0
  fi

  if [ -d "$repo_root/.git" ] && command -v git >/dev/null 2>&1; then
    origin_url="$(cd "$repo_root" && git remote get-url origin 2>/dev/null || true)"
    if [ -n "$origin_url" ]; then
      normalize_repo "$origin_url"
      return 0
    fi
  fi

  die "HARW_REPO is not set and no origin remote was found — set HARW_REPO=owner/repo (see --help)"
}

detect_target() {
  arch="$(uname -m)"
  case "$os:$arch" in
    Linux:x86_64) printf '%s\n' "x86_64-unknown-linux-gnu" ;;
    Linux:aarch64|Linux:arm64) printf '%s\n' "aarch64-unknown-linux-gnu" ;;
    *) die "no prebuilt binary for $os/$arch — use --source instead" ;;
  esac
}

fetch() {
  # fetch URL OUT_FILE
  url="$1"
  out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$out"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$out"
  else
    die "neither curl nor wget is available to download release assets"
  fi
}

sha256_check() {
  # sha256_check FILE SUMS_FILE
  file="$1"
  sums="$2"
  name="$(basename "$file")"
  dir="$(dirname "$file")"
  line="$(grep -F " $name" "$sums" || true)"
  [ -n "$line" ] || die "no checksum entry for $name in $(basename "$sums")"
  if command -v sha256sum >/dev/null 2>&1; then
    ( cd "$dir" && printf '%s\n' "$line" | sha256sum -c - >/dev/null ) \
      || die "checksum verification failed for $name"
  elif command -v shasum >/dev/null 2>&1; then
    expected="$(printf '%s\n' "$line" | awk '{print $1}')"
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
    [ "$expected" = "$actual" ] || die "checksum verification failed for $name"
  else
    die "neither sha256sum nor shasum is available to verify the download"
  fi
}

install_from_binary() {
  repo="$(resolve_repo)"
  target="$(detect_target)"

  tag="$version"
  if [ -z "$tag" ]; then
    if command -v curl >/dev/null 2>&1; then
      tag="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
        | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name":[[:space:]]*"([^"]+)".*/\1/')"
    elif command -v wget >/dev/null 2>&1; then
      tag="$(wget -qO- "https://api.github.com/repos/$repo/releases/latest" \
        | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name":[[:space:]]*"([^"]+)".*/\1/')"
    else
      die "neither curl nor wget is available to resolve the latest release"
    fi
    [ -n "$tag" ] || die "could not resolve the latest release tag for $repo"
  fi

  asset="harw-${tag}-${target}.tar.gz"
  base_url="https://github.com/$repo/releases/download/$tag"

  work_dir="$(mktemp -d)"
  trap 'rm -rf "$work_dir"' EXIT

  log "Downloading $asset ($repo @ $tag)…"
  fetch "$base_url/$asset" "$work_dir/$asset"
  fetch "$base_url/SHA256SUMS" "$work_dir/SHA256SUMS"

  log "Verifying checksum…"
  sha256_check "$work_dir/$asset" "$work_dir/SHA256SUMS"

  log "Extracting…"
  tar -xzf "$work_dir/$asset" -C "$work_dir"

  extracted_bin="$(find "$work_dir" -type f -name harw -perm -u+x | head -n1)"
  [ -n "$extracted_bin" ] || extracted_bin="$(find "$work_dir" -type f -name harw | head -n1)"
  [ -n "$extracted_bin" ] || die "downloaded archive did not contain a 'harw' binary"

  mkdir -p "$install_dir"
  install -m 0755 "$extracted_bin" "$install_dir/harw"
  log "Installed $install_dir/harw ($tag, $target)"
}

# --- Run ----------------------------------------------------------------------
case "$mode" in
  source) install_from_source ;;
  binary) install_from_binary ;;
  *) die "internal error: unknown mode $mode" ;;
esac

# --- PATH wiring (idempotent) ----------------------------------------------
add_path_line='export PATH="$HOME/.local/bin:$PATH"  # added by harw installer'
for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
  [ -e "$rc" ] || continue
  if ! grep -q "added by harw installer" "$rc" 2>/dev/null; then
    if ! printf '%s' "$PATH" | tr ':' '\n' | grep -qx "$install_dir"; then
      printf '\n%s\n' "$add_path_line" >> "$rc"
      log "Added $install_dir to PATH in $rc (open a new shell to pick it up)"
    fi
  fi
done

# --- Done -------------------------------------------------------------------
"$install_dir/harw" --version || true
cat <<EOF

Harwness installed. Next:
  harw                      # first run launches onboarding, then the chat TUI
  harw completions --install    # shell completions (detects \$SHELL; bash|zsh|fish|elvish|powershell)
  harw doctor               # validate config + health checks
EOF
