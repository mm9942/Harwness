#!/bin/sh
# End-to-end check of `harw agent build --native` (backend B,
# harw-agent-compiler/src/backend/native.rs): the only place a real native
# build runs. The golden tests only compare the generated crate's files.
#
# Steps:
#   1. build `harw` (package harw-cli, debug);
#   2. `harw agent build examples/agents/hello-analyst --native -o <tmp>/agent`
#      (generated crate + `cargo build --release` against this checkout);
#   3. run the native binary: `--verify`, `--verify --json`, `--manifest`,
#      `--requirements --json` and a one-shot with `--offline-echo`, checking
#      exit codes and output fragments.
#
# Environment:
#   NATIVE_E2E_HOME  harw home to use (default: a fresh temp directory). Set it
#                    to a persistent path to reuse the native build cache
#                    (<home>/cache/agent-builds/target) across runs; CI does.
#   HARW_BIN         an already built `harw` to use instead of building one.
#   CARGO_TARGET_DIR respected when locating the freshly built `harw`.
#
# Usage: sh scripts/native-e2e.sh
set -eu

log() { printf '==> %s\n' "$*"; }
fail() {
	printf 'native-e2e: FAIL: %s\n' "$*" >&2
	exit 1
}

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
example="$repo_root/examples/agents/hello-analyst"
agent_id="harwness.example.hello-analyst@1"
[ -f "$example/definition.toml" ] || fail "missing example definition: $example/definition.toml"

work=$(mktemp -d "${TMPDIR:-/tmp}/harw-native-e2e.XXXXXX")
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

HARW_HOME=${NATIVE_E2E_HOME:-$work/home}
export HARW_HOME
mkdir -p "$HARW_HOME" "$work/cwd"
# The sources come from --harw-src below, never from the caller's shell.
unset HARW_SRC || true

# ---------------------------------------------------------------------------
# 1. harw
# ---------------------------------------------------------------------------
if [ -n "${HARW_BIN:-}" ]; then
	harw_bin=$HARW_BIN
else
	log "building harw (cargo build -p harw-cli --bin harw)"
	(cd "$repo_root" && cargo build --locked -p harw-cli --bin harw)
	harw_bin="${CARGO_TARGET_DIR:-$repo_root/target}/debug/harw"
fi
[ -x "$harw_bin" ] || fail "no harw executable at $harw_bin"

# ---------------------------------------------------------------------------
# 2. native build
# ---------------------------------------------------------------------------
agent="$work/agent"
log "harw agent build $example --native -o $agent (HARW_HOME=$HARW_HOME)"
if ! (cd "$work/cwd" && "$harw_bin" agent build "$example" --native \
	--harw-src "$repo_root" -o "$agent") >"$work/build.out" 2>"$work/build.err"; then
	cat "$work/build.out" "$work/build.err" >&2
	fail "harw agent build --native failed"
fi
cat "$work/build.out"
grep -F "(native)" "$work/build.out" >/dev/null ||
	fail "build output does not name the native backend"
digest=$(sed -n 's/^artifact \([0-9a-f][0-9a-f]*\)$/\1/p' "$work/build.out" | head -n 1)
[ -n "$digest" ] || fail "build output has no 'artifact <digest>' line"
[ -x "$agent" ] || fail "no executable at $agent"

# ---------------------------------------------------------------------------
# 3. run the native agent
# ---------------------------------------------------------------------------
if command -v timeout >/dev/null 2>&1; then
	with_timeout() { timeout 120 "$@"; }
else
	with_timeout() { "$@"; }
fi

# run_agent EXPECTED_CODE ARGS…: runs the agent in an empty directory, output
# to $work/out and $work/err, and checks the exit code.
run_agent() {
	expected=$1
	shift
	log "agent $*"
	if (cd "$work/cwd" && with_timeout "$agent" "$@") </dev/null >"$work/out" 2>"$work/err"; then
		code=0
	else
		code=$?
	fi
	if [ "$code" -ne "$expected" ]; then
		cat "$work/out" "$work/err" >&2
		fail "agent $*: exit $code, expected $expected"
	fi
}

# expect FILE PATTERN (grep -E): FILE must match PATTERN.
expect() {
	if ! grep -E -- "$2" "$1" >/dev/null; then
		printf -- '--- %s ---\n' "$1" >&2
		cat "$1" >&2
		fail "expected /$2/ in $(basename "$1")"
	fi
}

run_agent 0 --verify
expect "$work/out" "^ok: $digest\$"

run_agent 0 --verify --json
expect "$work/out" "\"ok\":true"
expect "$work/out" "\"digest\":\"$digest\""

run_agent 0 --manifest
expect "$work/out" "^agent: $agent_id \\(hello-analyst\\)"
expect "$work/out" "^tools: .*fs\\.read"
expect "$work/out" "^shell: false"

run_agent 0 --requirements --json
expect "$work/out" "\"agent\": *\"$agent_id\""
expect "$work/out" "\"admitted\": *true"

# A release build ignores HARW_OFFLINE_ECHO without --offline-echo, so the
# flag is required. [models].required_env names ANTHROPIC_API_KEY; the echo
# never uses it, but the runtime checks it is set.
echo_reply="native e2e offline echo"
HARW_OFFLINE_ECHO=$echo_reply
ANTHROPIC_API_KEY=native-e2e-not-a-key
export HARW_OFFLINE_ECHO ANTHROPIC_API_KEY
run_agent 0 --offline-echo "What is in this folder?"
expect "$work/out" "$echo_reply"
expect "$work/err" "offline echo: model calls are answered locally"

log "native backend OK (artifact $digest)"
