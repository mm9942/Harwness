use std::io;
use std::time::{Duration, Instant};

use harw_onion::{OnionStream, ProtectedOnionClient};

use super::{Destination, DnsUpstream};

pub async fn dial_with_arti(
    client: &ProtectedOnionClient,
    upstream_dns: Option<&DnsUpstream>,
    destination: Destination,
    timeout: Duration,
) -> io::Result<OnionStream> {
    match destination {
        Destination::Onion(locator) => client.dial_app_onion(&locator, timeout).await,
        Destination::PublicName { host, port, ipv6 } => {
            let started = Instant::now();
            let host = match upstream_dns {
                Some(resolver) => resolver
                    .resolve(client, &host, ipv6, timeout)
                    .await?
                    .to_string(),
                None => host,
            };
            client
                .dial_app_exit(&host, port, timeout.saturating_sub(started.elapsed()))
                .await
        }
        Destination::PublicIp(address) => {
            client
                .dial_app_exit(&address.ip().to_string(), address.port(), timeout)
                .await
        }
    }
    .map_err(io::Error::other)
}
