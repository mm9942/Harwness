//! Commitments für versiegelte und geheime Argumente (matrix-game.md §2.4, §9.3).
//!
//! Ein Commitment ist `sha256(kanonisches JSON ‖ salt)` als Hex-String. Die
//! kanonische Serialisierung sortiert Objektschlüssel rekursiv und ist damit
//! unabhängig von der Feldreihenfolge und von `serde_json`-Features
//! (`preserve_order`) stabil. Der Salt wird deterministisch aus dem geheimen
//! Master-Seed und der Argument-ID abgeleitet: Replay erzeugt dieselben
//! Commitments, Spieler können den Salt ohne Master-Seed nicht erraten.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::MatrixResult;

/// Domänentrenner aller Hash-Ableitungen dieses Crates.
pub const DOMAIN: &[u8] = b"harw-matrix/v1";

/// SHA-256 über die Verkettung aller Teile.
#[must_use]
pub fn sha256_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Kleinbuchstaben-Hex-Kodierung.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Hex-Dekodierung (Groß- und Kleinbuchstaben); `None` bei ungültiger Eingabe.
#[must_use]
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() & 1 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = hex_val(*pair.first()?)?;
        let lo = hex_val(*pair.get(1)?)?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Kanonisches JSON: Objektschlüssel rekursiv sortiert, keine Leerzeichen.
///
/// # Errors
/// [`crate::MatrixError::Json`], wenn `value` nicht serialisierbar ist.
pub fn canonical_json<T: Serialize + ?Sized>(value: &T) -> MatrixResult<String> {
    let tree = serde_json::to_value(value)?;
    let mut out = String::new();
    write_canonical(&tree, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &Value, out: &mut String) -> MatrixResult<()> {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                if let Some(child) = map.get(key.as_str()) {
                    write_canonical(child, out)?;
                }
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other)?),
    }
    Ok(())
}

/// 16-Byte-Salt eines Commitments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Salt(pub [u8; 16]);

impl Salt {
    /// Leitet den Salt deterministisch aus Master-Seed und Argument-ID ab.
    #[must_use]
    pub fn derive(master_seed: &[u8; 32], argument_id: &str) -> Self {
        let digest = sha256_parts(&[DOMAIN, b"/salt", master_seed, argument_id.as_bytes()]);
        let mut salt = [0u8; 16];
        for (dst, src) in salt.iter_mut().zip(digest.iter()) {
            *dst = *src;
        }
        Self(salt)
    }

    /// Hex-Darstellung (32 Zeichen).
    #[must_use]
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// Liest einen Salt aus Hex; `None` bei falscher Länge oder Zeichen.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = from_hex(text)?;
        let arr: [u8; 16] = bytes.try_into().ok()?;
        Some(Self(arr))
    }
}

/// Hex-kodiertes SHA-256-Commitment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Commitment(pub String);

impl Commitment {
    /// Kurzform für Anzeigen (`sha 51e0…`).
    #[must_use]
    pub fn short(&self) -> &str {
        self.0.get(..8).unwrap_or(&self.0)
    }
}

impl std::fmt::Display for Commitment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Commitment über rohe Bytes: `sha256(content ‖ salt)`.
#[must_use]
pub fn commit_bytes(content: &[u8], salt: &Salt) -> Commitment {
    Commitment(to_hex(&sha256_parts(&[content, &salt.0])))
}

/// Commitment über das kanonische JSON eines Werts.
///
/// # Errors
/// [`crate::MatrixError::Json`], wenn `content` nicht serialisierbar ist.
pub fn commit<T: Serialize + ?Sized>(content: &T, salt: &Salt) -> MatrixResult<Commitment> {
    let canonical = canonical_json(content)?;
    Ok(commit_bytes(canonical.as_bytes(), salt))
}

/// Prüft eine Offenlegung: stimmt `sha256(canonical(content) ‖ salt)` mit dem
/// veröffentlichten Commitment überein?
///
/// # Errors
/// [`crate::MatrixError::Json`], wenn `content` nicht serialisierbar ist.
pub fn verify<T: Serialize + ?Sized>(
    content: &T,
    salt: &Salt,
    commitment: &Commitment,
) -> MatrixResult<bool> {
    Ok(commit(content, salt)? == *commitment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            to_hex(&sha256_parts(&[b"a", b"bc"])),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn canonical_json_sorts_keys_golden() -> TestResult {
        let value = json!({"b": 1, "a": [true, "x"]});
        assert_eq!(canonical_json(&value)?, r#"{"a":[true,"x"],"b":1}"#);
        let nested = json!({"z": {"y": 1, "x": null}, "a": "ä"});
        assert_eq!(
            canonical_json(&nested)?,
            r#"{"a":"ä","z":{"x":null,"y":1}}"#
        );
        Ok(())
    }

    #[test]
    fn commitment_golden() -> TestResult {
        let value = json!({"b": 1, "a": [true, "x"]});
        let c = commit(&value, &Salt([0u8; 16]))?;
        assert_eq!(
            c.0,
            "e38df7b65bf93907c64d6cae21bcaab2e49861fb8afc765e17d3f069b855fabf"
        );
        assert_eq!(c.short(), "e38df7b6");
        Ok(())
    }

    #[test]
    fn verify_detects_tampering() -> TestResult {
        let master = [7u8; 32];
        let salt = Salt::derive(&master, "r1-a2");
        let content = json!({"action": "Die Gilde sabotiert", "pros": ["a", "b"]});
        let c = commit(&content, &salt)?;
        assert!(verify(&content, &salt, &c)?);
        let tampered = json!({"action": "Die Gilde hilft", "pros": ["a", "b"]});
        assert!(!verify(&tampered, &salt, &c)?);
        let other_salt = Salt::derive(&master, "r1-a3");
        assert!(!verify(&content, &other_salt, &c)?);
        Ok(())
    }

    #[test]
    fn salt_derivation_is_deterministic_and_distinct() {
        let a = Salt::derive(&[1u8; 32], "s1");
        let b = Salt::derive(&[1u8; 32], "s1");
        let c = Salt::derive(&[2u8; 32], "s1");
        let d = Salt::derive(&[1u8; 32], "s2");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_eq!(Salt::from_hex(&a.to_hex()), Some(a));
    }

    #[test]
    fn hex_roundtrip_and_rejects_garbage() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(from_hex(&to_hex(&bytes)), Some(bytes.to_vec()));
        assert_eq!(from_hex("ABff"), Some(vec![0xab, 0xff]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }
}
