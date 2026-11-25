# Android feature recovery — PR #172

Forward-port changes on the current Onion/VPN branch; do not replace the native
runtime with a historical APK.

## User-facing modes

- Ordinary local workspace sessions: per-chat UIA, provider/model and vision
  policy. Direct vision passes original resolved image data through the GGUF
  template, with a four-new-image admission limit. Local captions are the
  explicit alternative. Neither is an automatic remote fallback.
- Private Assistant: explicitly selected installed local GGUF and projector;
  the selected model stays in RAM across navigation into the Library during
  the same room lease. No private room history is sent to an external model.
- Cloud Assistant (device room): explicit gateway model selection negotiated
  by both peers. Default and older-peer requests keep the existing wire format.
- Private and Cloud Library: explicitly selected installed local projector
  models or a separately approved API image description where supported.

## Gateway setup

To enable the Cloud Assistant model picker, set this trusted service environment
variable for the gateway process and restart the updated gateway:

    HARW_CLOUD_ASSISTANT_MODELS=provider-a/model-a,provider-b/model-b

Replace the examples with the exact configured provider and model IDs on that
gateway. A model ID may itself contain slashes. Entries are operator allowlisted,
not arbitrary client endpoints or credentials. Configure only providers that
are approved to receive the Cloud Assistant conversation context. An absent
or empty list retains the existing gateway default and does not negotiate the
new selection capability. The shared Cloud Assistant continues to use its
existing server-default route. It is not the device room.

Both Android and the gateway need the new protocol implementation to use the
new picker. The existing private room is not an external-API chat surface.

## Validation gate — not yet executed by the source-only integration

- Build and test the changed workspace crates (including harw-protocol,
  harw-session-host, harw-session-remote, harw-session-ws, harw-cli, harw-llama).
- In mobile/rust, test and check the local-host feature and the exact APK
  feature set; regenerate UniFFI bindings from that native library.
- Run Android JVM tests and the matching signed arm64 APK build on the
  configured Android toolchain. Do not reuse old generated bindings.
- On an actual device: two ordinary sessions with different UIAs/models;
  direct vision with 1–4 new images (including image-only), old image references,
  >4 rejection, missing projector, cancellation, caption mode, changed file
  and process death. Verify that image bytes reach MTMD.
- Select a second installed projector model for Cloud Library, cancel/switch
  links during loading, review and save only the original selected asset;
  verify chat defaults remain unchanged and external API consent is explicit.
- Navigate private Assistant -> Library -> back, keeping its selected model
  while the lease lives. Verify model switch exclusion during active inference,
  lock/re-authentication, and no private history export.
- Verify old/new gateway combinations, selected allowed provider/model reaching
  the real model request, unknown route rejection, same-ID retries with
  changed model/recall rejection, disconnection and revoke. Shared rooms
  remain unchanged.
- Exercise Onion/VPN reconnect and concurrent memory load; do not publish a
  stable OTA or merge the draft PR before device acceptance.

Historical source recovery and source commits do not imply a successful
build or verified device parity.
