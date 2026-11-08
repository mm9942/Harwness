//! Hex-Kodierung fester Bytefelder für Zustandsdateien und Schlüssel
//! (crate-intern).
//!
//! # Verantwortungsbereich
//! Nur Kleinbuchstaben, genau `2 * N` Zeichen beim Dekodieren — eine einzige
//! gültige Schreibweise je Wert. Ohne Indexzugriffe (`get`/Iteratoren).
//!
//! # Nebenläufigkeit
//! Reine Funktionen.

/// Encodes `bytes` as lowercase hex.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            if let Some(digit) = char::from_digit(u32::from(nibble), 16) {
                out.push(digit);
            }
        }
    }
    out
}

/// Decodes exactly `2 * N` lowercase hex characters into `[u8; N]`.
pub(crate) fn decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N.checked_mul(2)? {
        return None;
    }
    let mut out = [0u8; N];
    let mut chars = text.chars();
    for slot in &mut out {
        let high = nibble(chars.next()?)?;
        let low = nibble(chars.next()?)?;
        *slot = (high << 4) | low;
    }
    Some(out)
}

// Ein Hexzeichen in Kleinbuchstaben auf seinen Wert.
fn nibble(c: char) -> Option<u8> {
    match c {
        '0'..='9' | 'a'..='f' => c.to_digit(16).and_then(|value| u8::try_from(value).ok()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn test_roundtrip_and_strictness() {
        let bytes = [0x00, 0x0f, 0xa5, 0xff];
        let text = encode(&bytes);
        assert_eq!(text, "000fa5ff");
        assert_eq!(decode::<4>(&text), Some(bytes));
        assert_eq!(decode::<4>("000FA5FF"), None);
        assert_eq!(decode::<4>("000fa5f"), None);
        assert_eq!(decode::<4>("000fa5ffff"), None);
        assert_eq!(decode::<4>("000fa5fg"), None);
    }
}
