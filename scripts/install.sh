#!/usr/bin/env bash
# Harwness installer — builds `harw` from source and installs it to ~/.local/bin.
#
# Usage:
#   scripts/install.sh [--help]
#
# Environment:
#   HARW_INSTALL_DIR   Target bin directory (default: $HOME/.local/bin)
#
# The installer is idempotent: it never duplicates PATH entries and never
# overwrites an existing ~/.harw configuration (that is `harw`'s own job on
# first run).
set -euo pipefail

usage() {
  sed -n '2,13p' "$0"
  exit 0
}

[ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ] && usage || true

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
install_dir="${HARW_INSTALL_DIR:-$HOME/.local/bin}"

log() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarn:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# --- Prerequisites ----------------------------------------------------------
os="$(uname -s)"
case "$os" in
  Linux|Darwin) ;;
  *) die "unsupported OS: $os (Linux/macOS only)";;
esac

command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust via https://rustup.rs and re-run"

if [ "$os" = "Linux" ] && ! command -v bwrap >/dev/null 2>&1; then
  warn "bubblewrap (bwrap) not found — the sandbox will be unavailable until it is installed"
fi

# --- Build ------------------------------------------------------------------
log "Building harw (release)…"
( cd "$repo_root" && cargo build -p harw-cli --release )

built="$repo_root/target/release/harw"
[ -x "$built" ] || die "build did not produce $built"

# --- Install ----------------------------------------------------------------
mkdir -p "$install_dir"
install -m 0755 "$built" "$install_dir/harw"
log "Installed $install_dir/harw"

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
  harw completion zsh       # shell completions (bash|zsh|fish|powershell)
  harw doctor               # validate config + health checks
EOF
