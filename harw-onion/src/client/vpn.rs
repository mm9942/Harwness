//! Tor client for a TUN owner. Only relay sockets pass through the protection hook.

use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsFd, BorrowedFd};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use arti_client::config::BoolOrAuto;
use arti_client::{StreamPrefs, TorClient};
use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpSocket, TcpStream};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt as _};
use tor_rtcompat::tokio::TokioRustlsRuntime;
use tor_rtcompat::{
    CompoundRuntime, NetStreamListener, NetStreamProvider, RuntimeSubstExt, StreamOps,
};

use super::runtime::{client_config_builder, install_crypto_provider, map_arti_error};
use crate::{BootstrapProgress, OnionClientConfig, OnionError, OnionLocator, OnionStream};

pub trait RelaySocketProtector: Send + Sync + 'static {
    /// Protect and bind the still-unconnected socket to the physical network.
    fn protect(&self, socket: BorrowedFd<'_>) -> io::Result<()>;
}

#[derive(Clone)]
struct ProtectedTcp(Arc<dyn RelaySocketProtector>);

type Runtime = CompoundRuntime<
    TokioRustlsRuntime,
    TokioRustlsRuntime,
    TokioRustlsRuntime,
    ProtectedTcp,
    TokioRustlsRuntime,
    TokioRustlsRuntime,
    TokioRustlsRuntime,
>;

pub struct ProtectedOnionClient {
    hub: Arc<TorClient<Runtime>>,
    apps: Arc<TorClient<Runtime>>,
}

impl ProtectedOnionClient {
    pub fn new(
        config: &OnionClientConfig,
        protector: Arc<dyn RelaySocketProtector>,
    ) -> Result<Self, OnionError> {
        install_crypto_provider()?;
        let runtime = TokioRustlsRuntime::current()
            .map_err(|error| OnionError::Runtime(error.to_string()))?;
        let runtime = runtime.with_tcp_provider(ProtectedTcp(protector));
        let mut config = client_config_builder(&config.state_dir, &config.cache_dir);
        // Arti's approximate tracked-memory quota is separate from the TUN buffers.
        config.system().memory().max(128_usize * 1024 * 1024);
        let config = config
            .build()
            .map_err(|error| OnionError::Config(error.to_string()))?;
        let hub = TorClient::with_runtime(runtime)
            .config(config)
            .create_unbootstrapped()
            .map_err(|error| map_arti_error(&error, OnionError::Runtime))?;
        let apps = hub.isolated_client();
        Ok(Self { hub, apps })
    }

    pub fn progress(&self) -> BootstrapProgress {
        super::runtime::progress_of(&self.hub.bootstrap_status())
    }

    pub async fn bootstrap(&self) -> Result<(), OnionError> {
        self.hub
            .bootstrap()
            .await
            .map_err(|error| map_arti_error(&error, OnionError::Bootstrap))
    }

    pub async fn dial_hub(
        &self,
        target: &OnionLocator,
        timeout: Duration,
    ) -> Result<OnionStream, OnionError> {
        let preferences = super::isolation::preferences(target, false);
        self.dial(
            &self.hub,
            target.host(),
            target.port(),
            preferences,
            timeout,
        )
        .await
    }

    pub async fn dial_app_onion(
        &self,
        target: &OnionLocator,
        timeout: Duration,
    ) -> Result<OnionStream, OnionError> {
        let mut preferences = StreamPrefs::new();
        preferences
            .connect_to_onion_services(BoolOrAuto::Explicit(true))
            .isolate_every_stream();
        self.dial(
            &self.apps,
            target.host(),
            target.port(),
            preferences,
            timeout,
        )
        .await
    }

    pub async fn dial_app_exit(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<OnionStream, OnionError> {
        let mut preferences = StreamPrefs::new();
        preferences
            .connect_to_onion_services(BoolOrAuto::Explicit(false))
            .connect_to_local_addrs(BoolOrAuto::Explicit(false))
            .isolate_every_stream();
        self.dial(&self.apps, host, port, preferences, timeout)
            .await
    }

    async fn dial(
        &self,
        client: &TorClient<Runtime>,
        host: &str,
        port: u16,
        preferences: StreamPrefs,
        timeout: Duration,
    ) -> Result<OnionStream, OnionError> {
        let deadline = tokio::time::Instant::now() + timeout;
        super::retry::dial_before_deadline(deadline, timeout, self.bootstrap(), async {
            let stream = client
                .connect_with_prefs((host, port), &preferences)
                .await
                .map_err(|error| map_arti_error(&error, OnionError::Connect))?;
            Ok::<OnionStream, OnionError>(Box::pin(stream))
        })
        .await
    }
}

struct RelayStream(Compat<TcpStream>);

impl AsyncRead for RelayStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, bytes)
    }
}

impl AsyncWrite for RelayStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, bytes)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_close(cx)
    }
}

impl StreamOps for RelayStream {}

enum NoListener {}

impl NetStreamListener for NoListener {
    type Stream = RelayStream;
    type Incoming = futures::stream::Empty<io::Result<(RelayStream, SocketAddr)>>;

    fn incoming(self) -> Self::Incoming {
        match self {}
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[async_trait]
impl NetStreamProvider for ProtectedTcp {
    type Stream = RelayStream;
    type Listener = NoListener;
    type ConnectOptions = ();
    type ListenOptions = tor_rtcompat::TcpListenOptions;

    async fn connect(&self, address: &SocketAddr, _: &()) -> io::Result<Self::Stream> {
        let socket = if address.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        self.0.protect(socket.as_fd())?;
        Ok(RelayStream(socket.connect(*address).await?.compat()))
    }

    async fn listen(&self, _: &SocketAddr, _: &Self::ListenOptions) -> io::Result<Self::Listener> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Denied;

    impl RelaySocketProtector for Denied {
        fn protect(&self, _: BorrowedFd<'_>) -> io::Result<()> {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }

    #[tokio::test]
    async fn failed_protection_prevents_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider = ProtectedTcp(Arc::new(Denied));
        let result = provider.connect(&listener.local_addr().unwrap(), &()).await;
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
    }
}
