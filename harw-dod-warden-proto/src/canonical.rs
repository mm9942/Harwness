//! Kanonische, längenpräfixierte Bytefolgen für Proof v2 (C-WPROTO, W3).
//!
//! # Verantwortungsbereich
//! Crate-interne Hilfen, aus denen [`crate::signed`] die MAC-Eingabe und
//! [`crate::action::WardenAction::binding_digest`] die Aktionsbindung baut.
//! Jedes Feld wird als `u64` Big-Endian-Länge gefolgt von den Rohbytes
//! geschrieben — dadurch ist die Zerlegung eindeutig (keine zwei
//! verschiedenen Feldfolgen ergeben dieselben Bytes), unabhängig davon, was
//! ein Feld enthält. Serde/JSON wird hier bewusst **nicht** verwendet: die
//! MAC-Eingabe darf nicht davon abhängen, wie eine Serialisierungsbibliothek
//! Felder anordnet oder Zeichen maskiert.
//!
//! Außerdem die Hex-Kodierung für feste Bytefelder auf der Leitung
//! (`nonce`, `mac`): nur Kleinbuchstaben, exakt `2 * N` Zeichen — genau eine
//! gültige Schreibweise je Wert.
//!
//! # Nebenläufigkeit
//! Reine Funktionen ohne Zustand.

/// Domain-Separator der MAC-Eingabe von Proof v2 (Plan Teil B, „Warden-Proof v2“).
pub const PROOF_DOMAIN: &[u8] = b"harw:warden-proof:v2\0";

/// Domain-Separator der kanonischen Aktionsbindung (siehe
/// [`crate::action::WardenAction::binding_digest`]).
pub const ACTION_DOMAIN: &[u8] = b"harw:warden-action:v2\0";

// Hängt ein Feld als `u64`-BE-Länge plus Rohbytes an `buf` an.
pub(crate) fn put_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}

// Kodiert `bytes` als Hex in Kleinbuchstaben.
pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

// Ein Hex-Zeichen (nur `0-9`, `a-f`) auf seinen Wert; alles andere `None`.
fn lower_hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

// Dekodiert exakt `2 * N` Kleinbuchstaben-Hexzeichen in ein `[u8; N]`.
pub(crate) fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let raw = text.as_bytes();
    if raw.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (slot, pair) in out.iter_mut().zip(raw.chunks_exact(2)) {
        let high = lower_hex_nibble(pair[0])?;
        let low = lower_hex_nibble(pair[1])?;
        *slot = (high << 4) | low;
    }
    Some(out)
}

// Erzeugt ein `#[serde(with = "...")]`-Modul für ein festes `[u8; N]` als Hex-String.
macro_rules! hex_array_serde {
    ($modname:ident, $len:literal) => {
        pub(crate) mod $modname {
            use serde::{Deserialize, Deserializer, Serializer};

            pub(crate) fn serialize<S: Serializer>(
                bytes: &[u8; $len],
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&crate::canonical::encode_hex(bytes))
            }

            pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
                deserializer: D,
            ) -> Result<[u8; $len], D::Error> {
                let text = String::deserialize(deserializer)?;
                crate::canonical::decode_hex::<$len>(&text).ok_or_else(|| {
                    serde::de::Error::custom(concat!(
                        "expected exactly ",
                        $len,
                        " bytes as lowercase hex"
                    ))
                })
            }
        }
    };
}

hex_array_serde!(hex16, 16);
hex_array_serde!(hex32, 32);

#[cfg(test)]
mod tests {
    use super::{decode_hex, encode_hex, put_field};

    #[test]
    fn test_put_field_prefixes_big_endian_length() {
        let mut buf = Vec::new();
        put_field(&mut buf, b"ab");
        assert_eq!(buf, [0, 0, 0, 0, 0, 0, 0, 2, b'a', b'b']);
    }

    #[test]
    fn test_put_field_is_unambiguous_across_field_boundaries() {
        let mut left = Vec::new();
        put_field(&mut left, b"ab");
        put_field(&mut left, b"c");
        let mut right = Vec::new();
        put_field(&mut right, b"a");
        put_field(&mut right, b"bc");
        assert_ne!(left, right);
    }

    #[test]
    fn test_encode_hex_roundtrips_through_decode_hex() {
        let bytes: [u8; 4] = [0x00, 0x7f, 0xa5, 0xff];
        let text = encode_hex(&bytes);
        assert_eq!(text, "007fa5ff");
        assert_eq!(decode_hex::<4>(&text), Some(bytes));
    }

    #[test]
    fn test_decode_hex_rejects_uppercase_wrong_length_and_non_hex() {
        assert_eq!(decode_hex::<2>("ABCD"), None);
        assert_eq!(decode_hex::<2>("abc"), None);
        assert_eq!(decode_hex::<2>("abcdef"), None);
        assert_eq!(decode_hex::<2>("zz00"), None);
    }
}
