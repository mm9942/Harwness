# Cloud Assistant v1

Cloud Assistant is a separate text-only room on the Harw gateway. It is not a
coding session and does not read phone storage, import other conversations,
send attachments, call tools or start a gateway job. Only an explicit message
and the recent completed turns from that same Cloud room go to the configured
gateway provider. This v1 RPC returns a completed answer, not live deltas.

For listener configuration and smoke checks, follow the
[gateway setup](cloud-library.md#configure-the-authenticated-gateway).
Enable it on the gateway by supplying `HARW_CLOUD_ASSISTANT_KEY_FILE` as an
absolute path to 32 raw random bytes. Use a private regular file, with no hard
links, owned by the gateway service user. Keep this key distinct from the Cloud
Library credential. Missing or invalid custody leaves the feature unavailable;
it never falls back to an unencrypted store or a phone model. A plain local
Unix connection cannot negotiate the Cloud service. The gateway
stores XChaCha20-Poly1305 records under the active profile's remote state. Keep
the key out of the data directory and back it up separately if recovery is
required. This slice does not have automatic key rotation or rollback anchors.

The RPC only negotiates for an enrolled remote human device with observe and
steer capabilities and a verified peer-key pin. The room is scoped to the
gateway profile, tenant, node, key fingerprint and registered device principal.
Re-enrolling with a new key cannot open the old room automatically. No client-provided selector can identify a
different room. The current gateway lacks a stable authenticated account ID
above devices, so v1 intentionally does not share rooms across devices in the
same tenant. Being a gateway admin gives no right to inspect another owner.
Provision is explicit. Revoke hides the room and prevents re-provision, but
retains the encrypted record. Revocation is not deletion or cryptographic
erasure. No device-side Android vault or model is involved.

The host binds each request to its current authenticated connection and the
exact sealed device room, and revalidates that authority at the storage
boundary. Room revocation, device or connection invalidation, and gateway
drain cancel the local wait for in-flight inference and suppress late replies.
Cancellation cannot recall data already sent to a provider or undo a write
that won the race with revocation. The persisted attempt ID prevents implicit
new inference after an uncertain outcome.

Limits: 8 KiB input, 16 KiB answer, 48 KiB transcript, 64 completed turns, 128
unique submission attempts and at most four simultaneous turns per gateway.
A repeated identical message ID retrieves the committed answer or reports
unconfirmed delivery; it never triggers a second provider request. Capacity
limits stop new writes rather than silently dropping turns. A provider response
blocked, truncated, or interrupted is reported without storing it as complete.

## Explicit recall from the same room

`harw gateway control --host <alias> assistant history` shows the IDs of completed
turns in the authenticated room. When both sides negotiate the optional recall
capability, include up to four distinct completed turns using
`harw gateway control --host <alias> assistant send --message-id <new-id> --recall <prior-id>`.
Repeat `--recall` for additional turns and supply the new prompt via stdin.

The selected same-room content is made available to the configured gateway
provider for this message. This does not enable global memory, cross-device
rooms, or local-to-cloud synchronization. An older gateway that lacks the
recall capability refuses the operation; it does not send the prompt without
the requested recall. For an uncertain delivery, use the original message ID,
prompt and selections rather than starting a second turn.

Older gateway binaries can still read records that have not used recall. Once a
room records a recall attempt, do not downgrade the gateway binary against that
room's live storage: the earlier reader rejects the new receipt field. Retain a
consistent encrypted pre-upgrade backup and its key separately for rollback;
restoring that backup also discards newer room turns.


## Separately provisioned shared room

An enrolled device with a current [admin-issued user membership](cloud-device-membership.md)
can opt in to a **new empty** shared room. Clients must separately negotiate
`cloud_assistant_shared_v1` and call the typed `cloud_assistant.shared` method.
The original device room and its provider receipts are never imported or
opened by this method. The profile, tenant and user are resolved by the host,
not selected by the client.

1. The first verified device explicitly provisions the new room and becomes
   its founding device. Being issued a membership, or using a similar label,
   does not give another device access.
2. A second currently enrolled device of the same tenant-local user requests
   a join itself. Before approval it cannot read or submit to the room.
3. The founder reviews that request's exact device, node and full public-key
   fingerprint, then approves the gateway-issued request ID. Approval releases
   the new shared room's history to that device and allows submitting to it.
   The authority rejects approval if the target binding has since changed.
   Other members cannot approve, remove another device, or revoke the room.
4. The founder can remove a specific approved device or reject a request.
   A member can withdraw its own join. The founder can revoke the whole room;
   that retains an encrypted tombstone, not data erasure.

The host rechecks the registered membership and exact enrolled key for each
operation, before inference and before persistence. Device removal cancels
that device's local inference wait, not another participant's; an external
membership revocation is rechecked during inference. Already transmitted
provider content cannot be recalled. A re-enrollment is a different device
proof and is never automatically granted an existing room. If the founding
device is lost or revoked, no new owner is elected: previously approved
devices may continue while their own bindings are current, but cannot
administer it. The ordinary Assistant remains separate.

On Android the separate shared-room entry appears only on a compatible Cloud
link. The founder and the requesting device must compare the full public-key
fingerprint through a trusted channel before approving. Switching rooms does not
move or import history, and an uncertain write is not retried automatically.

### Explicitly shared room from the CLI

The existing `assistant` commands continue to use the device-private room. Put
`shared` between `assistant` and the verb to use the separate opt-in room,
for example `harw gateway control --host <alias> assistant shared status`.
The host must negotiate `cloud_assistant_shared_v1`; there is no fallback.

1. The founding enrolled device runs `assistant shared provision` to create a
   fresh room. Nothing is copied into it.
2. The second device, separately enrolled and bound to the same verified user,
   runs `assistant shared request-join`. Until approved it sees no conversation.
3. The founding device uses `assistant shared status`. Compare the full
   fingerprint to the intended device through a trusted channel, then run
   `assistant shared approve --request-id <id> --fingerprint <full-lowercase-hex>`.
4. An admitted device can use `assistant shared history` or pipe a message to
   `assistant shared send --message-id <unique-id>`. Use the same ID and text
   to recover an uncertain acknowledgement.
5. `assistant shared remove --request-id <id>` withdraws your own request or
   removes a device if you are the founder. `assistant shared revoke --confirm`
   revokes the whole shared room; it does not touch device-private rooms.
