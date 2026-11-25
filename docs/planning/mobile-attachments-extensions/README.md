# Mobile Attachments & Extension Management — E2E implementation plan

Status: proposal, not implemented. Source baseline: PR #172, c83eb6a7548f4e4181d636d80bc1f5d9c3635a17.
Keep the recovery PR's Onion/VPN and existing security contracts intact. Complete its native/Android/device acceptance before merging it; implement the following work in dependent, reviewable follow-up PRs. No code or protocol change is made by this document.

## 1. Outcomes and boundaries

1. Select one or multiple arbitrary files in the Android composer (new and existing ordinary chats), transmit to the actual session owner, persist durable attachment references and reopen/export them.
2. Give the selected model an honest representation of supported file content. Let authorized coding/analysis agents read files by scoped reference, create derivative files and return downloadable artifacts. Retain existing image/vision flows.
3. Manage connectors (MCP), declarative plugins and skills in Android Settings, on the explicitly selected device or gateway profile. Create/import, validate, review, activate, assign, revise, disable, revoke and remove them.
4. Associate approved extension versions with an agent/UIA where that runtime supports them, project or session. Assignment is not authorization.
5. Do not export a sealed private Assistant room's files or transcript to a gateway, MCP server or external inference route. Its file import and reading require a separate vault-native workstream. Shared Cloud Assistant stays a distinct capability, not an ordinary coding session.
6. Preserve existing compatibility and offline/reconnect behavior. No generic filesystem or arbitrary shell-execution RPC.

## 2. Verified code baseline and gaps

| Area | Existing source | Gap / direction |
| --- | --- | --- |
| Images | harw-protocol/src/media.rs; harw-media/src/store.rs; harw-protocol/src/items.rs | MediaRef has ImageFormat and dimensions. ContentPart supports Text, ImageUrl and Media only. Keep image type and add a distinct file reference. |
| Submit | harw-protocol/src/session_wire.rs; harw-session-host/src/host.rs; harw-session-host/src/driver.rs; harw-session-driver/src/bridge.rs | SubmitParams and TurnInput carry images, CAS cursor and message id; ingest_images decodes/stores them. Add negotiated file submissions and owner-bound receipt checks. |
| Transfer | harw-session-ws/src/limits.rs; harw-protocol/src/session_wire.rs | Default frame/message cap 1 MiB; existing media preview already has 256 KiB parts, but it is image-specific. No whole-file Base64 in turn.submit. |
| History | harw-core/src/history.rs; harw-session-host/src/archived_fork.rs; mobile/rust/src/core.rs | Image-only projection and retained/archive identity need additive file support and exhaustive audit. |
| Android | MainActivity.kt SessionDraft; HomeChat.kt NewChatDraft; SessionConversation.kt PromptComposer; PromptImages.kt; FilesScreen.kt | Existing SAF file manager has document/tree import and export; reuse selection/streaming concepts, not unscoped root grants. Composer currently only has image drafts. |
| Settings | SettingsHub.kt SettingsRoute; MainActivity.kt | Files, providers, agents, UIAs already exist; no dedicated connector/plugin/skill management routes in inspected enum. |
| MCP | harw-config/src/mcp_toml.rs; harw-mcp-client/src/lib.rs and stdio.rs; harw-runtime/src/mcp_wiring.rs | Typed Streamable HTTP and Stdio, initialize/tools/list/call, assembly contributor, NetworkAccess/ExecuteProcess checks; generic management and safe lifecycle still to build/verify. CLI mcp setup/check is Cloudflare-specific. |
| Extensions | harw-config/src/{plugin_toml,skill_toml,discovery,writer}.rs; harw-extension-api/src/registry.rs | Declarative manifests, config discovery and reference validation, atomic writer. No assumption of executable plugin loading or universal hot reload. |
| Skills | harw-home/assets/skills; harw-core/src/skill_injection.rs; harw-runtime/src/assembly.rs; harw-registry-defaults/src/{skill_tools,skill_proposal_tools}.rs; harw-cli/src/cli/skill.rs | Existing index, search/load, trigger preview, validation, proposal store and approval-sensitive commit/reject; expose these instead of inventing a second store. |
| Artifacts | harw-agent-artifact/src/lib.rs | Frozen compiled-agent format, not general chat-file storage. Reuse its integrity principles, not its binary format. |

Inspected source proves presence of these components, not successful builds, deploys or end-to-end integration.

## 3. Attachments architecture

### 3.1 Android UX

One + menu in existing/new ordinary conversation composer: Gallery/Camera (existing image route), Files (Storage Access Framework OpenMultipleDocuments), optional Library import (its own authorization), and later Scan/Voice. Display attachment chip/card with safe display name, detected type, size, status, destination, progress, retry/remove, preview. File-only and mixed image/text/file turns supported. Attachment removal before send revokes staging. Download output through ACTION_CREATE_DOCUMENT or a user-initiated Share; never open/execute binaries automatically.

Use ContentResolver streaming on IO dispatcher; no broad storage permission, file-path guessing, or loading arbitrary files into Kotlin ByteArray/Bundle/saved state. Native private staging is bounded, hash verified, encrypted where required; short-lived URI grant/copy is scoped to the selected draft. Bind selection callbacks to the same backend, profile, generation and draft identity. If user switches profile or session, stop stale work and require a new explicit transfer. Resume only the exact same owner-bound transfer. No automatic private-file background sync.

For new chats, select files first without uploading. Once session creation and model/UIA configuration succeed, stage/upload to that session owner, then submit the turn. On failure keep the recoverable draft and receipts, never send a partial prompt by silently dropping files. Existing image attachment behavior stays intact.

### 3.2 Leaf crate and data model (proposed)

Add harw-attachments (name provisional) as provider-neutral bounded I/O, validation, staging, immutable blob/reference and streaming primitives. Reuse harw-fsutil and harw-types where appropriate. Keep image sanitization in harw-media. Do not repurpose MediaRef or compiled agent artifacts.

Proposed FileRef: opaque attachment_id, owner-scoped reference, content_digest (BLAKE3), size_bytes, display_name, detected_mime, file_kind, immutable version. Store a claimed MIME separately if needed; never trust it. Ownership is decided by the host from the authenticated principal/session, not a client-supplied tenant or path. Physical blob dedup is possible within an authorized storage domain; no cross-tenant hash-existence oracle. Original bytes immutable; extraction and generated revisions have separate refs and provenance.

Durable message part: ContentPart::File { file: FileRef }. Add bounded typed attachment metadata to history projection, mobile transcript rows and artifact download. Avoid embedding bytes into transcripts, prompts, diagnostics or event logs. Add explicit provenance for derived text (source digest, parser version, page/section/line/table range, extraction status, truncation).

### 3.3 Negotiated transport (proposed names)

Feature attachments_v1 and a new typed submit method (e.g. turn.submit_attachments_v1) rather than sending unknown fields to legacy deny_unknown_fields parsers. Define wire types in harw-protocol; implement SessionPort, remote port, WS dispatcher, host, driver, local backend and UniFFI, with symmetrical caps checks. Do not modify existing Arti/VPN transport.

- attachment.begin: bind target session, client message id, filename, length, digest to authenticated owner; reserve quota, issue opaque upload token.
- attachment.chunk: bounded offset/sequence and chunk digest; verify bounds, replay idempotent for identical chunks, reject overlaps/conflicts.
- attachment.status: resume from verified committed offsets on the same authorized owner.
- attachment.finish: verify total bytes/digest, publish immutable completed ref atomically, return receipt; fail on partial or corrupt data.
- attachment.cancel: release staging/quota; cleanup abandoned uploads by bounded TTL.
- turn.submit_attachments_v1: exact CAS head, message id, text, legacy images and finished file receipts. Atomically validate/bind the attachment set to the admitted turn. Receipt identity includes ordered file refs and image/text/rewind parameters; changed payload with same id fails; identical retry does not create another turn.
- attachment.read / download.begin + download.chunk: resolve from an authorized session item or output artifact, not naked digest; bounded chunks and reauthorize each access; verify digest before publishing to Android.

Initial engineering limits (proposals, benchmark on real phone/Onion): raw chunk <= 256 KiB, JSON Base64 only per bounded chunk, <= 2 in-flight chunks per transfer, <= 25 MiB/file, <= 10 files/turn, <= 100 MiB/turn, 15 minute idle staging TTL. Independent per-profile storage quotas and active transfer limit configured by operator. File reads/writes stream to disk, never allocate entire accepted file in a JSON envelope. Do not raise global WS limits. Transfer authentication and quota reservation precede disk I/O.

Older peers: never send new RPC without feature handshake. Do not deliver new ContentPart variants to old readers unless explicitly compatible; use negotiated projection/feature gating for session history, archive, checkpoint and fork. If a legacy view cannot preserve exact file semantics, mark it unsupported/read-only rather than silently dropping attachments or breaking CAS/receipt integrity.

### 3.4 File understanding and model capabilities

| Type | V1 behavior | Later |
| --- | --- | --- |
| TXT, Markdown, code, logs, JSON, YAML, TOML, XML, CSV, JSONL | bounded text decoding, encoding/binary check, line/row range reads, explicit truncation | structure-aware retrieval and typed parse |
| PDF | preserve and inspect safely; isolated bounded text extraction, page provenance | OCR for scanned pages, page render into vision, document structure |
| DOCX, XLSX, PPTX | opaque upload/download initially; no macros or executable content | isolated OpenXML extraction with zip budgets; document/table/slide support |
| PNG/JPEG/GIF/WebP | preserve existing image/vision path, do not regress it | document page rendering / explicit original file preservation as separate route |
| Audio/video | store, metadata and download only | opt-in bounded ASR/frame extraction; no automatic external transcription |
| ZIP/TAR/unknown/binary | store opaque, no extraction/execution | explicit sandboxed safe archive listing/extraction, additional approved parsers |

PDF extraction may be delivered after the text-file milestone if the parser cannot meet resource and sandbox requirements. Every file has a visible status: stored, extraction pending, ready, unsupported, invalid, truncated or failed. Never imply that a model read a document just because it was uploaded.

For lean local GGUF without tools: inject bounded selected text segments with file/line references and untrusted-data labels, using a budget computed from context length, requested output and existing conversation. For normal tool-enabled sessions: provide scoped attachment.list/read/search (line or page ranges, bounded results); optional read-only staged mount under an owner-assigned sandbox path for authorized existing file tools. No arbitrary path access or new process privileges. For external model routes, show what file-derived content will be sent, enforce an explicit egress decision tied to exact route, file version and scope; default to no native file upload or external retrieval. Existing conversation/route changes cannot silently widen file access. No external URLs fetched on behalf of model. Provider-native files only as separate capability, with retention/deletion and limits explicitly handled; no transparent fallback.

A file's content and parser output are untrusted data, never system/developer instructions. No macros, scripts, executables, archives or plugin manifests launched/installed from a chat upload. Detect malformed inputs, decompression bombs, XML external entities, symlinks, traversal and oversized pages; sandbox parsers without egress, CPU/time/memory/disk limits and minimal readable inputs. Preview documents as inert text or safe render, never active HTML/JavaScript.

### 3.5 Output files and lifecycle

Allow authorized agents to publish explicit output artifacts from their bounded workspace through an artifact.publish operation. Verify path ownership, regular file and limits; snapshot bytes and digest; require explicit output publication rather than exposing whole workspace. Artifact is immutable and linked to turn, source files, producing agent, timestamp/version. Chat shows download, preview, share and reattach. Editing creates a new version or exported copy, never mutates the original input; writing back to SAF source requires separate user confirmation and expected revision.

Retention/GC considers active uploads, draft TTL, committed session history, forks/archives, output references, backups and legal/user retention policy. Deleting one ref cannot delete data another authorized retained ref needs; unreferenced staging/blob cleanup audited. Backup/restore versioned metadata and optional blob inclusion, no secret material or private-vault mixing. Orphaned or unavailable file bytes shown explicitly.

### 3.6 Private Assistant follow-up

Dedicated vault-native file refs, encrypted storage, per-item native lease/revision and foreground checks, bounded streaming into native custody, explicit import/selection from private Library or document picker. Keep sealed room history separate; no ordinary attachment RPC or cloud fallback. Implement local text extraction/inference with no egress, zeroization of sensitive transient buffers where feasible and cancellation on lock/revoke, preserving existing private attempt/mailbox semantics. Private file output/export is explicit and separate. Cloud and shared Assistant file support is a separately negotiated design after ordinary chat; current text-only RPCs must not be overloaded.

## 4. Extension management architecture

### 4.1 UI and ownership

Settings > Extensions: Connectors (MCP), Plugins, Skills, Assignments, optional diagnostics/audit. At top, always show scope: On this device / named authenticated gateway and selected profile. No silent fallback, no global activation while choosing a session. Read-only state for unauthorized peers; owner/admin management rights are separate from inference or ordinary steering.

Each item: name, version, origin, owner, state, dependencies, required tools/egress, last health check, assigned sessions/agents, effective permissions. Lifecycle draft -> validated -> installed disabled -> permission review -> enabled -> assigned; suspended/revoked/failed states visible. Delete shows dependencies and active usage; disabling/revocation cancels or denies future calls, cannot be undone by model instructions.

Proposed shared Rust application service (extension-management module/crate, name provisional) used by local native FFI and authenticated gateway management port. It reuses typed harw-config manifests, discovery, ConfigWriter and existing proposal/registry logic; no Kotlin TOML generator as authority, no remote arbitrary file writer. Use typed operations catalog/list, inspect, validate, prepare-change, apply approved change, health, assign, disable, remove. Mutations use scope, expected revision, idempotency key, canonical config diff, atomic persistence and audit. Keep credentials in owner-side secret custody (Android Keystore-backed references or gateway secret store); never return token/plaintext secrets, log them or embed in bundle. Existing MCP resolver only handles Env/File/FileJson, so keyring/secrets support is an explicit implementation gap to close, not an assumed feature.

### 4.2 Connectors / MCP

Create manually from server name, Streamable HTTP URL (HTTPS default), approved auth method, tool allowlist, optional description. Validate syntax without networking. Separate explicitly approved connection test runs initialize and bounded tools/list, surfaces tool descriptions, schemas, permissions and endpoint; no tool call from discovery. Support configured bearer secret refs first; OAuth only after implementing provider metadata discovery, PKCE/state, callback binding, token rotation/revoke, TLS/redirect constraints and secure storage. Treat MCP resources/prompts as later optional negotiated capabilities; do not claim they are wired now.

Stdio only on a host that supports mediated process execution and explicitly operator-approved installed executable/arguments; no arbitrary shell, package auto-install, root escalation or APK code loading. Use existing sandbox/job/process policy and cancellation. Android native Stdio execution is deferred unless the exact sandbox/ABI/process model is validated; a configured gateway can own Stdio. HTTP endpoints require explicit egress policy, revalidate DNS/redirect/address and reject unexpected local/metadata endpoints; intentional private/self-hosted MCP endpoints only via separately approved network policy. Limits on tool count, schema/result size, timeouts, concurrency, process resource use and reconnects. Namespaced stable tool names, validate duplicate/collision. Tool definitions and results untrusted, no instructions override or implicit approvals. Per-tool allow/ask/deny from actual runtime capability policy, not remote tool annotations.

### 4.3 Plugins

Use existing PluginToml declarative bundle: references to skills, MCPs, tools, channels and network egress. Imported bundle must be inert and installed disabled regardless of manifest default. Import manifest and optional bounded bundle from SAF/approved source; validate schema, paths, dependency closure, cycles, compatibility, versions and digest. Trust/provenance review (signed packages where available, explicit trust for local author); no native .so/DLL, dynamic execution or install hook in V1. External registry/distribution and executable extensions are distinct future projects. Activation previews exact capability diff and requires owner approval. Updates pin version/digest and re-review expanded permissions; no auto-activation. Rollback changes manifest revision but does not regrant revoked authority. Uninstall requires dependency check and explicit detach.

### 4.4 Skills

Use existing SkillToml, instructions, triggers, eval proposal data, SkillProposalStore and skill index. UI supports browse built-in/installed/custom, preview instructions and triggers, edit/fork/custom create, import/export, validation and trigger simulation, optional eval plan. Manual authoring and agent-assisted creation both produce staged proposals, not installed instructions. Show diff, referenced tools/MCPs, provenance, policy impact; owner confirms using existing always-ask/proposal ceiling or typed management approval. Skill commit/reject and revision history share the same store; avoid a second incompatible Android skill catalog. Existing bundled skills read-only; fork to editable custom skill. Imported skills disabled by default. An imported SKILL.md is a draft until adapted to the actual Harw manifest contract and reviewed.

### 4.5 Assignments and effective policy

Propose registry scope = device/gateway owner + profile; assignments = profile defaults, project, supported agent/UIA, session. Explicit selections can narrow inherited enabled sets, not grant unavailable tools. Resolve exact versioned extension set at turn start; UI shows source of inherited settings and effective result. Permissions are intersection of authenticated actor capabilities, runtime/sandbox/egress policy, selected session and agent surface, user grants and enabled extension state. Skill/Plugin metadata never grants authority. A UIA in ordinary local lean GGUF chat without tools may receive permitted skill instructions but cannot execute MCP merely because assigned; show unsupported instead of pretending. Keep private room isolated. Registry changes do not silently mutate an active turn: new turn snapshots approved config; revocation/deny takes effect immediately at dispatch and cancels affected work where supported. Existing assembly builds SkillIndex and MCP contributions; verify reload/snapshot support, otherwise require controlled idle runtime reload, no speculative hot reload.

## 5. Implementation order and acceptance gates

| ID | Work package | Depends | Acceptance |
| --- | --- | --- | --- |
| R0 | PR 172 recovery build/device gate | existing commits | real arm64 build, fresh UniFFI, UIA/model/image regression, Arti/VPN reconnect; no stable release on source-only results |
| A1 | FileRef, attachment storage and typed wire feature | R0 | bounded stream, digest, owner ACL, quotas, cleanup, restart; legacy wire unchanged; file-only turn and mixed parts via test host |
| A2 | Android SAF, drafts, transfer progress/resume and history | A1 | choose files in new/existing chats, explicit target, CAS/idempotency, offline/cancel/error recovery and authorized download; no whole-file RAM copy |
| A3 | Text/context extraction and agent file tools | A1/A2 | TXT/MD/code/JSON/CSV read by model or scoped tools with ranges, budget/truncation visible; egress opt-in, malicious text inert |
| A4 | Output artifact publication and safe document parsing | A3 | publish/download/reopen immutable derivative; PDF isolated extraction or explicit unsupported; Office/audio/archive phased |
| E1 | Typed extension catalog + management service and Settings pages | R0 | device/gateway separation, scoped read/mutation, revision CAS, atomic writes, audit, no secrets in DTOs |
| E2 | MCP management & HTTP transport test/activation | E1 | add disabled, approved initialize/list, selected tool permissions, revocation, TLS/SSRF/secret tests; gateway stdio only after mediated policy acceptance |
| E3 | Skill creation/import/proposals/editor | E1 | existing validator/index/approval store; create/fork/preview/test/approve/reject; no skill grant elevation |
| E4 | Declarative plugin bundles & dependency checks | E2/E3 | import inert, preview dependency/capability diff, version pin, approval/disable/rollback/uninstall; no execution hooks |
| E5 | Extension assignments, runtime rebuild/snapshots and backup | E2-E4 | independent sessions, correct inheritance, revocation, no runtime permission expansion, cross-profile denial |
| P1 | Private Assistant files and vault-native Library integration | A1-A4, separate review | foreground authorization, lock/cancel, encrypted custody, zero remote exposure |
| C1 | Optional Cloud/Shared Assistant file support | A1-A4, distinct review | negotiated scopes, disclosure and explicit authorization, no cross-room recall or attachment leak |

A-track and E-track can run in parallel after R0 with separate integration PRs. Prefer A1-A3 and E1-E3 as first usable increments; avoid bundling all formats, OAuth, external plugin registry and private-vault extensions into the first release.

## 6. E2E verification matrix

- Rust: leaf storage, parser budgets, schema and serde, replay/collision, auth and owner isolation, stale heads, quota/cancel/cleanup, crash between finish and submit, archive/fork/rewind, historical reference resolution, no legacy feature regression. Test both local and remote ports with old/new peers; enforce exhaustive ContentPart/TurnInput/projection updates.
- Gateway: real authenticated owner, wrong principal/tenant/profile, revoked link, reconnect resume, repeated chunk and changed digest, duplicate submit, oversized frame rejection, transport timeout, disk full and cleanup. No increase of global WS limits. Audit metadata only.
- Android: JVM/Compose tests for picker scope and model/UIA before first send; SAF transient/revoked URIs, long filename and unknown length, cancellation/background/process death, retry and file-only message; no stale callback after switching target. Real signed arm64 build with generated UniFFI. Measure peak RSS, storage, latency, chunk retries, thermal/battery impact and Arti/VPN stability on device.
- Model: prove actual file-derived input reaches selected model, bounded text visible, unsupported file explicit, correct image path, denied egress never calls provider; no automatic full-history file replay after model switch. Verify selected version and source ranges.
- Extensions: wrong scope denies all writes, disabled config not registered, invalid secret refs and stale revisions, DNS rebinding/redirect, schema/result bombs, spoofed tool names, untrusted skill instructions, prompt injection, plugin traversal/zip bomb and dependency cycle, denied executable, approval replay, concurrent updates and revoke during tool call. Verify audit and secret redaction.
- Private room: separate tests prove files and transcript never appear in ordinary chat/media store, gateway, MCP or model API; lock/revoke invalidates in-flight import/read/inference, only authorized native lease can open file.
- Release: no merge or stable APK/OTA claim without passing applicable build/test gates. Document actual tested devices, supported formats/routes and limitations.

## 7. Open decisions for implementation

Provisional choices: file limits/chunk size, retention/storage quotas, parser dependencies and isolation, explicit per-file external egress policy, exact legacy projection, extension management capability naming, extension snapshot/reload mechanism, whether project and UIA assignments override or narrow defaults (recommend narrowing and explicit user overrides), and private Assistant file scope. Validate against existing ownership/custody model and mobile device benchmarks. Keep initial scope ordinary chats, two distinct hosts and no automatic external transfer or plugin execution. Discuss private-vault UX and trust policy before expanding.
