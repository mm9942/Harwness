# PL-96 / 03 — Session, channel, trust, privilege and edge security

**Scope:** code-verified CURRENT and planned hardening. No operational vulnerability is asserted beyond the particular source paths stated in [E01–E20](01-evidence-register.md).

## 1. Session ownership and local/remote composition

**CURRENT:** `harw gateway --session-socket` is an opt-in real local control plane, with UDS peer credential binding and durable `CoreTurnDriver` composition. `harw-session-daemon/src/compose.rs::Daemon::start` uses `SessionHost::open`; the latter supplies `ToolHost::disabled()`. The Com layer can serve `session.*`, `tool.*` and `gateway.*` method tables via `PortOffer::All`, but the tool gateway explicitly has no sandbox/tools. Remote device ingress currently owns distinct state and defaults to Observer. More than one transport **does not yet** equal one shared session owner.

**TARGET:** introduce a SessionDirectory/OwnerLease *behind* existing SessionHost and state adapters. Bind SessionId -> one authoritative owner, epoch, fencing version, tenant, workspace and policy snapshot. Multiple clients attach to that owner, with **per-client** grants; no inherited local operator authority for remote identities.

Security invariants:
- Authenticated transport identity is minted at the trusted ingress (local SO_PEERCRED; remote verified device identity), never parsed from arbitrary headers or model text.
- A session has exactly one concurrent writable owner. Stale owner/lease is refused.
- Transcripts, approvals and jobs are durable but **not** permission authority.
- Reconnecting device cannot infer another device's approval actor.
- Approval is not authority expansion. Device revocation invalidates streams, pending input and in-flight permissions.
- An absent backend fails closed; do not return `tool.* available` just because the method name is routable.
- Shared Cloud Home is logical owner routing, **not** a shared writable filesystem without locking/tenant boundaries.

Implementation order: (a) common owner directory interface (no side effects); (b) existing UDS/remote adapter tests; (c) durable owner lease and fenced rebind; (d) ToolHost opt-in with grants/sandbox; (e) remote admission/revocation/replay; (f) migration/rollbacks with old separate hosts as fallback. Preserve session file formats and state dir compatibility with explicit migrations. Owner failures return retryable structured errors; do not randomly route a live SessionId to another node.

Suggested tests: two clients/one owner, reader+writer, three concurrent reconnects, lease expiry/host crash, old epoch write rejected, approval actor mismatch, UID mismatch, remote Observer denied privileged tool, revoked device interrupted during streaming, transcript replay ordering, per-tenant isolation, no gateway tools on disabled host.

## 2. Executable Gateway ToolHost and authority

**CURRENT:** `harw-session-host/src/tool_host.rs` already contains `ToolHostBuilder`, gateway sandbox interface, scoped ToolGrants and descriptor filtering; `open_with_tools` exists. Local daemon currently selects disabled tool host. Do not create a second tool-execution policy engine to bypass these seams.

**TARGET:** explicit `GatewayToolMount` in the composition root whose service provenance is pinned to a trusted, immutable runtime/config snapshot. The actual `ToolHost` must derive its `ToolGrant` and `SandboxSpec` from the same actor/agent/session chain as the local ToolExecutor. A valid agent connection to `tool.list` is not a grant for `tool.call`; revalidate on dispatch. There must be a capability to revoke/expire in-flight calls and a single audited approval backend. TUI human SessionPorts never silently obtain `tool_call`.

Tests: fake sandbox with two tools/one allowed; disabled host returns explicit refusal; tool listed but grant revoked before invocation is denied; sandbox fails unavailable; approval stays bound to actor and exact request; child/grandchild scopes never widen; unauthenticated remote route cannot call tool.

## 3. Telegram and common multi-surface operations

**CURRENT on *unmerged* Telegram branch** `feature/telegram-bot-api-10-3-command-registry@91db7b5904`: proposed command projection from `#[operation]` / OperationRegistry rather than duplicate hardcoded HANDLED_COMMANDS; native channel lifecycle commands remain special; human principal is at most Maintainer and dispatch revalidates permission. However `ChannelReduced` matches only the first argument token. `/dream review` may be declared channel-reduced while `/dream review <id> accept <proposal>` has a nested mutating operation. A first-token permission test cannot distinguish them.

**TARGET:** parse a typed command AST before admission and apply a surface-specific **full grammar/effect policy** to the parsed invocation. A read-only channel permission for `review` must not authorize `review ... accept`; likewise `/op` alias, Telegram aliases or whitespace/case normalization must not bypass policy.

Suggested contract sketch (proposal only):

```rust
struct ParsedOperation {
    canonical_id: OperationId,
    args: ValidatedArgs,
    effect: EffectClass,       // Read, ProposalWrite, Mutation, Privileged
    required_scope: ResourceScope,
}
trait SurfaceAdmission {
    fn admit(&self, actor: &Principal, channel: IngressSurface,
             op: &ParsedOperation, current: &RightsSnapshot) -> Decision;
}
```

Keep side-effect semantics in the **operation domain**, not Telegram-specific string switches. A command must still pass runtime trust/SandboxSpec, consent/approval and scoped service checks *after* surface policy. Do not grant a channel an Owner principal. Do not assume the branch is deployed or even merged.

Negative test matrix: allowed `/dream review` and `/dream review ID`; refused `/dream review ID accept P` and `reject` in read-only mode; allowed direct approval only with a separate, explicit effect permission, approved actor and backend; forbidden `/skills activate`, `/dream run` without explicit scope; malformed, aliases, `/op`, multi-token, escaped tokens, chat/group/topic isolation, command menu consistency. Existing paired/admitted Telegram model turns keep Observer identity unless separately authorized.

## 4. Warden proof migration is release-critical

**CURRENT source-proofed:** `harw-dod-warden-proto/src/signed.rs` defines sign/verify for v2 with keyed proof, keyring, expiry and nonce ledger. The older `proof.rs` expressly lacks MAC, nonce and expiration. **But** `harw-dod-warden/src/warden.rs::Warden::handle` still checks `request.proof.verify` on the legacy v1 type. A secure v2 module cannot substitute for a call-site migration.

**TARGET:** migrate Warden request decoding, Escalator proof issuance, Warden::handle validation and privileged binary adapter **atomically across the enforcement boundary**. Reject any v1 proof at the public privileged ingress, verify key provenance and key scope, version, action digest, matching cgroup/subject, stage, bounded timestamp, durable nonce transaction and legitimate request principal before irreversible action. Fail closed on ledger I/O, clock rollback/large skew, missing key or mismatched action. Audit attempt before action and final outcome after action. Preserve a migration plan for running old clients, but never accept unsigned v1 as “compatibility” on a privileged interface.

Required independent tests:
- one valid signed request with disposable executor/audit is admitted once;
- same nonce twice (including restart) is rejected, without consuming a nonce for invalid signature;
- changed action/stage/cgroup/subject/key ID denied;
- expired/future proofs denied; key rotation/revocation tested;
- malformed input, partial reads, concurrent requests and ledger failures never execute;
- no compile link/path allowing unsigned v1 at Warden action dispatch;
- DoD-specific dependency, no-C-build/arch gates and package-scoped test on exact SHA.

The privileged closure must remain separate from model SDKs, HTTP clients, generic app runtime and experimental proxy deps. **Release block:** no promotion to privileged production enforcement until the v2 end-to-end path is demonstrated; this planning PR does not do so.

## 5. Repo-local config trust

**CURRENT:** `harw-home/src/trust.rs` binds an approved project to canonical path, UID and BLAKE3 digest over security-relevant `.harw` sources. The untrusted repo merge in `harw-config` is restrictive. Source comments identify concurrent trust-store writer lost-update and digest-check-to-config-read TOCTOU windows.

**TARGET:** hold immutable verified config bytes and parse those bytes, not a second unbound path read; use locked/CAS writes for trust-store updates; do not add digest of *every* repo file or unbounded traversal. Revocation/revalidation invalidates old trusted snapshots as defined by session policy (decide whether existing sessions are allowed to finish). Adversarial swap tests, symlink/path traversal tests and integrity check for provider/MCP/secret/approval scope.

Do not present this as a demonstrated remote exploit: an attacker needs an actual writable trust-path and exploitable loading sequence. Test it, prove it, then assign severity.

## 6. Reverse Proxy vs node mutual-auth plane

**BRANCH only:** `coop/proxy-plan@918b5c8564` has a pure Rust route table/headers/upstream group policy, **no sockets, Pingora adapter or external edge deployment**. Unknown host/path must not fall back to default upstream; `x-harw-*` is stripped; health never overrides revoked NodeState. General weighted upstream selection is not for sessions—SessionId owner lookup is required.

**TARGET:** choose Pingora or existing hyper/Tower with a reviewed dependency/TLS policy before mounting an internet-facing listener. Keep Pingora (if adopted) at browser/public edge, not node-to-node. Enforce auth/identity at the trusted service, not through a client-supplied plaintext header. Evaluate rustls crypto-provider and PQ-hybrid differences without overclaiming browser transport guarantees. Check rate, size, timeout, WebSocket upgrade, SSRF and DNS resolution/rebinding limits and adversarial reverse-proxy header tests.

## 7. Security review boundaries for this PR

This planning PR **must not** alter runtime security, activate any agent or grant credentials. Reviewers should prioritize E12 Warden, E10 channel grammar and E06 provider integrity, but report whether issues are present on **dev**, only on **unmerged branches** or only in a target plan. Separate severity of a code gap from evidence of exploit reachability.
