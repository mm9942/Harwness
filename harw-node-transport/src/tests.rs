//! Crate tests: codec, sans-IO handshake, replay cache, TLS channel, loopback
//! transport and the DoD uplink.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use harw_types::NodeId;
use http::{Method, Request, StatusCode};
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::error::{HandshakeError, TransportError, UplinkError, VerifyError};
use crate::handshake::{
    ClientFinish, ClientHandshake, ClientHello, HandshakePolicy, ReplayCache, ServerHandshake,
    ServerHello, decode_server_accept, encode_server_accept,
};
use crate::identity::{NodeVerifier, PinnedPeers};
use crate::test_support::{
    TestError, TestResult, TestService, ctx, ensure, pinned, test_node, text_response,
};
use crate::uplink::{
    DodUplink, FindingSeverity, HealthStatus, UPLINK_PATH, UplinkEvent, UplinkLimits, encode_line,
    receive_uplink, uplink_channel, uplink_request,
};
use crate::{
    AuthenticatedPeer, ML_DSA_65_PUBLIC_KEY_LEN, NodeTransportClient, NodeTransportServer,
    ServerOptions, empty_body,
};

const NOW: u64 = 1_700_000_000_000;

fn node_id(name: &str) -> TestResult<NodeId> {
    NodeId::try_from_str(name).map_err(ctx("node id"))
}

// --- codec --------------------------------------------------------------------

mod codec {
    use super::*;

    fn sample_hello() -> TestResult<ClientHello> {
        Ok(ClientHello {
            version: crate::PROTOCOL_VERSION,
            client: node_id("node-b")?,
            server: node_id("node-a")?,
            client_nonce: [3; 32],
            client_time_ms: NOW,
        })
    }

    #[test]
    fn test_handshake_messages_round_trip() -> TestResult {
        let hello = sample_hello()?;
        assert_eq!(ClientHello::decode(&hello.encode())?, hello);

        let server_hello = ServerHello {
            version: 1,
            server: node_id("node-a")?,
            server_nonce: [4; 32],
            server_time_ms: NOW + 1,
            signature: vec![9; 100],
        };
        assert_eq!(ServerHello::decode(&server_hello.encode())?, server_hello);

        let finish = ClientFinish {
            signature: vec![5; 3309],
        };
        assert_eq!(ClientFinish::decode(&finish.encode())?, finish);
        decode_server_accept(&encode_server_accept())?;
        Ok(())
    }

    #[test]
    fn test_decoder_rejects_trailing_truncated_and_wrong_tag() -> TestResult {
        let mut bytes = sample_hello()?.encode();
        bytes.push(0);
        assert_eq!(
            ClientHello::decode(&bytes),
            Err(HandshakeError::Malformed("trailing bytes"))
        );
        let bytes = sample_hello()?.encode();
        assert!(ClientHello::decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(ServerHello::decode(&bytes).is_err());
        assert!(decode_server_accept(&[0x01]).is_err());
        Ok(())
    }

    #[test]
    fn test_decoder_rejects_blank_long_and_control_node_ids() -> TestResult {
        for bad in ["   ", "node\u{0}evil"] {
            let mut bytes = vec![0x01, 0x00, 0x01];
            let raw = bad.as_bytes();
            bytes.extend_from_slice(&u16::try_from(raw.len()).map_err(ctx("len"))?.to_be_bytes());
            bytes.extend_from_slice(raw);
            ensure(
                ClientHello::decode(&bytes).is_err(),
                format!("accepted id {bad:?}"),
            )?;
        }
        let long = ClientHello {
            client: NodeId("x".repeat(300)),
            ..sample_hello()?
        };
        assert_eq!(
            ClientHello::decode(&long.encode()),
            Err(HandshakeError::Malformed("node id too long"))
        );
        Ok(())
    }
}

// --- sans-IO handshake --------------------------------------------------------

mod sans_io {
    use super::*;

    struct Fixture {
        a: crate::test_support::TestNode,
        b: crate::test_support::TestNode,
        server_trust: PinnedPeers,
        client_trust: PinnedPeers,
        policy: HandshakePolicy,
        replay: ReplayCache,
    }

    fn fixture() -> TestResult<Fixture> {
        let a = test_node("node-a", 1)?;
        let b = test_node("node-b", 2)?;
        let server_trust = pinned(&[&b.identity])?;
        let client_trust = pinned(&[&a.identity])?;
        let policy = HandshakePolicy::default();
        let replay = ReplayCache::new(&policy, 16);
        Ok(Fixture {
            a,
            b,
            server_trust,
            client_trust,
            policy,
            replay,
        })
    }

    /// Runs one full handshake at `NOW` over `binding`; returns the hello and
    /// finish so tests can replay them.
    async fn full_handshake(
        f: &Fixture,
        client_nonce: [u8; 32],
        binding: [u8; 32],
    ) -> TestResult<(ClientHello, ClientFinish, AuthenticatedPeer)> {
        let (client, hello) = ClientHandshake::start(&f.b.id, &f.a.id, client_nonce, NOW);
        let pending = ServerHandshake::accept_hello(
            &f.a.id,
            &hello,
            [0xA5; 32],
            NOW,
            binding,
            &f.policy,
            &f.replay,
            &f.server_trust,
        )?;
        let server_sig = f.a.local.signer.sign(&pending.signing_payload()).await?;
        let server_hello = pending.server_hello(server_sig);
        let client_pending =
            client.on_server_hello(&server_hello, binding, &f.client_trust, &f.policy, NOW)?;
        ensure(
            client_pending.peer().node_id == f.a.id,
            "client authenticated wrong server",
        )?;
        let finish = ClientFinish {
            signature: f
                .b
                .local
                .signer
                .sign(&client_pending.signing_payload())
                .await?,
        };
        let peer = pending.finish(&finish, &f.server_trust, &f.replay, NOW)?;
        Ok((hello, finish, peer))
    }

    #[tokio::test]
    async fn test_two_test_signers_complete_the_handshake() -> TestResult {
        let f = fixture()?;
        let (_, _, peer) = full_handshake(&f, [1; 32], [7; 32]).await?;
        assert_eq!(peer.node_id, f.b.id);
        assert_eq!(peer.protocol_version, crate::PROTOCOL_VERSION);
        assert_eq!(f.replay.live_entries(NOW), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_replayed_client_hello_is_rejected() -> TestResult {
        let f = fixture()?;
        let (hello, _, _) = full_handshake(&f, [1; 32], [7; 32]).await?;
        let replayed = ServerHandshake::accept_hello(
            &f.a.id,
            &hello,
            [0x5A; 32],
            NOW + 5,
            [8; 32],
            &f.policy,
            &f.replay,
            &f.server_trust,
        );
        assert_eq!(
            replayed.map(|_| ()),
            Err(HandshakeError::Replay(f.b.id.clone()))
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_replayed_client_finish_on_new_session_is_rejected() -> TestResult {
        let f = fixture()?;
        let (_, old_finish, _) = full_handshake(&f, [1; 32], [7; 32]).await?;
        // Attacker opens a new session (fresh nonce, other TLS exporter) and
        // replays the recorded finish signature.
        let (_, hello) = ClientHandshake::start(&f.b.id, &f.a.id, [2; 32], NOW);
        let pending = ServerHandshake::accept_hello(
            &f.a.id,
            &hello,
            [0x11; 32],
            NOW,
            [9; 32],
            &f.policy,
            &f.replay,
            &f.server_trust,
        )?;
        let verdict = pending.finish(&old_finish, &f.server_trust, &f.replay, NOW);
        assert_eq!(
            verdict,
            Err(HandshakeError::Verify(VerifyError::BadSignature(
                f.b.id.clone()
            )))
        );
        // The failed attempt must not burn the nonce of the real client.
        assert_eq!(f.replay.live_entries(NOW), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_stale_and_future_hellos_are_rejected() -> TestResult {
        let f = fixture()?;
        let (_, hello) = ClientHandshake::start(&f.b.id, &f.a.id, [3; 32], NOW);
        for now in [NOW + 30_001, NOW - 30_001] {
            let verdict = ServerHandshake::accept_hello(
                &f.a.id,
                &hello,
                [0; 32],
                now,
                [0; 32],
                &f.policy,
                &f.replay,
                &f.server_trust,
            );
            ensure(
                matches!(verdict, Err(HandshakeError::ClockSkew { .. })),
                format!("hello at skew {now} accepted: {verdict:?}"),
            )?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_channel_binding_mismatch_fails_server_signature() -> TestResult {
        let f = fixture()?;
        let (client, hello) = ClientHandshake::start(&f.b.id, &f.a.id, [4; 32], NOW);
        let pending = ServerHandshake::accept_hello(
            &f.a.id,
            &hello,
            [0x22; 32],
            NOW,
            [6; 32], // server-side TLS leg of a man in the middle
            &f.policy,
            &f.replay,
            &f.server_trust,
        )?;
        let server_hello =
            pending.server_hello(f.a.local.signer.sign(&pending.signing_payload()).await?);
        let verdict =
            client.on_server_hello(&server_hello, [7; 32], &f.client_trust, &f.policy, NOW);
        assert_eq!(
            verdict.map(|_| ()),
            Err(HandshakeError::Verify(VerifyError::BadSignature(
                f.a.id.clone()
            )))
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_server_signature_is_not_accepted_as_client_finish() -> TestResult {
        // Role separation: even the same key could not reflect a signature.
        let f = fixture()?;
        let (_, hello) = ClientHandshake::start(&f.b.id, &f.a.id, [5; 32], NOW);
        let pending = ServerHandshake::accept_hello(
            &f.a.id,
            &hello,
            [0x33; 32],
            NOW,
            [1; 32],
            &f.policy,
            &f.replay,
            &f.server_trust,
        )?;
        let b_signs_server_role = f.b.local.signer.sign(&pending.signing_payload()).await?;
        let verdict = pending.finish(
            &ClientFinish {
                signature: b_signs_server_role,
            },
            &f.server_trust,
            &f.replay,
            NOW,
        );
        ensure(
            verdict.is_err(),
            "server-role signature accepted as client finish",
        )
    }

    #[test]
    fn test_unknown_client_and_wrong_recipient_are_rejected_before_signing() -> TestResult {
        let f = fixture()?;
        let stranger = node_id("node-x")?;
        let (_, hello) = ClientHandshake::start(&stranger, &f.a.id, [6; 32], NOW);
        let verdict = ServerHandshake::accept_hello(
            &f.a.id,
            &hello,
            [0; 32],
            NOW,
            [0; 32],
            &f.policy,
            &f.replay,
            &f.server_trust,
        );
        assert_eq!(
            verdict.map(|_| ()),
            Err(HandshakeError::Verify(VerifyError::UnknownPeer(stranger)))
        );

        let other = node_id("node-c")?;
        let (_, hello) = ClientHandshake::start(&f.b.id, &other, [7; 32], NOW);
        let verdict = ServerHandshake::accept_hello(
            &f.a.id,
            &hello,
            [0; 32],
            NOW,
            [0; 32],
            &f.policy,
            &f.replay,
            &f.server_trust,
        );
        ensure(
            matches!(verdict, Err(HandshakeError::WrongRecipient { .. })),
            format!("{verdict:?}"),
        )
    }

    #[test]
    fn test_unsupported_version_is_rejected() -> TestResult {
        let f = fixture()?;
        let (_, mut hello) = ClientHandshake::start(&f.b.id, &f.a.id, [8; 32], NOW);
        hello.version = 2;
        let verdict = ServerHandshake::accept_hello(
            &f.a.id,
            &hello,
            [0; 32],
            NOW,
            [0; 32],
            &f.policy,
            &f.replay,
            &f.server_trust,
        );
        assert_eq!(
            verdict.map(|_| ()),
            Err(HandshakeError::UnsupportedVersion(2))
        );
        Ok(())
    }
}

// --- replay cache / pinning ---------------------------------------------------

mod replay_and_pins {
    use super::*;

    #[test]
    fn test_replay_cache_is_bounded_and_expires() -> TestResult {
        let policy = HandshakePolicy {
            max_clock_skew: Duration::from_secs(1),
        };
        let cache = ReplayCache::new(&policy, 2);
        let node = node_id("node-b")?;
        cache.commit(&node, &[1; 32], NOW)?;
        cache.commit(&node, &[2; 32], NOW)?;
        assert_eq!(
            cache.commit(&node, &[1; 32], NOW),
            Err(HandshakeError::Replay(node.clone()))
        );
        assert_eq!(
            cache.commit(&node, &[3; 32], NOW),
            Err(HandshakeError::ReplayCacheFull)
        );
        // Retention is 2 × skew + 1 s = 3 s.
        cache.commit(&node, &[3; 32], NOW + 3_000)?;
        assert_eq!(cache.live_entries(NOW + 3_000), 1);
        Ok(())
    }

    #[test]
    fn test_pin_rejects_non_ml_dsa_65_keys() -> TestResult {
        let mut peers = PinnedPeers::new();
        let verdict = peers.pin(node_id("node-a")?, vec![0; 32]);
        ensure(verdict.is_err(), "32-byte key pinned")?;
        peers.pin(node_id("node-a")?, vec![0; ML_DSA_65_PUBLIC_KEY_LEN])?;
        assert_eq!(peers.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_pinned_peers_verify_real_ml_dsa_signatures() -> TestResult {
        let a = test_node("node-a", 1)?;
        let trust = pinned(&[&a.identity])?;
        let sig = a.local.signer.sign(b"payload").await?;
        trust
            .verify(&a.id, b"payload", &sig)
            .map_err(ctx("verify"))?;
        ensure(
            trust.verify(&a.id, b"other", &sig).is_err(),
            "wrong payload verified",
        )?;
        let mut revoked = trust.clone();
        ensure(revoked.unpin(&a.id), "unpin reported no pin")?;
        assert_eq!(
            revoked.verify(&a.id, b"payload", &sig),
            Err(VerifyError::UnknownPeer(a.id.clone()))
        );
        Ok(())
    }
}

impl From<crate::error::PinError> for TestError {
    fn from(err: crate::error::PinError) -> Self {
        TestError::Unexpected(err.to_string())
    }
}

// --- TLS channel ---------------------------------------------------------------

mod tls_channel {
    use super::*;

    #[tokio::test]
    async fn test_tls_uses_tls13_hybrid_pq_group_and_equal_exporters() -> TestResult {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(ctx("bind"))?;
        let addr = listener.local_addr().map_err(ctx("addr"))?;
        let acceptor = tokio_rustls::TlsAcceptor::from(crate::tls::server_config()?);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await?;
            let stream = acceptor.accept(tcp).await?;
            let binding = crate::tls::server_channel_binding(stream.get_ref().1)?;
            Ok::<_, TransportError>(binding)
        });
        let tcp = tokio::net::TcpStream::connect(addr)
            .await
            .map_err(ctx("connect"))?;
        let connector = tokio_rustls::TlsConnector::from(crate::tls::client_config()?);
        let stream = connector
            .connect(crate::tls::server_name()?, tcp)
            .await
            .map_err(ctx("tls connect"))?;
        let conn = stream.get_ref().1;
        assert_eq!(
            conn.protocol_version(),
            Some(rustls::ProtocolVersion::TLSv1_3)
        );
        assert_eq!(
            conn.negotiated_key_exchange_group()
                .map(|group| group.name()),
            Some(rustls::NamedGroup::X25519MLKEM768)
        );
        let client_binding = crate::tls::client_channel_binding(conn)?;
        let server_binding = server.await.map_err(ctx("join"))??;
        assert_eq!(client_binding, server_binding);
        Ok(())
    }
}

// --- loopback transport --------------------------------------------------------

async fn spawn_server(
    local: crate::LocalNode,
    trust: PinnedPeers,
    service: TestService,
) -> TestResult<(SocketAddr, JoinHandle<Result<(), TransportError>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(ctx("bind"))?;
    let addr = listener.local_addr().map_err(ctx("local addr"))?;
    let server = NodeTransportServer::new(local, Arc::new(trust), ServerOptions::default())?;
    let handle = tokio::spawn(async move { server.serve(listener, service).await });
    Ok((addr, handle))
}

fn whoami_service() -> TestService {
    TestService::new(|request: Request<hyper::body::Incoming>| async move {
        match request.extensions().get::<AuthenticatedPeer>() {
            Some(peer) => text_response(StatusCode::OK, peer.node_id.as_str()),
            None => text_response(StatusCode::INTERNAL_SERVER_ERROR, "no peer extension"),
        }
    })
}

fn is_rejection(err: &TransportError) -> bool {
    matches!(
        err,
        TransportError::Handshake(HandshakeError::RejectedByPeer) | TransportError::Io(_)
    )
}

mod loopback {
    use super::*;

    #[tokio::test]
    async fn test_nodes_authenticate_and_request_carries_peer_node_id() -> TestResult {
        let a = test_node("node-a", 1)?;
        let b = test_node("node-b", 2)?;
        let (addr, server) =
            spawn_server(a.local.clone(), pinned(&[&b.identity])?, whoami_service()).await?;

        let client_trust = pinned(&[&a.identity])?;
        let mut client = NodeTransportClient::connect(addr, &b.local, &a.id, &client_trust).await?;
        assert_eq!(client.peer().node_id, a.id);

        for _ in 0..2 {
            let request = Request::builder()
                .method(Method::GET)
                .uri("/whoami")
                .body(empty_body())
                .map_err(ctx("request"))?;
            let response = client.send_request(request).await?;
            assert_eq!(response.status(), StatusCode::OK);
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(ctx("body"))?
                .to_bytes();
            assert_eq!(body.as_ref(), b"node-b");
        }
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_wrong_pinned_server_key_is_rejected_by_client() -> TestResult {
        let a = test_node("node-a", 1)?;
        let b = test_node("node-b", 2)?;
        let impostor_key = test_node("node-a", 9)?; // same name, other key
        let (addr, server) =
            spawn_server(a.local.clone(), pinned(&[&b.identity])?, whoami_service()).await?;

        let wrong_trust = pinned(&[&impostor_key.identity])?;
        let verdict = NodeTransportClient::connect(addr, &b.local, &a.id, &wrong_trust).await;
        ensure(
            matches!(
                &verdict,
                Err(TransportError::Handshake(HandshakeError::Verify(VerifyError::BadSignature(id)))) if *id == a.id
            ),
            format!("{:?}", verdict.map(|c| c.peer().clone())),
        )?;
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_wrong_pinned_client_key_is_rejected_by_server() -> TestResult {
        let a = test_node("node-a", 1)?;
        let b = test_node("node-b", 2)?;
        let impostor_b = test_node("node-b", 8)?;
        let (addr, server) = spawn_server(
            a.local.clone(),
            pinned(&[&impostor_b.identity])?,
            whoami_service(),
        )
        .await?;

        let verdict =
            NodeTransportClient::connect(addr, &b.local, &a.id, &pinned(&[&a.identity])?).await;
        match verdict {
            Err(err) => ensure(is_rejection(&err), format!("unexpected error {err}"))?,
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "server accepted a wrong client key".into(),
                ));
            }
        }
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_unpinned_client_and_misaddressed_hello_are_rejected() -> TestResult {
        let a = test_node("node-a", 1)?;
        let b = test_node("node-b", 2)?;
        let (addr, server) =
            spawn_server(a.local.clone(), PinnedPeers::new(), whoami_service()).await?;
        let trust = pinned(&[&a.identity])?;
        match NodeTransportClient::connect(addr, &b.local, &a.id, &trust).await {
            Err(err) => ensure(is_rejection(&err), format!("unexpected error {err}"))?,
            Ok(_) => return Err(TestError::Unexpected("unpinned client accepted".into())),
        }
        server.abort();

        // Dialling an address while expecting another node: the server
        // refuses a hello addressed to someone else (IP is not identity).
        let (addr, server) =
            spawn_server(a.local.clone(), pinned(&[&b.identity])?, whoami_service()).await?;
        let other = test_node("node-c", 3)?;
        let trust = pinned(&[&a.identity, &other.identity])?;
        match NodeTransportClient::connect(addr, &b.local, &other.id, &trust).await {
            Err(err) => ensure(is_rejection(&err), format!("unexpected error {err}"))?,
            Ok(_) => return Err(TestError::Unexpected("misaddressed hello accepted".into())),
        }
        server.abort();
        Ok(())
    }
}

// --- uplink -------------------------------------------------------------------

fn finding(summary: &str) -> UplinkEvent {
    UplinkEvent::FindingSummary {
        finding_id: "f-1".into(),
        rule_id: "dod.proc.unexpected_exec".into(),
        severity: FindingSeverity::High,
        summary: summary.into(),
        observed_at_unix_ms: NOW,
    }
}

fn beat() -> UplinkEvent {
    UplinkEvent::HealthBeat {
        sequence: 7,
        status: HealthStatus::Degraded,
        observed_at_unix_ms: NOW + 1,
    }
}

/// In-memory body delivering fixed chunks.
struct ChunkedBody {
    chunks: VecDeque<Bytes>,
}

impl ChunkedBody {
    fn split(bytes: &[u8], chunk: usize) -> Self {
        Self {
            chunks: bytes
                .chunks(chunk.max(1))
                .map(Bytes::copy_from_slice)
                .collect(),
        }
    }
}

impl Body for ChunkedBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Ready(self.chunks.pop_front().map(|chunk| Ok(Frame::data(chunk))))
    }
}

fn lines(events: &[UplinkEvent]) -> TestResult<Vec<u8>> {
    let mut out = Vec::new();
    for event in events {
        out.extend_from_slice(&encode_line(event, usize::MAX)?);
    }
    Ok(out)
}

mod uplink_unit {
    use super::*;

    #[tokio::test]
    async fn test_receiver_reassembles_lines_across_chunks() -> TestResult {
        let events = vec![finding("exec of /tmp/x"), beat()];
        let raw = lines(&events)?;
        for chunk in [1, 7, raw.len()] {
            let mut got = Vec::new();
            let stats = receive_uplink(
                ChunkedBody::split(&raw, chunk),
                &UplinkLimits::default(),
                |e| {
                    got.push(e);
                },
            )
            .await?;
            assert_eq!(got, events);
            assert_eq!(stats.events, 2);
            assert_eq!(stats.bytes, u64::try_from(raw.len()).map_err(ctx("len"))?);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_receiver_enforces_line_limit_even_without_newline() -> TestResult {
        let limits = UplinkLimits {
            max_line_bytes: 64,
            ..UplinkLimits::default()
        };
        let raw = lines(&[finding(&"x".repeat(200))])?;
        let mut got = 0;
        let verdict = receive_uplink(ChunkedBody::split(&raw, 16), &limits, |_| got += 1).await;
        ensure(
            matches!(verdict, Err(UplinkError::LineTooLong { .. })),
            format!("{verdict:?}"),
        )?;
        assert_eq!(got, 0);

        let endless = vec![b'{'; 10_000];
        let verdict = receive_uplink(ChunkedBody::split(&endless, 10_000), &limits, |_| {}).await;
        match verdict {
            Err(UplinkError::LineTooLong { actual, limit }) => {
                ensure(actual <= 2 * limit, format!("buffered {actual} bytes"))?;
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_receiver_enforces_event_and_stream_limits() -> TestResult {
        let raw = lines(&[beat(), beat(), beat()])?;
        let limits = UplinkLimits {
            max_events: 2,
            ..UplinkLimits::default()
        };
        let mut got = 0;
        let verdict =
            receive_uplink(ChunkedBody::split(&raw, raw.len()), &limits, |_| got += 1).await;
        assert_eq!(verdict, Err(UplinkError::TooManyEvents { limit: 2 }));
        assert_eq!(got, 2);

        let limits = UplinkLimits {
            max_stream_bytes: 10,
            ..UplinkLimits::default()
        };
        let verdict = receive_uplink(ChunkedBody::split(&raw, 8), &limits, |_| {}).await;
        assert_eq!(verdict, Err(UplinkError::StreamTooLarge { limit: 10 }));
        Ok(())
    }

    #[tokio::test]
    async fn test_receiver_rejects_malformed_unknown_and_truncated_lines() -> TestResult {
        let limits = UplinkLimits::default();
        for (raw, what) in [
            (&b"{\"type\":\"command\",\"run\":\"rm\"}\n"[..], "unknown variant"),
            (
                &b"{\"type\":\"health_beat\",\"sequence\":1,\"status\":\"ok\",\"observed_at_unix_ms\":1,\"node_id\":\"spoof\"}\n"[..],
                "unknown field",
            ),
            (&b"\n"[..], "blank line"),
        ] {
            let verdict = receive_uplink(ChunkedBody::split(raw, raw.len()), &limits, |_| {}).await;
            ensure(
                matches!(verdict, Err(UplinkError::Malformed { line: 1, .. })),
                format!("{what}: {verdict:?}"),
            )?;
        }
        let mut raw = lines(&[beat()])?;
        raw.pop();
        let verdict = receive_uplink(ChunkedBody::split(&raw, raw.len()), &limits, |_| {}).await;
        assert_eq!(verdict, Err(UplinkError::TruncatedLine));
        Ok(())
    }

    #[test]
    fn test_sender_refuses_oversize_and_never_blocks() -> TestResult {
        let limits = UplinkLimits {
            max_line_bytes: 128,
            ..UplinkLimits::default()
        };
        let (sender, body) = uplink_channel(1, &limits);
        ensure(
            matches!(
                sender.publish(&finding(&"y".repeat(500))),
                Err(UplinkError::LineTooLong { .. })
            ),
            "oversize event queued",
        )?;
        sender.publish(&beat())?;
        assert_eq!(sender.publish(&beat()), Err(UplinkError::Backpressure));
        drop(body);
        assert_eq!(sender.publish(&beat()), Err(UplinkError::Closed));
        assert_eq!(
            UplinkError::Backpressure.http_status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            UplinkError::TooManyEvents { limit: 1 }.http_status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        Ok(())
    }
}

type Received = Arc<Mutex<Vec<(NodeId, UplinkEvent)>>>;

fn uplink_service(limits: UplinkLimits, received: Received) -> TestService {
    TestService::new(move |request: Request<hyper::body::Incoming>| {
        let received = Arc::clone(&received);
        async move {
            if request.method() != Method::POST || request.uri().path() != UPLINK_PATH {
                return text_response(StatusCode::NOT_FOUND, "");
            }
            let Some(peer) = request.extensions().get::<AuthenticatedPeer>().cloned() else {
                return text_response(StatusCode::INTERNAL_SERVER_ERROR, "no peer");
            };
            let mut batch = Vec::new();
            let verdict = receive_uplink(request.into_body(), &limits, |event| {
                batch.push((peer.node_id.clone(), event));
            })
            .await;
            if let Ok(mut store) = received.lock() {
                store.extend(batch);
            }
            match verdict {
                Ok(stats) => text_response(StatusCode::OK, stats.events.to_string()),
                Err(err) => text_response(err.http_status(), err.to_string()),
            }
        }
    })
}

mod uplink_loopback {
    use super::*;

    #[tokio::test]
    async fn test_uplink_stream_round_trip_attributes_events_to_peer() -> TestResult {
        let hub = test_node("security-hub", 1)?;
        let node = test_node("harw-node-03", 2)?;
        let received: Received = Arc::default();
        let (addr, server) = spawn_server(
            hub.local.clone(),
            pinned(&[&node.identity])?,
            uplink_service(UplinkLimits::default(), Arc::clone(&received)),
        )
        .await?;

        let mut client =
            NodeTransportClient::connect(addr, &node.local, &hub.id, &pinned(&[&hub.identity])?)
                .await?;
        let (sender, body) = uplink_channel(8, &UplinkLimits::default());
        let request = uplink_request(body)?;
        let events = vec![finding("exec of /tmp/x"), beat()];
        let publish = {
            let events = events.clone();
            async move {
                for event in &events {
                    sender.publish(event)?;
                    tokio::task::yield_now().await;
                }
                drop(sender); // ends the stream
                Ok::<(), UplinkError>(())
            }
        };
        let (response, published) = tokio::join!(client.send_request(request), publish);
        published?;
        let response = response?;
        assert_eq!(response.status(), StatusCode::OK);

        let store = received.lock().map_err(ctx("lock"))?.clone();
        let expected: Vec<(NodeId, UplinkEvent)> =
            events.into_iter().map(|e| (node.id.clone(), e)).collect();
        assert_eq!(store, expected);
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_uplink_receiver_rejects_oversize_event_with_413() -> TestResult {
        let hub = test_node("security-hub", 1)?;
        let node = test_node("harw-node-03", 2)?;
        let received: Received = Arc::default();
        let server_limits = UplinkLimits {
            max_line_bytes: 256,
            ..UplinkLimits::default()
        };
        let (addr, server) = spawn_server(
            hub.local.clone(),
            pinned(&[&node.identity])?,
            uplink_service(server_limits, Arc::clone(&received)),
        )
        .await?;
        let mut client =
            NodeTransportClient::connect(addr, &node.local, &hub.id, &pinned(&[&hub.identity])?)
                .await?;
        // The sender allows larger lines than this receiver: the receiver's
        // own limit must hold.
        let (sender, body) = uplink_channel(4, &UplinkLimits::default());
        sender.publish(&finding(&"z".repeat(2_000)))?;
        drop(sender);
        let response = client.send_request(uplink_request(body)?).await?;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        ensure(
            received.lock().map_err(ctx("lock"))?.is_empty(),
            "oversize event delivered",
        )?;
        server.abort();
        Ok(())
    }
}

// --- HTTP upgrade over the authenticated channel (PL-68 W02) ---------------------

mod upgrade {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Upgrades `/echo` to a raw stream: first line is the authenticated peer
    /// node id, then everything the client sends is echoed. Reports when the
    /// upgraded stream ends.
    fn echo_service(ended: tokio::sync::mpsc::UnboundedSender<()>) -> TestService {
        TestService::new(move |mut request: Request<hyper::body::Incoming>| {
            let ended = ended.clone();
            async move {
                let Some(peer) = request.extensions().get::<AuthenticatedPeer>().cloned() else {
                    return text_response(StatusCode::INTERNAL_SERVER_ERROR, "no peer extension");
                };
                let on_upgrade = hyper::upgrade::on(&mut request);
                tokio::spawn(async move {
                    if let Ok(upgraded) = on_upgrade.await {
                        let mut io = hyper_util::rt::TokioIo::new(upgraded);
                        let hello = format!("peer={}\n", peer.node_id.as_str());
                        if io.write_all(hello.as_bytes()).await.is_ok() {
                            let mut buf = [0u8; 64];
                            while let Ok(n) = io.read(&mut buf).await {
                                if n == 0 || io.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    let _ = ended.send(());
                });
                let mut response = text_response(StatusCode::SWITCHING_PROTOCOLS, "");
                response.headers_mut().insert(
                    http::header::CONNECTION,
                    http::HeaderValue::from_static("upgrade"),
                );
                response.headers_mut().insert(
                    http::header::UPGRADE,
                    http::HeaderValue::from_static("echo"),
                );
                response
            }
        })
    }

    fn upgrade_request() -> TestResult<Request<crate::NodeBody>> {
        Request::builder()
            .method(Method::GET)
            .uri("/echo")
            .header(http::header::CONNECTION, "upgrade")
            .header(http::header::UPGRADE, "echo")
            .body(empty_body())
            .map_err(ctx("request"))
    }

    #[tokio::test]
    async fn an_upgrade_carries_the_authenticated_peer_and_ends_with_the_connection() -> TestResult
    {
        let a = test_node("node-a", 1)?;
        let b = test_node("node-b", 2)?;
        let (ended_tx, mut ended_rx) = tokio::sync::mpsc::unbounded_channel();
        let (addr, server) = spawn_server(
            a.local.clone(),
            pinned(&[&b.identity])?,
            echo_service(ended_tx),
        )
        .await?;
        let client_trust = pinned(&[&a.identity])?;
        let mut client = NodeTransportClient::connect(addr, &b.local, &a.id, &client_trust).await?;

        let response = client.send_request(upgrade_request()?).await?;
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        let upgraded = hyper::upgrade::on(response).await.map_err(ctx("upgrade"))?;
        let mut io = hyper_util::rt::TokioIo::new(upgraded);

        let exchange = async {
            let mut first = [0u8; 12];
            io.read_exact(&mut first).await?;
            assert_eq!(&first, b"peer=node-b\n");
            io.write_all(b"ping").await?;
            let mut echoed = [0u8; 4];
            io.read_exact(&mut echoed).await?;
            assert_eq!(&echoed, b"ping");
            Ok::<_, std::io::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(10), exchange)
            .await
            .map_err(ctx("upgrade exchange timed out"))?
            .map_err(ctx("upgrade exchange"))?;

        // Dropping the client tears the authenticated connection down; the
        // upgraded stream must end with it.
        drop(io);
        drop(client);
        let ended = tokio::time::timeout(Duration::from_secs(5), ended_rx.recv()).await;
        assert!(
            matches!(ended, Ok(Some(()))),
            "upgraded stream ended with the connection"
        );
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn requests_without_an_upgrade_still_work_on_the_same_server() -> TestResult {
        let a = test_node("node-a", 1)?;
        let b = test_node("node-b", 2)?;
        let (addr, server) =
            spawn_server(a.local.clone(), pinned(&[&b.identity])?, whoami_service()).await?;
        let client_trust = pinned(&[&a.identity])?;
        let mut client = NodeTransportClient::connect(addr, &b.local, &a.id, &client_trust).await?;
        let request = Request::builder()
            .method(Method::GET)
            .uri("/whoami")
            .body(empty_body())
            .map_err(ctx("request"))?;
        let response = client.send_request(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        server.abort();
        Ok(())
    }
}
