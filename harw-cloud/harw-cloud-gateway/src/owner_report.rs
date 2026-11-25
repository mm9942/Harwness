//! Strict internal owner-observation protocol. The node identity is never
//! part of the report schema: only the authenticated transport owns it.

use std::num::NonZeroU64;
use std::time::Duration;

use http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING};
use http::{Method, Request};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};

use crate::owner_directory::valid_id;
use crate::owner_ingress::HubIngressError;
use crate::route_admission::OwnerFence;

/// Reserved internal route. No service is mounted by declaring it.
pub const OWNER_REPORT_PATH: &str = "/v1/hub/owner-observation";
pub(crate) const BODY_TIMEOUT: Duration = Duration::from_secs(5);
const SCHEMA: &str = "harw.hub.owner-observation";
const VERSION: u8 = 1;
const MAX_BYTES: usize = 2 * 1024;
const MAX_FRAMES: usize = 32;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObservationV1 {
    schema: String,
    version: u8,
    tenant: String,
    session_id: String,
    host_instance: String,
    generation: NonZeroU64,
    owner_epoch: u64,
}

/// Serialize the same bounded schema the authenticated ingress decodes. The
/// owner node is validated locally but deliberately omitted from the body.
pub(crate) fn encode(fence: &OwnerFence) -> Result<Vec<u8>, HubIngressError> {
    if ![
        fence.tenant.as_str(),
        fence.session_id.as_str(),
        fence.host_instance.as_str(),
        fence.node_id.as_str(),
    ]
    .into_iter()
    .all(valid_id)
    {
        return Err(HubIngressError::InvalidReport);
    }
    let report = ObservationV1 {
        schema: SCHEMA.to_owned(),
        version: VERSION,
        tenant: fence.tenant.clone(),
        session_id: fence.session_id.clone(),
        host_instance: fence.host_instance.clone(),
        generation: fence.generation,
        owner_epoch: fence.owner_epoch,
    };
    let bytes = serde_json::to_vec(&report).map_err(|_| HubIngressError::InvalidReport)?;
    if !(1..=MAX_BYTES).contains(&bytes.len()) {
        return Err(HubIngressError::BodyLimit);
    }
    Ok(bytes)
}

pub(crate) fn body_len<B>(request: &Request<B>) -> Result<usize, HubIngressError> {
    request_len(request, OWNER_REPORT_PATH, MAX_BYTES)
}

pub(crate) fn request_len<B>(request: &Request<B>, path: &str, cap: usize) -> Result<usize, HubIngressError> {
    let uri = request.uri();
    if request.method() != Method::POST
        || uri.path() != path
        || uri.query().is_some()
        || uri.scheme().is_some()
        || uri.authority().is_some()
    {
        return Err(HubIngressError::WrongProtocol);
    }
    json_len(request.headers(), cap)
}

pub(crate) fn json_len(headers: &http::HeaderMap, cap: usize) -> Result<usize, HubIngressError> {
    let types = headers.get_all(CONTENT_TYPE);
    let mut types = types.iter();
    if types.next().map(|value| value.as_bytes()) != Some(&b"application/json"[..])
        || types.next().is_some()
        || headers.contains_key(CONTENT_ENCODING)
        || headers.contains_key(TRANSFER_ENCODING)
    {
        return Err(HubIngressError::WrongProtocol);
    }
    let lengths = headers.get_all(CONTENT_LENGTH);
    let mut lengths = lengths.iter();
    let length = lengths
        .next()
        .and_then(|value| value.to_str().ok())
        .ok_or(HubIngressError::InvalidReport)?;
    if lengths.next().is_some() || length.is_empty() || !length.bytes().all(|b| b.is_ascii_digit()) {
        return Err(HubIngressError::InvalidReport);
    }
    let length = length.parse::<usize>().map_err(|_| HubIngressError::BodyLimit)?;
    match length {
        0 => Err(HubIngressError::InvalidReport),
        length if length > cap => Err(HubIngressError::BodyLimit),
        length => Ok(length),
    }
}

pub(crate) async fn read<B>(
    body: &mut B,
    expected_len: usize,
    trusted_node_id: &str,
) -> Result<OwnerFence, HubIngressError>
where
    B: BodyExt + Unpin,
    B::Data: AsRef<[u8]>,
{
    let payload = read_json(body, expected_len, MAX_BYTES).await?;
    decode(&payload, trusted_node_id)
}

pub(crate) async fn read_json<B>(
    body: &mut B,
    expected_len: usize,
    cap: usize,
) -> Result<Vec<u8>, HubIngressError>
where
    B: BodyExt + Unpin,
    B::Data: AsRef<[u8]>,
{
    if !(1..=cap).contains(&expected_len) {
        return Err(HubIngressError::BodyLimit);
    }
    let mut payload = Vec::with_capacity(expected_len);
    let mut frames = 0;
    while let Some(frame) = body.frame().await {
        frames += 1;
        if frames > MAX_FRAMES {
            return Err(HubIngressError::BodyLimit);
        }
        let data = frame
            .map_err(|_| HubIngressError::InvalidReport)?
            .into_data()
            .map_err(|_| HubIngressError::InvalidReport)?;
        let chunk = data.as_ref();
        if chunk.len() > expected_len - payload.len() {
            return Err(HubIngressError::BodyLimit);
        }
        payload.extend_from_slice(chunk);
    }
    if payload.len() != expected_len {
        return Err(HubIngressError::InvalidReport);
    }
    Ok(payload)
}

fn decode(bytes: &[u8], trusted_node_id: &str) -> Result<OwnerFence, HubIngressError> {
    let report: ObservationV1 =
        serde_json::from_slice(bytes).map_err(|_| HubIngressError::InvalidReport)?;
    if report.schema != SCHEMA || report.version != VERSION {
        return Err(HubIngressError::WrongProtocol);
    }
    if ![
        report.tenant.as_str(),
        report.session_id.as_str(),
        report.host_instance.as_str(),
        trusted_node_id,
    ]
    .into_iter()
    .all(valid_id)
    {
        return Err(HubIngressError::InvalidReport);
    }
    Ok(OwnerFence {
        tenant: report.tenant,
        session_id: report.session_id,
        node_id: trusted_node_id.to_owned(),
        host_instance: report.host_instance,
        generation: report.generation,
        owner_epoch: report.owner_epoch,
    })
}
