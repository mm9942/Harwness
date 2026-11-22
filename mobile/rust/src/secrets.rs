//! Secret storage bridge between Rust and the Android Keystore.
//!
//! The Android Keystore holds a non-exportable AES-GCM KEK. Kotlin
//! (`PlatformKeystore.kt`) keeps one encrypted slot per provider route in
//! `noBackupFilesDir/secrets/`. Rust holds plaintext only in `Zeroizing` or
//! `SecretString`. Residual risk: UniFFI copies a `ByteArray` across the
//! boundary without zeroizing it.

use std::fmt;
use std::sync::Arc;

use secrecy::SecretString;
use zeroize::Zeroizing;

/// Largest plaintext secret, in bytes, that is stored or resolved.
pub const MAX_SECRET_BYTES: usize = 16 * 1024;

/// Slot-name prefix of provider route secrets.
pub(crate) const ROUTE_SLOT_PREFIX: &str = "route-";

/// Failure reported by the platform keystore.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum KeystoreError {
    #[error("no secret is stored in this slot")]
    Missing,
    #[error("the Android keystore is unavailable")]
    Unavailable,
    #[error("the stored secret could not be decrypted")]
    Corrupt,
    #[error("invalid secret slot name")]
    InvalidSlot,
}

impl From<uniffi::UnexpectedUniFFICallbackError> for KeystoreError {
    fn from(_: uniffi::UnexpectedUniFFICallbackError) -> Self {
        // The payload is deliberately ignored: it must never be formatted.
        Self::Unavailable
    }
}

/// Encrypted slot storage implemented by Kotlin on top of the Android Keystore.
#[uniffi::export(with_foreign)]
pub trait PlatformKeystore: Send + Sync {
    fn wrap(&self, slot: String, plain: Vec<u8>) -> Result<(), KeystoreError>;
    fn unwrap(&self, slot: String) -> Result<Vec<u8>, KeystoreError>;
    /// A missing slot is `Ok(())`.
    fn delete(&self, slot: String) -> Result<(), KeystoreError>;
    fn contains(&self, slot: String) -> Result<bool, KeystoreError>;
}

/// Slot names match `^[a-z0-9][a-z0-9-]{0,63}$`.
pub(crate) fn is_valid_slot(slot: &str) -> bool {
    let bytes = slot.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    bytes.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && rest
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// Keystore slot that holds the secret of a provider route.
pub(crate) fn route_slot(route_id: &str) -> String {
    format!("{ROUTE_SLOT_PREFIX}{route_id}")
}

/// Why a secret could not be stored or resolved. Carries no secret material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretError {
    InvalidSlot,
    Missing,
    Unavailable,
    Corrupt,
    Empty,
    NotText,
    TooLarge,
}

impl SecretError {
    /// Fixed English sentence, safe to log and to show.
    pub(crate) fn diagnostic(self) -> &'static str {
        match self {
            Self::InvalidSlot => "invalid secret reference",
            Self::Missing => "no stored secret for this provider route",
            Self::Unavailable => "the Android keystore is unavailable",
            Self::Corrupt => "the stored secret could not be decrypted",
            Self::Empty => "the stored secret is empty",
            Self::NotText => "the stored secret is not text",
            Self::TooLarge => "the stored secret is too large",
        }
    }
}

impl From<KeystoreError> for SecretError {
    fn from(error: KeystoreError) -> Self {
        match error {
            KeystoreError::Missing => Self::Missing,
            KeystoreError::Unavailable => Self::Unavailable,
            KeystoreError::Corrupt => Self::Corrupt,
            KeystoreError::InvalidSlot => Self::InvalidSlot,
        }
    }
}

/// Resolves and manages route secrets through the platform keystore.
#[derive(Clone)]
pub struct KeystoreSecretResolver {
    keystore: Arc<dyn PlatformKeystore>,
}

impl fmt::Debug for KeystoreSecretResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeystoreSecretResolver")
            .finish_non_exhaustive()
    }
}

impl KeystoreSecretResolver {
    pub fn new(keystore: Arc<dyn PlatformKeystore>) -> Self {
        Self { keystore }
    }

    pub(crate) fn resolve_slot(&self, slot: &str) -> Result<SecretString, SecretError> {
        if !is_valid_slot(slot) {
            return Err(SecretError::InvalidSlot);
        }
        let bytes = Zeroizing::new(self.keystore.unwrap(slot.to_owned())?);
        if bytes.len() > MAX_SECRET_BYTES {
            return Err(SecretError::TooLarge);
        }
        let start = bytes
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(bytes.len());
        let end = bytes
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(start, |i| i + 1);
        let trimmed = &bytes[start..end];
        if trimmed.is_empty() {
            return Err(SecretError::Empty);
        }
        let text = std::str::from_utf8(trimmed).map_err(|_| SecretError::NotText)?;
        Ok(SecretString::from(text.to_owned()))
    }

    pub(crate) fn store(
        &self,
        slot: &str,
        mut secret: Zeroizing<Vec<u8>>,
    ) -> Result<(), SecretError> {
        if !is_valid_slot(slot) {
            return Err(SecretError::InvalidSlot);
        }
        if secret.is_empty() {
            return Err(SecretError::Empty);
        }
        if secret.len() > MAX_SECRET_BYTES {
            return Err(SecretError::TooLarge);
        }
        self.keystore
            .wrap(slot.to_owned(), std::mem::take(&mut *secret))?;
        Ok(())
    }

    pub(crate) fn remove(&self, slot: &str) -> Result<(), SecretError> {
        if !is_valid_slot(slot) {
            return Err(SecretError::InvalidSlot);
        }
        match self.keystore.delete(slot.to_owned()) {
            Ok(()) | Err(KeystoreError::Missing) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn has(&self, slot: &str) -> bool {
        is_valid_slot(slot) && self.keystore.contains(slot.to_owned()).unwrap_or(false)
    }
}

#[cfg(feature = "local-host")]
impl harw_provider_http::SecretResolver for KeystoreSecretResolver {
    fn resolve(&self, reference: &str) -> Result<SecretString, String> {
        if !reference.starts_with(ROUTE_SLOT_PREFIX) {
            return Err("only provider route secrets resolve on the phone".to_owned());
        }
        self.resolve_slot(reference)
            .map_err(|e| e.diagnostic().to_owned())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use super::{KeystoreError, PlatformKeystore};

    /// In-memory keystore for tests.
    pub(crate) struct MemoryKeystore {
        pub(crate) slots: Mutex<HashMap<String, Vec<u8>>>,
        fail: Mutex<Option<KeystoreError>>,
    }

    impl MemoryKeystore {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                slots: Mutex::new(HashMap::new()),
                fail: Mutex::new(None),
            })
        }

        pub(crate) fn stored(&self, slot: &str) -> Option<Vec<u8>> {
            self.slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(slot)
                .cloned()
        }

        /// Every call returns this error while it is set.
        pub(crate) fn fail_next(&self, e: KeystoreError) {
            *self.fail.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
        }

        fn check(&self) -> Result<(), KeystoreError> {
            match *self.fail.lock().unwrap_or_else(PoisonError::into_inner) {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    impl PlatformKeystore for MemoryKeystore {
        fn wrap(&self, slot: String, plain: Vec<u8>) -> Result<(), KeystoreError> {
            self.check()?;
            self.slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(slot, plain);
            Ok(())
        }

        fn unwrap(&self, slot: String) -> Result<Vec<u8>, KeystoreError> {
            self.check()?;
            self.stored(&slot).ok_or(KeystoreError::Missing)
        }

        fn delete(&self, slot: String) -> Result<(), KeystoreError> {
            self.check()?;
            self.slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&slot);
            Ok(())
        }

        fn contains(&self, slot: String) -> Result<bool, KeystoreError> {
            self.check()?;
            Ok(self
                .slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&slot))
        }
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::test_support::MemoryKeystore;
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn resolver() -> (Arc<MemoryKeystore>, KeystoreSecretResolver) {
        let keystore = MemoryKeystore::new();
        let resolver = KeystoreSecretResolver::new(keystore.clone());
        (keystore, resolver)
    }

    #[test]
    fn store_then_resolve_round_trips_trimmed_value() -> TestResult {
        let (keystore, resolver) = resolver();
        resolver
            .store("route-a", Zeroizing::new(b"  sk-test-123\n".to_vec()))
            .map_err(|e| e.diagnostic())?;
        assert!(resolver.has("route-a"));
        assert!(keystore.stored("route-a").is_some());
        let secret = resolver
            .resolve_slot("route-a")
            .map_err(|e| e.diagnostic())?;
        assert_eq!(secret.expose_secret(), "sk-test-123");
        resolver.remove("route-a").map_err(|e| e.diagnostic())?;
        assert!(!resolver.has("route-a"));
        resolver.remove("route-a").map_err(|e| e.diagnostic())?;
        Ok(())
    }

    #[test]
    fn missing_slot_is_missing() {
        let (_keystore, resolver) = resolver();
        assert!(matches!(
            resolver.resolve_slot("route-none"),
            Err(SecretError::Missing)
        ));
    }

    #[test]
    fn invalid_slots_never_reach_the_keystore() {
        let (keystore, resolver) = resolver();
        let long = "a".repeat(65);
        for slot in ["", "../x", "Route-A", "-a", long.as_str()] {
            assert!(!is_valid_slot(slot), "{slot}");
            assert!(matches!(
                resolver.resolve_slot(slot),
                Err(SecretError::InvalidSlot)
            ));
            assert!(matches!(
                resolver.store(slot, Zeroizing::new(b"x".to_vec())),
                Err(SecretError::InvalidSlot)
            ));
            assert!(matches!(
                resolver.remove(slot),
                Err(SecretError::InvalidSlot)
            ));
            assert!(!resolver.has(slot));
        }
        assert!(is_valid_slot(&"a".repeat(64)));
        assert!(keystore.stored("").is_none());
        assert!(
            keystore
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    #[test]
    fn oversize_empty_and_non_text_are_rejected() -> TestResult {
        let (keystore, resolver) = resolver();
        assert!(matches!(
            resolver.store("route-a", Zeroizing::new(vec![b'x'; MAX_SECRET_BYTES + 1])),
            Err(SecretError::TooLarge)
        ));
        assert!(matches!(
            resolver.store("route-a", Zeroizing::new(Vec::new())),
            Err(SecretError::Empty)
        ));
        keystore.wrap("route-big".to_owned(), vec![b'x'; MAX_SECRET_BYTES + 1])?;
        assert!(matches!(
            resolver.resolve_slot("route-big"),
            Err(SecretError::TooLarge)
        ));
        keystore.wrap("route-blank".to_owned(), b" \t\n ".to_vec())?;
        assert!(matches!(
            resolver.resolve_slot("route-blank"),
            Err(SecretError::Empty)
        ));
        keystore.wrap("route-bin".to_owned(), vec![0xff, 0xfe, b'k'])?;
        assert!(matches!(
            resolver.resolve_slot("route-bin"),
            Err(SecretError::NotText)
        ));
        Ok(())
    }

    #[test]
    fn keystore_failures_map_and_have_is_false() {
        let (keystore, resolver) = resolver();
        keystore.fail_next(KeystoreError::Unavailable);
        assert!(matches!(
            resolver.resolve_slot("route-a"),
            Err(SecretError::Unavailable)
        ));
        assert!(!resolver.has("route-a"));
        keystore.fail_next(KeystoreError::Corrupt);
        assert!(matches!(
            resolver.resolve_slot("route-a"),
            Err(SecretError::Corrupt)
        ));
    }

    #[test]
    fn diagnostics_never_contain_the_secret() {
        let secret = "sk-very-secret-value";
        let all = [
            SecretError::InvalidSlot,
            SecretError::Missing,
            SecretError::Unavailable,
            SecretError::Corrupt,
            SecretError::Empty,
            SecretError::NotText,
            SecretError::TooLarge,
        ];
        for error in all {
            assert!(!error.diagnostic().contains(secret));
        }
    }

    #[cfg(feature = "local-host")]
    #[test]
    fn provider_http_resolver_only_resolves_route_slots() -> TestResult {
        use harw_provider_http::SecretResolver;

        let (_keystore, resolver) = resolver();
        let value = "sk-never-leak";
        resolver
            .store("route-abc", Zeroizing::new(value.as_bytes().to_vec()))
            .map_err(|e| e.diagnostic())?;
        let secret = SecretResolver::resolve(&resolver, "route-abc")?;
        assert_eq!(secret.expose_secret(), value);
        let err = SecretResolver::resolve(&resolver, "identity")
            .err()
            .ok_or("identity must not resolve")?;
        assert!(!err.contains(value));
        Ok(())
    }
}
