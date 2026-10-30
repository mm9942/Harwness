#!/usr/bin/env bash
# Build/install through the default source installer, from the final source asset.
set -euo pipefail
tag="${1:?TAG required}"
dist="$(realpath "${2:?DIST required}")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/mirror/$tag" "$work/home"
cp "$dist"/*.tar.gz "$dist/SHA256SUMS" "$work/mirror/$tag/"
bash scripts/release-manifest.sh "$tag" "$dist" > "$work/mirror/version.json"
env HOME="$work/home" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" \
  RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" HARW_HOME="$work/home/.harw" \
  HARW_BASE_URL="file://$work/mirror" HARW_INSTALL_DIR="$work/bin" \
  HARW_SOURCES_DIR="$work/sources" HARW_RELEASES_URL='' HARW_RELEASE_TAG="$tag" \
  bash scripts/install.sh
"$work/bin/harw" --version | grep -F "${tag#v}"
for binary in harw killer harw-job-exec harw-agent-runner; do
  test -x "$work/bin/$binary"
done
"$work/bin/harw-agent-runner" --capabilities | python3 -m json.tool >/dev/null
