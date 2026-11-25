# Private Assistant: hidden UI versus revoked authority

## Current behavior

The Android source distinguishes temporary Activity hide from explicit lock.
A foreground-service-backed text or private photo send may request native
park for exactly its current admitted operation. Hide always removes the
private presentation. Native park synchronously removes room and Library
read/admission authority; if it does not succeed, Android uses normal hard
revoke. Model loading/queueing, fresh reservations, photo preprocessing and
prefill, explicit Back/lock, profile loss and process death do not get a
continuation. Android negotiates v1 text-only or v2 text and photo. This
behavior is source-level only until exercised in a built app and on hardware.

An accepted turn writes one bounded encrypted and signed attempt marker. It
contains a random attempt ID, the existing room revision, and the fingerprint
of the exact authenticated sealed room file. It contains no question, image
selection, answer, or resumable credential. The 24-byte legacy marker can
still report an interruption after unlock but cannot authorize a commit.
Successful commit advances the room revision; reopening after no commit
shows an interruption and requires a new explicit send. A fingerprint
mismatch at the same revision is unknown, not confirmation or permission to
overwrite. No worker survives hard revocation with publication authority.

The existing foreground path does now use a one-shot native publisher.
After authenticating the attempt and complete answer, the vault passes it
only the sealed response, pinned directory, fixed names, and exact
ciphertext witnesses for the room and attempt. It stages/fsyncs encrypted
bytes, rechecks both witnesses and ownership under the room transaction
lock, and serializes the single fd-relative rename with Stop/revoke.
The exact attempt's publication fence is independent of the admission
registry and carries no room or vault handle. The authenticated successor
can arm it once; sealing, staging and rename must pass its 30-second
monotonic publication window, and an expired parked attempt is fenced
before entering crypto even if its cancellation timer is delayed. Stop, revoke, or an observed expiry close it; it cannot
be rearmed, extended, or reused. This is not a timer or lifetime for
background inference. Directory fsync is outside the attempt lock; an
error after publication is reported as uncertain. An encrypted staging
orphan is never replayed.

During current inference, the worker carries a native-only admission
witness: the exact authenticated room and durable attempt ciphertext hashes,
scope, unchanged base context, and an in-memory-only fingerprint of the
accepted text and photo identity. It does not hold a strong room or Library
lease. Both text and photo completion now create the same one-shot actor
while the scope is authenticated. Its mailbox accepts only the validated
full answer and exact attempt-bound approval, and returns only commit
status. The private consuming sealer holds the public recipient and the
existing ML-DSA signer, not the private recipient, root, PIN, Library owner,
or a general room handle. It can append only that recorded input to its
authenticated base and compare both exact ciphertexts before sealing.
The photo variant admits only the exact selected encrypted Library revision
and item, binds its stored-JPEG fingerprint to the prompt, and moves the
normalized zeroizing bytes exclusively to the pinned local vision worker.
The sealer receives only the authenticated photo identity, no reader or
image bytes. Both text and photo bind the actually admitted native
runtime's profile, and recheck that profile and the exact cancel gate at
seal and publish.
There is no new signature, verifier, or on-disk trust format; in-process
Rust capability isolation is not hardware or process isolation. No signer
is kept in the model worker.

Private text first pins its local GGUF. For photo inference, native
foreground admission opens the actual non-lean GGUF/projector and returns a
consuming engine pin (resolved profile/model/file identity plus the mapping
fence), with no host, network resolver or vault authority. The durable run
witness is created only after that admission. The image dispatch cannot load
or choose another model and checks cancellation and exact file identity
before and after the local run. The private vision provider has a
payload-free one-shot hook at the actual engine callback for its first
nonempty generated chunk, following real model/projector prefill and token
sampling. The request still forbids an ordinary stream sink. The callback
matches the exact photo attempt and cannot be substituted by queueing work,
model loading, pinning a projector, or merely completing without a chunk.
No Library owner or store, including a weak handle, is retained by the
model or sealer through inference or publication. The Rust-owned original
and normalized JPEG are zeroizing from creation; sanitization pre-reserves
once, so partial output on error and allocation growth cannot leave a
second unsanitized allocator copy. llama.cpp's own decoded pixels,
embeddings and inference buffers are outside this guarantee: the pinned
MTMD API does not provide a zeroizing mutable owner.

A taken JNI attempt is not itself permission to publish. The text/photo
worker claims exactly one RAM-only response owner only after the durable
witness and real model pin. Only a complete bounded model reply advances
that exact owner to a one-shot approval, bound to the attempt ciphertext
and answer fingerprint. Dropping an unfinished owner cancels its worker;
dropping an unused approval closes the slot. Vault commit checks that
proof in addition to the previously admitted input and both sealed
frames; a mismatched response, Stop, revoke, or expired window cannot
arm or publish it. Neither owner stores keys, authorizes a new turn, nor
obtains other room rights.

For text, a no-export local sink first observes real generation (a text
or reasoning delta, immediately scrubbed); for photo, the private vision
callback above gives the distinct readiness signal. No queue or prefill-only
job is kept alive on hide. A completed reply that did not emit a delta can
still seal in the foreground, but does not become parkable. Park sets a single
120-second monotonic deadline, revokes the room and Library under the
registry lock, and keeps only the exact cancellation fence and the existing
cross-process owner-file lock until the response actor/worker settles. A
native deadline task signals cooperative cancellation; the commit gate also
checks expiry synchronously before approving/arming/publishing, so a delayed
timer cannot permit a late rename. The worker receives no completion event
requiring an unlocked view: the next authenticated open reconciles durable
state. Explicit Stop/revoke, Android screen-off, or deadline before rename
prevents publication;
after rename, a possibly committed response can only be reconciled from
the vault. No retry or reopen of a previous permit.

The pre-PIN native hint recognizes only the supplied exact vault identity
and key generation in this process, including a stopped/expired attempt
still retaining its owner-lock tombstone. It leaks no revision, attempt ID,
result or new authority and does not replace the owner-lock check in normal
authenticated open. The hint is not a cross-process proof of absence.

## Invariants for one-shot background work

1. **Admission while foreground and unlocked.** Transition only an already
   accepted native run, after the durable marker, authenticated base
   transcript, pinned on-device model, and any precisely authorized sanitized
   photo are held by its actual worker. A queued/unstarted run, model opening,
   stale profile, new source read, or validation pending cannot park.
2. **Split authority, not lifetime.** Removing the UI synchronously revokes
   the public room token, reader, preview, Library and memory capabilities.
   The accepted worker may retain just the immutable prepared input and a
   single-use final-seal capability scoped to that attempt, exact vault and
   generation, room, base transcript revision, and ciphertext witness.
   The capability can prepare the next sealed pair with that room's
   recipient public key and restricted signer. It does not retain the KEM
   private key, PIN root, hardware proof, FFI token, or general store
   handle. Changing a flag on a live `AuthenticatedRoomLease` is not a
   substitute.
3. **Final authenticated CAS.** While admitted, pin the authenticated sealed
   attempt frame as well as the sealed base transcript. After hiding, only
   compare those exact opaque ciphertext bytes under the existing private
   transaction lock; no decryption or new authority lookup. Revalidate
   room/profile/generation and atomically publish one complete pair. Prepare
   the new sealed file earlier, then fence the final rename against one
   shared permit-cancellation state. The first linearized result wins:
   revoke before publication prevents it, already-published success is
   reconciled after fresh unlock. Never infer a committed reply from a
   worker return or an uncertain write.
4. **Limits and explicit cancellation.** Keep at most one such permit per
   room and a bounded monotonic lifetime; use the existing model input and
   output bounds, and make expiry a real native cancel signal. No new jobs
   or model selection after hide. A process-scoped coordinator may hold
   only a cancellation route for a parked worker. The current short
   ciphertext-publication window cannot substitute for the separate
   monotonic inference deadline/timer, prepared model pin, or pause
   handoff. Explicit lock, Back if
   treated as explicit close, profile change, foreground notification Stop
   and app-level revocation must cancel that specific permit even after
   the original Activity drops its reference. New unlock must never race
   the background writer; either fence it or expose only authenticated
   read-only state until that permit resolves.
5. **Failure remains safe.** No automatic retry or restart after process
   death, no plaintext persistence, no partial answer, and no preserving
   a reusable unlock. Keep the interruption marker and current behavior
   whenever the full park predicate is not met.

Before treating this as verified, exercise pause after work starts, pause before
native acceptance, screen lock and explicit Stop while paused, profile or
model change, a competing unlock, process death, stale/missing frames, and
cancellation immediately before and after the durable rename. Distinguish
application verification from device/OS foreground execution evidence.
