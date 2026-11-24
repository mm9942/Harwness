# Cloud integration

This subtree supplies cloud operations, enrollment primitives, gateway configuration
and the local control CLI. It is not a complete deployed cloud service. The
integration roadmap is [PR #155](https://github.com/mm9942/Harwness/pull/155);
[current delivery status](../docs/planning/linux-user-cloud-control/08-cloud-implementation-status.md)
distinguishes implemented behavior from remaining gates.

## Profile-bound diagnostics

`cloud.enrollments.list` uses the runtime-bound `ResolvedHomeContext` and the
canonical device registry at `<profile>/session-host/remote/node-devices.conf`.
A trusted caller tenant restricts the result to that tenant's devices. The local
human TUI operator can instead receive an explicit read-only capability for all
devices in the selected profile, matching local `harw device list` administration.
The result labels this `local_profile_operator`; an explicit tenant always takes
precedence. Hosted/remote, child, model-principal and observer contexts receive no
such capability. Missing caller/profile binding is an error, never an
environment/default-profile fallback. The model-facing text contains the bounded device list as well as the
structured operation result.

The registry is limited to 256 KiB and 128 returned rows; labels are bounded and
control characters are sanitized. Rejected records are counted without disclosing
their raw contents. Six-field legacy records and the seventh approval-opt-in field
use the same parser as the listener. A missing registry is empty; unreadable,
nonregular or oversized input is an error.

`cloud.status` reports the selected profile and filesystem observation. Its schema
version 2 fields `installed`, `configured`, `running` and `ready` are nullable:
without profile-bound service evidence they are **unknown**, not false. A profile
directory does not prove valid service configuration. This operation does not
spawn systemctl, open guessed ports, or start a service.

The standalone gateway TCP probe reports `any_port_listening`; its `running` and
`ready` fields are also nullable and remain unknown. Consumers of the old boolean
`running` must migrate to this distinction. A reachable unrelated process never
establishes cloud readiness.

## Explicit local service administration

For an already installed service:

```sh
harw-cloudctl up --service harw-gateway
harw-cloudctl down --service harw-gateway
harw-cloudctl restart --service harw-gateway
```

These commands use the current user's systemd manager on Linux or LaunchAgents on
macOS. They do not install services, select system scope, change enablement, or
infer a unit from an active profile. A service name is mandatory. Other platforms
return unsupported until they implement the lifecycle methods.

`harw-cloudctl status` remains a legacy TCP diagnostic for its documented loopback
ports, not a status query for the explicitly selected service above.

Enrollment/revocation commands no longer enable/disable a system service. They
refuse until a device identity workflow is supplied. Existing `harw device list`,
`harw device set-tier`, and `harw device revoke` remain the canonical device
administration surface.

Runtime `cloud.cloudctl.up/down/restart` operations require a typed
`CloudServiceControl` adapter and trusted caller/profile context. This delivery
does not install a production adapter: unbound calls fail without spawning a
PATH-resolved helper. This preserves the separation between an admitted model
operation and local operator service administration.

## Remote attach and recovery

`harw --home /path/to/home attach --host ALIAS` now connects node aliases through
the existing authenticated node transport. The alias must carry its expected node
identity; an address alone is insufficient. The selected home/profile supplies
trusted `[session_listener].node_id`, `[infrastructure]` AuthHub configuration and
`session-host/remote/node-peers.conf`. The client's identity key must already exist
in AuthHub and the server must have enrolled the client. This command does not
perform enrollment or enable a listener.

At local and remote ingress startup, transcripts for persisted hosted-session
records undergo writer-side tail recovery using the existing per-session locks.
Missing transcripts are clean; torn tails are quarantined by the existing store;
contention and complete-record corruption prevent startup. Orphan transcripts
are not swept. This is not a new owner-election mechanism.

Automatic reconnect, one shared local/remote SessionHost, durable cloud directory,
remote build submission, coding workers and placement remain separate deliveries.
macOS native worker isolation is not implied: strict workloads require an
explicitly chosen Linux node or Linux VM and backend enforcement evidence.

## Cloud workload profiles

The opt-in `harw-runtime` feature `cloud-workload-placement` implements a
bounded workload filter on the existing deployment profiles from
[Cloud Home Hub v2](../docs/planning/65-cloud-sessions/CLOUD-HOME-HUB-PROFILES-V2.md).
It does not install services or accept remote work. Its trusted operator policy
is an explicit v1 TOML document, not the node enrollment or route directory:

```toml
version = 1
revision = 1

[[nodes]]
node = "agent-node"
profiles = ["worker-hub"]
workloads = ["agent-orchestration"]

[[nodes]]
node = "build-node"
profiles = ["compile-hub", "container-hub"]
workloads = ["compile", "podman-build"]
```

The selected worker process must match the independently authenticated node
and observed instance, the operator's allowed profiles, the actual mounted
service set, and a fresh `NodeOffer`. Separate processes on one node cannot
pool their mounts. The Cloud Hub remains a control plane and cannot share a
process with an execution profile.

| Workload | Required profiles on one process | Exact eligible backend family |
| --- | --- | --- |
| `compile` (build, test or lint) | Compile Hub | Landlock, Bubblewrap or Darwin seatbelt |
| `agent-orchestration` | Worker Hub | Landlock, Bubblewrap or Darwin seatbelt |
| `podman-build` | Compile Hub and Container Hub | Rootless Podman |
| `docker-build` | Compile Hub and Container Hub | Docker, only when expressly configured and the trusted request separately permits a rootful engine |

Each accepted candidate requires the specified backend and full observed
enforcement of filesystem, network, no-new-privileges, capability and
resource-limit dimensions. An absent, stale or ambiguous offer, a draining
node, an incompatible profile, or a weaker restriction cannot select a
different backend or node. A report cannot grant those rights. Even a
matching candidate is only placement evidence, not a reservation, job
authorization or executed build. Placement can inspect at most 128
independently observed worker processes, and rejects conflicting duplicate
node/instance observations. The inspection order is not a rank. A selected
candidate must be the sole eligible worker or the exact node and instance
chosen by trusted policy. The advisory binds the original policy snapshot,
workload, backend, exact target and independently observed activation epoch
until the earliest evidence expiry, with a five-second maximum. A subsequent
activation of the same process cannot reuse the older decision. Revalidation
does not extend the deadline or fall back.

An operator can inspect the owner-private policy locally with
`harw cloud policy validate --file PATH` or
`harw cloud policy describe --file PATH`. This is configuration inspection only:
neither command observes live nodes, activates the policy, nor starts work.

Still needed for real workload routing: authenticated mounted-service and
runtime advertisement production, lifecycle/policy revision authority, the
typed job admission/claim and reservation boundary, and the dedicated worker
dispatch and result path. This module does not open a shell or container
socket, boot a worker, or submit a job.

## Root-Hub route admission prerequisite

The gateway's `route_admission` module checks an enrolled caller's current
session grant against the durable owner fence and a separate live observation
of that exact owner. The destination must repeat the check using fresh local
identity, policy and ownership state. A routing decision is not a portable
lease or permission token. A missing directory record, stale observation or
unavailable owner never selects an alternate route.

On Unix, `owner_directory` supplies the durable generation side independently of
owner reports. The operator explicitly provisions a private store under a
trusted local filesystem path; ordinary startup never recreates missing state.
The directory assigns generation 1 once, and a same-node incarnation refresh
compares the complete prior fence before incrementing the generation under
the cross-process lock. There is no cross-node failover, row deletion, or
support for restoring an older directory snapshot as the live authority.
The opt-in Unix `hub-owner-liveness` feature adds `HubOwnerRoutes`, a bounded
observation cache attached to that selected directory. Only the trusted
node-transport owner dispatcher can supply peer provenance; a report must
match the durable fence, the authenticated NodeId, and independent Active
lifecycle policy. Hub-monotonic receipt time limits validity to 30 seconds.
An independently issued, increasing Active epoch prevents Drain/Undrain from
reviving an old observation. Admission and pre-attach revalidation consult
both stores and never accept observations or targets from client attach
requests. The `owner_ingress` adapter binds separate report and attach scopes
to NodeTransportServer's existing authenticated peer extension, then obtains
a fresh exact-session grant from an independent authorization service before
entering Hub admission. Its owner-report dispatcher recognizes only
`POST /v1/hub/owner-observation`, a plain JSON body with explicit byte length,
schema `harw.hub.owner-observation`, version `1`, and the exact tenant, session,
instance, generation and owner epoch. It denies additional fields and obtains
the node ID exclusively from authenticated transport. Reading is limited to
2 KiB and 32 data frames. The five-second ingress deadline includes the
independent Active epoch lookup before consuming the body and rechecking
that exact epoch after decoding and reading ownership. An in-flight report
cannot switch to a newer activation. The concurrency permit stays with
blocking work even if the HTTP request is cancelled. This does not mount a
listener or publish reports automatically. The unmounted owner-report client can send one
explicit report to one configured socket address with one pinned Hub node
key using the AuthHub-wrapped handshake. It binds the locally selected owner
fence before dialing, shares the ingress payload format and limits, and
requires an empty HTTP 204 acknowledgement within a fixed deadline. The
ingress response helper returns 204 only when dispatch succeeds, with a
uniform empty 403 for all refusals. It does not mount the route. No DNS,
redirect, retry or report scheduler is included.

The destination SessionHost now has a read-only owner preflight for a
negotiated, end-to-end authenticated client connection. It requires current
verified membership, the exact creator-device provenance, the expected
tenant and owner process epoch, and an existing published hosted record.
It refuses profile-only rows, stale or draining hosts, and closed records.
At most 16 preflight reads run together, including work whose waiter has
been canceled; it does not hydrate or attach a session or grant control. The Hub directory
generation and confirmed destination node/instance remain separate checks.
No wire endpoint uses this preflight yet.

For Linux, opt-in `hub-owner-netsec` provides `NetsecOwnerLifecycle` for
composition from an explicitly configured, peer-UID-pinned
`harw_infra_client::OwnerEpochClient`. The separate NetSec local UID allowlist
permits only the read-only epoch lookup and defaults to disabled. Responses
come from the actual writer instance, never from a second opener of its
state file. The Hub adapter uses fresh bounded reads, with no cache or
fallback on denial, malformed reply, missing socket, exhausted capacity, or
timeout. The private directory and expected peer UID are deployment trust
settings; enabling the library feature does not configure them or mount any
route. This is a lifecycle observation, not inter-node identity or permission.

`HubTransportIngress::with_pinned_netsec` composes that selected local
lifecycle source with one durable directory and an independently supplied
exact-session grant resolver. A one-use internal observe preflight retains
the original transport peer and takes at most five seconds from ingress.
Before returning the exact node and instance selected by the directory it
refreshes the caller's same grant (including policy revision), rechecks the
durable owner and its active epoch, and refuses a generation change or
expired request. It takes no destination address, caller grant, or instance
from a network request. The result is local metadata, not an endpoint,
credential, or evidence of an authenticated destination process: the node
transport authenticates a node key, not a specific owner process instance.
No route mounts this flow or opens any connection.

The optional Unix `hub-owner-local-host` feature closes the target-process
identity check for a co-located, trusted composition only. Bootstrap pins
one actual `SessionHost` and its configured node/instance once, before
routing. A different host handle, another process or a reopened host
cannot satisfy that pin by copying names or the on-disk epoch. The
gateway's consume-once observe path verifies the origin node, device,
tenant and the key used on its authenticated channel against the
already-negotiated destination connection, runs the destination's
independent live membership and hosted-record checks, then rechecks the
Hub grant, directory generation and activation before returning success.
It fails closed unless the trusted transport composition can provide the
fingerprint for the actual originating handshake; current-key lookup by
node ID is not proof. No production resolver supplies that proof today.
No connection is created, no output is streamed, and no capability leaves
this function. Cross-process or node-to-node routing still requires an
independently provisioned, channel-bound destination proof plus the
originating device's end-to-end authorization. It must not use a process
label or ordinary node signature as a substitute.

Optional Unix `hub-owner-process-proof` is an offline verification building
block, not that routing transport. An operator must independently provision
a public ML-DSA-65 process key held only by the exact host incarnation and
select it against the trusted owner node, instance and owner epoch. The
current long-term node key is a mandatory distinct-key check, not evidence
that another key has process custody. The versioned, domain-separated
transcript uses one public random challenge, the actual destination
channel's 32-byte exporter, exact node/instance/tenant/session, directory
generation, owner epoch, NetSec activation, and originating node/device
grant revision. The reply must use the same observed exporter and current
pin. Verification consumes the challenge and refreshes existing Hub
authorization within the original deadline; withdrawal or replacement
invalidates pending proofs. This proves only possession of the independently
selected key for that challenge. No production composition yet distributes
or revokes such pins, establishes their custody, or mounts an authenticated
proof dispatcher. Origin-device authorization at the owner remains separate.

Optional `hub-owner-process-custody` adds an unmounted owner-side signing
adapter only for an already-provisioned, exclusive process key that matches
the separately pinned public key. It neither generates nor loads/exports
private material or uses the AuthHub node-handshake key. It reconstructs the
same bounded canonical observe transcript from independent local fence and
grant inputs plus the actual accepted-channel exporter for the selected Hub.
The only remotely supplied variable used to build it is the public challenge
nonce; any difference from the canonical reconstruction is refused. The
original device must have its own existing independently authenticated
owner HostConnection, and the exact owner record and membership are checked
before signing and again before releasing a signature. Work is capped at
eight concurrent sign attempts, the reply has a three-second local deadline,
and a bounded replay ledger refuses reused nonces. No status, read access,
stream or grant is returned by the signer, and there is no production key
provisioner, policy/owner composition, listener mount or signer endpoint.

The Hub composition accepts only a typed, live exporter from an
authenticated outbound NodeTransport channel to the directory-selected
node (not an unsolicited inbound channel). It
does not accept a raw exporter, peer reply location, or per-request trust
key. The offline process-pin set is empty by default and holds at most
64 independently provisioned exact node/instance/epoch entries, with
neither duplicate coordinates nor reused process keys. Trusted local
withdrawal invalidates outstanding proofs without substituting another
owner. At most 16 pending proof challenges can be held at a time; holders
must drop expired work. Proof verification on the original live channel
still yields only confirmation, not a data stream. The separately
available co-located path can return only a finite status from the actual
host after the already-negotiated caller's exact membership and record
checks and a final Hub policy/fence recheck. There is no transcript,
list, write, session-control or generic relay endpoint.

For a direct device-to-owner route the Hub can now select one exact local
`DirectOwnerStatusPlan`. The bounded operator configuration pins a literal
socket and one enrolled node key per owner, and must agree with the
independent process pin for the directory-selected instance/epoch. The
chooser requires the original inbound device's sealed handshake key and
an independent exact-session resolver for that key, checks the grant again
after the Hub's owner and activation revalidation, then returns only
nonserializable dial metadata under the original deadline. There is no
dialing, discovery, redirect, fallback or mutable session action. To use the
negotiated `session.direct_owner_status` method, the device must establish its
own independently authenticated direct owner connection. The separate
status RPC accepts a session ID but does not currently attest the
Hub-selected process generation/epoch. The route plan is not such an
attestation or permission to use an unmatched status result; the Hub's
process challenge on its own channel must not be presented as original
device authority at the owner.

The selected plan can be consumed by the Hub's in-process before/after
owner-fence authority. It checks the exact enrolled owner on its separate
accepted control channel (including the key that proved the handshake),
the original device's verified channel/key and exact current grant, the
same durable generation and NetSec activation, and expiry. A replacement
channel or a withdrawn process pin refuses completion. The co-located
adapter can return only coarse status from the actual independently
pinned `SessionHost` through the original device's already-negotiated
connection, after both Hub checks have succeeded. A distinct node does not
receive the in-process handoff; it still needs an independently
authenticated owner→Hub read-only fence verifier plus the original device's
own direct owner link. For the existing Hub→owner challenge, the match now
also requires the sealed ML-DSA key fingerprint from the actual outgoing
handshake, not NodeId or operator key text alone.

The unmounted fixed owner-control operation `POST /v1/hub/owner-placement`
now backs that verifier. The owner request supplies one opaque 32-byte
one-use selector only; there is no node, tenant, session, host, route, or
address argument. A pair can be minted only by consuming a valid dedicated
process signature for the exact selected direct plan, then rechecking the
original enrolled device’s exact grant and existing placement authority.
The Hub checks the live proof channel, actual accepted owner handshake key,
and current original device grant/owner generation/activation on each
lookup. It keeps at most 64 selectors, rejects repeated consumption, and
makes AFTER spendable only after the BEFORE check succeeds. Both replies
carry the same correlation and exact bound original device identity, key
fingerprint, and owner fence. They are not credentials or bearer grants.
Malformed, stale and unauthorized requests have the same empty refusal.

`OwnerPlacementLookupClient` implements the Host’s explicit placement
authority when the local-host feature is selected. Each selector is used
once on one independently preconfigured, exactly pinned Hub address/key;
the client checks the actual dialed Hub fingerprint, strict bounded response,
expiry and local owner NodeId. No address/pin comes from the reply, and
there is no retry, cached placement or fallback. Optional `hub-owner-placement-listener` can explicitly run the two fixed
routes on an already selected, pinned, non-upgrading NodeTransport
listener. Placement lookup is restricted to the selected enrolled owner.
The optional `POST /v1/hub/direct-owner-placement-pair` accepts only the
exact session on the original authenticated device link and sends its
short-lived identifiers only in that same request’s response after grant,
owner and process proof checks. The Hub retains the actual proof connection
until both lookups finish or the original deadline expires. The optional
pair route refuses to run without a trusted exact process-proof exchange;
the listener does not discover owners or proxy connections. Nothing starts
this listener by default. The fenced direct owner method separately
requires the original device’s independently authenticated owner connection
and the explicitly installed lookup client; standalone v1 status does not
use the placement route. The concrete proof exchange dials only the exact literal owner endpoint
and enrolled key from the selected plan. Its fixed owner
`POST /v1/owner/observe-proof` sends an opaque reservation, a one-use pre-sign
selector, and the canonical challenge. The owner checks the actual Hub
key and redeems only that selector over an independently pinned Hub link.
The Hub's fixed `POST /v1/hub/owner-placement-presign` returns only the
current authorized original device and placement snapshot bound to that
reservation and challenge. It checks the durable owner fence, NetSec
activation, pending proof channel, and original sealed device grant before
answering; unsigned or stale scopes do not mint a pair. The owner then
claims the exact original device WebSocket reservation, signs only the
canonical reservation-bound v2 challenge and marks the local reservation
signed only after revalidation. V2 pair delivery echoes the reservation;
both placement phases carry it as well. None of these operations can
rebuild an identity from request fields or grant session control. Trusted
owner listener routing, process-key provisioning and an end-to-end
two-node exercise remain necessary; nothing enables these routes by default.

The optional Pingora placeholder still returns HTTP 501. The store is not yet
mounted by a production Hub: the enrolled caller resolver, separately
configured NetSec reader, production report publisher, and independent
destination transport/attachment protocol still have to be composed before
enabling relay.
Existing Android direct and Onion sessions do not use it.

## Pairing validation

`validate_pairing_at` accepts an injected instant and parses expiry as an absolute
timestamp. UTC offsets and fractional seconds are compared chronologically;
exact expiry is expired and invalid timestamps fail closed. Debug formatting of
`PairingCode` redacts the code. This helper does not consume an invitation or
atomically bind registry and peer pins: it is not a completed enrollment flow.
