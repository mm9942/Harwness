# Harwness

Harwness is a standalone Rust agent harness. Its trust boundaries are local to
this workspace: configuration is layered under `.harw/`, session transcripts
are append-only JSONL, remote channels can only reduce capabilities, and
secrets/audit state has a dedicated crate.

## Run

```sh
cargo run -p harw-cli -- init
cargo run -p harw-cli -- help
cargo run -p harw-cli -- classify '/help'
cargo run -p harw-cli -- doctor --config-dir .harw
cargo run -p harw-cli -- serve --config-dir .harw
```

`harw init` is the recommended first step on a fresh checkout: it scaffolds the
minimal local/echo configuration shown below into `.harw/` (override the target
with `--config-dir <DIR>`) so `doctor` and `run` work immediately with zero
hand-authored TOML. It only creates files that are absent and never overwrites
an existing config, so it is safe to re-run.

`harw doctor` validates both typed TOML and cross-catalog references (including
legacy plaintext provider keys and duplicate channel-token references), then
reports the loaded catalog counts. It deliberately refuses to continue when
`<config-dir>/config.toml` is absent.

`harw serve` loads and validates the same configuration, then binds the
Streamable HTTP MCP listener. It fails closed and refuses to bind when
`mcp_listener.enabled = false`, when no principal is configured, or when any
principal's `credential_ref` cannot be resolved (only `env:` and `file:` are
supported by the standalone runtime; `keyring:`/`secrets:` are rejected until
their backends are composed).

Minimal configuration:

```toml
# .harw/config.toml
default_provider = "local"
default_model = "echo"

[mcp_listener]
# Explicit opt-in: the standalone Streamable HTTP listener uses this exact
# loopback-only endpoint once runtime composition is enabled.
enabled = false
listen_addr = "127.0.0.1:1337"
path = "/mcp"

[[mcp_listener.principals]]
id = "mia-local"
credential_ref = "env:HARW_MCP_TOKEN"
tenant = "mia"
workspace = "harwness"
job_capabilities = ["read_own", "cancel_own"]
```

```toml
# .harw/providers/local.toml
name = "local"
api = "local-echo"
base_url = "http://127.0.0.1:0"
```

```toml
# .harw/models/echo.toml
id = "echo"
provider = "local"
```

The `local` records make the catalog valid for `doctor`; `harw run` remains an
explicit in-process echo bootstrap and does not make a network request.

## Verification

```sh
cargo check --workspace
cargo test --workspace
```

## Architecture

- `harw-core`: sessions and turn lifecycle
- `harw-tui`: shared input classification and command admission
- `harw-channel` / `harw-channel-telegram`: capability-reducing ingress
- `harw-session-store`: locked, synced JSONL transcript persistence
- `harw-job-runtime`: budgets, leases, retries, and job state
- `harw-knowledge`: memory, diary, dream, workbench, and Kanban data models
- `harw-secrets`: secret-envelope and tamper-evident audit boundaries

The workspace is intentionally dependency-directional: low-level identity and
protocol crates do not depend on terminal, provider, or transport crates.
