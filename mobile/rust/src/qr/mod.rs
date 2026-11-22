//! Pairing QR scanning and decoding (server profile QR codes).

mod base45;
mod profile;
#[cfg(test)]
mod tests;

use crate::types::{MobileError, ServerProfile};

/// A server profile read from a pairing QR, validated, with a fingerprint for confirmation.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ScannedProfile {
    pub profile: ServerProfile,
    /// BLAKE3 of the server public key bytes, first 16 bytes, lowercase hex in 8 groups of 4 separated by single spaces.
    pub fingerprint: String,
    /// Canonical profile JSON (`serde_json::to_string(&profile)`) for storage/import.
    pub document_json: String,
}

fn invalid(detail: &str) -> MobileError {
    MobileError::InvalidProfile {
        detail: detail.into(),
    }
}

/// Scans one camera frame (Y plane). Ok(None): no QR code in this frame.
#[uniffi::export]
pub fn scan_pairing_frame(
    luma: Vec<u8>,
    width: u32,
    height: u32,
    row_stride: u32,
) -> Result<Option<ScannedProfile>, MobileError> {
    let (w, h, stride) = (width as usize, height as usize, row_stride as usize);
    if !(1..=4096).contains(&width)
        || !(1..=4096).contains(&height)
        || stride < w
        || luma.len() < (h - 1) * stride + w
    {
        return Err(invalid("camera frame has invalid dimensions"));
    }
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * stride + x]);
    let mut first_error = None;
    for grid in img.detect_grids() {
        let Ok((_, text)) = grid.decode() else {
            continue;
        };
        if !text.starts_with("HARW:") {
            continue;
        }
        match decode_pairing_text(text) {
            Ok(scanned) => return Ok(Some(scanned)),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

/// Decodes and validates pairing text (e.g. from a pasted code or tests).
#[uniffi::export]
pub fn decode_pairing_text(text: String) -> Result<ScannedProfile, MobileError> {
    let profile = profile::decode_profile_text(&text)?;
    let profile = crate::identity::ValidatedProfile::new(profile)?.profile;
    let fingerprint = fingerprint(&profile.server_public_key_hex)?;
    let document_json = serde_json::to_string(&profile).map_err(|error| MobileError::Internal {
        detail: format!("cannot serialize server profile: {error}"),
    })?;
    Ok(ScannedProfile {
        profile,
        fingerprint,
        document_json,
    })
}

pub(crate) fn fingerprint(public_key_hex: &str) -> Result<String, MobileError> {
    let hex = public_key_hex.as_bytes();
    if !hex.len().is_multiple_of(2) {
        return Err(invalid("server public key is not valid hex"));
    }
    let mut key = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks_exact(2) {
        let byte = std::str::from_utf8(pair)
            .ok()
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .ok_or_else(|| invalid("server public key is not valid hex"))?;
        key.push(byte);
    }
    let digest = blake3::hash(&key);
    let groups: Vec<String> = digest.as_bytes()[..16]
        .chunks_exact(2)
        .map(|c| format!("{:02x}{:02x}", c[0], c[1]))
        .collect();
    Ok(groups.join(" "))
}
