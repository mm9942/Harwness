# Android embedded Arti / gateway recovery — stacked follow-up to PR #170

Status: implementation started; release blocked on end-to-end physical-device and gateway verification.
Baseline: fe20fd5021c6c3a6f7cb6d6fe567ae7611950fa6 (Android 0.3.28 / build 31, PR #170).
Retain embedded Arti 0.48, pinned node authority, ML-DSA handshake, registry/revocation policy and explicit route selection. No direct fallback in Onion-only mode. Do not wipe client guards, caches, enrollment, or gateway onion identity as a routine recovery operation.

## Incident evidence and boundaries

- User reports successful operation Nov 23 evening and Nov 24 daytime; recurrence after Android reclaimed background process. No exact known-good APK hash available; do not infer that a particular commit was deployed just from its timestamp.
- Diagnostic export contains two `connecting` records, connection 5 at 72038 ms and connection 6 at 78573 ms, but neither a success nor failure record. Baseline emitted only connecting/connected/disconnected; failure or cancellation was intentionally not emitted. This is an observability hole, not proof of a particular protocol stage or of a timeout.
- Earlier incident report: `Failed to obtain hidden service circuit`. Independent client also timed out. Gateway bootstrap reached 100%, descriptor uploads reportedly failed; startup reported not confirmed reachable after 180 s. A temporary external tor@harw test reportedly published the same onion and reached TLS 1.3. These are prior incident observations, not newly reproduced results. TLS success does not prove node authentication or session.hello success.
- Thus distinguish (A) gateway descriptor publication/reachability, (B) Android cold start/lifecycle/recovery, (C) subsequent authenticated session establishment. Both A and B can be broken simultaneously. Do not attribute the incident solely to Android.

## Git evidence

| Ref | Relevant behavior |
|---|---|
| wip/onion @ 92dd41617bad | Ancestor of baseline (compare ahead 105, behind 0); not a verified known-good APK. |
| fix/android-onion-connect @ 5da56005e6ef | Retry once after a network timeout, 5 s delay, same preferences; not the patch to cherry-pick over PR 170. |
| 1643fb619406 | Retry transient/stalled first dial with new isolation group, sharing original Arti client and time budget. |
| afa01433dcd3 | Sticky service restoration and Kotlin error-string whitelist, APK 0.3.27 build 30. |
| fe20fd5021c6 | Serialized per-Dialer cold initialization, distinct bootstrap/dial deadline errors, persisted state live-test scenario, APK 0.3.28 build 31. |
| wip/mobile-next @ 714dd252dd8c / wip/apk-next @ ffc43e70085c | Earlier mobile lineage inspected at tips; not device-level evidence. |

Branch inventory and relevant tips examined; default-branch commit search did not return unmerged Onion history. This is a targeted history analysis, not an exhaustive traversal of every commit on every branch.

## ComNet comparison

Reference mm9942/comnet @ 331febba8607404e44fbe5adefb5b3a0510c62f4.
- legacy/comnet-client/src/tor_router.rs: creates a bootstrapped embedded TorClient, retains shared router; also launches a local hidden service (not necessary to copy for Harw's outbound-only client).
- legacy/comnet-client/src/client.rs: separate Tor, TLS and WebSocket stages. Prior analysis of v1 found outer connection retry reuses the router; initial router bootstrap itself occurs before retries.
- legacy/comnet/src/main.rs: creates bootstrapped Arti and launches onion; 'service live' only proves a local service handle, not remote descriptor reachability.
- crates/comnet-transport/src/arti.rs: v2 explicit embedded TokioRustlsRuntime, process provider initialization, separate state/cache, typed transport stages and error kinds; do not copy ComNet's protocol or change Harw's pinned authority.

## Actual path and ownership

Android HarwConnectionService / HarwConnection -> UniFFI MobileCore -> drive_connection -> Dialer -> harw-onion OnionClient -> Arti bootstrap -> hidden service descriptor/introduction/rendezvous -> OnionStream -> harw-node-transport pinned handshake/TLS -> HTTP upgrade -> session.hello -> read-only list probe/attachments.
Gateway session_listener -> OnionServer -> RunningOnionService descriptor publication -> accept_loop -> StreamIntake -> authenticated session transport.

Baseline:
1. drive_connection stops on first failure even if native classifier says Retry. Kotlin sticky restore retries only four substrings and normal explicit connect does not share that policy.
2. Native network_available wakes a backoff or re-enters retryable Failed; it does not interrupt the in-flight dial on network migration. Kotlin only observes onAvailable. Retained Dialer never explicitly retires a client generation.
3. One 120 s dial budget includes bootstrap and two circuit attempts. When bootstrap consumes most of it, each rendezvous attempt receives very little time. Cold-start lock serialization does not prove healthy post-bootstrap behavior.
4. Client state in noBackupFilesDir/tor/state survives process death; cache in cacheDir/tor may be reclaimed. Circuits and process runtime do not survive. Treat process death, cache eviction, and live-runtime network migration as separate cases.
5. Gateway launches an OnionServer, writes locator, waits up to 180 s for reachable, then continues even if false. A local handle or ready Tor client is not an externally reachable Onion service.

## Phase A — implemented in this follow-up

- Rust owns the first connection retry for Onion: two additional attempts after a transient failure, independently capped even if network wake resets backoff. Existing direct-only first failure and post-connect retry policy remain. No retry on classified terminal refusals; no route policy bypass. The initial UniFFI call waits for its bounded outcome.
- Android sticky service starts a single native run and holds foreground during initial recovery; remove separate substring-based retry loop. Surface native attempt and nextRetryAtMs during initial polling.
- Explicit per-connection failed/cancelled records with fixed stage and retryability, elapsed time, no error strings or host identifiers. Failed and cancelled never masquerade as disconnected. Existing diagnostic prefix retained; unknown stage used for unclassified errors.
- Gateway logs Arti service reachability transitions throughout the service lifetime, independently of bootstrap. This is observability, not a claim of external availability or automatic descriptor recovery.
- Unit tests for bounded retry policy, direct-only/terminal refusal behavior, stage labels, explicit terminal diagnostic records.
- Stage classification still uses existing stable RemoteError text prefixes. This is an intermediate measure, not the final typed transport contract.

## Phase B — gateway service lifecycle (highest priority)

1. Reproduce independently before changing phone code further: on the same deployed gateway use a separate Tor client and test the existing onion identity/port, then complete pinned authentication and session.hello. Record bootstrap, service status transitions, descriptor upload outcomes, inbound rendezvous/stream admission and session handshake stages. Compare gateway process/build/config fingerprints against known-good deployment. Never log full onion locator, key, token or session data.
2. Expose gateway health stages separately: process running; Tor ready; descriptor reachable (Arti-reported); independent read-only authenticated session probe healthy. Do not advertise ready just because the process starts or Tor is at 100%. Keep gateway's other routes available according to existing explicit policy.
3. Add a single service supervisor around OnionServer ownership. Only recycle after sustained unreachability or a terminated accept/status loop, with bounded backoff and cooldown; never restart on one remote client's failed dial. First distinguish no usable network/clock, descriptor publishing failures, exhausted limits and fatal identity/config errors.
4. Shut down and join old tasks, ensure state lock released, then relaunch with same persisted onion keystore. Verify locator/pinned authority unchanged, never regenerate identity silently. No parallel instances sharing state; if identity restore fails, fail closed. A transient gateway recycle must not restart unrelated hosted sessions.
5. Use independent client probe for readiness; local service-status boolean remains necessary evidence but not sufficient. Verify recovery when hidden-service publication is deliberately delayed and when gateway restarts with persisted identity.

## Phase C — Android Arti runtime supervisor

- One owner for Arti runtime per cloud process, with explicit generation and single-flight async start. Separate start, bootstrap readiness, per-target dial and session establishment. Evaluate Arti on-demand bootstrap semantics and avoid competing startup work; do not replace Arti with external Tor/Orbot.
- Preserve client state and guard continuity across runtime recovery; cache is disposable but only consider targeted repair when evidence identifies corruption. Do not blindly rotate guards, wipe state or weaken permissions.
- Validate network capability/default network generation and loss/return, debounce events; offline cancels or pauses current dial with explicit reason; migration should invalidate stale transport work, not blindly start competing connection tasks. Use monotonic deadlines and bounded recovery. Android foreground restrictions/manual disconnect/profile switch/revocation remain authoritative.
- Split cold bootstrap budget and Onion rendezvous budget, with a hard end-to-end cap; initial target 120 s bootstrap + up to 120 s Onion dial + existing bounded handshake/hello budgets, refine with actual timings. Do not let late bootstrap starve both dial attempts.
- Fresh isolation is already in baseline; it is not guaranteed to rebuild all lower-level Arti circuits or recover a stale runtime. If repeated typed transient failures plus runtime health indicate a stale generation, drain all in-flight dials and release client/task ownership before one controlled restart. Reuse persisted state. A remote service outage alone must not trigger continual client recycling.
- Map typed error stages/kinds through harw-onion, harw-node-transport, harw-session-remote and UniFFI; distinguish bootstrap, descriptor, rendezvous, transport auth, upgrade, hello, cancellation and revocation. Do not classify by arbitrary peer text. Preserve per-profile route policy and no replay of mutating RPCs.
- Ensure recovery works after Android LMK/reclaimed service, explicit app reopen, upgrade installation, cache eviction, Doze, Wi-Fi/mobile handover. Explicit force-stop and user manual stop must not silently restart contrary to Android policy/user intent.

## Phase D — regression and acceptance

Automated:
- cargo test -p harw-onion (with configured client/service features); live_onion gated integration, add service publication and persisted client lifecycle cases.
- cargo test --manifest-path mobile/rust/Cargo.toml (with onion feature, use actual repo build configuration); deterministic fake dialer tests for delayed bootstrap, failure/retry, cancellation, retry bound, authority refusal, stale generation, network migration.
- Android Gradle unit tests; add sticky null-intent restore, explicit connect, manual stop, stale profile/route, service notification lifetime and no duplicate native run scenarios. Regenerate UniFFI bindings as part of APK build.
- Build signed arm64 APK using established mobile release scripts, ensure versionCode >31, signature continuity and exact commit provenance. No APK publication until verification.

Physical phone + actual gateway:
| Scenario | Acceptance |
|---|---|
| Warm connection | Onion stream, authenticated handshake, hello, session list and attachment succeed |
| Cold process launch / OS reclaim | same enrolled identity and selected route; successful authenticated session recovery without manual retry |
| Cache eviction, persisted state | bootstrap and reconnect without reenrollment |
| Gateway restart | stable onion identity, readiness transitions, phone reconnects without app reinstall |
| Independent client | same onion service and authenticated hello; if both fail diagnose gateway/descriptor first |
| Wi-Fi to mobile / airplane mode | bounded, single recovery run, no direct leakage in Onion-only |
| Manual stop / revoked / profile switch | no unauthorized retries or stale state publication |
| Live session recovery | cursor replay/dedupe, no duplicate submit or approvals |

Run at least 10 cold-reclaim cycles and 5 network transitions, report success count and stage latency, not just bootstrap %. Distinguish app force-stop from system reclaim. A failure must have a stage or explicit unknown/cancellation record; collect gateway counterpart. No claim of resolution until the exact reported reproduction succeeds on physical hardware.

## Exit criteria / outstanding

A merged PR, passing unit tests, a successful TLS probe or a 100% Tor UI are insufficient separately. Require independently confirmed gateway Onion reachability, authenticated session.hello from the physical phone, preserved identity and route, deterministic recovery after process reclaim, no repeated mutations. Phase A source changes are not yet compiled or device-tested in this session. Phase B/C supervisor implementation is deliberately pending evidence and lifecycle tests; no external Tor production replacement, no key rotation, no merge, no release.
