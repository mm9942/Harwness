# Cloud Library and Assistant on a Harw gateway

Cloud Library is the enrolled device's private image catalogue on one gateway.
It supports metadata listing, bounded JPEG import and preview, rename, description
changes and removal. A Library revision protects image selection and updates.
It is not the phone's Private Library, a shared tenant media folder, or an
arbitrary file or URL reader. The [Cloud Assistant](cloud-assistant.md) is a
separate text-only service; it does not read this catalogue.

## Configure the authenticated gateway

1. Enable the global `[session_listener]` for `harw gateway`; configure the
   AuthHub node identity, its listener transport and the active gateway profile.
   Keep the existing remote node enrollment and peer-pin policy. Tor and direct
   TCP use the same authenticated node protocol and do not grant rights on their
   own. Do not expose a plain WebSocket or Unix socket in their place.
2. Enroll any allowed phone or CLI identity as a human device with **observe**
   and **steer** (an observer-only device will not negotiate either service).
   Do not grant gateway admin or approval authority just for these services.
   An ordinary local Unix connection, even to the same host, is insufficient.
3. Under the gateway service account, provision a separate private regular
   key file for each service to enable. A key is **32 raw random bytes**, not
   hex/base64/text; use an absolute path, mode `0600`, one link, and no
   symlinks. Keep the files outside the backed-up data directories and keep
   their custody separate from the operator-visible logs, configuration and
   enrollment files. Use different random material, not copies of one key.
   Keep protected recovery copies; do not generate fresh files for an
   existing store.
4. Set the key file paths in the **gateway process** environment, not the
   client or an interactive shell that does not launch the service:

   ```ini
   [Service]
   Environment="HARW_CLOUD_LIBRARY_KEY_FILE=/absolute/path/to/library.key"
   Environment="HARW_CLOUD_ASSISTANT_KEY_FILE=/absolute/path/to/assistant.key"
   ```

   These lines are an example for a systemd drop-in. Use the actual existing
   gateway unit and protected file paths. Both features are off by default;
   set only the desired variable if enabling one. Assistant also requires the
   gateway's configured text provider to be usable.

The profile selection and custody paths are read during startup; use a planned
gateway restart for deliberate configuration changes. Missing configuration
or unavailable custody does not grant the affected capability. Startup logs
report service availability without exposing credential paths or bytes. The
other service and ordinary ingress can continue if just one configured key or
store is unusable. Never “recover” a locked service by deleting its state,
enabling a less trusted transport, or switching to a plaintext store.

## Verify from the intended enrolled client

- Connect with the existing pinned Cloud profile so the client requests
  `cloud_library_v1` and/or `cloud_assistant_v1` (wire minor 5). Use Cloud mode
  on Android; in the CLI use the enrolled remote host, not the Unix recovery
  socket. If a capability is missing, check the gateway startup log, active
  profile, client enrollment and requested capabilities before changing anything.
- On a disposable or explicitly approved profile, list the Library, import
  one non-sensitive JPEG (up to 180,000 bytes and 1280 pixels per dimension)
  and confirm it appears on a fresh authenticated connection. Verify that
  another enrolled device does not see this catalogue. A CLI and a phone
  enrolled as distinct devices have separate owners; these v1 services do
  not pool their data across devices.
- For Assistant, confirm status before explicit room provision, send a short
  non-sensitive test message, then confirm the answer survives reconnect.
  An actual Assistant message and its recent same-room history go to the
  configured gateway provider. Do not retry an unconfirmed message under
  a fresh ID just to test.
- Test one observer-only or unconfigured connection and a Unix socket: no
  Cloud Library or Assistant capability should be granted.

Library records are encrypted under the active profile's
`session-host/remote/host/cloud-library-v1/` state. Its key/profile anchor refuses
the wrong key instead of presenting a replacement catalogue. Assistant has
its own `session-host/remote/cloud-assistant-v1/` state and key. Treat restoring
the exact data and respective credential as an operator procedure. Neither
service has automatic key rotation.
