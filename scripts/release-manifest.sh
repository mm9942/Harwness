#!/usr/bin/env bash
# Write version.json, the release manifest of the get.harw.dev mirror, from
# the SHA256SUMS of one release directory.
# Usage: scripts/release-manifest.sh vX.Y.Z DIR > version.json
#
# The installer reads "tag" from it (before the plain `latest` file). Every
# file path is relative to the mirror base (<tag>/<file>), and every sha256 is
# the line of SHA256SUMS for that file, so the manifest never disagrees with
# the checksums the installer verifies.
set -euo pipefail

tag="${1:?usage: $0 vX.Y.Z DIR}"
dir="${2:?usage: $0 vX.Y.Z DIR}"
case "$tag" in v[0-9]*) ;; *) echo "tag must look like v0.9.0" >&2; exit 1 ;; esac
version="${tag#v}"
sums="$dir/SHA256SUMS"
[ -f "$sums" ] || { echo "missing $sums" >&2; exit 1; }

entry() { # entry FILE -> {"file": ..., "sha256": ...} or nothing
  local hash
  hash="$(awk -v f="$1" '{ name = $2; sub(/^\*/, "", name); sub(/^\.\//, "", name) } name == f { print $1; exit }' "$sums")"
  [ -n "$hash" ] || return 1
  printf '{"file": "%s/%s", "sha256": "%s"}' "$tag" "$1" "$hash"
}

source_entry="$(entry "harwness-$version-source.tar.gz" || true)"
targets=""
while read -r _ name; do
  name="${name#\*}"
  name="${name#./}"
  case "$name" in
    "harw-$tag-"*.tar.gz)
      target="${name#"harw-$tag-"}"
      target="${target%.tar.gz}"
      [ -z "$targets" ] || targets="$targets,"
      targets="$targets
    \"$target\": $(entry "$name")"
      ;;
  esac
done < "$sums"

printf '{\n'
printf '  "version": "%s",\n' "$version"
printf '  "tag": "%s",\n' "$tag"
printf '  "published_at": "%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [ -n "$source_entry" ]; then
  printf '  "source": %s,\n' "$source_entry"
fi
printf '  "targets": {%s\n  }\n' "$targets"
printf '}\n'
