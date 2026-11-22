# Android native interface

The generated Kotlin package is `uniffi.harw_mobile_ffi` and its native library is `libharw_mobile_ffi.so`. Keep one `MobileCore` per app process. Methods are synchronous. Invoke them on `Dispatchers.IO`, including identity generation, and render immutable typed records. The Rust runtime receives events independently of the app. The app may register `setListener(CoreListener)` for push hints and still reads all state through `status()`, `snapshot()` and `listSessions()`.

## Kotlin methods

```kotlin
fun generateIdentity(label: String): IdentityMaterial
fun parseServerProfile(json: String): ServerProfile

class MobileCore(privateKey: ByteArray, label: String) {
    fun enrollment(): DeviceEnrollment
    fun exportEnrollmentJson(): String
    fun connect(profile: ServerProfile)
    fun disconnect()
    fun reconnect()
    fun networkAvailable()
    fun status(): ConnectionStatus
    fun listSessions(): List<MobileSession>
    fun createSession(title: String?): MobileSession
    fun openSession(sessionId: String): SessionSnapshot
    fun snapshot(sessionId: String): SessionSnapshot
    fun detachSession(sessionId: String)
    fun chatgptProviderStatus(profile: ServerProfile): ChatgptProviderStatus
    fun linkChatgptProvider(profile: ServerProfile, credential: ChatgptProviderCredential): ChatgptProviderStatus
    fun unlinkChatgptProvider(profile: ServerProfile): ChatgptProviderStatus
    fun submit(sessionId: String, text: String, expectedHead: SessionCursor,
               clientMessageId: String): SubmitReceipt
    fun interrupt(sessionId: String, turnId: String)
    fun decide(sessionId: String, approvalId: String, decision: ApprovalDecision): ApprovalReceipt
    fun setTorDirectories(stateDir: String, cacheDir: String)
    fun setRoutePreference(preference: RoutePreference)
    fun setListener(listener: CoreListener)
    fun clearListener()
    fun connectLocal(filesDir: String, keystore: PlatformKeystore)
    fun activeHost(): HostRef?
}
```

`openSession` shares an attachment across windows showing the same session. It can return before the replay is complete. While waiting, the snapshot is read-only and `detail` describes the state. Each other session has its own replay and snapshot. `detachSession` releases a process attachment when the last window stops using it. It leaves the hosted session running.

`connect` completes the pinned handshake and `session.hello`. `disconnect` cancels the connection and marks retained snapshots read-only. `reconnect` reuses the selected profile and restarts open attachments. A transport failure after connecting also triggers bounded background reconnect. The connection identity consists of the normalized endpoint, node ID and pinned key. Editing the label preserves cached sessions and submission IDs. Changing the connection identity clears these caches. The Rust client never replays a mutating RPC after disconnect.

`networkAvailable()` signals that the platform reports a usable network again. The app calls it from a `ConnectivityManager.NetworkCallback` on a platform thread, never from the Tokio runtime thread. It first applies a pending revocation. While `CONNECTING` or `RECONNECTING` with an active background run, it ends the current backoff pause, resets the attempt counter and dials at once. In `FAILED` after exhausted or transient retryable dial failures, it restarts the connection like `reconnect`. It does nothing in `REVOKED`, `DISCONNECTED` or `CONNECTED`, in `CONNECTING`/`RECONNECTING` without an active run, or in `FAILED` after a final error such as a pin, handshake or route-plan failure or HTTP 403. It never lifts a revocation, never overrides a user-chosen `disconnect` and never resends a mutating RPC.

`status().attempt` counts reconnect attempts since the last successful connection and is 0 while connected or idle. `status().nextRetryAtMs` is the Unix time in milliseconds of the next scheduled dial while waiting in backoff, and `null` otherwise.

A refused upgrade such as HTTP 403 sets the connection to `FAILED` and preserves the enrollment and read-only view. An explicit revocation RPC error, session frame or revoked WebSocket close code sets `REVOKED`, clears session content and stops automatic reconnect. The host can use this signal after invalidating connection policy. Keep the enrolled identity, profile and submission ID protection. Clear visible session content and drafts, then let the user reconnect explicitly after the operator restores access.

The bridge resumes from the observed durable position and reconstructs the live text from the host's replay. At turn completion and approval pauses, it refreshes the authoritative transcript tail to reconcile provisional chat entries with stored messages and tool outcomes.

Capture the displayed `head` and a fresh UUID when the user submits a draft. Reuse the UUID for any repeated callback for that action. A stale receipt means the displayed durable position must catch up before the user tries again. A lost reply can be reported as `MobileException.Ambiguous` even if the server accepted the prompt. Keep the draft and its UUID until the user can check the transcript. A repeated UUID never resends a known or uncertain submission within the selected profile in this process.

`decide` requires a live attachment, the host's approval grant and a current pending request that offers the selected decision. The server decides among racing responses. The app must call `interrupt` only for the currently displayed running turn. The current server treats the wire interrupt as session-wide.

`setTorDirectories(stateDir, cacheDir)` takes absolute app-private directories. Call it once after constructing `MobileCore`, before any onion dial. The Android app uses `noBackupFilesDir/tor/state` and `cacheDir/tor`. The core creates them with mode 0700. Changing them after Tor has started throws `MobileException.Internal`. If they are unset, an onion dial fails with `Connect` detail `tor: Tor storage is not configured`. Tor starts lazily on the first onion dial, not at construction. Its first connection can take up to about two minutes while the network bootstraps. `status().tor` reports the bootstrap progress while it runs.

`setRoutePreference(preference)` selects how `connect` and provider requests choose a route. `AUTO` tries the `.onion` route first when the profile has one, then the direct TCP endpoint. `ONION` uses only the `.onion` route and fails with `InvalidProfile` for a profile without one. `DIRECT` uses only the endpoint. Within one dial, routes are tried in order. A final failure, such as a node handshake, TLS or hello rejection, or a 4xx response, stops the attempt. Any other failure, including every `tor:` error, moves to the next route. `status().route` shows the route being dialed while connecting or reconnecting, and the link's route while connected. It is `null` otherwise. `status().availableRoutes` lists the routes the selected profile offers.

Tor is only a transport. The pinned ML-DSA-65 handshake, `session.hello` and device enrollment still decide access. Reaching a `.onion` address grants nothing. Onion dial timeouts are 120 s for Tor including cold bootstrap, 30 s for the node handshake, and 60 s for upgrade plus hello. The arti library may terminate the app process when the Tor network starts requiring protocol features that this build lacks. This is a known limit of the MVP. Update the app when that happens. On the next start, the onion route fails with an outdated-build error, and the user should update the app.

## Change notifications

```kotlin
interface CoreListener {
    fun statusChanged()
    fun sessionsChanged()
    fun sessionChanged(sessionId: String)
}
```

Register one listener with `setListener`. It is immediately told that status and sessions may have changed. Calls are hints, not data: read the new state with `status()`, `listSessions()` and `snapshot(sessionId)`.

- Calls come from one dedicated native thread named `harw-mobile-notify`, never under a core lock.
- Calls are coalesced (about 20 ms), so one call can stand for several changes.
- A callback must return quickly and never throw. Hand work to a coroutine.
- A callback may call `MobileCore` afterwards on its own thread, including `clearListener()`.

After `clearListener()` returns, the listener is never called again. Call `clearListener()` before `destroy()`.

## On-device host

`connectLocal(filesDir, keystore)` stops the remote connection, clears the profile and the session caches, and connects to the in-process host. `filesDir` is an absolute app-private directory. Status then shows phase `CONNECTED`, `route` null, `detail` "On-device host", and `label`, `endpoint` and `nodeId` null. A failure of the local host gives `FAILED` with no automatic retry.

In builds without the local host it throws `MobileException.Session` "the on-device host is not available in this build" and leaves the current connection alone. If the previous on-device host is still stopping, it throws `MobileException.Session` "the previous on-device host is still stopping".

`activeHost()` returns `HostRef(id = "local", kind = LOCAL, label = "Phone", reachable = true)` for the on-device host, `HostRef(id = "remote", kind = SERVER, label = <profile label>, reachable = <link active>)` while a profile is set, and `null` otherwise.

## Local models

```kotlin
fun modelCatalog(): List<CatalogModel>
fun localModels(modelsDir: String): List<LocalModel>
fun finalizeModel(modelsDir: String, modelId: String): LocalModel
fun deleteModel(modelsDir: String, modelId: String)
```

`modelsDir` must be absolute, else `MobileException.Internal` "models directory must be absolute". The core creates it with mode 0700. The catalog is fixed and ordered. Each model has the final file `<fileName>` and the partial download `<fileName>.part` in `modelsDir`. The app downloads into the `.part` file.

`localModels` returns one entry per catalog model, in catalog order. `Complete` means the final file has exactly `sizeBytes`. `Invalid` means the final file has another length. Otherwise `Partial` (with `bytesOnDisk` the `.part` length) or `Missing`.

`finalizeModel` needs a `.part` file of exactly `sizeBytes`. It checks SHA-256 and renames it to the final name, returning `Complete`. Failures throw `MobileException.Session`: "model file failed verification" (the `.part` file is deleted), "model download is incomplete", "unknown model". `deleteModel` removes both files and succeeds when they are missing.

## Device tools

```kotlin
fun deviceLimits(): DeviceLimits
fun checkShellRequest(request: ShellRequest): ShellRequest
fun normalizeDevicePath(path: String): String

interface ShellExecutor {
    fun available(): Boolean
    fun run(request: ShellRequest): ShellOutput
}

interface FileAccess {
    fun roots(): List<SafRoot>
    fun list(rootId: String, path: String): List<SafEntry>
    fun read(rootId: String, path: String, maxBytes: ULong): ByteArray
    fun write(rootId: String, path: String, data: ByteArray)
}
```

The foreign interfaces throw `DeviceException`. `FileAccess.write` is create-only: an existing file throws `DeviceException.Exists`. The Termux shell tool is off by default: SharedPreferences `harw_device_tools`, key `termux_shell_enabled`, default false. While off, `run` throws `Disabled`.

`DeviceException` variants: `Disabled`, `Unavailable(detail)`, `Declined`, `TimedOut`, `Invalid(detail)`, `InvalidPath(detail)`, `NotFound`, `Exists`, `TooLarge`, `Failed(detail)`.

`checkShellRequest` returns the request with limits applied, or throws. Empty argv gives `Invalid` "empty command". More than 64 arguments gives "too many arguments". An empty first argument, or any argument over 4096 bytes or with a NUL, gives "invalid argument". `cwd` follows the path rules or is absolute under `/data/data/com.termux/files/`, else `InvalidPath` "working directory outside Termux home". `timeoutMs` 0 becomes 30000 and is capped at 120000. `maxOutputBytes` 0 or above 65536 becomes 65536.

Path rules (`normalizeDevicePath`, mirrored in Kotlin as `normalizeSafPath`): paths are relative and `/`-separated, one trailing `/` is stripped, and `""` is the root. Rejected with `InvalidPath`:

| Condition | `detail` |
| --- | --- |
| leading `/` | `absolute path` |
| `\` | `backslash` |
| NUL | `nul byte` |
| empty segment | `empty segment` |
| `.` or `..` segment | `dot segment` |
| segment over 255 bytes | `segment too long` |
| path over 1024 bytes | `path too long` |

## Sign in with ChatGPT

Android completes browser authorization and verifies the ID token, including the nonce for the current attempt. After the user approves the transfer, pass the same immutable server profile used for consent. The Rust bridge checks the active profile and proves the server key again before sending a bounded request to `/v1/provider-link/chatgpt`. The server manages later refreshes. [OpenAI guidance for self-hosted VMs](https://developers.openai.com/siwc/token-sharing-open-source/self-hosted-vms)

Provider status, import and unlink use `GET`, `POST` and `DELETE`. A `204` response confirms the operation. The status result is `ChatgptProviderStatus(linked, availability)`:

- `GET 204` gives `linked` true and `AVAILABLE`.
- `GET 404` carrying the response header `x-harw-provider-link: unavailable` means the server has no provider linking because its `provider_link_device` is unset. The result is `linked` false and `NOT_CONFIGURED`, and the app hides "Continue with ChatGPT".
- `GET 401` or `403` means this phone is not the provider link device of the server. The result is `linked` false and `NOT_LINK_DEVICE`.
- `GET 404` without the header means not linked. The result is `linked` false and `AVAILABLE`. Older servers cannot say more, so the app still offers the flow.

The server sends the header only on a `404` and only when no provider link is configured. Old phones ignore it.

Failures of import and unlink arrive as `MobileException` with these fixed details:

| HTTP status / method | Exception | Detail |
| --- | --- | --- |
| `404` on `POST` or `DELETE`, with header `unavailable` | `Denied` | `This server is not set up for ChatGPT plan linking` |
| `404` on `POST` or `DELETE`, without header | `Denied` | `This server does not offer ChatGPT linking` |
| `401` or `403` on `POST` or `DELETE` | `Denied` | `This phone is not the ChatGPT link device of this server` |
| `409` | `Denied` | `Unlink the existing ChatGPT account before linking a different account` |
| `503` | `Transport` | `ChatGPT linking is temporarily unavailable on this server` |
| `500` | `Session` | `The server could not update its ChatGPT credential store` |
| `400`, `413` or `422` | `Denied` | `The server refused the ChatGPT registration` |
| `DELETE 202` | `Session` | `The server removed its local ChatGPT link. Revocation at OpenAI was not confirmed. Review the agent in ChatGPT Settings.` |
| any other status | `Transport` | `The server could not complete the ChatGPT request (HTTP {n})` |

Details are fixed literals and never contain server bytes or token material. `DELETE 202` means the server removed the local link while upstream revocation remains unconfirmed. Keep the ChatGPT Settings guidance visible and refresh provider status. The mobile core never retries the transfer automatically. Check status before retrying an import whose outcome is uncertain.

## Kotlin records

| Record | Fields |
| --- | --- |
| `IdentityMaterial` | `privateKey: ByteArray`, `enrollment: DeviceEnrollment` |
| `DeviceEnrollment` | `version: UInt`, `nodeId: String`, `publicKeyHex: String`, `label: String` |
| `ServerProfile` | `version: UInt`, `label: String`, `endpoint: String`, `serverNodeId: String`, `serverPublicKeyHex: String`, `onion: String?` |
| `ConnectionStatus` | `phase: ConnectionPhase`, `nodeId: String?`, `endpoint: String?`, `label: String?`, `detail: String?`, `canCreate: Boolean`, `canSubmit: Boolean`, `canApprove: Boolean`, `route: ConnectionRoute?`, `routePreference: RoutePreference`, `availableRoutes: List<ConnectionRoute>`, `tor: TorBootstrap?`, `attempt: UInt`, `nextRetryAtMs: ULong?` |
| `TorBootstrap` | `percent: UInt`, `ready: Boolean`, `blocked: String?`, `summary: String` |
| `MobileSession` | `sessionId: String`, `title: String?`, `state: MobileSessionState`, `attached: UInt`, `updatedAt: String`, `model: String?` |
| `SessionCursor` | `generation: UInt`, `durable: ULong`, `live: UInt` |
| `SessionSnapshot` | `sessionId: String`, `head: SessionCursor?`, `online: Boolean`, `canSubmit: Boolean`, `canApprove: Boolean`, `canInterrupt: Boolean`, `runningTurnId: String?`, `transcript: List<TranscriptEntry>`, `liveText: String?`, `approvals: List<MobileApproval>`, `lastFailure: String?`, `lastSyncMillis: Long?`, `detail: String?`, `agents: List<MobileAgent>`, `jobs: List<MobileJob>`, `reasoning: List<ReasoningLine>`, `finished: List<FinishedCategory>` |
| `TranscriptEntry` | `id: String`, `kind: TranscriptKind`, `text: String`, `toolState: MobileToolState?` |
| `MobileApproval` | `id: String`, `summary: String`, `risk: ApprovalRisk`, `requestedAt: String`, `timeoutAt: String`, `decisions: List<ApprovalDecision>` |
| `SubmitReceipt` | `outcome: SubmitOutcome`, `position: UInt?`, `head: SessionCursor?`, `detail: String?` |
| `ApprovalReceipt` | `outcome: ApprovalOutcome`, `detail: String?` |
| `ChatgptProviderCredential` | `clientId: String`, `accessToken: String`, `refreshToken: String`, `idToken: String`, `scope: String`, `expiresAt: ULong`, `earliestRefreshAt: ULong?`, `subject: String`, `email: String?` |
| `MobileAgent` | `id: String`, `parent: String?`, `role: String`, `model: String?`, `state: MobileAgentState`, `currentTool: String?`, `startedMs: ULong`, `toolCalls: UInt` |
| `MobileJob` | `workId: String`, `kind: String`, `state: String`, `summary: String?`, `updatedMs: ULong` |
| `ReasoningLine` | `seq: ULong`, `agent: String`, `role: String`, `kind: ReasoningKind`, `ok: Boolean?`, `text: String` |
| `FinishedCategory` | `label: String`, `total: UInt`, `ok: UInt`, `failed: UInt`, `cancelled: UInt` |
| `HostRef` | `id: String`, `kind: HostKind`, `label: String`, `reachable: Boolean` |
| `CatalogModel` | `id: String`, `label: String`, `repo: String`, `revision: String`, `fileName: String`, `url: String`, `sizeBytes: ULong`, `sha256: String`, `minRamBytes: ULong`, `recommended: Boolean` |
| `LocalModel` | `id: String`, `fileName: String`, `state: ModelFileState`, `bytesOnDisk: ULong`, `sizeBytes: ULong` |
| `DeviceLimits` | `maxArgs: UInt`, `maxArgBytes: UInt`, `defaultTimeoutMs: ULong`, `maxTimeoutMs: ULong`, `maxOutputBytes: UInt`, `maxReadBytes: ULong`, `maxWriteBytes: ULong`, `maxPathBytes: UInt` |
| `ShellRequest` | `argv: List<String>`, `cwd: String?`, `timeoutMs: ULong`, `maxOutputBytes: UInt` |
| `ShellOutput` | `exitCode: Int?`, `stdout: String`, `stderr: String`, `truncated: Boolean`, `timedOut: Boolean` |
| `SafRoot` | `id: String`, `label: String`, `writable: Boolean` |
| `SafEntry` | `name: String`, `path: String`, `isDir: Boolean`, `sizeBytes: ULong`, `modifiedMs: Long?` |
| `ChatgptProviderStatus` | `linked: Boolean`, `availability: ChatgptLinkAvailability` |

| Enum | Kotlin values |
| --- | --- |
| `ConnectionPhase` | `DISCONNECTED`, `CONNECTING`, `CONNECTED`, `RECONNECTING`, `FAILED`, `REVOKED` |
| `MobileSessionState` | `IDLE`, `RUNNING`, `WAITING_FOR_APPROVAL`, `WAITING_FOR_CHILD`, `QUEUED`, `INTERRUPTED`, `FAILED`, `CLOSED` |
| `TranscriptKind` | `USER`, `ASSISTANT`, `TOOL`, `ERROR` |
| `MobileToolState` | `RUNNING`, `DONE`, `FAILED` |
| `ApprovalRisk` | `LOW`, `MEDIUM`, `HIGH`, `CRITICAL` |
| `ApprovalDecision` | `APPROVED`, `REJECTED`, `APPROVED_ONCE` |
| `SubmitOutcome` | `ACCEPTED`, `STALE`, `QUEUE_FULL`, `DENIED`, `IN_FLIGHT` |
| `ApprovalOutcome` | `RESOLVED`, `ALREADY_RESOLVED`, `EXPIRED`, `DENIED` |
| `RoutePreference` | `AUTO`, `ONION`, `DIRECT` |
| `ConnectionRoute` | `DIRECT`, `ONION` |
| `MobileAgentState` | `RUNNING`, `WAITING`, `DONE`, `FAILED` |
| `ReasoningKind` | `THINKING`, `TOOL_START`, `TOOL_END`, `FINISHED` |
| `HostKind` | `LOCAL`, `SERVER`, `CLOUD` |
| `ModelFileState` | `MISSING`, `PARTIAL`, `COMPLETE`, `INVALID` |
| `ChatgptLinkAvailability` | `AVAILABLE`, `NOT_CONFIGURED`, `NOT_LINK_DEVICE` |

Fallible methods throw `MobileException`. Variants with `detail: String` are `InvalidIdentity`, `InvalidProfile`, `Connect`, `Session`, `Denied`, `Transport`, `Ambiguous`, and `Internal`. `Disconnected` and `Revoked` carry no fields. Device tools throw `DeviceException` instead.

## Public documents and identity

Enrollment JSON has required fields `version` (integer `1`), `node_id`, `public_key_hex`, and `label`. The public key is raw 1,952-byte ML-DSA-65 material encoded as 3,904 hex digits. The export contains only these public fields.

Connection profile JSON has required fields `version` (integer `1`), `label`, `endpoint`, `server_node_id`, and `server_public_key_hex`. `endpoint` is an explicit IP socket address such as `127.0.0.1:7410` or `[::1]:7410`. The pinned key has the same public encoding as enrollment. Obtain the profile from the server operator over a trusted channel.

The profile may carry an optional `onion` field, `<56 base32 chars>.onion:<port>`, for example `2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443`. The 56 characters use lowercase `a-z2-7` and must form a valid v3 onion address with a correct SHA3-256 checksum. The port is 1 to 65535 with no leading zeros. The profile version stays `1`. Old apps reject a profile that contains `onion` because it is an unknown field. `endpoint` remains required for the direct route. Validated profiles hold the canonical lowercase locator.

QR codes encode profiles without `onion` as `HARW:PROFILE:1:` followed by the existing payload. Profiles with `onion` use `HARW:PROFILE:2:` followed by base45 of the bytes `[labelLen, nodeLen, endpointLen, onionLen]`, then the label (UTF-8, 1 to 128 bytes), the node ID (printable ASCII, 1 to 128), the endpoint (printable ASCII, 1 to 63), the onion locator (printable ASCII, 62 to 68), and the 1,952 raw public-key bytes. Kotlin decodes both versions to the same JSON, where v2 adds `onion`. The maximum base45 text length is 3600 characters.

The only private value at this boundary is the opaque `IdentityMaterial.privateKey` byte array. Encrypt it with Android Keystore before storing it. Use decrypted bytes to construct the core and zero the Kotlin arrays after storage or construction. Persist the label alongside the wrapped identity to reproduce its enrollment. Provider credentials remain on the server.
