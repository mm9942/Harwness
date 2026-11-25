use std::time::Duration;

use harw_onion::{OnionError, OnionStream, ProtectedOnionClient};

use super::Destination;

pub async fn dial_with_arti(
    client: &ProtectedOnionClient,
    destination: Destination,
    timeout: Duration,
) -> Result<OnionStream, OnionError> {
    match destination {
        Destination::Onion(locator) => client.dial_app_onion(&locator, timeout).await,
        Destination::PublicName { host, port } => client.dial_app_exit(&host, port, timeout).await,
        Destination::PublicIp(address) => {
            // The numeric address comes from the admitted TUN tuple. No resolver is involved.
            client
                .dial_app_exit(&address.ip().to_string(), address.port(), timeout)
                .await
        }
    }
}
