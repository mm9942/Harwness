#!/usr/bin/env bash
# Execute the actual packaged Linux binaries through the public installer.
set -euo pipefail
tag="${1:?TAG required}"
target="${2:?TARGET required}"
dist="$(realpath "${3:?DIST required}")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/mirror/$tag" "$work/home" "$work/bin"
name="harw-$tag-$target.tar.gz"
cp "$dist/$name" "$work/mirror/$tag/"
(cd "$work/mirror/$tag" && sha256sum "$name" > SHA256SUMS)
printf '%s\n' "$tag" > "$work/mirror/latest"
# HOME is isolated for installer PATH setup; no operator files are touched.
env HOME="$work/home" HARW_HOME="$work/home/.harw" \
  HARW_BASE_URL="file://$work/mirror" HARW_INSTALL_DIR="$work/bin" \
  HARW_RELEASES_URL='' HARW_RELEASE_TAG="$tag" \
  bash scripts/install.sh --binary
"$work/bin/harw" --version | grep -F "${tag#v}"
"$work/bin/harw" --help >/dev/null
"$work/bin/harw-agent-runner" --capabilities > "$work/capabilities.json"
python3 -m json.tool "$work/capabilities.json" >/dev/null
export HARW_HOME="$work/home/.harw"
agent="$work/bin/release-smoke-agent"
"$work/bin/harw" agent build examples/agents/hello-analyst -o "$agent"
"$agent" --verify
"$agent" --manifest >/dev/null
# Metadata is insufficient: admit and execute the packaged runner too.
env ANTHROPIC_API_KEY=release-smoke-not-a-key \
  "$agent" --requirements --json > "$work/requirements.json"
python3 - "$work/requirements.json" <<'PY'
import json, sys
with open(sys.argv[1]) as source:
    result = json.load(source)
if result.get('verdict', {}).get('admitted') is not True:
    raise SystemExit('Packaged agent admission was not successful')
PY
env ANTHROPIC_API_KEY=release-smoke-not-a-key HARW_OFFLINE_ECHO=release-smoke-ok \
  timeout 120 "$agent" --offline-echo 'Release smoke test' \
  > "$work/echo.out" 2> "$work/echo.err"
grep -F 'release-smoke-ok' "$work/echo.out"
grep -F 'offline echo: model calls are answered locally' "$work/echo.err"
