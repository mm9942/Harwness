//! PKCE-Hilfen (RFC 7636, Methode `S256`) — rein und testbar.
//!
//! Der `code_verifier` ist ein zufälliger, base64url-kodierter String; die
//! `code_challenge` ist `BASE64URL(SHA-256(verifier))` ohne Padding.

use base64::Engine as _;
use rand::Rng as _;
use sha2::{Digest as _, Sha256};

/// base64url-Engine ohne Padding (RFC 7636 verlangt padding-frei).
const URL_SAFE_NO_PAD: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Ein PKCE-Paar aus Verifier und daraus abgeleiteter Challenge.
#[derive(Clone)]
pub struct PkcePair {
    /// Geheimer `code_verifier` (wird beim Token-Exchange mitgeschickt).
    pub verifier: String,
    /// Öffentliche `code_challenge` (`S256`, für die Authorize-URL).
    pub challenge: String,
}

/// Erzeugt ein frisches PKCE-Paar mit 32 Byte Zufall.
///
/// # Concurrency
/// Nutzt den thread-lokalen RNG; sicher aus beliebigen Threads aufrufbar.
#[must_use]
pub fn generate_pkce() -> PkcePair {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = challenge_from_verifier(&verifier);
    PkcePair {
        verifier,
        challenge,
    }
}

/// Leitet die `code_challenge` (`S256`) aus einem `code_verifier` ab.
///
/// # Examples
/// ```
/// // RFC 7636 Appendix B Testvektor.
/// let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
/// assert_eq!(
///     harw_oauth::challenge_from_verifier(verifier),
///     "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
/// );
/// ```
#[must_use]
pub fn challenge_from_verifier(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rfc7636_appendix_b_vector() {
        // Der kanonische RFC-7636-Testvektor.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_from_verifier(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn test_generate_pkce_roundtrip() {
        let pair = generate_pkce();
        // Verifier ist padding-frei base64url (kein '=').
        assert!(!pair.verifier.contains('='));
        // Challenge ist deterministisch aus dem Verifier ableitbar.
        assert_eq!(pair.challenge, challenge_from_verifier(&pair.verifier));
    }

    #[test]
    fn test_generate_pkce_is_random() {
        assert_ne!(generate_pkce().verifier, generate_pkce().verifier);
    }
}
