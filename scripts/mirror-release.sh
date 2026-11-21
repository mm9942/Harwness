#!/usr/bin/env bash
# Mirrors downloaded GitHub release bytes, then promotes discovery pointers.
# Caller must download GitHub assets into DIST and verify their identity first.
set -euo pipefail
tag="${1:?TAG required}"
dist="$(realpath "${2:?DIST required}")"
: "${R2_ACCOUNT_ID:?R2_ACCOUNT_ID required}"
: "${AWS_ACCESS_KEY_ID:?R2 access key required}"
: "${AWS_SECRET_ACCESS_KEY:?R2 secret key required}"
: "${R2_BUCKET:?R2_BUCKET required}"
: "${R2_PREFIX:?R2_PREFIX required}"
public_base="${R2_PUBLIC_BASE:-https://get.harw.dev/harw}"
python3 scripts/release_bundle.py verify "$tag" "$dist"
endpoint="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
s3() { aws s3 cp --endpoint-url "$endpoint" --only-show-errors "$@"; }
for path in "$dist"/*.tar.gz "$dist/SHA256SUMS"; do
  file="$(basename "$path")"
  destination="s3://${R2_BUCKET}/${R2_PREFIX}/${tag}/${file}"
  s3 "$path" "$destination"
  s3 "$destination" "$work/$file"
  cmp "$path" "$work/$file"
  # A configured bucket does not prove the public route serves these bytes.
  curl --fail --silent --show-error --location --retry 3 \
    "$public_base/$tag/$file" -o "$work/public-$file"
  cmp "$path" "$work/public-$file"
done
# Mutable discovery files come only after every versioned object is verified.
# A legacy ZIP is unnecessary: the default source path now has a checked tarball.
s3 scripts/install.sh "s3://${R2_BUCKET}/${R2_PREFIX}/install.sh" \
  --content-type 'text/x-shellscript; charset=utf-8' --cache-control no-cache
curl --fail --silent --show-error --location --retry 3 \
  "$public_base/install.sh" -o "$work/public-install.sh"
cmp scripts/install.sh "$work/public-install.sh"
bash scripts/release-manifest.sh "$tag" "$dist" > "$work/version.json"
printf '%s\n' "$tag" > "$work/latest"
for file in version.json latest; do
  s3 "$work/$file" "s3://${R2_BUCKET}/${R2_PREFIX}/${file}" --cache-control no-cache
  curl --fail --silent --show-error --location --retry 3 \
    "$public_base/$file" -o "$work/public-$file"
  cmp "$work/$file" "$work/public-$file"
done
