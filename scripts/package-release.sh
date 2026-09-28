#!/usr/bin/env bash
# Package one built target into a release tarball and refresh SHA256SUMS.
#
# Usage: scripts/package-release.sh TAG TARGET [DIST]
#
# Expects the binaries of a `--target TARGET` build:
#   target/TARGET/release/harw, target/TARGET/release/killer,
#   target/TARGET/release-runner/harw-agent-runner
# Writes DIST/harw-TAG-TARGET.tar.gz (one directory harw-TAG-TARGET/ with
# the binaries (no killer on Android), README.md and both licenses) and rewrites
# DIST/SHA256SUMS over every tarball in DIST. This is the layout
# `harw update` and `scripts/install.sh --binary` read; `make release` and
# `.github/workflows/release.yml` both call this script.
set -euo pipefail

die() { printf 'package-release: %s\n' "$*" >&2; exit 1; }

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || die "usage: $0 TAG TARGET [DIST]"
tag="$1"
target="$2"
dist="${3:-dist}"
case "$tag" in v[0-9]*) ;; *) die "TAG must look like vX.Y.Z, got '$tag'" ;; esac

stage_name="harw-${tag}-${target}"
release_dir="target/${target}/release"
runner_dir="target/${target}/release-runner"
# `killer` needs Linux procfs and pidfd (harw-killer refuses other target
# OSes at compile time); Android releases ship without it.
binaries=("$release_dir/harw" "$runner_dir/harw-agent-runner")
case "$target" in
  *-android) ;;
  *) binaries+=("$release_dir/killer") ;;
esac
for binary in "${binaries[@]}"; do
  [ -x "$binary" ] || die "missing $binary (build with --target $target first)"
done

mkdir -p "$dist"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
stage="$work/$stage_name"
mkdir -p "$stage"
cp "${binaries[@]}" "$stage/"
cp README.md LICENSE-MIT LICENSE-APACHE "$stage/"
# ustar only: harw update and install.sh --binary reject GNU long-name and
# pax headers, links and special files before unpacking.
tar --format=ustar -C "$work" -czf "$dist/$stage_name.tar.gz" "$stage_name"

(
  cd "$dist"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- *.tar.gz > SHA256SUMS
  else
    shasum -a 256 -- *.tar.gz > SHA256SUMS
  fi
)
printf 'packaged %s/%s.tar.gz\n' "$dist" "$stage_name"
