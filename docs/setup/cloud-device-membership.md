# Admin-issued user/device membership

This is local administrative inventory for the active Gateway profile. It is
not an AuthHub login and has no remotely callable endpoint. Run these commands
as the service account that already owns the Gateway's private state directory,
not as an enrolled phone or as a user of the Admin website.

1. Enroll the device and install its peer pin using the existing process. Run
   `harw admin membership inspect --device <device>`. Verify the device ID,
   node ID, current tenant and complete BLAKE3 fingerprint independently.
2. Explicitly issue a binding:
   `harw admin membership issue --device <device> --node <node> --tenant <tenant> --key-fingerprint <64-hex>`.
   The command issues an opaque user ID on the first binding. To put another
   independently verified device in the same user's membership, repeat the
   process and pass `--user <issued-user-id>`. IDs are tenant- and profile-local;
   matching display names or credentials are never membership evidence.
3. Revoke with `harw admin membership revoke` and the same device, node, tenant,
   fingerprint and user flags. The tombstone is retained. This does not revoke
   Gateway login; `harw device revoke` controls that independently. A retired
   device can still have its exact old membership revoked.

Every issuance checks the existing Gateway device and pin registries. An
unknown, revoked, duplicate, changed or unpinned device cannot be issued a
binding. An active association for a device under a previous pin must be
explicitly revoked first; re-enrollment does not carry membership over. Store
updates are bounded and atomically persisted with a revision in the owner-only
remote state directory. The file contains local enrollment metadata, not
private keys or Assistant conversations; it is not encrypted with the
Assistant key. Existing permissions and role assignments never change.

The Gateway mounts a read-only host boundary for its exact authenticated
connection. This boundary matches the original handshake fingerprint to the
current trusted peer pin and device policy, then resolves only an exact
non-revoked row while holding a shared registry lock. An unissued or revoked
membership returns no member; an invalid current device policy cannot resolve.
There is no remote membership endpoint. The separately negotiated, opt-in
[shared Cloud Assistant room](cloud-assistant.md#separately-provisioned-shared-room)
uses this lookup for live admission and additionally requires an explicit
founder grant for other devices. Existing device-bound rooms, Memory, Library
data and credentials are not migrated, copied or shared. Membership remains
revocable independently of gateway login. A registry revision is not an
external rollback anchor.
