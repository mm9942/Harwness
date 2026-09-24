# Telegram work requests and sandboxed execution

> Status: partially implemented · Last reviewed: 2026-09-24

This document turns a Telegram request into
an auditable, server-resolved work launch without ever treating chat text or
model output as an authority over a filesystem path or capability set.

## Invariant

Telegram supplies **intent** only. The harness alone resolves identity,
workspace, catalog, permissions, tools, and process isolation.

```
Telegram event
  -> paired identity + replay gate
  -> typed WorkRequest(workspace alias, role, task)
  -> authoritative workspace and policy resolution
  -> effective sandbox = policy ∩ workspace ∩ channel ∩ role
  -> approval, if required, over an immutable request digest
  -> isolated worker launch
```

No stage may use a natural-language path, attachment filename, model tool
argument, or Telegram callback payload as a workspace selector or permission
grant.

## Typed request boundary

The remote command has a deliberately small grammar:

```text
/request <workspace-alias> <role> <task text>
/review <work-id>
/approve <work-id>
/deny <work-id>
/cancel <work-id>
```

`workspace-alias` is a configured `WorkspaceId`, limited to a conservative
ASCII slug grammar. It is not a path, repository URL, branch, or display name.
The server resolves `(TenantId, WorkspaceId)` to a canonical root registered
at startup. The task text remains untrusted data. Ordinary chat can ask for a
plan but can never implicitly create a work request or change its workspace.

The durable request stores the requester identity, paired tenant, canonical
workspace identity, role, catalog snapshot id, sandbox digest, normalized task
content digest, source update id, and lifecycle state. Follow-up operations
refer only to its generated `WorkId`.

## Sandbox model

`harw-sandbox` is the canonical policy representation. It provides:

- `WorkspaceRegistry`: startup-only canonicalization of configured workspace
  roots below the harness root;
- `WorkspaceBinding`: immutable `(tenant, workspace alias, canonical root)`;
- `PermissionSet`: set intersection only after a trusted policy decision;
- `SandboxSpec`: frozen workspace plus effective permissions, with child specs
  required to be subsets of their parent;
- symlink-aware existing-target and create-target resolution for
  workspace-relative paths.

The baseline permission vocabulary is `ReadWorkspace`, `WriteWorkspace`,
`ExecuteProcess`, `NetworkAccess`, `ReadSecrets`, and `ManagePlugins`. It
intentionally has no "filesystem outside workspace" permission. Tools must
perform the permission and workspace path check at their syscall boundary;
instructions and model context are not enforcement.

The Telegram profile is a reducer, never an authority. Its default removes
write, process execution, secret reads, and plugin management even if an
upstream policy allows them. A Telegram approval may confirm an operation
within the already-resolved ceiling, but cannot restore a capability removed by
the channel profile.

## Worker filesystem and process isolation

The selected workspace is not the worker's unrestricted current directory.
Each work item receives a private run directory such as
`<harness-data>/runs/<work-id>/`, then the execution backend mounts or exposes
only the resolved workspace according to the frozen spec. A write-capable
workflow should use a disposable worktree/copy-on-write staging area and make
promotion back to the registered project an explicit reviewed operation.

On this Linux host, `bwrap` is present at `/usr/bin/bwrap`; the first concrete
backend should translate `SandboxSpec` into a bubblewrap launch plan with a
minimal mount table, empty inherited environment allowlist, no host home
directory, and network disabled unless `NetworkAccess` survives every
intersection. The backend must fail closed if the requested isolation primitive
is unavailable. A future platform backend can implement the same launch-plan
contract, but cannot weaken it.

Process commands, MCP subprocesses, and network egress require separate
allowlists in addition to the coarse permission. A selected skill, plugin, or
MCP server is advisory catalog context until it survives both the catalog
snapshot filter and sandbox policy. Suggesting a server never starts it;
installing, enabling, or changing its launch command remains local
operator-only work.

## Telegram lifecycle

1. Verify webhook credentials (or trusted long-poll source) and deduplicate the
   update id durably before dispatch.
2. Require a paired, non-revoked actor. Unpaired traffic only reaches
   session-free onboarding and cannot create a session, workspace binding, or
   work request.
3. Parse the closed work-request grammar and resolve the alias from the
   authoritative tenant registry. Unknown, disabled, cross-tenant, absolute,
   traversal, or confusable aliases fail closed.
4. Persist and audit the request before queueing. The audit record contains
   stable ids and digests, never bot tokens, pairing codes, raw secret values,
   or attachment bytes.
5. Resolve the effective `SandboxSpec` and per-run capability catalog. Render
   a preview of workspace alias, requested role, and capability reduction.
6. For any configured high-risk operation, create a single-use pending approval
   bound to the requester, chat, thread, message, request id, immutable tool
   digest, allowed decisions, and expiry. Telegram callback data is opaque and
   authenticated; it is never `request_id:approve`.
7. On a valid approval, resume only the stored request/tool payload. A callback
   never supplies command arguments, paths, patches, or a new capability set.
   Timeout, replay, revocation, wrong peer, wrong thread, or stale callback is
   a terminal denial.

## Integration order

1. Wire `harw-sandbox` into session construction and tool execution context.
2. Add configured workspace registrations and a durable paired-identity
   projection; do not derive bindings from Telegram config maps alone.
3. Implement typed `WorkRequest` persistence, update-id deduplication, and
   opaque approval callbacks.
4. Build the bubblewrap launch-plan/backend and test it with real denied write,
   process, network, and path-escape cases.
5. Only then enable Telegram transport ingress. `/sandbox`, `/config`,
   `/policy`, `/plugins`, `/bind`, and `/unbind` remain local-admin/TUI-only.

## Acceptance gates

- A text message containing `/srv`, `..`, a symlink path, a URL, or an
  attachment filename cannot alter the resolved `WorkspaceBinding`.
- A child spawn preserves the parent workspace and cannot add a permission.
- A repeated update or approval callback causes at most one state transition.
- An approved Telegram request cannot write, execute, read secrets, or manage
  plugins unless a future explicitly configured remote profile permits that
  operation; default Telegram never does.
- The process backend demonstrably cannot access an unmounted host path even
  when a model attempts to request it.
