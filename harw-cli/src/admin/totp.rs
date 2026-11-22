//! TOTP nach RFC 6238 (HMAC-SHA1, 6 Stellen, 30 s), ohne eigene Krypto:
//! `hmac` + `sha1` aus RustCrypto.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq as _;

const STEP_SECS: u64 = 30;
const DIGITS: u32 = 6;
/// Geheimnislänge: 20 Bytes (160 Bit, Empfehlung RFC 4226).
pub(crate) const SECRET_BYTES: usize = 20;

/// Frisches zufälliges Geheimnis.
pub(crate) fn generate_secret() -> Result<Vec<u8>, String> {
    let mut secret = vec![0_u8; SECRET_BYTES];
    getrandom::fill(&mut secret).map_err(|_| "Zufallsquelle nicht verfügbar".to_owned())?;
    Ok(secret)
}

/// Base32 ohne Padding (Format der Authenticator-Apps).
pub(crate) fn base32(secret: &[u8]) -> String {
    data_encoding::BASE32_NOPAD.encode(secret)
}

pub(crate) fn from_base32(text: &str) -> Option<Vec<u8>> {
    data_encoding::BASE32_NOPAD
        .decode(text.trim().to_ascii_uppercase().as_bytes())
        .ok()
}

fn code_at(secret: &[u8], step: u64) -> u32 {
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(secret) else {
        return 0;
    };
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let bin = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    bin % 10_u32.pow(DIGITS)
}

/// Prüft `input` (6 Ziffern) im Fenster ±1 Schritt. Liefert den akzeptierten
/// Zeitschritt, wenn er größer als `last_step` ist (Replay-Schutz). Alle
/// Fenster werden konstant-zeitig verglichen.
pub(crate) fn verify(secret: &[u8], input: &str, now: u64, last_step: u64) -> Option<u64> {
    let input = input.trim();
    if input.len() != DIGITS as usize || !input.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let center = now / STEP_SECS;
    let mut accepted = None;
    for step in [center.saturating_sub(1), center, center + 1] {
        let expected = format!("{:0width$}", code_at(secret, step), width = DIGITS as usize);
        if bool::from(expected.as_bytes().ct_eq(input.as_bytes())) && step > last_step {
            accepted = Some(step);
        }
    }
    accepted
}

/// `otpauth://`-URI für Authenticator-Apps.
pub(crate) fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let enc = |text: &str| -> String {
        text.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    };
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}",
        enc(issuer),
        enc(account),
        base32(secret),
        enc(issuer)
    )
}

/// QR-Code als Unicode-Blöcke fürs Terminal.
pub(crate) fn qr_text(data: &str) -> Option<String> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build(),
    )
}
