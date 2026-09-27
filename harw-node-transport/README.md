# harw-node-transport

Layer A remote node transport for Harwness (crypto masterplan v2 §19, §28,
§29, §33, H11; CryptGuard service plan §29; WP-15 DoD uplink).

It gives two Harwness nodes a mutually authenticated, encrypted channel,
exposes a Tower service over Hyper on it with the authenticated peer as a
request extension, and carries the read-only DoD uplink from a node to the
central SecurityHub.

## Which TLS / crypto stack, and why

**rustls 0.23.45 with the aws-lc-rs 1.18.1 provider, via tokio-rustls
0.26.4.** Nothing new enters the build graph:

| Evidence in the workspace `Cargo.lock` | |
|---|---|
| `rustls 0.23.45` depends on `aws-lc-rs` **and** `ring` | both providers are already compiled |
| `aws-lc-rs 1.18.1` → `aws-lc-sys 0.45.0` | pulled by reqwest 0.13 (`rustls` feature: `harw-model-catalog`, `harw-browser-thirtyfour` via thirtyfour) and quinn-proto |
| `ring 0.17.14` | pulled by reqwest 0.12 `rustls-tls` (`harw-egress`, `harw-oauth`, `harw-mcp-client`, …) |
| `tokio-rustls 0.26.4`, `hyper 1.11.1`, `hyper-util 0.1.20`, `tower-service 0.3.3` | already locked |

Both providers are C/asm-backed and both are already built; this crate
introduces neither. Of the two it uses **aws-lc-rs**, the rustls default
provider, because only it offers the hybrid post-quantum key exchange
`X25519MLKEM768` — the masterplan asks for PQC/hybrid establishment
(§19). The same aws-lc-rs build also verifies ML-DSA-65 (FIPS 204), so node
signatures need no second PQ library. The `aws-lc-rs` feature set declared
here (`aws-lc-sys`, `prebuilt-nasm`) is exactly the one rustls already
enables; it adds no dependency edge.

**No Pingora.** Pingora would add BoringSSL/OpenSSL (`-sys`) crates. Per
masterplan §33 it belongs at the network edge (browser ingress), not
between nodes; a later edge deployment can sit in front of this crate
without changing it.

**`harw-dod-encrypt`** is a dependency too, but not part of the TLS/crypto
stack above: it defines the canonical `SignTranscript` encoding the AuthHub
node-identity key signs (`src/authhub_signer.rs`; see Identity, below). It
is a workspace member already in the lockfile; its only external dependency
is `crypt_guard_service`, also already locked — this edge adds nothing to
the build graph either.

Configuration details (`src/tls.rs`):

* TLS 1.3 only, key exchange **only** `X25519MLKEM768` (a peer without it
  fails the handshake instead of downgrading).
* The provider is passed explicitly (`builder_with_provider`); no
  process-wide default provider is installed or required.
* The server authenticates the TLS layer with an **ephemeral Ed25519 raw
  public key** (RFC 7250) generated per server instance — no X.509, no CA,
  no certificate files. The client checks the TLS 1.3 `CertificateVerify`
  signature against it but pins nothing: that key carries no identity.
* No session tickets, no resumption, no SNI: every connection runs the
  full node handshake.

## Security model

### Identity

A node's identity is its **ML-DSA-65 long-term key**, named by
`NodeIdentity { node_id, public_key, key_ref }`. The private half stays in
the Auth/Crypto Hub; this crate only calls the `NodeSigner` trait. Peers are
trusted only through `PinnedPeers` (node id → pinned public key), consumed
through the `NodeVerifier` trait.

**The IP address is never identity** (H11 exit criterion, masterplan §28):
the client names the node it expects at an address, the server names itself,
and both must prove it with the pinned key.

**Wrapped-transcript fleets (`src/authhub_signer.rs`):** a node whose key
lives in the Auth/Crypto Hub signs through `AuthHubNodeSigner`, which embeds
the raw handshake transcript in a canonical `SignTranscript` of purpose
`NodeHandshake` (`harw-dod-encrypt`) before it reaches the hub — the hub's
usage authorizer only lets the node-identity key sign that shape. Every peer
that pins such a node must verify with `TranscriptWrappedVerifier`, never
plain `PinnedPeers`: `PinnedPeers` checks the raw transcript and rejects
every wrapped signature (`VerifyError::BadSignature`), and vice versa — a
mismatch fails closed, it never authenticates. The wrapped form is a
fleet-wide setting, not negotiated per connection; a fleet must not mix
wrapped and unwrapped nodes. In-process signers (development, tests) join a
wrapped fleet through `TranscriptWrappedSigner`.

### Node handshake (`src/handshake.rs`)

Runs inside TLS, three messages plus an accept byte:

```text
ClientHello {v, client_id, server_id, Nc, Tc}         →
          ← ServerHello {v, server_id, Ns, Ts, Sig_S(T("server-signs"))}
ClientFinish {Sig_C(T("client-signs"))}                →
          ← ServerAccept
```

`T(role)` is a canonical, length-prefixed, big-endian encoding of:

```text
"harw-node-transport/handshake/v1\0" ‖ role ‖ version ‖ client_id ‖ server_id
  ‖ Nc ‖ Ns ‖ Tc ‖ Ts ‖ TLS-Exporter("EXPORTER-harw-node-transport-v1", 32)
```

| Threat | Countermeasure |
|---|---|
| Impersonation | ML-DSA-65 signature under the key pinned for the claimed node id; unknown node ids are refused before the server signs anything |
| Man in the middle / relay | the TLS exporter is in the transcript; a MITM terminating TLS has two sessions with two exporter values, so neither side's signature verifies on the other leg |
| Replayed `ClientHello` | `ReplayCache` of `(client_id, Nc)` for completed handshakes, kept for 2 × skew + 1 s; bounded, **fail closed** when full |
| Replayed `ClientFinish` / `ServerHello` | signatures bind the fresh nonces of both sides and the exporter of this session |
| Stale or pre-dated messages | `Tc` / `Ts` must lie within ±30 s (`HandshakePolicy::max_clock_skew`) |
| Reflection (one side's signature presented as the other's) | role label `server-signs` / `client-signs` in the transcript |
| Mis-addressed connection | server refuses a hello whose `server_id` is not its own; client refuses a server whose id differs from the one it dialled |
| Cross-protocol use of the node key | fixed domain prefix `TRANSCRIPT_DOMAIN`; `AuthHubNodeSigner` and the hub's own usage policy each permit the node-identity key to sign only payloads with that prefix (masterplan §5: the KMS must not become a signing oracle) |
| Resource exhaustion | handshake deadline (10 s), 16 KiB frame cap before allocation, connection limit (256), HTTP header read timeout (30 s) |

A failed verification is never recorded in the replay cache, so an attacker
cannot burn a legitimate client's nonce. The server closes the channel on
rejection without telling the client why.

The handshake is **sans-IO** (`ClientHandshake`, `ServerHandshake`,
`ServerPending`): the security logic is testable without sockets, and the
socket code (`src/server.rs`, `src/client.rs`) only moves frames.

As the masterplan asks (§19), ML-DSA runs once per connection; traffic is
then protected by the TLS 1.3 AEAD record layer with its sequence numbers.
There is no per-frame signature.

### Service exposure

* `NodeTransportServer::serve(listener, service)` / `serve_with_shutdown`
  run any `tower_service::Service<Request<Incoming>>` over Hyper HTTP/1.
  After the node handshake every request carries an `AuthenticatedPeer`
  extension (`node_id`, `protocol_version`), inserted by the transport and
  overwriting anything present — the only identity a handler may use.
* `NodeTransportClient::connect(addr, local, expected_peer, verifier)`
  returns an authenticated HTTP/1 connection (`send_request`, `peer()`).

### DoD uplink (WP-15, `src/uplink.rs`)

* **Read-only**: events flow node → hub. The hub answers the whole stream
  with one status; no command path exists back through the uplink, and this
  crate has no edge to Warden, Sentinel or sensor crates (masterplan §20:
  no widening of the local DoD TCB).
* **Identity from the channel**: `UplinkEvent` (`FindingSummary`,
  `HealthBeat`) has no node field and unknown fields are rejected; the
  receiver attributes events to the connection's `AuthenticatedPeer`.
* **Wire**: `POST /v1/dod/uplink`, `application/x-ndjson`, one JSON object
  per `\n`-terminated line, streamed (chunked) in the request body.
* **Sender** (`DodUplink` trait, `UplinkSender`): refuses events larger than
  the line limit and never blocks — a full queue drops the event with
  `UplinkError::Backpressure`.
* **Receiver** (`receive_uplink`): enforces `max_line_bytes`,
  `max_events` and `max_stream_bytes` on raw bytes before parsing; its
  buffer never exceeds twice the line limit; maps errors to HTTP 413 / 400
  via `UplinkError::http_status`.

## Not in this crate (yet)

* Missing: an `impl AuthHubSign for AuthHubClient` (or other adapter
  bridging `AuthHubClient::sign(&KeyRef, &[u8])` to this trait's
  `sign_with_key(namespace, key_id, message)`). `AuthHubClient::sign`
  already exists; `harw-infra-client` is the only crate allowed to dial the
  hub (masterplan §11/§24/§34 H4), so the adapter lands there, or here
  behind a `harw-infra-client` dependency. Enrollment / distribution of
  `PinnedPeers` keys is likewise still outside this crate.
* Pin rotation and revocation lists beyond `PinnedPeers::unpin`.
* A long-lived application-level channel (`ChannelId` actor, CryptGuard
  plan §29): TLS 1.3 already provides directional AEAD keys and sequence
  numbers; add it only if benchmarks demand it (H11).

## Tests

`cargo test -p harw-node-transport` (tokio, loopback only). Test keys come
from `DeterministicTestSigner` (`src/test_support.rs`, `#[cfg(test)]` only):
real ML-DSA-65 keys derived from fixed seeds — a stand-in for the AuthHub,
never usable outside tests.
