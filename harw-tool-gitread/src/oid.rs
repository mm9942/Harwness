//! Objekt-IDs (SHA-1, 20 Bytes) und das Hashen von Git-Objekten.
//!
//! SHA-256-Repositories (`extensions.objectformat = sha256`) werden nicht
//! unterstützt; sie fallen beim Lesen von `HEAD`/Refs mit 64-stelligen IDs auf
//! und werden mit einer klaren Meldung abgelehnt.

use sha1::{Digest, Sha1};
use std::fmt;

/// Länge einer Objekt-ID in Bytes.
pub const OID_LEN: usize = 20;

/// Länge einer Objekt-ID in Hex-Zeichen.
pub const OID_HEX_LEN: usize = 40;

/// Eine Objekt-ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid([u8; OID_LEN]);

impl Oid {
    /// Die Null-ID (nicht existierendes Objekt).
    pub const ZERO: Self = Self([0; OID_LEN]);

    /// Aus genau 20 Bytes.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let array: [u8; OID_LEN] = bytes.try_into().ok()?;
        Some(Self(array))
    }

    /// Die rohen Bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; OID_LEN] {
        &self.0
    }

    /// Aus genau 40 Hex-Zeichen (beliebige Groß-/Kleinschreibung).
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != OID_HEX_LEN {
            return None;
        }
        let mut bytes = [0u8; OID_LEN];
        for (index, chunk) in text.as_bytes().chunks(2).enumerate() {
            let high = hex_value(chunk[0])?;
            let low = hex_value(chunk[1])?;
            bytes[index] = high << 4 | low;
        }
        Some(Self(bytes))
    }

    /// Kleingeschriebene Hex-Darstellung (40 Zeichen).
    #[must_use]
    pub fn hex(&self) -> String {
        let mut out = String::with_capacity(OID_HEX_LEN);
        for byte in &self.0 {
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        out
    }

    /// Die ersten 7 Hex-Zeichen.
    #[must_use]
    pub fn short(&self) -> String {
        self.hex().chars().take(7).collect()
    }

    /// `true` für die Null-ID.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0 == [0; OID_LEN]
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.hex())
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

/// Zerlegt ein Hex-Präfix (4 bis 40 Zeichen) in volle Bytes und ein
/// optionales letztes Halbbyte.
#[must_use]
pub fn parse_hex_prefix(text: &str) -> Option<(Vec<u8>, Option<u8>)> {
    if text.len() < 4 || text.len() > OID_HEX_LEN {
        return None;
    }
    let bytes = text.as_bytes();
    let mut full = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        full.push(hex_value(chunk[0])? << 4 | hex_value(chunk[1])?);
    }
    let half = if bytes.len() % 2 == 1 {
        Some(hex_value(bytes[bytes.len() - 1])?)
    } else {
        None
    };
    Some((full, half))
}

/// Die Objekt-ID von `data` als Objekt der Art `kind` (`blob`, `tree`, `commit`, `tag`).
#[must_use]
pub fn hash_object(kind: &str, data: &[u8]) -> Oid {
    let mut hasher = Sha1::new();
    hasher.update(format!("{kind} {}\0", data.len()).as_bytes());
    hasher.update(data);
    let digest = hasher.finalize();
    Oid::from_bytes(digest.as_slice()).unwrap_or(Oid::ZERO)
}

/// Hasht einen Lese-Strom als Blob bekannter Länge (ohne ihn zu puffern).
///
/// # Errors
/// I/O-Fehler oder wenn der Strom nicht genau `len` Bytes liefert.
pub fn hash_blob_stream<R: std::io::Read>(reader: &mut R, len: u64) -> Result<Oid, String> {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {len}\0").as_bytes());
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > len {
            return Err("file grew while hashing".to_owned());
        }
        hasher.update(&buf[..read]);
    }
    if total != len {
        return Err("file shrank while hashing".to_owned());
    }
    Oid::from_bytes(hasher.finalize().as_slice()).ok_or_else(|| "bad digest length".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn hex_roundtrip_and_validation() -> TestResult {
        let text = "0123456789abcdef0123456789abcdef01234567";
        let oid = Oid::from_hex(text).ok_or(crate::test_support::TestError::Missing("oid"))?;
        assert_eq!(oid.hex(), text);
        assert_eq!(oid.short(), "0123456");
        assert_eq!(Oid::from_hex(&text.to_uppercase()), Some(oid));
        for bad in [
            "",
            "abc",
            &text[..39],
            &format!("{text}0"),
            "g123456789abcdef0123456789abcdef01234567",
        ] {
            assert_eq!(Oid::from_hex(bad), None, "{bad}");
        }
        assert!(Oid::ZERO.is_zero());
        assert!(!oid.is_zero());
        assert_eq!(Oid::from_bytes(&[1; 19]), None);
        Ok(())
    }

    #[test]
    fn prefixes() -> TestResult {
        assert_eq!(parse_hex_prefix("abcd"), Some((vec![0xab, 0xcd], None)));
        assert_eq!(
            parse_hex_prefix("abcde"),
            Some((vec![0xab, 0xcd], Some(0xe)))
        );
        for bad in ["abc", "", "abcg", &"a".repeat(41)] {
            assert_eq!(parse_hex_prefix(bad), None, "{bad}");
        }
        Ok(())
    }

    #[test]
    fn known_git_object_ids() -> TestResult {
        // `git hash-object` des leeren Blobs und von "hello\n".
        assert_eq!(
            hash_object("blob", b"").hex(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            hash_object("blob", b"hello\n").hex(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        // Der leere Baum.
        assert_eq!(
            hash_object("tree", b"").hex(),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
        let mut stream: &[u8] = b"hello\n";
        assert_eq!(
            hash_blob_stream(&mut stream, 6)
                .map_err(crate::test_support::TestError::Unexpected)?
                .hex(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert!(hash_blob_stream(&mut &b"hello\n"[..], 5).is_err());
        assert!(hash_blob_stream(&mut &b"hello\n"[..], 7).is_err());
        Ok(())
    }
}
