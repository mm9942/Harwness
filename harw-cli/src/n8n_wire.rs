//! Reqwest-backed outbound transport for the n8n bridge.
use crate::n8n_bridge::{N8nTransport, TransportError};
use std::time::Duration;

/// Synchronous HTTPS-only transport. The token and request headers are never
/// retained or included in formatted errors.
pub struct ReqwestN8nTransport {
    client: reqwest::blocking::Client,
}

impl ReqwestN8nTransport {
    /// Creates a client with a finite per-request timeout.
    pub fn new(timeout: Duration) -> Result<Self, TransportError> {
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .timeout(timeout)
            .build()
            .map_err(|_| TransportError::Other)?;
        Ok(Self { client })
    }
}

impl N8nTransport for ReqwestN8nTransport {
    fn post(&self, endpoint: &str, bearer_token: &str, body: &[u8]) -> Result<Vec<u8>, TransportError> {
        // Validate before constructing or sending the request, independently of
        // reqwest's HTTPS-only client guard.
        let url = reqwest::Url::parse(endpoint).map_err(|_| TransportError::Other)?;
        if url.scheme() != "https" {
            return Err(TransportError::Other);
        }
        let response = self.client
            .post(url)
            .bearer_auth(bearer_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .map_err(|error| {
                if error.is_timeout() || error.is_connect() {
                    TransportError::Timeout
                } else {
                    TransportError::Other
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            return if status.as_u16() == 429 || status.is_server_error() {
                Err(TransportError::Http(status.as_u16()))
            } else {
                Err(TransportError::Other)
            };
        }
        response.bytes().map(|bytes| bytes.to_vec()).map_err(|error| {
            if error.is_timeout() || error.is_connect() {
                TransportError::Timeout
            } else {
                TransportError::Other
            }
        })
    }
}

impl std::fmt::Debug for ReqwestN8nTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReqwestN8nTransport").finish_non_exhaustive()
    }
}
