# Explicit owner-placement listener (source-only Linux integration)

`harw gateway owner-placement-hub --config /absolute/private/owner-hub.toml`
is a separate foreground command in a Linux build with the
`owner-placement-hub` feature. It is off by default. It does not start or
change the ordinary gateway, session host, system service, or Pingora proxy.
Exactly one route mode is required: the original explicit loopback mode, or
(for builds with `owner-placement-hub-onion`) an Onion-only mode. The latter
binds no TCP listener and uses no owner TCP path if bootstrap, rendezvous, the
locator pin, authentication or the proof deadline fails. Tor carries bytes;
pinned node authentication and the live exporter stay mandatory.

The named Harwness profile under the explicitly selected home supplies the
existing `session-host/remote/node-peers.conf`, device enrollment, and active
user membership authority. The local configured peer file and the Hub
configuration must be regular files owned by the service UID with no
group/other access under private directories. The operator must also control
their ancestry, the selected Owner Directory and policy paths, AuthHub and
NetSec socket paths, and the optional AuthHub credential file. No request can
choose any of these paths or entries.

Configuration keys:

| Key | Authority and format |
| --- | --- |
| `version` | Integer `1`. Unknown fields are errors. |
| `listen` | Literal IPv4/IPv6 loopback socket for this dedicated listener. Omit in Onion mode. |
| `[onion]` | Omit in loopback mode. Use `locator` (the independently pinned existing inbound v3 Onion service with virtual port) and four absolute, existing, private directories: `service_state`, `service_cache`, `outbound_state`, `outbound_cache`. They may not overlap or alias. The fixed Arti service nickname is `harw-owner-placement-hub`. |
| `profile` | Existing Harwness profile name (not the active profile selector). |
| `node_id`, `hub_key_blake3` | Existing AuthHub node ID and independent pin of the full raw ML-DSA-65 public key; 64 hexadecimal digits with no prefix. |
| `auth_socket`, `auth_peer_uid` | Absolute AuthHub Unix socket and explicit kernel peer UID. The UID is checked before HTTP or bearer data is sent. |
| `auth_token_file` | Optional absolute path of the existing AuthHub bearer credential, if required by the daemon. |
| `netsec_socket`, `netsec_peer_uid` | Absolute NetSec Unix socket and trusted writer's kernel UID. That writer must allow the Hub service UID to read current activation epochs. |
| `owner_directory` | Absolute path of an already provisioned durable Owner Directory. This command does not initialize or assign owners. |
| `observe_policy_directory`, `observe_policy_revision`, `observe_policy_sha256` | Existing absolute directory containing `owner-observe-grants-v1.json`, its exact nonzero revision, and the SHA-256 of the entire file (64 hexadecimal digits). |
| `[[owners]]` | One to 64 operator-provisioned rows. Each needs `node_id`, `instance`, nonzero `epoch`, either literal loopback `proof_addr` in loopback mode or a pinned v3 `proof_onion` locator in Onion mode, and the exact raw independent process key in `process_public_key_hex`. The owner node key instead comes from the selected profile's peer pins. |

The Hub composes fixed one-use placement and signing-preflight endpoints
with the bounded proof exchange. Unknown routes, ordinary attachments,
Control, and upgrades are unavailable. The status grant must agree with an
active enrolled device and session, the current durable owner, and the
live independently read NetSec epoch. Server/request data cannot replace
an independently pinned role. Any change to dedicated configuration or
trust pins requires deliberate restart; the session policy reader separately
retires its snapshot if it detects replacement or corruption.

The command does not create an initial Owner Directory assignment, refresh
it after an incarnation change, provision a process key, launch the owner
proof service, or export a device endpoint. Those are independently trusted
operations. An opened but empty directory provides no status. This document
records what is wired in source, not an installed or verified deployment.

In Onion mode, requests are disabled until Arti reports the separately
configured service locator exactly. A different service identity never opens a
Hub route; service launch may initialize Arti state, which is why the service
state directory must be provisioned for the expected locator ahead of time.
The process does not publish or log an unredacted locator itself. The Owner
must run its exact proof route and use the matching Onion mode for pre-sign.
This narrow route only serves exact placement observation, not transparent
gateway forwarding. The existing short authorization and reservation
deadlines are unchanged. Cold Tor bootstrap or slow rendezvous can fail the
operation without a TCP retry; actual latency remains to be measured.

### Liveness and exact client selection

The fixed Hub listener accepts a bounded owner observation from the exact
configured owner node transport. It rechecks the live connection's verified
key, the current NetSec activation, and the exact existing durable fence
before refreshing the in-memory observation. An observation does not create
or update an ownership assignment or bypass separate process proof.

For the opt-in Onion status client, use
`harw gateway control --host OWNER_ALIAS fenced-owner-status --hub HUB_ALIAS SESSION_ID`.
Both aliases must identify distinct v3 Onion services and distinct nodes
with preexisting exact profile pins. The client authenticates both paths
using the same enrolled profile key, then reserves the exact session.
It outputs coarse status only. It does not retry over the standalone V1
status route, a direct address, or a local socket.

Android optionally accepts `owner_status_hub` in a consciously imported
private profile file. The record contains `node_id`, `public_key_hex`,
and `onion` for a separately provisioned Hub. The owner and Hub must
have distinct node IDs, verification keys and Onion services. QR
pairing and online enrollment cannot supply this second trust anchor.
In an already open Onion chat, **Verify owner status** in chat settings
runs the one read, not a background check, and does not change that
chat's normal connection route. Old profiles need no change and do
not show the action. This describes source, not a released APK.

The owner may set up to four distinct `[[report_assignments]]` rows in its
private `owner-placement-hub.toml`: `tenant`, exact `session`, and nonzero
`generation` from a trusted, already-established Hub assignment. With no
rows it does not publish. Each row must also match the running owner's
instance and epoch and its current durable hosted session. The publisher
uses bounded periodic checks on the same pinned route; it does not create
session routes or authority and does not reassign after a restart. There is
still no production operator flow here to create a new trusted assignment or
prove the old writer was fenced before changing its epoch.

### Local writer isolation foundation

On supported Linux builds, the explicit `HARW_PRIVATE_REMOTE_WRITER=1`
mode reserves a new, dedicated private hosted-session namespace. It does
not adopt, migrate, or read the legacy hosted-session database. The same
private namespace cannot be opened by a second cooperating Harw writer,
and its kernel lock remains held until process exit, not just until the
listener stops. It covers the hosted transcript, record and approval
store; it is not an account-wide filesystem or media lock.

The mode rejects profile sharing, owner-status/proof/report activation,
cloud Library/Assistant custody and provider linking. This is a local
prerequisite for future ownership management, not proof that an older
process or another machine has lost write access. No Hub assignments or
replacement of previous owners take place. Opting in does not migrate
existing sessions or confer new network permissions.

### Historical creation evidence (explicit Linux source path)

On the isolated writer only, opt in at startup with
`HARW_PRIVATE_WRITER_CREATE_RECEIPTS=1`. In the exact active profile's private
remote state directory, provide `writer-creation-pin-v1.json` with `node`,
`instance` and `receipt_public_key_hex` (raw ML-DSA-65 public key, lowercase).
The unit must supply the corresponding independently provisioned 32-byte seed
as the read-only systemd credential `harw-private-writer-create-seed`. Never
reuse the transport key or the read-only Observe process key. Trust pins are
operator managed; neither startup nor a request generates or enrolls a key.
This opt-in does not broaden the isolated writer's ingress or mount Observe.

The creation lane covers a fresh authenticated human session on the guarded
writer only. Failed/prepared writes, forks, archive import, existing loaded
records, and later recovery do not produce a fresh receipt. Keep this evidence
private. To export one exact existing receipt from the active profile, use
`harw gateway owner-creation-receipt --tenant TENANT SESSION`. It only reads
the guarded store; it does not start a gateway or mutate owner state. Transfer
the output separately to the operator's private evidence store. A Hub operator
can check it offline with `harw gateway verify-owner-creation-receipt --pin FILE
--tenant TENANT SESSION < RECEIPT`. The independent pin is not derived from
receipt claims.

This evidence establishes historical creation by the pinned writer, not current
liveness or retirement. Verification never creates an assignment, invokes an
owner route, enables a hosted tool, or proves that a previous process stopped.
Do not feed it into refresh or failover as a substitute for those independent
checks. Existing read-only route and publication behavior remains unchanged.

The offline pin JSON has `version: 1`, `node`, `instance`, `epoch`,
`namespace_id_hex`, `receipt_public_key_hex`, `node_public_key_hex`, and the
explicit policy `observe: {"status":"disabled"}` for this isolated mode.
For an independently provisioned Observe deployment the separate comparison key
can instead be declared as `observe: {"status":"pinned","public_key_hex":"…"}`.
The receipt/transport/Observe public key bytes must not be reused.
Take the namespace ID independently from the BLAKE3 hash of the trusted
`private-writer-v1/writer.lock` marker, and the incarnation epoch from the
guarded `private-writer-v1/host/hosted-sessions/epoch` under operator control,
not from the receipt being checked. Epoch and instance pinning intentionally
do not follow any later restart. The pin JSON must be privately stored on the
verifying machine.

### Explicit co-located readiness lane, no assignment

Linux `direct-owner-placement` builds can additionally opt in at startup with
`HARW_PRIVATE_WRITER_READINESS=1`, requiring both isolated writer and historical
receipt options. The fixed listener is the owner-only `readiness-v2.sock` under
the held `private-writer-v1` namespace. It accepts the same local UID and one
separately pinned Hub node identity over the existing authenticated TLS
transport. It offers only one bounded non-upgrading readiness request per
connection, never ordinary sessions, routes, assignment writes, TCP, Onion or
fallback. The Hub identity is explicitly excluded from the ordinary session
mapper. Stop or loss of the ordinary producer listener closes this lane and
its in-flight proofs. No old socket is automatically replaced or adopted.

Startup expects an independent owner-private `writer-readiness-pin-v1.json`:
`version: 1`, `mode: "colocated_netsec_v2"`, `owner_node`, `hub_node`,
`instance`, `namespace_id_hex`, `hub_public_key_hex`,
`receipt_public_key_hex`, `readiness_public_key_hex`, `netsec_socket`,
`netsec_peer_uid`. It also requires the separately provisioned read-only
systemd credential `harw-private-writer-readiness-seed`. The readiness key
is not the creation, transport, or Observe key. NetSec needs root-controlled
socket ancestry and an independently authorized reader UID. Start with no
proof target; after a fresh session commits, the local operator may select
that exact record while the original writer is still alive using a private
`writer-readiness-scope-v1.json` containing `version: 1`, `tenant`,
`session`. Its exact file snapshot must remain current during the proof.
Neither tenant nor session is selectable through the proof request.

Both ends independently read the protected NetSec V2 activation view. Its
ephemeral daemon-instance marker binds the response to the same authoritative
NetSec process, not just an equal numeric epoch. The marker enters the
signature input independently, not the proof request or response fields.
Mismatched or restarted NetSec, expired or wrong sealed connection, guard
loss, or missing/current-record mismatch all fail closed. The gateway client
must select the same co-located Unix lane and independently verify the scope;
an ordinary TCP/Onion Hub listener does not enable this query. This is a
point-in-time verification only; it cannot attest old writer retirement, grant
session access, authorize placement, or automate failover.

In a new private namespace, establish the guard once without readiness, then
stop that bootstrap writer and configure the namespace and process pins. The
creation receipt selected for a readiness proof must come from a new eligible
session under the still-running configuration, not from the previous startup.
If a forced exit leaves the Unix socket pathname behind, startup refuses to
replace it. The operator must verify the old writer has stopped and clean up
only that exact stale socket in the protected namespace before restarting.
A root-owned socket activation service can report a kernel peer UID different
from the socket file's UID. NetSec identity relies on the configured kernel
peer pin and the live V2 marker; protected pathname checks remain separate.

The explicit, local Hub caller is
`harw gateway verify-owner-creation-readiness --pin FILE --tenant TENANT SESSION
< CREATION-RECEIPT.json`. Its strict private JSON includes the fields of the
historical verification pin with `observe: {"status":"disabled"}`, plus
`readiness_public_key_hex`, `hub_node`, `hub_key_blake3`, the exact guarded
`writer_socket`, `auth_socket`, `auth_socket_owner_uid`, `auth_peer_uid`,
optional `auth_token_file`, `netsec_socket`, `netsec_socket_owner_uid` and
`netsec_peer_uid`. The on-disk owner pins are independent of kernel peer
UIDs. The command verifies the historical receipt before dialing, uses only
the existing AuthHub NodeHandshake purpose to authenticate the pinned Hub
key, then consumes one current proof on the same local authenticated
channel. It prints a fixed success message, not identity details, key data
or NetSec state. It does not run a daemon, grant access, or write an owner row.
