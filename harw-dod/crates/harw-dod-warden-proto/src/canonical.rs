//! Kanonische, längenpräfixierte Bytefolgen für Proof v2 (C-WPROTO, W3).
//!
//! # Verantwortungsbereich
//! Crate-interne Hilfen, aus denen [`crate::signed`] die MAC-Eingabe baut.
//! Jedes Feld wird als `u64` Big-Endian-Länge gefolgt von den Rohbytes
//! geschrieben — dadurch ist die Zerlegung eindeutig (keine zwei
//! verschiedenen Feldfolgen ergeben dieselben Bytes), unabhängig davon, was
//! ein Feld enthält. Serde/JSON wird hier bewusst **nicht** verwendet: die
//! MAC-Eingabe darf nicht davon abhängen, wie eine Serialisierungsbibliothek
//! Felder anordnet oder Zeichen maskiert.
//!
//! # Aufteilung (PL-100 WP 3.0)
//! Die Grundbausteine (`put_field`, `put_nested`, `put_u64`,
//! `put_timestamp`) und der Domain-Separator der Aktionsbindung
//! ([`ACTION_DOMAIN`]) liegen seit der Aufteilung in
//! `harw_dod_warden_types::canonical`, weil
//! [`crate::action::WardenAction::binding_digest`] und
//! [`crate::target::CgroupTarget::put_canonical`] dort mitgewandert sind. Dieses
//! Modul importiert sie von dort (die Bytes sind unverändert) und behält nur
//! die signierspezifischen Teile: [`PROOF_DOMAIN`], Wahrheitswerte, die
//! optionale Haltedauer und die Hex-Kodierung von Nonce und MAC.
//!
//! Außerdem die Hex-Kodierung für feste Bytefelder auf der Leitung
//! (`nonce`, `mac`): nur Kleinbuchstaben, exakt `2 * N` Zeichen — genau eine
//! gültige Schreibweise je Wert.
//!
//! # Zusammengesetzte Felder (PL-100 WP 3.1)
//! Werte mit mehreren Teilen (Freigebender, Ziel-cgroup) werden zuerst in
//! einen eigenen Puffer aus längenpräfixierten Teilfeldern geschrieben und
//! dieser Puffer dann als **ein** Feld angehängt (`put_nested`). Damit
//! bleibt jedes Feld der MAC-Eingabe genau ein längenpräfixierter Block,
//! gleich wie viele Teile es hat. Optionale Werte und Wahrheitswerte tragen
//! ein festes Markierungsbyte (`put_opt_u32`, `put_bool`), sodass `None`
//! und `Some(0)` bzw. `false` und `true` nie dieselben Bytes ergeben.
//!
//! # Nebenläufigkeit
//! Reine Funktionen ohne Zustand.

/// Domain-Separator der MAC-Eingabe von Proof v2 (Plan Teil B, „Warden-Proof v2“).
pub const PROOF_DOMAIN: &[u8] = b"harw:warden-proof:v2\0";

/// Domain-Separator der kanonischen Aktionsbindung (siehe
/// [`crate::action::WardenAction::binding_digest`]); seit PL-100 WP 3.0 in
/// `harw_dod_warden_types::canonical` definiert und hier unter dem bisherigen
/// Pfad re-exportiert.
pub use harw_dod_warden_types::canonical::ACTION_DOMAIN;

// Grundbausteine aus dem Typ-Crate (PL-100 WP 3.0, Bytes unverändert):
// `put_field` (Länge `u64` BE plus Rohbytes), `put_nested` (zusammengesetztes
// Feld) und `put_timestamp` (Sekunden ‖ Nanosekunden).
pub(crate) use harw_dod_warden_types::canonical::{put_field, put_nested, put_timestamp};

// Hängt einen Wahrheitswert als genau ein Byte (`0` oder `1`) an.
pub(crate) fn put_bool(buf: &mut Vec<u8>, value: bool) {
    put_field(buf, &[u8::from(value)]);
}

// Hängt ein optionales `u32` an: `None` = `[0]`, `Some(v)` = `[1] ‖ v (BE)`.
pub(crate) fn put_opt_u32(buf: &mut Vec<u8>, value: Option<u32>) {
    match value {
        None => put_field(buf, &[0]),
        Some(value) => {
            let mut bytes = Vec::with_capacity(5);
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
            put_field(buf, &bytes);
        }
    }
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
    for (slot, pair) in out.iter_mut().zip(raw.as_chunks::<2>().0) {
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
    use super::{ACTION_DOMAIN, PROOF_DOMAIN, decode_hex, encode_hex, put_bool, put_opt_u32};

    #[test]
    fn test_put_opt_u32_distinguishes_none_from_some_zero() {
        let mut none = Vec::new();
        put_opt_u32(&mut none, None);
        let mut zero = Vec::new();
        put_opt_u32(&mut zero, Some(0));
        assert_ne!(none, zero);
        assert_eq!(none, [0, 0, 0, 0, 0, 0, 0, 1, 0]);
        assert_eq!(zero, [0, 0, 0, 0, 0, 0, 0, 5, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn test_put_bool_encodes_one_marker_byte() {
        let mut no = Vec::new();
        put_bool(&mut no, false);
        let mut yes = Vec::new();
        put_bool(&mut yes, true);
        assert_eq!(no, [0, 0, 0, 0, 0, 0, 0, 1, 0]);
        assert_eq!(yes, [0, 0, 0, 0, 0, 0, 0, 1, 1]);
    }

    #[test]
    fn test_domains_are_unchanged_and_distinct() {
        assert_eq!(PROOF_DOMAIN, b"harw:warden-proof:v2\0");
        // Re-Export aus `harw-dod-warden-types`: derselbe Wert wie vor der
        // Aufteilung (PL-100 WP 3.0).
        assert_eq!(ACTION_DOMAIN, b"harw:warden-action:v2\0");
        assert_ne!(PROOF_DOMAIN, ACTION_DOMAIN);
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
