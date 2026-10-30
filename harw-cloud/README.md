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

## Pairing validation

`validate_pairing_at` accepts an injected instant and parses expiry as an absolute
timestamp. UTC offsets and fractional seconds are compared chronologically;
exact expiry is expired and invalid timestamps fail closed. Debug formatting of
`PairingCode` redacts the code. This helper does not consume an invitation or
atomically bind registry and peer pins: it is not a completed enrollment flow.
