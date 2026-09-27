//! [`AuthHubDekWrapper`]: the AuthHub KMS as a `harw-secrets`
//! [`DekWrapper`] for V3 (`KmsWrappedV3`) secret records
//! (Crypto-Masterplan v2 §16.2–§16.4, work package H6).
//!
//! The per-secret DEK is wrapped and unwrapped by the hub under one pinned
//! key version; the Harw process never holds the long-term KEK.
//!
//! # Key reference and generation
//! The wrapper serves exactly one Harwness key generation (0-based). The hub
//! key is addressed as `namespace/id@version` with the CryptGuard version
//! `generation + 1` ([`crypt_guard_key_version`]). [`DekWrapper::key_id`] is
//! the unversioned `namespace/id` that `harw-secrets` persists and binds into
//! the record AAD. Unwrapping any other generation is refused with
//! [`SecretsError::KeyGenerationMismatch`] before the hub is contacted; the
//! wrapper never falls back to another key version.
//!
//! # Sync/async bridge
//! [`DekWrapper`] is synchronous while [`AuthHubClient`] is async. The
//! wrapper must work both from plain threads and from code that already runs
//! inside a tokio runtime (where `Handle::block_on` / `Runtime::block_on`
//! would panic with "Cannot start a runtime from within a runtime"). Every
//! call therefore runs on a **dedicated, short-lived OS thread** that owns its
//! own current-thread tokio runtime (`std::thread::scope` +
//! `Builder::new_current_thread().enable_all()`); the calling thread blocks
//! on the join and receives the result. No runtime state outlives the call.
//!
//! Cost per wrap/unwrap: one thread spawn, one current-thread runtime build
//! and one Unix-socket connection (the client never pools), i.e. well under a
//! millisecond of overhead on top of the hub round trip. That is fine because
//! `SecretStore` create/get/migrate calls are infrequent (configuration
//! load, credential resolution), never per request on a hot path. When the
//! calling thread is a tokio worker it is blocked for the duration of the
//! call (bounded by [`crate::ClientOptions::timeout`]); async callers that
//! care should wrap the `SecretStore` call in `spawn_blocking`.
//!
//! # Error mapping
//! | Client error | Operation | `SecretsError` |
//! |---|---|---|
//! | `Remote(AuthenticationFailed)` (422) | unwrap | `Open(pq_hpke::Error::AuthenticationFailed)` (same as the local wrapper) |
//! | `Unavailable`, `Timeout` | any | `DekWrapperUnavailable` |
//! | anything else (401/403/404/409/5xx, protocol, 422 on wrap) | any | `DekWrapperUnavailable` with the client error's payload-free text |
//! | bridge failure (thread spawn, runtime build, worker panic) | any | `DekWrapperUnavailable` |
//!
//! Reasons are built only from [`InfraClientError`]'s `Display` (static,
//! payload-free) and fixed strings: no DEK, wrapped bytes or AAD ever enter
//! an error.

use core::fmt;
use std::future::Future;

use harw_secrets::dek_wrapper::{
    DekWrapper, Zeroizing, crypt_guard_key_version, validate_wrapper_identity,
};
use harw_secrets::{SecretsError, SecretsResult};

use crate::auth::AuthHubClient;
use crate::error::{InfraClientError, RemoteErrorKind};
use crate::key::{KeyContext, KeyRef, WrappedKey};

/// Crypto profile id of [`AuthHubDekWrapper`], persisted as
/// `crypto_profile_id` of V3 records.
pub const AUTHHUB_DEK_PROFILE_ID: &str = "authhub-cgk1-v1";

/// HPKE `info` the hub binds into every V3 DEK wrap. Distinct from the local
/// wrapper's info, so a locally wrapped DEK is never accepted as a hub one.
pub const AUTHHUB_DEK_WRAP_INFO: &[u8] = b"harwness:secrets:dek-wrap:v3:authhub";

/// Name of the per-call bridge thread.
const BRIDGE_THREAD_NAME: &str = "harw-authhub-dek";

/// [`DekWrapper`] backed by the AuthHub KMS (`wrap_key` / `unwrap_key` on
/// one pinned key version). See the module docs for the sync/async bridge.
pub struct AuthHubDekWrapper {
    client: AuthHubClient,
    key_ref: KeyRef,
    key_id: String,
    generation: u32,
}

impl AuthHubDekWrapper {
    /// A wrapper for hub key `key` (its namespace and id) at Harwness key
    /// `generation` (0-based); calls go to key version `generation + 1`.
    ///
    /// `key` may be unversioned or already pinned to exactly
    /// `generation + 1`.
    ///
    /// # Errors
    /// [`SecretsError::InvalidDekWrapperIdentity`] if `generation` has no
    /// CryptGuard version (`u32::MAX`), `key` pins another version, or the
    /// identity fails [`validate_wrapper_identity`].
    pub fn new(client: AuthHubClient, key: &KeyRef, generation: u32) -> SecretsResult<Self> {
        let version = crypt_guard_key_version(generation)
            .ok_or_else(|| invalid_identity("key generation has no CryptGuard key version"))?;
        if key.version().is_some_and(|pinned| pinned != version.get()) {
            return Err(invalid_identity(
                "key reference pins a version other than generation + 1",
            ));
        }
        let key_id = format!("{}/{}", key.namespace(), key.id());
        validate_wrapper_identity(&key_id, AUTHHUB_DEK_PROFILE_ID)?;
        let key_ref = key
            .clone()
            .with_version(version.get())
            .map_err(|_| invalid_identity("key version is invalid"))?;
        Ok(Self {
            client,
            key_ref,
            key_id,
            generation,
        })
    }

    /// The hub key this wrapper calls, pinned to `generation + 1`.
    pub fn key_ref(&self) -> &KeyRef {
        &self.key_ref
    }

    fn context(aad: &[u8]) -> KeyContext {
        KeyContext::new(AUTHHUB_DEK_WRAP_INFO, aad)
    }
}

impl fmt::Debug for AuthHubDekWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthHubDekWrapper")
            .field("client", &self.client)
            .field("key_ref", &self.key_ref)
            .field("generation", &self.generation)
            .finish()
    }
}

impl DekWrapper for AuthHubDekWrapper {
    fn profile_id(&self) -> &str {
        AUTHHUB_DEK_PROFILE_ID
    }

    fn key_id(&self) -> &str {
        &self.key_id
    }

    fn key_generation(&self) -> u32 {
        self.generation
    }

    fn wrap_dek(&self, dek: &[u8], aad: &[u8]) -> SecretsResult<Vec<u8>> {
        let context = Self::context(aad);
        let material = Zeroizing::new(dek.to_vec());
        let client = &self.client;
        let key = &self.key_ref;
        run_on_bridge_thread(move || async move { client.wrap_key(key, &context, material).await })
            .map(WrappedKey::into_vec)
            .map_err(|failure| map_failure(Operation::Wrap, failure))
    }

    fn unwrap_dek(
        &self,
        wrapped: &[u8],
        key_generation: u32,
        aad: &[u8],
    ) -> SecretsResult<Zeroizing<Vec<u8>>> {
        if key_generation != self.generation {
            return Err(SecretsError::KeyGenerationMismatch {
                key_id: self.key_id.clone(),
                expected: self.generation,
                found: key_generation,
            });
        }
        let context = Self::context(aad);
        let wrapped = WrappedKey::new(wrapped.to_vec());
        let client = &self.client;
        let key = &self.key_ref;
        run_on_bridge_thread(
            move || async move { client.unwrap_key(key, &context, &wrapped).await },
        )
        .map_err(|failure| map_failure(Operation::Unwrap, failure))
    }
}

/// Which hub operation failed (selects the 422 mapping).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Wrap,
    Unwrap,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Wrap => "wrap",
            Self::Unwrap => "unwrap",
        }
    }
}

/// Why a bridged call failed.
#[derive(Debug, PartialEq, Eq)]
enum BridgeFailure {
    /// The hub call itself failed.
    Client(InfraClientError),
    /// The bridge could not run the call (static, payload-free reason).
    Bridge(&'static str),
}

/// Run `make_call()` to completion on a fresh OS thread with its own
/// current-thread tokio runtime and return its result.
///
/// Safe from any context: a plain thread or a tokio worker (the future never
/// runs on the caller's runtime, so there is no nested `block_on`). The
/// scoped thread lets the future borrow the caller's data; it is always
/// joined before this returns.
fn run_on_bridge_thread<T, F, Fut>(make_call: F) -> Result<T, BridgeFailure>
where
    T: Send,
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<T, InfraClientError>>,
{
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new()
            .name(BRIDGE_THREAD_NAME.to_owned())
            .spawn_scoped(scope, move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| BridgeFailure::Bridge("could not build the bridge runtime"))?;
                runtime.block_on(make_call()).map_err(BridgeFailure::Client)
            });
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or(Err(BridgeFailure::Bridge("bridge thread panicked"))),
            Err(_) => Err(BridgeFailure::Bridge("could not spawn the bridge thread")),
        }
    })
}

/// Map a bridged failure to the `harw-secrets` error (see the module-level
/// mapping table). No payload bytes enter the result.
fn map_failure(operation: Operation, failure: BridgeFailure) -> SecretsError {
    let reason = match failure {
        BridgeFailure::Client(InfraClientError::Remote(RemoteErrorKind::AuthenticationFailed))
            if operation == Operation::Unwrap =>
        {
            return SecretsError::Open(crypt_guard::pq_hpke::Error::AuthenticationFailed);
        }
        BridgeFailure::Client(InfraClientError::Unavailable) => "AuthHub is unreachable".to_owned(),
        BridgeFailure::Client(InfraClientError::Timeout) => "AuthHub call timed out".to_owned(),
        BridgeFailure::Client(other) => format!("AuthHub {} failed: {other}", operation.name()),
        BridgeFailure::Bridge(reason) => format!("AuthHub {} failed: {reason}", operation.name()),
    };
    SecretsError::DekWrapperUnavailable {
        profile: AUTHHUB_DEK_PROFILE_ID.to_owned(),
        reason,
    }
}

fn invalid_identity(reason: &str) -> SecretsError {
    SecretsError::InvalidDekWrapperIdentity {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use harw_secrets::{
        CryptoPolicy, KekProvenance, KeyVersion, SecretEnvelopeFormat, SecretStore,
    };
    use secrecy::{ExposeSecret, SecretBox};

    use super::*;
    use crate::ClientOptions;
    use crate::test_server::MockHub;
    use crate::test_support::{TestError, TestResult, ctx};

    /// The mock hub on its own thread and current-thread runtime, so tests
    /// can call the synchronous wrapper from a plain thread or from inside
    /// another runtime without starving the hub.
    struct HubThread {
        socket: PathBuf,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl HubThread {
        fn start() -> TestResult<Self> {
            let (ready_tx, ready_rx) = mpsc::channel::<Result<PathBuf, String>>();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = Arc::clone(&stop);
            let handle = std::thread::Builder::new()
                .name("mock-authhub".to_owned())
                .spawn(move || {
                    let runtime = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(err) => {
                            let _ = ready_tx.send(Err(err.to_string()));
                            return;
                        }
                    };
                    runtime.block_on(async move {
                        let hub = match MockHub::start().await {
                            Ok(hub) => hub,
                            Err(err) => {
                                let _ = ready_tx.send(Err(err.to_string()));
                                return;
                            }
                        };
                        let _ = ready_tx.send(Ok(hub.socket.clone()));
                        while !stop_flag.load(Ordering::Acquire) {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        drop(hub);
                    });
                })?;
            let socket = ready_rx
                .recv()
                .map_err(ctx("mock hub thread ended before it was ready"))?
                .map_err(TestError::Unexpected)?;
            Ok(Self {
                socket,
                stop,
                handle: Some(handle),
            })
        }

        fn client(&self) -> AuthHubClient {
            AuthHubClient::new(&self.socket, None, ClientOptions::default())
        }
    }

    impl Drop for HubThread {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn wrapper(
        client: AuthHubClient,
        namespace: &str,
        generation: u32,
    ) -> TestResult<AuthHubDekWrapper> {
        let key = KeyRef::latest(namespace, "dek-kek")?;
        AuthHubDekWrapper::new(client, &key, generation).map_err(secrets_err)
    }

    fn secrets_err(err: SecretsError) -> TestError {
        TestError::Unexpected(format!("secrets error: {err}"))
    }

    fn expect_unavailable<T>(result: SecretsResult<T>) -> TestResult {
        match result {
            Err(SecretsError::DekWrapperUnavailable { profile, .. })
                if profile == AUTHHUB_DEK_PROFILE_ID =>
            {
                Ok(())
            }
            Err(other) => Err(TestError::Unexpected(format!(
                "expected DekWrapperUnavailable, got {other}"
            ))),
            Ok(_) => Err(TestError::Unexpected(
                "expected DekWrapperUnavailable, got Ok".to_owned(),
            )),
        }
    }

    fn round_trip(wrapper: &AuthHubDekWrapper) -> TestResult {
        let dek = [0x42_u8; 32];
        let wrapped = wrapper.wrap_dek(&dek, b"record-aad").map_err(secrets_err)?;
        assert_ne!(wrapped.as_slice(), dek.as_slice());
        let unwrapped = wrapper
            .unwrap_dek(&wrapped, wrapper.key_generation(), b"record-aad")
            .map_err(secrets_err)?;
        assert_eq!(unwrapped.as_slice(), dek.as_slice());
        Ok(())
    }

    #[test]
    fn test_identity_pins_generation_plus_one() -> TestResult {
        let client = AuthHubClient::new("/nonexistent/hub.sock", None, ClientOptions::default());
        let wrapper = wrapper(client.clone(), "secrets", 4)?;
        assert_eq!(wrapper.profile_id(), AUTHHUB_DEK_PROFILE_ID);
        assert_eq!(wrapper.key_id(), "secrets/dek-kek");
        assert_eq!(wrapper.key_generation(), 4);
        assert_eq!(
            wrapper.key_ref(),
            &KeyRef::versioned("secrets", "dek-kek", 5)?
        );
        assert!(validate_wrapper_identity(wrapper.key_id(), wrapper.profile_id()).is_ok());

        // Already pinned to generation + 1 is accepted, anything else refused.
        let pinned = KeyRef::versioned("secrets", "dek-kek", 5)?;
        assert!(AuthHubDekWrapper::new(client.clone(), &pinned, 4).is_ok());
        let other = KeyRef::versioned("secrets", "dek-kek", 3)?;
        assert!(matches!(
            AuthHubDekWrapper::new(client.clone(), &other, 4),
            Err(SecretsError::InvalidDekWrapperIdentity { .. })
        ));
        assert!(matches!(
            AuthHubDekWrapper::new(client, &KeyRef::latest("secrets", "dek-kek")?, u32::MAX),
            Err(SecretsError::InvalidDekWrapperIdentity { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_round_trip_from_a_plain_thread() -> TestResult {
        let hub = HubThread::start()?;
        round_trip(&wrapper(hub.client(), "secrets", 0)?)
    }

    #[tokio::test]
    async fn test_round_trip_from_inside_a_tokio_runtime_does_not_nest_runtimes() -> TestResult {
        let hub = HubThread::start()?;
        // Synchronous call on a runtime worker: must not panic with
        // "Cannot start a runtime from within a runtime".
        round_trip(&wrapper(hub.client(), "secrets", 2)?)
    }

    #[test]
    fn test_foreign_generation_is_refused_before_the_hub_is_called() -> TestResult {
        // No hub at all: the typed refusal must come first.
        let client = AuthHubClient::new("/nonexistent/hub.sock", None, ClientOptions::default());
        let wrapper = wrapper(client, "secrets", 3)?;
        match wrapper.unwrap_dek(b"Wxyz", 2, b"aad") {
            Err(SecretsError::KeyGenerationMismatch {
                key_id,
                expected: 3,
                found: 2,
            }) if key_id == "secrets/dek-kek" => Ok(()),
            Err(other) => Err(TestError::Unexpected(format!(
                "expected KeyGenerationMismatch, got {other}"
            ))),
            Ok(_) => Err(TestError::Unexpected(
                "expected KeyGenerationMismatch, got Ok".to_owned(),
            )),
        }
    }

    #[test]
    fn test_hub_down_fails_closed_as_unavailable() -> TestResult {
        let dir = tempfile::tempdir()?;
        let client = AuthHubClient::new(
            dir.path().join("absent.sock"),
            None,
            ClientOptions::default(),
        );
        let wrapper = wrapper(client, "secrets", 0)?;
        expect_unavailable(wrapper.wrap_dek(&[0x42_u8; 32], b"aad"))?;
        expect_unavailable(wrapper.unwrap_dek(b"Wxyz", 0, b"aad"))
    }

    #[test]
    fn test_timeout_and_remote_errors_map_to_unavailable() -> TestResult {
        let hub = HubThread::start()?;
        let fast = ClientOptions {
            timeout: Duration::from_millis(200),
            ..ClientOptions::default()
        };
        let slow = wrapper(AuthHubClient::new(&hub.socket, None, fast), "slow", 0)?;
        expect_unavailable(slow.wrap_dek(&[0x42_u8; 32], b"aad"))?;

        // `status/<code>` answers <code>: 500 and a 422 on *wrap* stay
        // "unavailable".
        for code in ["500", "404", "422"] {
            let key = KeyRef::latest("status", code)?;
            let failing = AuthHubDekWrapper::new(hub.client(), &key, 0).map_err(secrets_err)?;
            expect_unavailable(failing.wrap_dek(&[0x42_u8; 32], b"aad"))?;
        }
        Ok(())
    }

    #[test]
    fn test_unwrap_authentication_failure_maps_to_open() -> TestResult {
        let hub = HubThread::start()?;
        let wrapper = wrapper(hub.client(), "secrets", 0)?;
        // The mock hub answers 422 for a blob it did not produce.
        match wrapper.unwrap_dek(b"not-a-hub-blob", 0, b"aad") {
            Err(SecretsError::Open(crypt_guard::pq_hpke::Error::AuthenticationFailed)) => Ok(()),
            Err(other) => Err(TestError::Unexpected(format!(
                "expected Open(AuthenticationFailed), got {other}"
            ))),
            Ok(_) => Err(TestError::Unexpected(
                "expected Open(AuthenticationFailed), got Ok".to_owned(),
            )),
        }
    }

    #[test]
    fn test_bridge_failure_reasons_carry_no_payload() {
        let err = map_failure(
            Operation::Wrap,
            BridgeFailure::Bridge("bridge thread panicked"),
        );
        assert_eq!(
            err.to_string(),
            "DEK wrapper 'authhub-cgk1-v1' unavailable: AuthHub wrap failed: bridge thread panicked"
        );
        let err = map_failure(
            Operation::Wrap,
            BridgeFailure::Client(InfraClientError::Remote(
                RemoteErrorKind::AuthenticationFailed,
            )),
        );
        assert!(matches!(err, SecretsError::DekWrapperUnavailable { .. }));
    }

    #[test]
    fn test_secret_store_v3_round_trip_through_the_hub() -> TestResult {
        let hub = HubThread::start()?;
        let root = tempfile::tempdir()?;
        let adapter: Arc<dyn DekWrapper> = Arc::new(wrapper(hub.client(), "secrets", 1)?);
        let provenance = KekProvenance::EnvSeed {
            var: "HARW_TEST_UNUSED_SEED".to_owned(),
        };
        let mut store = SecretStore::new(
            root.path().to_path_buf(),
            CryptoPolicy::strongest(),
            provenance,
            KeyVersion::initial(),
        )
        .with_dek_wrapper(adapter);

        let value = SecretBox::new(b"provider-token-value".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &value)
            .map_err(secrets_err)?;

        let record = store.record(&id).map_err(secrets_err)?;
        assert_eq!(record.envelope_format, SecretEnvelopeFormat::KmsWrappedV3);
        assert_eq!(record.key_id.as_deref(), Some("secrets/dek-kek"));
        assert_eq!(record.key_generation, Some(1));
        assert_eq!(
            record.crypto_profile_id.as_deref(),
            Some(AUTHHUB_DEK_PROFILE_ID)
        );

        let read_back = store.get(&id).map_err(secrets_err)?;
        assert_eq!(read_back.expose_secret().as_ref(), b"provider-token-value");
        Ok(())
    }
}
