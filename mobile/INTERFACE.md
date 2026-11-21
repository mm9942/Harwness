# Android native interface

The generated Kotlin package is `uniffi.harw_mobile_ffi` and its native library is `libharw_mobile_ffi.so`. Keep one `MobileCore` per app process. Methods are synchronous. Invoke them on `Dispatchers.IO`, including identity generation, and poll `status()` and `snapshot()` to render immutable typed records. The Rust runtime receives events independently of polling.

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
}
```

`openSession` shares an attachment across windows showing the same session. It can return before the replay is complete. While waiting, the snapshot is read-only and `detail` describes the state. Each other session has its own replay and snapshot. `detachSession` releases a process attachment when the last window stops using it. It leaves the hosted session running.

`connect` completes the pinned handshake and `session.hello`. `disconnect` cancels the connection and marks retained snapshots read-only. `reconnect` reuses the selected profile and restarts open attachments. A transport failure after connecting also triggers bounded background reconnect. The connection identity consists of the normalized endpoint, node ID and pinned key. Editing the label preserves cached sessions and submission IDs. Changing the connection identity clears these caches. The Rust client never replays a mutating RPC after disconnect.

A refused upgrade such as HTTP 403 sets the connection to `FAILED` and preserves the enrollment and read-only view. An explicit revocation RPC error, session frame or revoked WebSocket close code sets `REVOKED`, clears session content and stops automatic reconnect. The host can use this signal after invalidating connection policy. Keep the enrolled identity, profile and submission ID protection. Clear visible session content and drafts, then let the user reconnect explicitly after the operator restores access.

The bridge resumes from the observed durable position and reconstructs the live text from the host's replay. At turn completion and approval pauses, it refreshes the authoritative transcript tail to reconcile provisional chat entries with stored messages and tool outcomes.

Capture the displayed `head` and a fresh UUID when the user submits a draft. Reuse the UUID for any repeated callback for that action. A stale receipt means the displayed durable position must catch up before the user tries again. A lost reply can be reported as `MobileException.Ambiguous` even if the server accepted the prompt. Keep the draft and its UUID until the user can check the transcript. A repeated UUID never resends a known or uncertain submission within the selected profile in this process.

`decide` requires a live attachment, the host's approval grant and a current pending request that offers the selected decision. The server decides among racing responses. The app must call `interrupt` only for the currently displayed running turn. The current server treats the wire interrupt as session-wide.

## Sign in with ChatGPT

Android completes browser authorization and verifies the ID token, including the nonce for the current attempt. After the user approves the transfer, pass the same immutable server profile used for consent. The Rust bridge checks the active profile and proves the server key again before sending a bounded request to `/v1/provider-link/chatgpt`. The server manages later refreshes. [OpenAI guidance for self-hosted VMs](https://developers.openai.com/siwc/token-sharing-open-source/self-hosted-vms)

Provider status, import and unlink use `GET`, `POST` and `DELETE`. A `204` response confirms the operation. `GET 404` indicates there is no link available. A `404` on import or unlink indicates that the server lacks provider linking. `DELETE 202` means the server removed the local link while upstream revocation remains unconfirmed. Keep the ChatGPT Settings guidance visible and refresh provider status. The mobile core never retries the transfer automatically. Check status before retrying an import whose outcome is uncertain.

## Kotlin records

| Record | Fields |
| --- | --- |
| `IdentityMaterial` | `privateKey: ByteArray`, `enrollment: DeviceEnrollment` |
| `DeviceEnrollment` | `version: UInt`, `nodeId: String`, `publicKeyHex: String`, `label: String` |
| `ServerProfile` | `version: UInt`, `label: String`, `endpoint: String`, `serverNodeId: String`, `serverPublicKeyHex: String` |
| `ConnectionStatus` | `phase: ConnectionPhase`, `nodeId: String?`, `endpoint: String?`, `label: String?`, `detail: String?`, `canCreate: Boolean`, `canSubmit: Boolean`, `canApprove: Boolean` |
| `MobileSession` | `sessionId: String`, `title: String?`, `state: MobileSessionState`, `attached: UInt`, `updatedAt: String`, `model: String?` |
| `SessionCursor` | `generation: UInt`, `durable: ULong`, `live: UInt` |
| `SessionSnapshot` | `sessionId: String`, `head: SessionCursor?`, `online: Boolean`, `canSubmit: Boolean`, `canApprove: Boolean`, `canInterrupt: Boolean`, `runningTurnId: String?`, `transcript: List<TranscriptEntry>`, `liveText: String?`, `approvals: List<MobileApproval>`, `lastFailure: String?`, `lastSyncMillis: Long?`, `detail: String?` |
| `TranscriptEntry` | `id: String`, `kind: TranscriptKind`, `text: String`, `toolState: MobileToolState?` |
| `MobileApproval` | `id: String`, `summary: String`, `risk: ApprovalRisk`, `requestedAt: String`, `timeoutAt: String`, `decisions: List<ApprovalDecision>` |
| `SubmitReceipt` | `outcome: SubmitOutcome`, `position: UInt?`, `head: SessionCursor?`, `detail: String?` |
| `ApprovalReceipt` | `outcome: ApprovalOutcome`, `detail: String?` |
| `ChatgptProviderCredential` | `clientId: String`, `accessToken: String`, `refreshToken: String`, `idToken: String`, `scope: String`, `expiresAt: ULong`, `earliestRefreshAt: ULong?`, `subject: String`, `email: String?` |
| `ChatgptProviderStatus` | `linked: Boolean` |

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

Fallible methods throw `MobileException`. Variants with `detail: String` are `InvalidIdentity`, `InvalidProfile`, `Connect`, `Session`, `Denied`, `Transport`, `Ambiguous`, and `Internal`. `Disconnected` and `Revoked` carry no fields.

## Public documents and identity

Enrollment JSON has required fields `version` (integer `1`), `node_id`, `public_key_hex`, and `label`. The public key is raw 1,952-byte ML-DSA-65 material encoded as 3,904 hex digits. The export contains only these public fields.

Connection profile JSON has required fields `version` (integer `1`), `label`, `endpoint`, `server_node_id`, and `server_public_key_hex`. `endpoint` is an explicit IP socket address such as `127.0.0.1:7410` or `[::1]:7410`. The pinned key has the same public encoding as enrollment. Obtain the profile from the server operator over a trusted channel.

The only private value at this boundary is the opaque `IdentityMaterial.privateKey` byte array. Encrypt it with Android Keystore before storing it. Use decrypted bytes to construct the core and zero the Kotlin arrays after storage or construction. Persist the label alongside the wrapped identity to reproduce its enrollment. Provider credentials remain on the server.
