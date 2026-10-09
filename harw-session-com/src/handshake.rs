//! The server handshake: our policy first, `hyper-tungstenite` for the rest.
//!
//! `hyper-tungstenite` derives `Sec-WebSocket-Accept` and turns the hyper
//! upgrade into a `WebSocketStream`. It deliberately does **not** look at
//! `Origin`, `Sec-WebSocket-Protocol`, the path or the method. Those are the
//! session protocol's policy and stay in `harw_session_ws::upgrade`
//! ([`validate_upgrade`]): exact subprotocol `harw.session.v1`, `Origin`
//! refused, `GET` on the session path, a valid key and version 13. Only a
//! request that passes that policy reaches the crate, and the 101 response
//! then gets the subprotocol header the crate does not set.

use std::future::Future;

use harw_protocol::session_wire::SESSION_WS_SUBPROTOCOL;
use harw_session_ws::WsLimits;
use harw_session_ws::upgrade::{UpgradeRejection, validate_upgrade};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::{HeaderValue, SEC_WEBSOCKET_PROTOCOL};
use hyper::{Request, Response};
use hyper_tungstenite::HyperWebsocketStream;

/// Validates `request`, answers the 101 and, once the upgrade completes,
/// hands the WebSocket stream to `on_socket` on a spawned task.
///
/// Must be called inside a Tokio runtime, on a request that came from a
/// hyper connection served `with_upgrades`.
///
/// # Errors
/// The [`UpgradeRejection`] to answer with; nothing is spawned then.
pub(crate) fn accept<B, F, Fut>(
    request: Request<B>,
    limits: &WsLimits,
    on_socket: F,
) -> Result<Response<Full<Bytes>>, UpgradeRejection>
where
    B: Send + 'static,
    F: FnOnce(HyperWebsocketStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    validate_upgrade(&request)?;
    // The crate re-checks key and version; after our policy it cannot refuse.
    let (mut response, websocket) =
        hyper_tungstenite::upgrade(request, Some(limits.tungstenite_config()))
            .map_err(|_| UpgradeRejection::MissingKey)?;
    response.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(SESSION_WS_SUBPROTOCOL),
    );
    tokio::spawn(async move {
        match websocket.await {
            Ok(stream) => on_socket(stream).await,
            Err(error) => {
                tracing::warn!(%error, "session com: websocket upgrade did not complete");
            }
        }
    });
    Ok(response)
}
