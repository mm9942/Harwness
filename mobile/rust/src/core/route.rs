//! Network route selection and dialing (direct TCP or embedded Tor onion).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use harw_node_transport::{ClientOptions, LocalNode, NodeTransportClient};
use harw_session_remote::{ConnectOptions, RemoteConnection, RemoteError, connect_node};

use crate::identity::ValidatedProfile;
use crate::{ConnectionRoute, MobileError, RoutePreference, TorBootstrap};

/// Bound for the Tor dial, including a cold bootstrap.
pub(crate) const ONION_DIAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Bound for the node handshake over an onion stream.
pub(crate) const ONION_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for the HTTP upgrade plus `session.hello` over an onion stream.
pub(crate) const ONION_HELLO_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(not(feature = "onion"))]
const TOR_UNAVAILABLE: &str = "this build has no Tor support";

/// Ordered routes to try for `preference` against `profile`.
pub(crate) fn route_plan(
    preference: RoutePreference,
    profile: &ValidatedProfile,
) -> Result<Vec<ConnectionRoute>, MobileError> {
    let routes = match preference {
        RoutePreference::Onion => profile.onion.as_ref()
            .map(|_| vec![ConnectionRoute::Onion])
            .ok_or_else(|| MobileError::InvalidProfile {
                detail: "Tor-only: this profile has no .onion locator; direct connection is disabled".into(),
            })?,
        RoutePreference::Direct => profile.direct_endpoint.as_ref()
            .map(|_| vec![ConnectionRoute::Direct])
            .ok_or_else(|| MobileError::InvalidProfile {
                detail: "This profile has no configured direct endpoint".into(),
            })?,
        RoutePreference::Auto => available_routes(profile),
    };
    Ok(routes)
}

/// Routes the profile advertises, onion first. This is not permission to dial direct.
pub(crate) fn available_routes(profile: &ValidatedProfile) -> Vec<ConnectionRoute> {
    let mut routes = Vec::with_capacity(2);
    if profile.onion.is_some() {
        routes.push(ConnectionRoute::Onion);
    }
    if profile.direct_endpoint.is_some() {
        routes.push(ConnectionRoute::Direct);
    }
    routes
}

/// Human-readable route name.
pub(crate) fn route_label(route: ConnectionRoute) -> &'static str {
    match route {
        ConnectionRoute::Direct => "Direct",
        ConnectionRoute::Onion => "Tor (.onion)",
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn create_private_dir(path: &Path) -> Result<(), MobileError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|error| MobileError::Internal {
            detail: format!("cannot create Tor directory: {error}"),
        })
}

#[derive(Default)]
struct DialerInner {
    dirs: Mutex<Option<(PathBuf, PathBuf)>>,
    #[cfg(feature = "onion")]
    tor: Mutex<Option<Arc<harw_onion::OnionClient>>>,
}

/// Dials the selected route; owns the lazily started embedded Tor client.
#[derive(Clone, Default)]
pub(crate) struct Dialer {
    inner: Arc<DialerInner>,
}

impl Dialer {
    /// Sets the Tor state and cache directories (absolute, created 0700).
    pub(crate) fn set_tor_directories(
        &self,
        state_dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Result<(), MobileError> {
        if !state_dir.is_absolute() || !cache_dir.is_absolute() {
            return Err(MobileError::Internal {
                detail: "Tor directories must be absolute paths".into(),
            });
        }
        create_private_dir(&state_dir)?;
        create_private_dir(&cache_dir)?;
        let mut dirs = lock(&self.inner.dirs);
        let new = Some((state_dir, cache_dir));
        #[cfg(feature = "onion")]
        if *dirs != new && lock(&self.inner.tor).is_some() {
            return Err(MobileError::Internal {
                detail: "Tor storage cannot change while Tor runs".into(),
            });
        }
        *dirs = new;
        Ok(())
    }

    /// Live Tor bootstrap progress, or `None` until Tor was started.
    pub(crate) fn tor_status(&self) -> Option<TorBootstrap> {
        #[cfg(feature = "onion")]
        {
            let client = lock(&self.inner.tor).clone()?;
            let progress = client.progress();
            Some(TorBootstrap {
                percent: u32::from(progress.percent),
                ready: progress.ready,
                blocked: progress.blocked,
                summary: progress.summary,
            })
        }
        #[cfg(not(feature = "onion"))]
        {
            None
        }
    }

    /// Dials the node and completes `session.hello` over `route`.
    pub(crate) async fn connect_session(
        &self,
        route: ConnectionRoute,
        profile: &ValidatedProfile,
        local: &LocalNode,
        mut options: ConnectOptions,
    ) -> Result<RemoteConnection, RemoteError> {
        match route {
            ConnectionRoute::Direct => {
                let direct = profile.direct_endpoint.clone()
                    .ok_or_else(|| RemoteError::Connect("no configured direct endpoint".into()))?;
                connect_node(direct, local, &profile.verifier, options).await
            }
            ConnectionRoute::Onion => {
                let stream = self
                    .dial_onion(profile)
                    .await
                    .map_err(|detail| RemoteError::Connect(format!("tor: {detail}")))?;
                options.limits.hello_timeout =
                    options.limits.hello_timeout.max(ONION_HELLO_TIMEOUT);
                harw_session_remote::conn::connect_node_over(
                    stream,
                    &profile.expected_node,
                    local,
                    &profile.verifier,
                    options,
                    &onion_transport_options(),
                )
                .await
            }
        }
    }

    /// Opens an authenticated HTTP transport to the node over `route`.
    pub(crate) async fn connect_transport(
        &self,
        route: ConnectionRoute,
        profile: &ValidatedProfile,
        local: &LocalNode,
    ) -> Result<NodeTransportClient, MobileError> {
        let connected = match route {
            ConnectionRoute::Direct => {
                let direct = profile.direct_endpoint.as_ref().ok_or_else(|| MobileError::InvalidProfile {
                    detail: "This profile has no configured direct endpoint".into(),
                })?;
                NodeTransportClient::connect(direct.addr, local, &profile.expected_node, &profile.verifier)
                    .await.map_err(drop)
            },
            ConnectionRoute::Onion => match self.dial_onion(profile).await {
                Ok(stream) => NodeTransportClient::connect_over(
                    stream,
                    local,
                    &profile.expected_node,
                    &profile.verifier,
                    &onion_transport_options(),
                )
                .await
                .map_err(drop),
                Err(_) => Err(()),
            },
        };
        connected.map_err(|()| MobileError::Connect {
            detail: "Could not authenticate the selected server for ChatGPT access".into(),
        })
    }

    #[cfg(feature = "onion")]
    async fn dial_onion(
        &self,
        profile: &ValidatedProfile,
    ) -> Result<harw_onion::OnionStream, String> {
        let locator = profile
            .onion
            .as_ref()
            .ok_or_else(|| "this server profile has no .onion route".to_owned())?;
        let client = self.tor_client()?;
        client
            .dial_with_retry(locator, ONION_DIAL_TIMEOUT)
            .await
            .map_err(|error| error.to_string())
    }

    /// Prepare both explicitly selected status destinations on one existing
    /// device's Arti client. No route planning, direct address, or retry.
    #[cfg(feature = "onion")]
    pub(super) async fn dial_owner_status_pair(
        &self,
        owner: &harw_onion::OnionLocator,
        hub: &harw_onion::OnionLocator,
    ) -> Result<(harw_onion::OnionStream, harw_onion::OnionStream), MobileError> {
        let unavailable = || MobileError::Connect {
            detail: "Could not reach both pinned Onion services for owner status".into(),
        };
        let client = self.tor_client().map_err(|_| unavailable())?;
        tokio::try_join!(
            client.dial(owner, ONION_DIAL_TIMEOUT),
            client.dial(hub, ONION_DIAL_TIMEOUT),
        ).map_err(|_| unavailable())
    }

    /// Without Tor support there is no stream; the placeholder type is never produced.
    #[cfg(not(feature = "onion"))]
    async fn dial_onion(
        &self,
        _profile: &ValidatedProfile,
    ) -> Result<tokio::io::DuplexStream, String> {
        Err(TOR_UNAVAILABLE.to_owned())
    }

    /// Returns the running Tor client, starting it on first use.
    #[cfg(feature = "onion")]
    fn tor_client(&self) -> Result<Arc<harw_onion::OnionClient>, String> {
        if let Some(client) = lock(&self.inner.tor).as_ref() {
            return Ok(Arc::clone(client));
        }
        // Serialize cold starts and directory changes without holding the
        // status mutex during disk I/O. A discarded second launch can still
        // open the same Arti state/cache and start bootstrap tasks.
        let dirs = lock(&self.inner.dirs);
        if let Some(client) = lock(&self.inner.tor).as_ref() {
            return Ok(Arc::clone(client));
        }
        let (state, cache) = dirs
            .as_ref()
            .ok_or_else(|| "Tor storage is not configured".to_owned())?;
        let config = harw_onion::OnionClientConfig::new(state.clone(), cache.clone());
        let client = Arc::new(
            harw_onion::OnionClient::launch(&config).map_err(|error| error.to_string())?,
        );
        *lock(&self.inner.tor) = Some(Arc::clone(&client));
        Ok(client)
    }
}

pub(super) fn onion_transport_options() -> ClientOptions {
    ClientOptions {
        handshake_timeout: ONION_HANDSHAKE_TIMEOUT,
        ..ClientOptions::default()
    }
}
