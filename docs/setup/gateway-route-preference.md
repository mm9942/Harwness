# Gateway route preference

The Android client's default for a new or unrecognized profile is **Onion only
(no direct gateway connection)**. This applies to session traffic, reconnects,
and the gateway API used for provider-link commands. If the profile has no
valid v3 Onion locator, or Tor cannot connect, Harw reports the failure rather
than resolving or dialing the gateway's direct locator. A profile intended
only for Onion may omit the `endpoint` field, or leave it empty.

Only an explicit route choice for the exact current profile may permit direct
TCP. An Onion-first fallback mode tries Onion first, then the configured
direct endpoint if authorized and if the failure is eligible for fallback.
Direct-only is a separate opt-in. A different node, key, direct endpoint or
Onion locator requires its own choice. Labels do not authorize routing.
Changing the active policy cancels retained dials/reconnects. This controls the
Harw-to-Gateway path, not the Android browser's separate setup traffic or the
network traffic necessary to reach Tor relays.

## CLI host aliases

`harw attach --host <alias>` and `harw gateway control --host <alias>` accept the
following existing alias record shape, with the route conveyed by the locator:

- `node:<v3-onion-address>:<port>`: embedded Tor only. Requires a CLI compiled
  with the `onion` feature. A bad locator, absent Tor support or a failed Tor
  dial never invokes direct TCP or the system DNS for that hostname.
- `node-direct:<host-or-IP>:<port>`: deliberate direct TCP opt-in for this
  alias. A hostname uses the regular resolver. Do not put an Onion locator
  in this form.
- `unix:<absolute-socket-path>`: unchanged local transport.

Both Node forms require the existing `expected_node` field and its valid
public-key pin in the selected profile's trust store. A legacy `node:` record
with a non-Onion hostname or IP is still readable as local inventory but is
not dialed. The operator must replace it with an approved Onion address or
decide explicitly to opt that alias in with the `node-direct:` prefix. The CLI
does not silently upgrade aliases or fall back between transports.

## Direct owner status (optional, read only)

A SessionHost operator can explicitly enable exact-session status on its
existing authenticated Node listener by defining
`HARW_DIRECT_OWNER_STATUS_INSTANCE` at service startup. The value is an
operator-chosen instance label, not a credential or proof of a Root-Hub
process key. Omission leaves the operation off. The server refuses the
opt-in without current verified peer pins, an active enrolled device and a
mounted membership authority.

A device already enrolled at that owner can request the single status it is
authorized to observe using the owner's explicitly configured Node alias:

```sh
harw gateway control --host OWNER_ALIAS direct-owner-status SESSION_ID
```

The command honors the alias's Onion-only or explicitly approved direct
route; it never uses a local socket or an alternate route. It returns one of
`idle`, `active`, `interrupted`, or `failed`, not a transcript or a control
handle. This is a direct device-to-owner request, not a Root-Hub relay or a
claim that an owner's process generation was verified across two links.
