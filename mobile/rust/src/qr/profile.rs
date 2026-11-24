//! Decoding of `HARW:PROFILE:` server profile QR texts (port of `decodeProfileQr`).

use super::base45;
use crate::types::{MobileError, ServerProfile};

const PREFIX_V1: &str = "HARW:PROFILE:1:";
const PREFIX_V2: &str = "HARW:PROFILE:2:";
const ENROLL_PREFIX: &str = "HARW:ENROLL:";
const PUBLIC_KEY_SIZE: usize = 1952;

fn invalid(detail: &str) -> MobileError {
    MobileError::InvalidProfile {
        detail: detail.into(),
    }
}

fn printable_ascii(bytes: &[u8]) -> Result<String, MobileError> {
    if bytes.iter().all(|b| (0x21..=0x7e).contains(b)) {
        String::from_utf8(bytes.to_vec()).map_err(|_| invalid("The profile QR has invalid fields."))
    } else {
        Err(invalid("The profile QR has invalid fields."))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Decodes a server profile QR text. Performs no IP/key validation.
pub(crate) fn decode_profile_text(text: &str) -> Result<ServerProfile, MobileError> {
    if text.starts_with(ENROLL_PREFIX) {
        return Err(invalid(
            "This is a phone enrollment code, not a server profile.",
        ));
    }
    let (payload, v2) = if let Some(rest) = text.strip_prefix(PREFIX_V1) {
        (rest, false)
    } else if let Some(rest) = text.strip_prefix(PREFIX_V2) {
        (rest, true)
    } else {
        return Err(invalid("This is not a Harw server profile QR code."));
    };
    let bytes = base45::decode(payload).map_err(|_| invalid("Invalid Harw QR encoding."))?;
    let header = if v2 { 4 } else { 3 };
    if bytes.len() < PUBLIC_KEY_SIZE + header {
        return Err(invalid("The server profile code is incomplete."));
    }
    let label_len = bytes[0] as usize;
    let node_len = bytes[1] as usize;
    let endpoint_len = bytes[2] as usize;
    let onion_len = if v2 { bytes[3] as usize } else { 0 };
    if !(1..=128).contains(&label_len)
        || !(1..=128).contains(&node_len)
        || !(1..=63).contains(&endpoint_len)
        || (v2 && !(62..=68).contains(&onion_len))
    {
        return Err(invalid("The profile QR has invalid fields."));
    }
    let total = header + label_len + node_len + endpoint_len + onion_len + PUBLIC_KEY_SIZE;
    if bytes.len() != total {
        return Err(invalid("The profile QR has an invalid length."));
    }
    let mut at = header;
    let mut take = |n: usize| {
        let slice = &bytes[at..at + n];
        at += n;
        slice
    };
    let label = String::from_utf8(take(label_len).to_vec())
        .map_err(|_| invalid("The profile QR has invalid fields."))?;
    let server_node_id = printable_ascii(take(node_len))?;
    let endpoint = printable_ascii(take(endpoint_len))?;
    let onion = if v2 {
        Some(printable_ascii(take(onion_len))?)
    } else {
        None
    };
    let server_public_key_hex = hex_lower(take(PUBLIC_KEY_SIZE));
    Ok(ServerProfile {
        version: 1,
        label,
        endpoint,
        server_node_id,
        server_public_key_hex,
        onion,
        admin_url: None,
        owner_status_hub: None,
    })
}
