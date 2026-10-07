---
id: GAP-WEB-AUTHZ
title: Gap hunt — area web-authz (confirmed findings)
status: open
date: 2026-09-27
tags: [gap-hunt, findings, web-authz]
related:
  - ../patterns.md
  - ../R15-patterns.md
---

# Area `web-authz`: confirmed findings

Workflow `gap-hunt-area`, read-only:
- 3 Opus finders and a completeness critic did the search.
- Verification used reproduce and intent; scope was added as a tie-breaker when the two disagreed.
- **41 confirmed**, 0 rejected.

Single-file findings (1F = yes) go to `gap-fix`. Findings spanning several files go into a contract wave.

| # | Severity | Pattern | File:Line | 1F | Title | Fix |
|---|---|---|---|---|---|---|
| 1 | medium | M3 | `harw-authority/src/lib.rs:105` | yes | PermissionSet docs claim there is no public policy or generic grant constructor, yet `from_policy` is exactly that (96 caller files) | Rewrite both doc blocks to match reality. `from_policy` is a public, unrestricted constructor for code-defined grants and ceilings. The trust boundary lies with the callers of `SandboxSpec::from_resolved*` and `PolicyBootstrap`, not with this type. Delete t… |
| 2 | medium | NEW:deserialize-bypasses-constructor | `harw-authority/src/lib.rs:522` | yes | WorkspaceBinding derives Deserialize, bypassing the registry canonicalization its doc says is required | Remove `Deserialize` from WorkspaceBinding. Have AuthoritySnapshot persist a plain serializable `(TenantId, WorkspaceId)` reference instead, and have `reissue` compare that reference with the tenant and workspace of the freshly resolved binding. Add a compi… |
| 3 | medium | M3 | `harw-authority/src/lib.rs:976` | no | test-support docs claim PolicyBootstrap is the only production path to a non-empty NetworkScope; ungated from_resolved_with_network exists and PolicyBootstrap has no production caller | Correct the docs so they name `from_resolved_with_network` as the runtime path to a non-empty scope and state that `test-support` exists only to mark the origin `TestOnly`. Remove the 'sonst nirgends' claims from the three Cargo.toml comments. Also record i… |
| 4 | medium | M2 | `harw-netsec/src/store.rs:320` | no | Revoked nodes count toward max_nodes forever and nothing can delete them, so max_nodes acts as a lifetime enrolment quota | Count only non-revoked nodes against max_nodes and keep revoked ids as bounded tombstones (e.g. a compact id set with its own cap or retention window) so ids still cannot be reused. Alternatively, add an operator-only purge path. Correct the Revoked doc. Al… |
| 5 | medium | M2 | `harw-netsec/src/store.rs:476` | no | persist() writes state files that load() then refuses (over 32 MiB), and max_nodes has no upper bound, so the daemon cannot restart | In persist(), compare `bytes.len()` against MAX_STATE_BYTES before creating the temp file and return an error (e.g. CapacityExhausted or StoreTooLarge), so mutate() keeps the old state. Also cap max_nodes in NetsecConfig::validate at a value derived from MA… |
| 6 | medium | NEW:stale-lock-takeover | `harw-oauth/src/codex_refresh.rs:192` | yes | Codex refresh lock is not mutually exclusive: a live holder is treated as stale, and its guard deletes the next owner's lock | (a) Put a tokio::time::timeout around the POST and body read that is clearly shorter than LOCK_STALE_AFTER (for example 8 s), so a live holder can never look stale. (b) Write a unique nonce (pid + random + time) into the lock file when it is created. Store … |
| 7 | medium | M2 | `harw-security-hub/src/table.rs:111` | no | Context table has only a global cap: one policy peer can fill it and cause TableFull for all other peers | Track live entries per owner_uid in ContextTable (for example HashMap<u32, usize>, decremented on purge) and refuse inserts over a per-owner cap (new `max_contexts_per_peer` in config, default for example 64 or max_contexts / 16) with a distinct error such … |
| 8 | medium | M1 | `harw-web/src/identity.rs:531` | yes | security_hub mode with require_context=false: omitting the context header drops the hub-assigned tenant, so the caller runs without a tenant and sees all tenants | In without_context(): if the peer's uid has a uid_principals binding, deny (return `denial`) unless uid_tenants also pins a tenant for that uid. Alternatively, reject at build_resolver_with any config in security_hub mode with require_context=false where a … |
| 9 | low | M3 | `docs/design/hardening-gap-analysis.md:288` | yes | Gap analysis (last reviewed 2026-09-24) says harw-protocol wire types lack deny_unknown_fields; they all have it | Mark this evidence bullet as resolved (see wire.rs, test `inbound_envelopes_deny_unknown_fields`) or remove it. |
| 10 | low | M3 | `harw-authority/src/lib.rs:228` | yes | is_public_dns_name doc says [a-z0-9-.] but the code also accepts '_' | Either drop `// b == b'_'` (stricter, fail-closed) or document `_`. Dropping it is preferred for a public-hostname check. |
| 11 | low | M1 | `harw-authority/src/lib.rs:1229` | yes | Trusted policy is checked by path and then opened following symlinks; the OperatorHome source has no owner or parent-directory check | Open with O_NOFOLLOW (rustix::fs::open with OFlags::RDONLY/NOFOLLOW/CLOEXEC; rustix is a workspace dependency) and compare (dev, ino) from fstat with the earlier symlink_metadata. For OperatorHome, require `metadata.uid() == rustix::process::geteuid().as_ra… |
| 12 | low | P5 | `harw-egress/src/classify.rs:26` | yes | unwrap() on IP parsing in classify doctests | Parse with `?` (`let ip: std::net::IpAddr = "...".parse()?;`) and end with `# Ok::<(), std::net::AddrParseError>(())`, or use the const constructors Ipv4Addr::new / Ipv6Addr::new. |
| 13 | low | M1 | `harw-egress/src/client.rs:110` | no | build_client enforces address classes only inside the DNS resolver, so IP-literal URLs skip it unless every caller remembers check_url | Minimal: in MistralOcrClient::new call `policy.check_url(&config.base_url)?` and keep the checked base URL, so an IP literal goes through check_addr. Structural (harw-egress): export an `EgressClient { client, policy }` whose only request constructor is `re… |
| 14 | low | M3 | `harw-egress/src/lib.rs:3` | yes | harw-egress crate doc covers only the in-process client; the SOCKS5 proxy and netns relay are missing | Add bullets for `proxy` (SOCKS5 for sandboxed foreign processes, same policy) and `relay` (the harw-netns-relay binary). List EgressProxy, ProxyLimits and RelayConfig under the central types. Rephrase the errors section to cover both EgressError and RelayEr… |
| 15 | low | M3 | `harw-egress/src/lib.rs:13` | yes | Broken intra-doc link to harw_sandbox::egress::host_matches_suffix, and the resolver is described as check_addr instead of check_resolved | Link [`harw_authority::host_matches_suffix`] and describe the resolver filter as [`EgressPolicy::check_resolved`] (public-only for names allowed only through the open web, otherwise check_addr). |
| 16 | low | P5 | `harw-egress/src/policy.rs:99` | no | `.unwrap()` in doctests across harw-egress, harw-web and harw-protocol | Rewrite each doctest to end with `# Ok::<(), Box<dyn std::error::Error>>(())` (or the crate error type) and use `?` instead of `.unwrap()`. For no_run examples, wrap the code in a `fn demo() -> Result<…>`. |
| 17 | low | M3 | `harw-egress/src/policy.rs:256` | yes | EgressPolicy::digest doc omits the open-public marker that the code hashes | Add to the doc: 'then, only if open_public, the bytes `\0open-public` (existing digests stay unchanged)'. |
| 18 | low | P5 | `harw-egress/src/proxy.rs:189` | yes | unwrap() in the EgressProxy::new doctest | Use `?` plus `# Ok::<(), harw_egress::EgressError>(())`. |
| 19 | low | P5 | `harw-egress/src/relay.rs:145` | yes | unwrap() in relay doctests (from_args, bind_relay, relay_connection, run_child) | Use `?` with `# Ok::<(), Box<dyn std::error::Error>>(())`. For `cfg.command.unwrap()` use `cfg.command.ok_or("no command")?` or `assert!(matches!(..))`. |
| 20 | low | M2 | `harw-egress/src/relay.rs:393` | yes | The proxy's idle timeout does not free the relay slot: after the proxy closes, the upstream thread blocks until the sandboxed client writes or closes | Pass an idle bound to relay_connection and set a read timeout on client_read (or poll the proxy socket for POLLHUP), so the thread ends once the proxy side is fully gone. This keeps the change inside harw-egress. |
| 21 | low | M3 | `harw-netsec/src/store.rs:494` | yes | persist() fails after the rename when the directory fsync fails: disk holds the new state, memory keeps the old, and the next mutation overwrites it | Treat the rename as the commit point. Split persist into write+rename (on error, memory unchanged) and a directory fsync. After a successful rename, set `*current = next` first, then return the fsync failure as a distinct NetsecError (for example Durability… |
| 22 | low | M3 | `harw-node-transport/README.md:135` | yes | README says the AuthHub NodeSigner adapter and the CryptGuard encoding fix are not in this crate; authhub_signer.rs implements both | Replace the two bullets with: 'Missing: the production `AuthHubSign` impl (waits for `AuthHubClient::sign`).' In Identity/Handshake, add a short paragraph on the wrapped-transcript fleet setting (AuthHubNodeSigner with TranscriptWrappedVerifier, no mixed fl… |
| 23 | low | NEW:derived-errors-and-foreign-types | `harw-node-transport/src/error.rs:11` | no | harw-node-transport is the only workspace crate using thiserror, and its public TransportError exposes rustls/hyper/http error types | Write the error types by hand the way harw-oauth/src/error.rs does, with hand-written Display and Error impls, or use the in-house `#[derive(HarwError)]` like harw-web. Store the rustls, hyper and http causes as `String`, and convert them at the call sites … |
| 24 | low | NEW:integer-overflow | `harw-oauth/src/codex_refresh.rs:122` | yes | jwt_needs_refresh computes `exp - now` unchecked on an untrusted i64 claim | Use `exp.saturating_sub(now) <= window_seconds`. Optionally treat an `exp` outside a plausible range (for example exp <= 0) as needing a refresh. |
| 25 | low | M1 | `harw-oauth/src/codex_refresh.rs:235` | yes | Refreshed Codex tokens go into a fixed temp path opened with create+truncate, so 0600 is not enforced and symlinks are followed | Reuse the store.rs approach: a unique temp name (pid+nanos+counter) opened with `create_new(true)` and `mode(0o600)`. O_EXCL also refuses an existing symlink. Additionally call `file.set_permissions(Permissions::from_mode(0o600))` on the handle before writi… |
| 26 | low | M3 | `harw-oauth/src/error.rs:17` | yes | TokenExchange.body is documented as the raw response body but is always a redacted placeholder | Change the doc to state that the field is always a redacted placeholder and that the raw body is never copied, because it may carry tokens. |
| 27 | low | M1 | `harw-oauth/src/flow.rs:182` | no | Env-overridable token endpoints accept any scheme and host, and responses are read unbounded without a deadline | Parse the override with url::Url and require https (allow http only for loopback in tests). Wrap send and body read in tokio::time::timeout and read at most a fixed number of bytes (e.g. 64 KiB) via bytes_stream or a Content-Length check before parsing. |
| 28 | low | M3 | `harw-protocol/src/approvals.rs:165` | yes | Broken intra-doc link to harw_core::AgentSession::begin_approval: harw-protocol does not depend on harw-core | Replace the link with plain code text (`harw_core::AgentSession::begin_approval`), as approvals.rs:193 already does for PendingApproval::is_timed_out. |
| 29 | low | P5 | `harw-protocol/src/items.rs:106` | yes | unwrap() in the ResultTrust and OpaqueReasoning doctests | Use `?` and end with `# Ok::<(), serde_json::Error>(())`. |
| 30 | low | M1 | `harw-security-hub/src/config.rs:186` | yes | The SecurityHub reads its authority-defining policy file with no size bound, regular-file check, or owner/mode check | Open without following symlinks, check metadata().is_file(), reject mode & 0o022 and (for the system path) a non-root owner, and read through take(MAX+1). All in harw-security-hub/src/config.rs. |
| 31 | low | M1 | `harw-security-hub/src/findings.rs:208` | no | Findings export is opened before the is_file check: a FIFO at the path blocks forever, stalls posture requests and hangs shutdown | Check fs::symlink_metadata(path) for is_file() before opening. Then open with O_NONBLOCK and O_NOFOLLOW (std::os::unix::fs::OpenOptionsExt::custom_flags with rustix/libc constants), and re-check is_file() on the opened handle. Also wrap the spawn_blocking a… |
| 32 | low | M2 | `harw-security-hub/src/server.rs:290` | no | SecurityHub and harw-web Unix-socket servers have no connection cap, unlike netsec, node-transport and egress | Mirror netsec: add `max_connections` to the hub ServerConfig (default for example 256) and use an Arc<Semaphore> with try_acquire_owned before spawning. Move the permit into serve_connection and drop the stream when no permit is available. Do the same in ha… |
| 33 | low | P5 | `harw-web/src/events.rs:160` | yes | unwrap() in four WebEvent/WebEventBus doctests | Use `?` plus `# Ok::<(), harw_web::error::WebError>(())`. |
| 34 | low | M1 | `harw-web/src/identity.rs:712` | yes | uid_tenants and uid_principals accept aliased UID keys ('1000', '01000', '+1000', ' 1000'); the last one in string order silently wins | In tenant_map and principal_map, keep a seen-uid set and return a new IdentityConfigError::DuplicateUid. Optionally accept only canonical decimal keys (no sign, no leading zeros, no whitespace). |
| 35 | low | M3 | `harw-web/src/lib.rs:37` | no | harw-web docs still say every decision comes from decide_route/PeerAuthorizer, but since H12 it is decide_resolved_route via LocalPeerIdentityResolver | Rewrite these passages to say that decisions go through router::decide_resolved_route (same matrix as decide_route) with the tier from identity::LocalPeerIdentityResolver, whose tier cap is still the PeerAuthorizer. Meta routes use the same resolver plus au… |
| 36 | low | P5 | `harw-web/src/lib.rs:111` | no | todo!() panic macro in the crate-level and server module doctests | Make the factory a parameter of the hidden helper, `# async fn run(context_factory: Arc<harw_web::server::WebContextFactory>) -> Result<(), harw_web::error::WebError> {`, and drop the closure. |
| 37 | low | M3 | `harw-web/src/meta.rs:21` | yes | meta.rs says /v1/health is unauthenticated 'like harw-netsec' and that PeerAuthorizer/decide_route decide; both claims are wrong | Remove the netsec comparison (or say netsec gates health by allowlist), and name decide_resolved_route and LocalPeerIdentityResolver as the decision path. |
| 38 | low | M3 | `harw-web/src/router.rs:7` | no | harw-web docs name decide_route as the single access decision and say server::handle decides nothing; production uses decide_resolved_route plus separate checks in handle_meta and handle_events | Rewrite the docs: the route decision is `decide_resolved_route` over `LocalPeerIdentityResolver`; `decide_route` is the tier-map variant used by tests; the meta routes and `/events` make their own resolver-plus-tier_permits checks. Replace the link at route… |
| 39 | low | P5 | `harw-web/src/router.rs:198` | yes | unwrap() in doctests of decide_route, from_registry and method_for | Use `?` and end each doctest with `# Ok::<(), harw_web::error::WebError>(())`. |
| 40 | low | M1 | `harw-web/src/server.rs:717` | yes | Execute path silently runs with an unscoped OpContext if no resolved identity is present | Before building ctx and reading the body: `let Some(resolved) = resolved else { return Ok(forbidden_response(forbidden_reason_str(ForbiddenReason::UnknownPeer))); };`, then `resolved.scope_op_context(ctx)` with no Option. A stronger alternative is to let Ro… |
| 41 | low | M1 | `harw-web/src/server.rs:831` | no | GET /events authorizes only at subscription: revoked or expired security contexts keep streaming, and there is no tenant filter | Pass the ResolvedPeer into sse_response. If summary() is Some, end the stream at summary.expires_at (tokio::time::sleep_until in the select) and re-resolve on each heartbeat, closing the stream on failure. Once WebEventKind carries a tenant, filter Operatio… |

## Addenda to fixes

### #20: Relay slot after the proxy side ends (`harw-egress/src/relay.rs`)

Implemented differently from what the finding asked for. The finding wanted the
slot released as soon as the proxy side is completely gone. The fix (R16
`wa-egress`, refined in R16 `ripple-egress`) polls `client_read` after the
proxy→client direction has ended, using `DEFAULT_RELAY_IDLE_POLL` (500 ms). It
releases the slot only once the client has been silent for
`DEFAULT_RELAY_IDLE_BUDGET` (300 s) without interruption; every byte read
resets the total.

- Reason: On EOF in one direction, the proxy only half-closes that direction
  (`proxy.rs`, `pump`). A client that still sends after a pause must not be
  cut off earlier than the proxy itself would do it
  (`ProxyLimits::default().idle_timeout`, 300 s).
- Cost: If the proxy closes completely (`PumpEnd::Idle` or
  `PumpEnd::ByteLimit`) and the client stays silent, the slot remains occupied
  for up to 300 s. With `DEFAULT_RELAY_MAX_CONNECTIONS` = 128, silent clients
  can hold all slots for that long; new connections are rejected with
  `RelayError::ConnectionLimit` in the meantime.
- Open: release the slot earlier when the relay detects that the proxy has
  ended both directions rather than only half-closing. This is a trade-off
  between half-close tolerance and slot hold time and needs a decision of its
  own.
