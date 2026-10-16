# Patches for other branches (not applied)

## `s05-daemon-on-com-layer.patch`

Target: branch `ws/s05-daemon` (PR #91 line), `harw-session-daemon` only.
Apply from the repository root: `git apply docs/planning/68-local-cloud-websocket/patches/s05-daemon-on-com-layer.patch`.
It also needs the scribe wiring for `harw-session-com` (workspace member,
`arch-policy` layer A) and the dependency line the patch adds to
`harw-session-daemon/Cargo.toml`.

What it does: `UdsServer::run` keeps the socket lifecycle (private directory,
stale-socket probe, identity-checked cleanup, accept backoff) and hands each
accepted stream to `harw_session_com::ComServer::serve_io`. The per-connection
code (`local_identity`, `handle_accept`, `serve_stream`, about 110 lines) is
removed.

Verified in a scratch worktree of `origin/ws/s05-daemon` with `harw-session-com`
copied in: all 5 `tests/compose.rs` and all 11 original `tests/e2e_uds.rs`
tests pass unchanged, plus 2 new ones in the patch: an owner-tier peer reaches
`gateway.status`; an operator-tier peer is denied it.

Behaviour that changes: `tool.*` and `gateway.*` are served (they were not),
local identities carry the tier's gateway caps, a refused caller gets HTTP 403
or 503 before the upgrade instead of a silent close after it.

This is the user's decision (see PL-68 integration status); nothing here
changes `ws/s05-daemon`.
