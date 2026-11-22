//! Base45 (RFC 9285) as used by the Harw pairing QR codes (`PairingQr.kt`).

const ALPHABET: &[u8; 45] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";
const MAX_TEXT_LEN: usize = 3600;

fn value_of(c: u8) -> Result<u32, String> {
    ALPHABET
        .iter()
        .position(|&a| a == c)
        .map(|v| v as u32)
        .ok_or_else(|| format!("invalid base45 character {:?}", c as char))
}

/// Decodes base45 text into bytes.
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if bytes.len() > MAX_TEXT_LEN {
        return Err("base45 text is too long".into());
    }
    if bytes.len() % 3 == 1 {
        return Err("base45 text has an invalid length".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 3 * 2 + 1);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let value = value_of(chunk[0])? + value_of(chunk[1])? * 45 + value_of(chunk[2])? * 2025;
        if value > 0xffff {
            return Err("base45 triple out of range".into());
        }
        out.push((value >> 8) as u8);
        out.push((value & 0xff) as u8);
    }
    let rest = chunks.remainder();
    if rest.len() == 2 {
        let value = value_of(rest[0])? + value_of(rest[1])? * 45;
        if value > 0xff {
            return Err("base45 pair out of range".into());
        }
        out.push(value as u8);
    }
    Ok(out)
}

/// Encodes bytes as base45 text.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 3).div_ceil(2));
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        let value = u32::from(chunk[0]) * 256 + u32::from(chunk[1]);
        out.push(ALPHABET[(value % 45) as usize] as char);
        out.push(ALPHABET[(value / 45 % 45) as usize] as char);
        out.push(ALPHABET[(value / 2025) as usize] as char);
    }
    if let [last] = chunks.remainder() {
        let value = u32::from(*last);
        out.push(ALPHABET[(value % 45) as usize] as char);
        out.push(ALPHABET[(value / 45) as usize] as char);
    }
    out
}
