//! Sans-IO node handshake: messages, canonical transcript, replay cache.
//!
//! Runs inside an established TLS 1.3 channel and binds itself to it
//! through the TLS exporter ([`crate::tls::TLS_EXPORTER_LABEL`]):
//!
//! ```text
//! client                                         server
//!   ClientHello{v, client_id, server_id, Nc, Tc}  ->
//!                                                  check v, server_id == self,
//!                                                  client pinned, |Tc-now| ≤ skew,
//!                                                  (client_id, Nc) not seen
//!               <-  ServerHello{v, server_id, Ns, Ts, Sig_S(T("server"))}
//!   check server_id == dialled, |Ts-now| ≤ skew,
//!   verify Sig_S under pinned key
//!   ClientFinish{Sig_C(T("client"))}              ->
//!                                                  verify Sig_C under pinned key,
//!                                                  commit (client_id, Nc)
//!               <-  ServerAccept
//! ```
//!
//! `T(role)` is [`Transcript::signing_bytes`]: domain tag, role label,
//! version, both node ids, both nonces, both timestamps and the 32-byte TLS
//! exporter value, length-prefixed and big-endian. The role label stops a
//! signature from one side being reflected as the other's; the exporter
//! stops a signature from being replayed on, or relayed into, a different
//! TLS session; the nonce cache plus time window stop a replayed hello.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harw_types::NodeId;

use crate::error::HandshakeError;
use crate::identity::NodeVerifier;

/// Handshake protocol version implemented by this crate.
pub const PROTOCOL_VERSION: u16 = 1;

/// Domain-separation prefix of every signed transcript.
pub const TRANSCRIPT_DOMAIN: &[u8] = b"harw-node-transport/handshake/v1\0";

/// Nonce length in bytes.
pub const NONCE_LEN: usize = 32;

/// Length of the TLS exporter value used as channel binding.
pub const CHANNEL_BINDING_LEN: usize = 32;

/// Upper bound for one handshake frame (the largest is a `ServerHello`
/// with an ML-DSA-87-sized signature and two maximal node ids).
pub const MAX_HANDSHAKE_FRAME: usize = 16 * 1024;

/// Upper bound for a node id on the wire.
pub const MAX_NODE_ID_LEN: usize = 256;

/// Upper bound for a signature on the wire (ML-DSA-87 is 4627 bytes).
pub const MAX_SIGNATURE_LEN: usize = 8 * 1024;

const TAG_CLIENT_HELLO: u8 = 0x01;
const TAG_SERVER_HELLO: u8 = 0x02;
const TAG_CLIENT_FINISH: u8 = 0x03;
const TAG_SERVER_ACCEPT: u8 = 0x04;

const ROLE_SERVER: &[u8] = b"server-signs";
const ROLE_CLIENT: &[u8] = b"client-signs";

/// Which side a transcript signature belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The accepting node.
    Server,
    /// The dialling node.
    Client,
}

/// Freshness policy of the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandshakePolicy {
    /// Accepted difference between the peer's claimed time and local time.
    pub max_clock_skew: Duration,
}

impl Default for HandshakePolicy {
    fn default() -> Self {
        Self {
            max_clock_skew: Duration::from_secs(30),
        }
    }
}

impl HandshakePolicy {
    fn skew_ms(&self) -> u64 {
        u64::try_from(self.max_clock_skew.as_millis()).unwrap_or(u64::MAX)
    }

    fn check_fresh(&self, peer_ms: u64, local_ms: u64) -> Result<(), HandshakeError> {
        let max_skew_ms = self.skew_ms();
        if peer_ms.abs_diff(local_ms) > max_skew_ms {
            return Err(HandshakeError::ClockSkew {
                peer_ms,
                local_ms,
                max_skew_ms,
            });
        }
        Ok(())
    }
}

/// Current wall-clock time in Unix milliseconds (0 before the epoch).
#[must_use]
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// --- messages -----------------------------------------------------------------

/// First message, client → server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHello {
    /// Protocol version the client speaks.
    pub version: u16,
    /// The dialling node.
    pub client: NodeId,
    /// The node the client means to reach.
    pub server: NodeId,
    /// Fresh client nonce.
    pub client_nonce: [u8; NONCE_LEN],
    /// Client wall-clock time (Unix ms).
    pub client_time_ms: u64,
}

/// Second message, server → client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHello {
    /// Protocol version the server speaks.
    pub version: u16,
    /// The accepting node.
    pub server: NodeId,
    /// Fresh server nonce.
    pub server_nonce: [u8; NONCE_LEN],
    /// Server wall-clock time (Unix ms).
    pub server_time_ms: u64,
    /// Server signature over `Transcript::signing_bytes(Role::Server)`.
    pub signature: Vec<u8>,
}

/// Third message, client → server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientFinish {
    /// Client signature over `Transcript::signing_bytes(Role::Client)`.
    pub signature: Vec<u8>,
}

impl ClientHello {
    /// Canonical wire encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(64 + self.client.as_str().len() + self.server.as_str().len());
        out.push(TAG_CLIENT_HELLO);
        out.extend_from_slice(&self.version.to_be_bytes());
        put_node_id(&mut out, &self.client);
        put_node_id(&mut out, &self.server);
        out.extend_from_slice(&self.client_nonce);
        out.extend_from_slice(&self.client_time_ms.to_be_bytes());
        out
    }

    /// Strict decoding (exact length, no trailing bytes).
    ///
    /// # Errors
    /// [`HandshakeError::Malformed`] for anything that is not a well-formed
    /// `ClientHello`.
    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeError> {
        let mut r = Reader::new(bytes);
        r.expect_tag(TAG_CLIENT_HELLO)?;
        let hello = Self {
            version: r.u16()?,
            client: r.node_id()?,
            server: r.node_id()?,
            client_nonce: r.nonce()?,
            client_time_ms: r.u64()?,
        };
        r.finish()?;
        Ok(hello)
    }
}

impl ServerHello {
    /// Canonical wire encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.server.as_str().len() + self.signature.len());
        out.push(TAG_SERVER_HELLO);
        out.extend_from_slice(&self.version.to_be_bytes());
        put_node_id(&mut out, &self.server);
        out.extend_from_slice(&self.server_nonce);
        out.extend_from_slice(&self.server_time_ms.to_be_bytes());
        put_bytes(&mut out, &self.signature);
        out
    }

    /// Strict decoding.
    ///
    /// # Errors
    /// [`HandshakeError::Malformed`].
    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeError> {
        let mut r = Reader::new(bytes);
        r.expect_tag(TAG_SERVER_HELLO)?;
        let hello = Self {
            version: r.u16()?,
            server: r.node_id()?,
            server_nonce: r.nonce()?,
            server_time_ms: r.u64()?,
            signature: r.signature()?,
        };
        r.finish()?;
        Ok(hello)
    }
}

impl ClientFinish {
    /// Canonical wire encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.signature.len());
        out.push(TAG_CLIENT_FINISH);
        put_bytes(&mut out, &self.signature);
        out
    }

    /// Strict decoding.
    ///
    /// # Errors
    /// [`HandshakeError::Malformed`].
    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeError> {
        let mut r = Reader::new(bytes);
        r.expect_tag(TAG_CLIENT_FINISH)?;
        let finish = Self {
            signature: r.signature()?,
        };
        r.finish()?;
        Ok(finish)
    }
}

/// Encoding of the final server acceptance frame.
#[must_use]
pub fn encode_server_accept() -> Vec<u8> {
    vec![TAG_SERVER_ACCEPT]
}

/// Checks a server acceptance frame.
///
/// # Errors
/// [`HandshakeError::Malformed`] for anything but the accept frame.
pub fn decode_server_accept(bytes: &[u8]) -> Result<(), HandshakeError> {
    if bytes == [TAG_SERVER_ACCEPT] {
        Ok(())
    } else {
        Err(HandshakeError::Malformed("expected server accept"))
    }
}

// --- transcript ---------------------------------------------------------------

/// Everything both sides sign, bound to one TLS session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// Protocol version.
    pub version: u16,
    /// Dialling node.
    pub client: NodeId,
    /// Accepting node.
    pub server: NodeId,
    /// Client nonce.
    pub client_nonce: [u8; NONCE_LEN],
    /// Server nonce.
    pub server_nonce: [u8; NONCE_LEN],
    /// Client time (Unix ms).
    pub client_time_ms: u64,
    /// Server time (Unix ms).
    pub server_time_ms: u64,
    /// TLS exporter value of the carrying session.
    pub channel_binding: [u8; CHANNEL_BINDING_LEN],
}

impl Transcript {
    /// Canonical, domain-separated bytes signed by `role`.
    #[must_use]
    pub fn signing_bytes(&self, role: Role) -> Vec<u8> {
        let label = match role {
            Role::Server => ROLE_SERVER,
            Role::Client => ROLE_CLIENT,
        };
        let mut out = Vec::with_capacity(
            TRANSCRIPT_DOMAIN.len() + 160 + self.client.as_str().len() + self.server.as_str().len(),
        );
        out.extend_from_slice(TRANSCRIPT_DOMAIN);
        put_bytes(&mut out, label);
        out.extend_from_slice(&self.version.to_be_bytes());
        put_bytes(&mut out, self.client.as_str().as_bytes());
        put_bytes(&mut out, self.server.as_str().as_bytes());
        out.extend_from_slice(&self.client_nonce);
        out.extend_from_slice(&self.server_nonce);
        out.extend_from_slice(&self.client_time_ms.to_be_bytes());
        out.extend_from_slice(&self.server_time_ms.to_be_bytes());
        put_bytes(&mut out, &self.channel_binding);
        out
    }
}

/// The authenticated remote node of one connection.
///
/// Inserted as a request extension by [`crate::NodeTransportServer`]; the
/// only source of peer identity a handler may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPeer {
    /// Node id proven by the handshake.
    pub node_id: NodeId,
    /// Handshake protocol version of the session.
    pub protocol_version: u16,
}

// --- replay cache -------------------------------------------------------------

/// Remembers `(client node, client nonce)` pairs of completed handshakes for
/// the replay window. Bounded; when full after purging expired entries it
/// refuses new handshakes (fail closed) instead of forgetting live ones.
#[derive(Debug)]
pub struct ReplayCache {
    state: Mutex<ReplayState>,
    capacity: usize,
    retention_ms: u64,
}

#[derive(Debug, Default)]
struct ReplayState {
    seen: HashSet<(NodeId, [u8; NONCE_LEN])>,
    order: VecDeque<(u64, NodeId, [u8; NONCE_LEN])>,
}

impl ReplayState {
    fn purge(&mut self, now_ms: u64) {
        while let Some((expiry, _, _)) = self.order.front() {
            if *expiry > now_ms {
                break;
            }
            if let Some((_, node, nonce)) = self.order.pop_front() {
                self.seen.remove(&(node, nonce));
            }
        }
    }
}

impl ReplayCache {
    /// Default number of remembered handshakes.
    pub const DEFAULT_CAPACITY: usize = 65_536;

    /// A cache matching `policy`: entries are kept for twice the skew
    /// window plus one second, i.e. until any hello carrying that nonce
    /// would be refused as stale anyway.
    #[must_use]
    pub fn new(policy: &HandshakePolicy, capacity: usize) -> Self {
        Self {
            state: Mutex::new(ReplayState::default()),
            capacity,
            retention_ms: policy.skew_ms().saturating_mul(2).saturating_add(1_000),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ReplayState> {
        // A poisoned lock only means another handshake panicked while
        // holding it; the set itself is still consistent.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Fails with [`HandshakeError::Replay`] if the pair was seen.
    ///
    /// # Errors
    /// [`HandshakeError::Replay`].
    pub fn check(
        &self,
        node: &NodeId,
        nonce: &[u8; NONCE_LEN],
        now_ms: u64,
    ) -> Result<(), HandshakeError> {
        let mut state = self.lock();
        state.purge(now_ms);
        if state.seen.contains(&(node.clone(), *nonce)) {
            return Err(HandshakeError::Replay(node.clone()));
        }
        Ok(())
    }

    /// Atomically checks and records the pair.
    ///
    /// # Errors
    /// [`HandshakeError::Replay`] if already recorded,
    /// [`HandshakeError::ReplayCacheFull`] if no room is left.
    pub fn commit(
        &self,
        node: &NodeId,
        nonce: &[u8; NONCE_LEN],
        now_ms: u64,
    ) -> Result<(), HandshakeError> {
        let mut state = self.lock();
        state.purge(now_ms);
        let key = (node.clone(), *nonce);
        if state.seen.contains(&key) {
            return Err(HandshakeError::Replay(node.clone()));
        }
        if state.seen.len() >= self.capacity {
            return Err(HandshakeError::ReplayCacheFull);
        }
        state.seen.insert(key);
        state.order.push_back((
            now_ms.saturating_add(self.retention_ms),
            node.clone(),
            *nonce,
        ));
        Ok(())
    }

    /// Number of live entries (after purging at `now_ms`).
    #[must_use]
    pub fn live_entries(&self, now_ms: u64) -> usize {
        let mut state = self.lock();
        state.purge(now_ms);
        state.seen.len()
    }
}

// --- state machines -----------------------------------------------------------

/// Client side, waiting for the `ServerHello`.
#[derive(Debug)]
pub struct ClientHandshake {
    hello: ClientHello,
}

/// Client side after the server was authenticated: sign and send the finish.
#[derive(Debug)]
pub struct ClientPending {
    transcript: Transcript,
}

impl ClientHandshake {
    /// Starts a handshake from `local` to `expected_server`.
    #[must_use]
    pub fn start(
        local: &NodeId,
        expected_server: &NodeId,
        client_nonce: [u8; NONCE_LEN],
        now_ms: u64,
    ) -> (Self, ClientHello) {
        let hello = ClientHello {
            version: PROTOCOL_VERSION,
            client: local.clone(),
            server: expected_server.clone(),
            client_nonce,
            client_time_ms: now_ms,
        };
        (
            Self {
                hello: hello.clone(),
            },
            hello,
        )
    }

    /// Authenticates the server.
    ///
    /// # Errors
    /// Version, identity, freshness or signature failures.
    pub fn on_server_hello(
        self,
        server_hello: &ServerHello,
        channel_binding: [u8; CHANNEL_BINDING_LEN],
        verifier: &dyn NodeVerifier,
        policy: &HandshakePolicy,
        now_ms: u64,
    ) -> Result<ClientPending, HandshakeError> {
        if server_hello.version != PROTOCOL_VERSION {
            return Err(HandshakeError::UnsupportedVersion(server_hello.version));
        }
        if server_hello.server != self.hello.server {
            return Err(HandshakeError::PeerMismatch {
                expected: self.hello.server,
                actual: server_hello.server.clone(),
            });
        }
        policy.check_fresh(server_hello.server_time_ms, now_ms)?;
        let transcript = Transcript {
            version: PROTOCOL_VERSION,
            client: self.hello.client,
            server: self.hello.server,
            client_nonce: self.hello.client_nonce,
            server_nonce: server_hello.server_nonce,
            client_time_ms: self.hello.client_time_ms,
            server_time_ms: server_hello.server_time_ms,
            channel_binding,
        };
        verifier.verify(
            &transcript.server,
            &transcript.signing_bytes(Role::Server),
            &server_hello.signature,
        )?;
        Ok(ClientPending { transcript })
    }
}

impl ClientPending {
    /// Bytes the local signer must sign for the `ClientFinish`.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        self.transcript.signing_bytes(Role::Client)
    }

    /// The authenticated server.
    #[must_use]
    pub fn peer(&self) -> AuthenticatedPeer {
        AuthenticatedPeer {
            node_id: self.transcript.server.clone(),
            protocol_version: self.transcript.version,
        }
    }

    /// The agreed transcript.
    #[must_use]
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }
}

/// Server side after the `ClientHello` was accepted: sign and send the
/// `ServerHello`, then wait for the `ClientFinish`.
#[derive(Debug)]
pub struct ServerPending {
    transcript: Transcript,
}

/// Server side entry point.
#[derive(Debug, Clone, Copy)]
pub struct ServerHandshake;

impl ServerHandshake {
    /// Screens a `ClientHello` before any signature is spent on it.
    ///
    /// # Errors
    /// Version, recipient, unknown client, freshness or replay failures.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_hello(
        local: &NodeId,
        hello: &ClientHello,
        server_nonce: [u8; NONCE_LEN],
        now_ms: u64,
        channel_binding: [u8; CHANNEL_BINDING_LEN],
        policy: &HandshakePolicy,
        replay: &ReplayCache,
        verifier: &dyn NodeVerifier,
    ) -> Result<ServerPending, HandshakeError> {
        if hello.version != PROTOCOL_VERSION {
            return Err(HandshakeError::UnsupportedVersion(hello.version));
        }
        if &hello.server != local {
            return Err(HandshakeError::WrongRecipient {
                addressed: hello.server.clone(),
                local: local.clone(),
            });
        }
        if !verifier.is_known(&hello.client) {
            return Err(crate::error::VerifyError::UnknownPeer(hello.client.clone()).into());
        }
        policy.check_fresh(hello.client_time_ms, now_ms)?;
        replay.check(&hello.client, &hello.client_nonce, now_ms)?;
        Ok(ServerPending {
            transcript: Transcript {
                version: PROTOCOL_VERSION,
                client: hello.client.clone(),
                server: local.clone(),
                client_nonce: hello.client_nonce,
                server_nonce,
                client_time_ms: hello.client_time_ms,
                server_time_ms: now_ms,
                channel_binding,
            },
        })
    }
}

impl ServerPending {
    /// Bytes the local signer must sign for the `ServerHello`.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        self.transcript.signing_bytes(Role::Server)
    }

    /// Builds the `ServerHello` around the local signature.
    #[must_use]
    pub fn server_hello(&self, signature: Vec<u8>) -> ServerHello {
        ServerHello {
            version: self.transcript.version,
            server: self.transcript.server.clone(),
            server_nonce: self.transcript.server_nonce,
            server_time_ms: self.transcript.server_time_ms,
            signature,
        }
    }

    /// Authenticates the client and records its nonce.
    ///
    /// # Errors
    /// Signature failure, replay, full replay cache.
    pub fn finish(
        self,
        finish: &ClientFinish,
        verifier: &dyn NodeVerifier,
        replay: &ReplayCache,
        now_ms: u64,
    ) -> Result<AuthenticatedPeer, HandshakeError> {
        verifier.verify(
            &self.transcript.client,
            &self.transcript.signing_bytes(Role::Client),
            &finish.signature,
        )?;
        replay.commit(
            &self.transcript.client,
            &self.transcript.client_nonce,
            now_ms,
        )?;
        Ok(AuthenticatedPeer {
            node_id: self.transcript.client,
            protocol_version: self.transcript.version,
        })
    }

    /// The agreed transcript.
    #[must_use]
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }
}

// --- codec helpers ------------------------------------------------------------

fn put_node_id(out: &mut Vec<u8>, id: &NodeId) {
    let bytes = id.as_str().as_bytes();
    // Ids longer than `MAX_NODE_ID_LEN` are encoded as-is and refused by the
    // peer's `Reader::node_id`; the saturation only guards the cast.
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], HandshakeError> {
        if self.rest.len() < n {
            return Err(HandshakeError::Malformed("message truncated"));
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn expect_tag(&mut self, tag: u8) -> Result<(), HandshakeError> {
        match self.take(1)? {
            [t] if *t == tag => Ok(()),
            _ => Err(HandshakeError::Malformed("unexpected message type")),
        }
    }

    fn u16(&mut self) -> Result<u16, HandshakeError> {
        let mut buf = [0u8; 2];
        buf.copy_from_slice(self.take(2)?);
        Ok(u16::from_be_bytes(buf))
    }

    fn u32(&mut self) -> Result<u32, HandshakeError> {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(self.take(4)?);
        Ok(u32::from_be_bytes(buf))
    }

    fn u64(&mut self) -> Result<u64, HandshakeError> {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(buf))
    }

    fn nonce(&mut self) -> Result<[u8; NONCE_LEN], HandshakeError> {
        let mut buf = [0u8; NONCE_LEN];
        buf.copy_from_slice(self.take(NONCE_LEN)?);
        Ok(buf)
    }

    fn node_id(&mut self) -> Result<NodeId, HandshakeError> {
        let len = usize::from(self.u16()?);
        if len > MAX_NODE_ID_LEN {
            return Err(HandshakeError::Malformed("node id too long"));
        }
        let raw = self.take(len)?;
        let text =
            std::str::from_utf8(raw).map_err(|_| HandshakeError::Malformed("node id not utf-8"))?;
        if text.chars().any(char::is_control) {
            return Err(HandshakeError::Malformed(
                "node id contains control characters",
            ));
        }
        NodeId::try_from_str(text).map_err(|_| HandshakeError::Malformed("node id blank"))
    }

    fn signature(&mut self) -> Result<Vec<u8>, HandshakeError> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if len > MAX_SIGNATURE_LEN {
            return Err(HandshakeError::Malformed("signature too long"));
        }
        Ok(self.take(len)?.to_vec())
    }

    fn finish(self) -> Result<(), HandshakeError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(HandshakeError::Malformed("trailing bytes"))
        }
    }
}
