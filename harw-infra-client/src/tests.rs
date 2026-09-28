//! End-to-end client tests against the in-test hub ([`crate::test_server`]).

use std::time::Duration;

use zeroize::Zeroizing;

use crate::test_server::{MockHub, TEST_TOKEN};
use crate::test_support::{TestError, TestResult};
use crate::{
    AuthHubClient, BearerToken, ClientOptions, HealthState, InfraClientError, KeyContext,
    KeyProfile, KeyRef, KeyState, NetworkControlClient, PublicKeyBytes, RemoteErrorKind,
    SecurityHubClient, WrappedKey,
};

fn client(hub: &MockHub) -> AuthHubClient {
    AuthHubClient::new(&hub.socket, None, ClientOptions::default())
}

fn token(bytes: &[u8]) -> TestResult<BearerToken> {
    Ok(BearerToken::from_secret(Zeroizing::new(bytes.to_vec()))?)
}

fn expect_err<T: std::fmt::Debug>(
    result: Result<T, InfraClientError>,
    expected: InfraClientError,
) -> TestResult {
    match result {
        Err(err) if err == expected => Ok(()),
        other => Err(TestError::Unexpected(format!(
            "expected Err({expected:?}), got {other:?}"
        ))),
    }
}

#[tokio::test]
async fn test_health_version_capabilities() -> TestResult {
    let hub = MockHub::start().await?;
    let auth = client(&hub);

    let health = auth.health().await?;
    assert_eq!(health.state, HealthState::Ok);
    assert_eq!(health.service.as_deref(), Some("mock-hub"));

    let version = auth.version().await?;
    assert_eq!(version.service, "mock-hub");
    assert_eq!(version.version, "0.3.0");
    assert_eq!(version.protocol, 1);

    // Unknown fields are tolerated; the document is descriptive.
    let capabilities = auth.capabilities().await?;
    assert_eq!(
        capabilities.crypto_profiles,
        vec!["harw-strong-v1".to_owned()]
    );
    assert!(capabilities.lists_operation("key.rotate"));
    assert!(!capabilities.lists_operation("key.export"));
    Ok(())
}

#[tokio::test]
async fn test_network_and_security_skeletons_speak_the_common_surface() -> TestResult {
    let hub = MockHub::start().await?;
    let network = NetworkControlClient::new(&hub.socket, ClientOptions::default());
    let security = SecurityHubClient::new(&hub.socket, ClientOptions::default());
    assert_eq!(network.health().await?.state, HealthState::Ok);
    assert_eq!(network.version().await?.protocol, 1);
    assert_eq!(security.health().await?.state, HealthState::Ok);
    assert_eq!(security.capabilities().await?.service, "mock-hub");
    Ok(())
}

#[tokio::test]
async fn test_key_lifecycle_calls_use_cgk1_routes() -> TestResult {
    let hub = MockHub::start().await?;
    let auth = client(&hub);

    let created = auth
        .generate_key(&KeyRef::latest("app", "k1")?, KeyProfile::PqHpkeDefault)
        .await?;
    assert_eq!(created.key, KeyRef::versioned("app", "k1", 1)?);
    assert_eq!(
        created.public,
        Some(PublicKeyBytes::new(b"pk:pq-hpke-default".to_vec()))
    );

    let description = auth.describe(&KeyRef::versioned("app", "k1", 3)?).await?;
    assert_eq!(description.key, KeyRef::versioned("app", "k1", 3)?);
    assert_eq!(description.state, KeyState::Enabled);
    assert_eq!(description.profile(), Some(KeyProfile::PqHpkeDefault));

    let public = auth.public_key(&KeyRef::latest("app", "k1")?).await?;
    assert_eq!(public.as_bytes(), b"pk:k1");

    let rotated = auth.rotate(&KeyRef::versioned("app", "k1", 1)?).await?;
    assert_eq!(rotated.key, KeyRef::versioned("app", "k1", 2)?);
    assert_eq!(rotated.public, None);
    Ok(())
}

#[tokio::test]
async fn test_wrap_unwrap_rewrap_roundtrip() -> TestResult {
    let hub = MockHub::start().await?;
    let auth = client(&hub);
    let key = KeyRef::latest("secrets", "kek")?;
    let context = KeyContext::new(b"harw:dek:v1".to_vec(), b"record-7".to_vec());

    let wrapped = auth
        .wrap_key(
            &key,
            &context,
            Zeroizing::new(b"data-encryption-key".to_vec()),
        )
        .await?;
    assert_eq!(wrapped.as_bytes(), b"Wyek-noitpyrcne-atad");

    let plaintext: Zeroizing<Vec<u8>> = auth.unwrap_key(&key, &context, &wrapped).await?;
    assert_eq!(plaintext.as_slice(), b"data-encryption-key");

    let to = KeyRef::versioned("secrets", "kek2", 4)?;
    let rewrapped = auth
        .rewrap_key(&key, &context, &to, &KeyContext::default(), &wrapped)
        .await?;
    assert_eq!(
        rewrapped.as_bytes(),
        b"R:secrets/kek2@4:Wyek-noitpyrcne-atad"
    );

    // A blob the hub cannot open is `422` → AuthenticationFailed.
    expect_err(
        auth.unwrap_key(&key, &context, &WrappedKey::new(b"tampered".to_vec()))
            .await,
        InfraClientError::Remote(RemoteErrorKind::AuthenticationFailed),
    )
}

#[tokio::test]
async fn test_status_codes_map_to_typed_errors() -> TestResult {
    let hub = MockHub::start().await?;
    let auth = client(&hub);
    let cases = [
        ("401", InfraClientError::Unauthenticated),
        ("403", InfraClientError::Forbidden),
        ("404", InfraClientError::NotFound),
        ("503", InfraClientError::Unavailable),
        ("409", InfraClientError::Remote(RemoteErrorKind::Conflict)),
        (
            "418",
            InfraClientError::Remote(RemoteErrorKind::Status(418)),
        ),
    ];
    for (code, expected) in cases {
        let key = KeyRef::latest("status", code)?;
        expect_err(auth.describe(&key).await, expected)?;
    }
    Ok(())
}

#[tokio::test]
async fn test_timeout_is_reported_as_timeout() -> TestResult {
    let hub = MockHub::start().await?;
    let options = ClientOptions {
        timeout: Duration::from_millis(100),
        ..ClientOptions::default()
    };
    let auth = AuthHubClient::new(&hub.socket, None, options);
    expect_err(
        auth.describe(&KeyRef::latest("slow", "k")?).await,
        InfraClientError::Timeout,
    )
}

#[tokio::test]
async fn test_missing_socket_is_unavailable() -> TestResult {
    let hub = MockHub::start().await?;
    let auth = AuthHubClient::new(hub.missing_socket(), None, ClientOptions::default());
    expect_err(auth.health().await, InfraClientError::Unavailable)?;

    // A path that exists but is not a socket is unavailable, too.
    let not_a_socket = hub.missing_socket().with_file_name("plain-file");
    std::fs::write(&not_a_socket, b"x")?;
    let auth = AuthHubClient::new(not_a_socket, None, ClientOptions::default());
    expect_err(auth.health().await, InfraClientError::Unavailable)
}

#[tokio::test]
async fn test_malformed_frame_and_oversized_body_are_protocol_errors() -> TestResult {
    let hub = MockHub::start().await?;
    let auth = client(&hub);
    match auth.describe(&KeyRef::latest("garbage", "k")?).await {
        Err(InfraClientError::Protocol(_)) => {}
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }

    let small = ClientOptions {
        max_response_bytes: 1024,
        ..ClientOptions::default()
    };
    let limited = AuthHubClient::new(&hub.socket, None, small);
    match limited.public_key(&KeyRef::latest("big", "k")?).await {
        Err(InfraClientError::Protocol(_)) => {}
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }

    // Wrong content type: a CGK1 answer where octets are expected.
    match auth.public_key(&KeyRef::latest("garbage", "k")?).await {
        Err(InfraClientError::Protocol(reason)) => {
            assert_eq!(reason, "unexpected response content type");
            Ok(())
        }
        other => Err(TestError::Unexpected(format!("{other:?}"))),
    }
}

#[tokio::test]
async fn test_bearer_token_is_sent_and_checked() -> TestResult {
    let hub = MockHub::start().await?;
    let key = KeyRef::latest("private", "k")?;

    let anonymous = client(&hub);
    expect_err(
        anonymous.describe(&key).await,
        InfraClientError::Unauthenticated,
    )?;

    let authed = AuthHubClient::new(
        &hub.socket,
        Some(token(TEST_TOKEN)?),
        ClientOptions::default(),
    );
    assert_eq!(authed.describe(&key).await?.state, KeyState::Enabled);

    let wrong = AuthHubClient::new(
        &hub.socket,
        Some(token(b"wrong-token")?),
        ClientOptions::default(),
    );
    expect_err(wrong.health().await, InfraClientError::Unauthenticated)
}

#[tokio::test]
async fn test_token_never_appears_in_debug() -> TestResult {
    let auth = AuthHubClient::new(
        "/run/harw/infra/secure.sock",
        Some(token(b"very-secret-bearer")?),
        ClientOptions::default(),
    );
    let debug = format!("{auth:?}");
    assert!(!debug.contains("very-secret-bearer"), "{debug}");
    assert!(debug.contains("REDACTED"), "{debug}");
    assert!(debug.contains("secure.sock"), "{debug}");
    Ok(())
}

fn assert_network_handle<T: Clone + Send + Sync + 'static>() {}

#[tokio::test]
async fn test_clone_is_a_shared_immutable_network_handle() -> TestResult {
    assert_network_handle::<AuthHubClient>();
    assert_network_handle::<NetworkControlClient>();
    assert_network_handle::<SecurityHubClient>();

    let hub = MockHub::start().await?;
    let options = ClientOptions {
        timeout: Duration::from_secs(3),
        max_response_bytes: 4096,
    };
    let original = AuthHubClient::new(&hub.socket, Some(token(TEST_TOKEN)?), options);
    let clone = original.clone();

    // Same `Arc`, same immutable parameters: nothing to diverge.
    assert!(clone.shares_transport_with(&original));
    assert_eq!(clone.socket_path(), original.socket_path());
    assert_eq!(clone.options(), options);

    // Each call opens its own connection, so clones work concurrently.
    let key = KeyRef::latest("private", "k")?;
    let (a, b) = tokio::join!(original.describe(&key), clone.describe(&key));
    assert_eq!(a?.key, b?.key);

    // A new client with equal parameters is a different handle.
    let other = AuthHubClient::new(&hub.socket, None, options);
    assert!(!other.shares_transport_with(&original));
    Ok(())
}
