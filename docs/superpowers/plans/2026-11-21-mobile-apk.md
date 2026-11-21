# Android phone-test APK implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Track completion with the checkboxes below.

**Goal:** Produce a signed, installable arm64 Harw Android test APK using the real authenticated session protocol, with a reproducible server/USB test path.

**Architecture:** A thin Kotlin/Compose shell owns Activities, foreground service and Android Keystore. A process-owned Rust UniFFI core reuses `harw-session-remote`, `harw-node-transport` and `harw-mobile-core` for connections, sessions, chat and approvals. One connection serves independently controlled session windows.

**Tech stack:** Existing pinned Rust toolchain; UniFFI; Kotlin/Compose; Android SDK/NDK available on this host; arm64-v8a.

**Spec:** `/root/harw-archive/2026-11-20/cloud-concept/KONZEPT.md` sections 5.1–5.5 and `ROADMAP.md` P3, together with the exported user requests and the user's 2026-11-21 instruction to follow those exports. This is the first phone-test delivery in that roadmap. It does not certify later Onion, file replication, local-agent relay or compute-plane phases as implemented.

## Global constraints

- Continue in `/root/harw` on `consolidate/2026-11`; the user's consolidation requires one canonical source directory. Preserve existing work.
- Parent is running `make install`; do not interrupt it or start competing heavy builds. Request the build slot from the parent.
- Use GPT-6.2-Sol for integration/implementation; Luna only for simple mechanical work, as requested.
- Preserve the existing authenticated PQ transport and wrapped ML-DSA transcript. Never substitute plaintext WebSocket, trust-all verification, hardcoded credentials or a mock backend in the APK.
- Phone identity originates on the phone and its private bytes are encrypted by Android Keystore at rest. Provider credentials stay on the server.
- Server setup is opt-in and confined to a private test directory and loopback. No firewall/public listener changes or production service reconfiguration.
- Kotlin UI work must not block the main thread. Share the core across Activities; rotation must not create a new execution.
- Keep new mobile Cargo work separate from the root workspace while the server install is building; do not edit root Cargo manifests/lockfile without coordination.
- Every visible action must use real functionality. Label test fixtures explicitly; do not present the full cloud roadmap as completed.

## Review focus

- Wrong host pin or malformed enrollment must fail before session access.
- Disconnect/reconnect and repeated taps must not duplicate a prompt or resolve a stale approval.
- Concurrent windows must preserve independent session IDs and one process-owned core.
- Rotation/service lifecycle must preserve keys and avoid doing network work on the UI thread.
- APK packaging must contain the arm64 native library, matching generated bindings, and a valid debug signature; physical phone results remain unverified until the phone is connected.

## Task 1 — Rust mobile bridge and transport lifecycle

**Owns:** `mobile/rust/` except `src/bin/mobile-bootstrap.rs`; `mobile/INTERFACE.md`.

**Consumes:** Existing `connect_node`, `TranscriptWrappedSigner/Verifier`, `RemotePort`, `Controller`.

**Produces:** `libharw_mobile_ffi.so` and generated Kotlin bindings in package `uniffi.harw_mobile_ffi`. Publish exact record and method names in `mobile/INTERFACE.md` before the Android consumer binds to them. Provide typed identity/profile/status/session/transcript/approval records, session list/create/open/snapshot/submit/interrupt/decide operations and explicit connect/disconnect/reconnect errors. Secret identity material is returned only for platform encryption; never included in public enrollment or logs.

- [ ] Define the typed FFI contract and coordinate it directly with Task 2.
- [ ] Write meaningful tests for identity round-trip, pin/config validation and session/action state; run them failing before implementation when a build slot is available.
- [ ] Implement an owned Tokio multithread runtime and session workers so waiting for events cannot starve user commands.
- [ ] Reuse existing replay/cursor semantics; fail closed offline, on stale submits and on insufficient approval capability.
- [ ] Test native bridge against a real protocol host, including incorrect pin and multiple sessions.
- [ ] Generate Kotlin bindings and cross-compile arm64 using NDK after parent releases the build slot.

## Task 2 — Android app and platform integration

**Owns:** `mobile/android/`; consumes Task 1 contract without editing Rust files.

**Produces:** An APK for application ID `dev.harw.mobile`, named Harw, with a connection/setup screen, live session list, streaming chat, turn/tool status, interrupt and approval decisions, and a real new-window Activity action.

- [ ] Scaffold a reproducible Gradle project using installed/cached tooling; avoid unrelated app assets and source.
- [ ] Integrate generated bindings and a process-owned connection/core with coroutines off the UI thread.
- [ ] Encrypt native identity using Android Keystore; export public enrollment and import the public server connection profile through Android file APIs.
- [ ] Implement foreground-service notification and lifecycle, explicit stop, responsive portrait/landscape UI and independent multi-Activity sessions.
- [ ] Test profile parsing and critical state/lifecycle behavior; visibly report connection, pin, permission and submission failures.
- [ ] Build `assembleDebug`, validate manifest/native-library/signature, and provide an unambiguous final APK path.

## Task 3 — Real server bootstrap and phone-test tooling

**Owns:** `mobile/rust/src/bin/mobile-bootstrap.rs`, `mobile/scripts/`, `mobile/PHONE-TEST.md`. Coordinate any dependencies/bin registration with Task 1; do not concurrently edit its manifest.

**Consumes:** Public phone enrollment and public server profile schema agreed with Task 1; existing AuthHubClient/DeviceRegistry and installed `harw gateway`.

**Produces:** Safe repeatable tools to create an isolated test home, provision server identity through AuthHub, enroll only the supplied phone public key with explicit capability limits, emit the public connection profile, and run a loopback gateway. A deterministic local model fixture may be used only by a clearly labeled smoke mode; real-provider mode must use an operator-selected existing configuration without exporting secrets.

- [ ] Publish enrollment/profile schema jointly with Task 1.
- [ ] Write tests for malformed device keys, traversal/unsafe paths and correct registry generation; verify failure before implementing.
- [ ] Implement bootstrap without replacing live production config/services. Refuse overwrites of non-owned directories and protect private files.
- [ ] Add scripts for native build/binding generation, APK packaging and ADB install/reverse/import/export, with explicit serial selection when multiple devices exist.
- [ ] Verify an actual authenticated gateway connection, session creation/attachment and a smoke reply; wrong pin must be rejected.
- [ ] Document USB-local and remote-server/SSH-forwarding variants, visible limitations and exact phone test steps.

## Integration and delivery — parent

- [ ] Finish current `make install`; verify installed binary versions and runner capabilities.
- [ ] Review each agent's scoped diff, reconcile the FFI contract, then run builds serially within disk budget.
- [ ] Validate bridge tests, Gradle tests, APK signature/ABI and packaged native symbols/bindings.
- [ ] Run independent integration/code review and fix material findings.
- [ ] Copy final APK and checksum to an accessible artifact directory; report what was tested and what awaits the phone.

## Execution rulings

- The user redirected repeated platform clarification to the chat exports; proceed with their requested Sol/Luna execution and the archived Compose + Rust recommendation.
- The root workspace baseline contained compile/lint failures before this task. Existing mechanical fixes are retained and tracked separately; do not claim a green full workspace suite without an actual successful run.
- P1/P3 roadmap capabilities beyond the current wire protocol must not be silently simulated. Deliver the working authenticated phone-test slice and identify remaining roadmap phases precisely.
