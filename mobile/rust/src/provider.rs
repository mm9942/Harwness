//! Authenticated credential import. Account selection, OpenAI browser OAuth,
//! and the user's approval of the transfer stay in the Android shell.

use std::time::Duration;

use bytes::Bytes;
use harw_node_transport::{LocalNode, NodeBody, full_body};
use http::{Method, Request, StatusCode, header};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, Zeroizing};

use crate::core::route::{Dialer, ONION_DIAL_TIMEOUT};
use crate::{ConnectionRoute, MobileError};
use crate::identity::ValidatedProfile;

const LINK_PATH: &str = "/v1/provider-link/chatgpt";
const MAX_BODY_BYTES: usize = 64 * 1024;
const LINK_TIMEOUT: Duration = Duration::from_secs(20);
const REQUIRED_SCOPES: &[&str] = &[
    "openid",
    "offline_access",
    "chatgpt.tokens.use.direct",
    "resource.invoke",
];

// Keep secrets out of Debug and use the value only to build a bounded request.
#[derive(Serialize, uniffi::Record)]
pub struct ChatgptProviderCredential {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub scope: String,
    pub expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_refresh_at: Option<u64>,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl Zeroize for ChatgptProviderCredential {
    fn zeroize(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
        self.id_token.zeroize();
        self.subject.zeroize();
        if let Some(email) = &mut self.email {
            email.zeroize();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatgptProviderStatus {
    pub linked: bool,
}

impl ChatgptProviderCredential {
    pub(crate) fn request_body(&self) -> Result<NodeBody, MobileError> {
        let now = jiff::Timestamp::now().as_second();
        let expires = i64::try_from(self.expires_at).ok();
        let id_parts: Vec<_> = self.id_token.split('.').take(4).collect();
        if self.client_id == "dynamic_agent_client"
            || !valid_token(&self.client_id, 1024)
            || !valid_token(&self.access_token, MAX_BODY_BYTES)
            || !valid_token(&self.refresh_token, MAX_BODY_BYTES)
            || !valid_token(&self.id_token, MAX_BODY_BYTES)
            || !valid_token(&self.subject, 1024)
            || self
                .email
                .as_ref()
                .is_some_and(|email| !valid_token(email, 1024))
            || id_parts.len() != 3
            || id_parts.iter().any(|part| part.is_empty())
            || self.scope.len() > 2048
            || !REQUIRED_SCOPES.iter().all(|required| {
                self.scope
                    .split_ascii_whitespace()
                    .any(|scope| scope == *required)
            })
            || expires.is_none_or(|expires| expires <= now)
        {
            return Err(MobileError::Denied {
                detail: "ChatGPT sign-in returned invalid, expired, or insufficient credentials"
                    .into(),
            });
        }
        let payload =
            Zeroizing::new(serde_json::to_vec(self).map_err(|_| MobileError::Internal {
                detail: "Could not encode the ChatGPT registration".into(),
            })?);
        if payload.len() > MAX_BODY_BYTES {
            return Err(MobileError::Denied {
                detail: "The ChatGPT registration exceeds the import limit".into(),
            });
        }
        Ok(full_body(Bytes::from_owner(payload)))
    }
}

fn valid_token(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && !value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
}

pub(crate) async fn request_provider(
    dialer: Dialer,
    route: ConnectionRoute,
    local: LocalNode,
    profile: ValidatedProfile,
    stop: CancellationToken,
    method: Method,
    body: NodeBody,
) -> Result<ChatgptProviderStatus, MobileError> {
    let request = async {
        let mut client = dialer.connect_transport(route, &profile, &local).await?;
        let mut request = Request::builder()
            .method(method.clone())
            .uri(LINK_PATH)
            .header(header::HOST, "localhost");
        if method == Method::POST {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        let request = request.body(body).map_err(|_| MobileError::Internal {
            detail: "Could not build the ChatGPT provider request".into(),
        })?;
        let response = client
            .send_request(request)
            .await
            .map_err(|_| provider_interrupted(&method))?;
        match response.status() {
            StatusCode::NO_CONTENT => Ok(ChatgptProviderStatus {
                linked: method != Method::DELETE,
            }),
            StatusCode::ACCEPTED if method == Method::DELETE => Err(MobileError::Session {
                detail: "The server removed its local ChatGPT link. Revocation at OpenAI was not confirmed. Review the agent in ChatGPT Settings.".into(),
            }),
            StatusCode::NOT_FOUND if method == Method::GET => {
                Ok(ChatgptProviderStatus { linked: false })
            }
            StatusCode::NOT_FOUND => Err(MobileError::Denied {
                detail: "This server does not offer ChatGPT linking".into(),
            }),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(MobileError::Denied {
                detail: "This phone is not allowed to manage ChatGPT access on the server".into(),
            }),
            StatusCode::CONFLICT => Err(MobileError::Denied {
                detail: "Unlink the existing ChatGPT account before linking a different account"
                    .into(),
            }),
            StatusCode::SERVICE_UNAVAILABLE => Err(MobileError::Transport {
                detail: "ChatGPT linking is temporarily unavailable on this server".into(),
            }),
            StatusCode::INTERNAL_SERVER_ERROR => Err(MobileError::Session {
                detail: "The server could not update its ChatGPT credential store".into(),
            }),
            StatusCode::BAD_REQUEST
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNPROCESSABLE_ENTITY => Err(MobileError::Denied {
                detail: "The server refused the ChatGPT registration".into(),
            }),
            status => Err(MobileError::Transport {
                detail: format!(
                    "The server could not complete the ChatGPT request (HTTP {})",
                    status.as_u16()
                ),
            }),
        }
    };
    let timeout = match route {
        ConnectionRoute::Direct => LINK_TIMEOUT,
        ConnectionRoute::Onion => LINK_TIMEOUT + ONION_DIAL_TIMEOUT,
    };
    tokio::select! {
        biased;
        () = stop.cancelled() => Err(provider_interrupted_for_stop(&method)),
        result = tokio::time::timeout(timeout, request) => result.unwrap_or_else(|_| Err(provider_interrupted(&method))),
    }
}

fn provider_interrupted_for_stop(method: &Method) -> MobileError {
    if method == Method::GET {
        MobileError::Disconnected
    } else {
        provider_interrupted(method)
    }
}

fn provider_interrupted(method: &Method) -> MobileError {
    MobileError::Transport {
        detail: if method == Method::GET {
            "Could not check the server's ChatGPT connection".into()
        } else {
            "The server's ChatGPT connection may have changed. Check its status before trying again.".into()
        },
    }
}
