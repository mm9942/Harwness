//! KEK provenance and key management (spec §3). The KEK secret half must
//! survive restarts without touching disk in plaintext, or be re-derivable
//! identically on every start. v1 default is a `0600` key file; OS keyring and
//! env-seed are alternatives.
//!
//! The `0600` refuse-to-start permission check is implemented in full (it is
//! the one behavior the doc marks as a hard startup gate). Hybrid v3 PQ HPKE
//! recipient-key derivation from the 32-byte seed goes through `crypt_guard`
//! (`RecipientPrivateKey::from_seed_bytes` + `public_key`) after a per-KEM
//! SHA-256 domain separation of the root seed (`derive_kem_seed`); OS keyring
//! retrieval goes through the optional `keyring` crate.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::fs::{File, OpenOptions};
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::io::Read;

use crypt_guard::pq_hpke::{RecipientPrivateKey, RecipientPublicKey};
use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretBox};
use sha2::digest::generic_array::GenericArray;
use sha2::{Digest, Sha256};

use crate::error::{SecretsError, SecretsResult};
use crate::policy::{CryptoPolicy, KemAlgo};
use crate::store::KekMaterial;

/// Length of the root KEK seed and of every per-KEM hybrid recipient seed
/// (`HYBRID_SEED_BYTES` in `crypt_guard-3.0.1/src/hpke_pq/mod.rs:1698`).
pub(crate) const KEK_SEED_LEN: usize = 32;

/// Domain-separation prefix for per-KEM recipient seeds. The full context is
/// `KEM_SEED_CONTEXT ‖ KemAlgo::wire_name ‖ 0x00`; changing any part makes
/// every persisted store unreadable.
const KEM_SEED_CONTEXT: &[u8] = b"harw-secrets kek seed v2 ";

/// Where the KEK seed / secret half comes from (§3). Selected via config, never
/// auto-detected, so headless behavior stays predictable.
#[derive(Clone, Debug)]
pub enum KekProvenance {
    /// Default: a 32-byte seed at a `0600` file path.
    KeyFile { path: PathBuf },
    /// Opt-in OS keyring (Secret Service / Keychain / Credential Manager).
    OsKeyring { service: String, account: String },
    /// CI/headless override: seed supplied via an environment variable.
    EnvSeed { var: String },
}

/// Refuse to start if the KEK key file is readable/writable by group or other
/// (§3).
///
/// On Linux and Android, this opens the requested path without following a
/// final symlink and validates the resulting file handle. [`load_seed`] uses
/// the same handle-validation path before reading, so permission validation
/// and the eventual read cannot observe different files.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn check_key_file_permissions(path: &Path) -> SecretsResult<()> {
    let file = open_key_file(path)?;
    validate_key_file(path, &file)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_key_file(path: &Path) -> SecretsResult<File> {
    use std::os::unix::fs::OpenOptionsExt;

    // Linux's `O_NOFOLLOW` prevents the final component from being a symlink.
    // Other targets fail closed below rather than relying on an unverified
    // platform-specific flag value.
    const O_NOFOLLOW: i32 = 0o400_000;

    OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .map_err(SecretsError::Io)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn validate_key_file(path: &Path, file: &File) -> SecretsResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = file.metadata().map_err(SecretsError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(SecretsError::KekUnavailable {
            kind: "key-file".to_owned(),
            reason: format!("'{}' is not a regular file", path.display()),
        });
    }
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(SecretsError::UnsafeKeyFilePermissions {
            path: path.display().to_string(),
            mode,
        });
    }
    Ok(())
}

/// Refuse key-file provenance on targets without the verified Linux/Android
/// no-follow and mode-bit guarantees required for this security boundary.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn check_key_file_permissions(_path: &Path) -> SecretsResult<()> {
    Err(SecretsError::KekUnavailable {
        kind: "key-file".to_owned(),
        reason: "key-file KEK loading requires verified no-follow and permission validation"
            .to_owned(),
    })
}

/// Load the raw KEK seed for `provenance` into a zeroizing `SecretBox`.
///
/// The key-file path validates the opened `0600` file handle before reading
/// from that same handle; the env-seed path reports
/// [`SecretsError::KekUnavailable`] when the variable is unset. OS-keyring
/// retrieval is available when this crate is built with its `keyring` feature.
pub fn load_seed(provenance: &KekProvenance) -> SecretsResult<SecretBox<[u8]>> {
    match provenance {
        KekProvenance::KeyFile { path } => load_key_file_seed(path),
        KekProvenance::EnvSeed { var } => {
            let value = std::env::var_os(var).ok_or_else(|| SecretsError::KekUnavailable {
                kind: "env-seed".to_owned(),
                reason: format!("environment variable '{var}' is unavailable"),
            })?;
            env_seed_from_os_value(var, &value)
        }
        KekProvenance::OsKeyring { service, account } => load_os_keyring_seed(service, account),
    }
}

/// Validiert einen bereits gelesenen Umgebungswert und legt ihn in einen
/// löschenden `SecretBox`.
///
/// # Description
/// Dies ist die reine Hälfte des `EnvSeed`-Zweigs von [`load_seed`]: der
/// Prozesszustand (`std::env::var_os`) bleibt beim Aufrufer, die Prüfung steht
/// hier. Der Wert wird bewusst **nicht** als gewöhnlicher `String`
/// materialisiert — die OS-Bytes werden geliehen, als UTF-8 geprüft und
/// direkt in den löschenden Container kopiert.
///
/// Die Trennung existiert, damit die Prüfung ohne `std::env::set_var` testbar
/// ist. `set_var` ist seit Edition 2024 `unsafe`, weil es prozessglobalen
/// Zustand neben laufenden Threads verändert; ein Test, der es braucht, würde
/// dem gesamten Crate `unsafe` aufzwingen.
///
/// # Arguments
/// - `var` (`&str`): Name der Variablen, nur für die Fehlermeldung.
/// - `value` (`&OsStr`): der gelesene Wert.
///
/// # Returns
/// `SecretBox<[u8]>` mit genau 32 Bytes.
///
/// # Errors
/// - [`SecretsError::KekUnavailable`]: der Wert ist kein gültiges UTF-8 oder
///   nicht genau 32 Bytes lang. Der Wert selbst erscheint in keiner Meldung.
fn env_seed_from_os_value(var: &str, value: &OsStr) -> SecretsResult<SecretBox<[u8]>> {
    let seed =
        std::str::from_utf8(value.as_encoded_bytes()).map_err(|_| SecretsError::KekUnavailable {
            kind: "env-seed".to_owned(),
            reason: format!("environment variable '{var}' is not valid UTF-8"),
        })?;
    if seed.len() != 32 {
        return Err(SecretsError::KekUnavailable {
            kind: "env-seed".to_owned(),
            reason: format!("expected exactly 32 UTF-8 bytes, found {}", seed.len()),
        });
    }
    Ok(SecretBox::new(seed.as_bytes().to_vec().into_boxed_slice()))
}

#[cfg(feature = "keyring")]
fn load_os_keyring_seed(service: &str, account: &str) -> SecretsResult<SecretBox<[u8]>> {
    let entry =
        keyring::Entry::new(service, account).map_err(|_| SecretsError::KekUnavailable {
            kind: "os-keyring".to_owned(),
            reason: format!(
            "could not initialize OS-keyring entry for service '{service}' and account '{account}'"
        ),
        })?;
    let secret = entry
        .get_secret()
        .map_err(|_| SecretsError::KekUnavailable {
            kind: "os-keyring".to_owned(),
            reason: format!(
            "could not retrieve OS-keyring secret for service '{service}' and account '{account}'"
        ),
        })?;

    secret_box_from_exact_32_bytes("os-keyring", secret)
}

#[cfg(not(feature = "keyring"))]
fn load_os_keyring_seed(_service: &str, _account: &str) -> SecretsResult<SecretBox<[u8]>> {
    Err(SecretsError::KekUnavailable {
        kind: "os-keyring".to_owned(),
        reason: "OS-keyring KEK loading was built without the `keyring` feature".to_owned(),
    })
}

fn secret_box_from_exact_32_bytes(kind: &str, secret: Vec<u8>) -> SecretsResult<SecretBox<[u8]>> {
    if secret.len() != 32 {
        return Err(SecretsError::KekUnavailable {
            kind: kind.to_owned(),
            reason: format!("expected exactly 32 bytes, found {}", secret.len()),
        });
    }
    Ok(SecretBox::new(secret.into_boxed_slice()))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn load_key_file_seed(path: &Path) -> SecretsResult<SecretBox<[u8]>> {
    let mut file = open_key_file(path)?;
    validate_key_file(path, &file)?;

    let mut seed = Vec::with_capacity(32);
    file.read_to_end(&mut seed).map_err(SecretsError::Io)?;
    if seed.len() != 32 {
        return Err(SecretsError::KekUnavailable {
            kind: "key-file".to_owned(),
            reason: format!("expected exactly 32 raw bytes, found {}", seed.len()),
        });
    }
    Ok(SecretBox::new(seed.into_boxed_slice()))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn load_key_file_seed(path: &Path) -> SecretsResult<SecretBox<[u8]>> {
    check_key_file_permissions(path)?;
    unreachable!("unsupported-platform key-file validation always fails closed")
}

/// Load the configured KEK seed once and deterministically derive its paired
/// hybrid PQ HPKE material for `policy`.
///
/// This is the provenance-to-store boundary: the seed remains in its
/// zeroizing container while the hybrid recipient public key is derived
/// exactly once, then the deterministic seed is retained in [`KekMaterial`]'s
/// zeroizing container.
///
/// # Errors
/// - [`SecretsError::KekUnavailable`]: the provenance cannot supply exactly 32
///   seed bytes.
/// - [`SecretsError::UnsupportedLegacyKem`]: `policy` names a retired pure
///   ML-KEM level.
/// - [`SecretsError::KekDerivation`]: `crypt_guard` rejected the derivation.
pub fn load_kek_material(
    policy: &CryptoPolicy,
    provenance: &KekProvenance,
) -> SecretsResult<KekMaterial> {
    let seed = load_seed(provenance)?;
    let public_key = derive_public_key(policy, &seed)?;

    // `KekMaterial` retains the original 32-byte root seed. Opening an
    // envelope derives the per-KEM recipient seed from it on demand
    // (`derive_kem_seed` with the record's KEM), so records of every hybrid
    // KEM stay openable and nothing KEM-specific is retained here.
    KekMaterial::new(public_key, seed)
}

/// Derive the KEK's hybrid PQ HPKE public key (needed to seal) from `seed` per
/// `policy`.
///
/// The derivation is deterministic: the same root seed and the same KEM always
/// yield the same public-key bytes. The recipient key is derived from the
/// domain-separated per-KEM seed (`derive_kem_seed`), never from the root
/// seed directly, so no two hybrid KEMs share key material.
///
/// # Errors
/// - [`SecretsError::KekUnavailable`]: `seed` is not exactly 32 bytes long.
/// - [`SecretsError::UnsupportedLegacyKem`]: `policy` names a retired pure
///   ML-KEM level.
/// - [`SecretsError::KekDerivation`]: `crypt_guard` rejected the seed or the
///   derivation.
pub fn derive_public_key(policy: &CryptoPolicy, seed: &SecretBox<[u8]>) -> SecretsResult<Vec<u8>> {
    let (_, public_key) = derive_recipient(policy, seed)?;
    Ok(public_key.as_bytes().to_vec())
}

/// Derive the KEM-specific hybrid recipient secret seed for `policy.kem` from
/// the root KEK `seed`, returned in a zeroizing `SecretBox`.
///
/// The result is **not** the root KEK seed: it is the domain-separated seed
/// from `derive_kem_seed`, i.e. exactly the bytes
/// `crypt_guard::pq_hpke::RecipientPrivateKey::from_seed_bytes` expects for
/// `policy.kem`. The public-key derivation still runs so an unusable seed is
/// reported here.
///
/// # Security
/// - The returned bytes are the complete recipient private key for
///   `policy.kem`: they open every envelope wrapped for that KEM under this
///   KEK generation. Never log, persist, or transmit them.
/// - Do not pass them to [`crate::envelope::open`] or
///   [`KekMaterial::new`]: both take the **root** KEK seed and apply the domain
///   separation themselves; a derived seed would be derived a second time and
///   fail to open anything.
/// - The root KEK seed is strictly more sensitive (it yields the recipient
///   keys of all hybrid KEMs); it is only held inside [`KekMaterial`].
///
/// # Errors
/// Same as [`derive_public_key`].
pub fn derive_secret_key(
    policy: &CryptoPolicy,
    seed: &SecretBox<[u8]>,
) -> SecretsResult<SecretBox<[u8]>> {
    let (private_key, _) = derive_recipient(policy, seed)?;
    Ok(SecretBox::new(private_key.as_seed_bytes().to_vec().into_boxed_slice()))
}

/// Derive the 32-byte hybrid recipient seed of `kem` from the root KEK seed.
///
/// `SHA-256("harw-secrets kek seed v2 " ‖ kem.wire_name() ‖ 0x00 ‖ seed)`.
/// Without this step crypt_guard 3.0.1 would expand the same root seed to the
/// *same* ML-KEM-768 key for `MlKem768P256` and `MlKem768X25519`
/// (`SHAKE256(seed)[..64]` in both, `hpke_pq/mod.rs:2238-2250`, `:2350-2360`).
/// Every production path that calls `RecipientPrivateKey::from_seed_bytes`
/// must go through this function.
///
/// The caller is responsible for checking that `seed` is exactly
/// [`KEK_SEED_LEN`] bytes; this function hashes whatever it is given.
pub(crate) fn derive_kem_seed(kem: KemAlgo, seed: &[u8]) -> Zeroizing<[u8; KEK_SEED_LEN]> {
    let mut hasher = Sha256::new();
    hasher.update(KEM_SEED_CONTEXT);
    hasher.update(kem.wire_name().as_bytes());
    hasher.update([0_u8]);
    hasher.update(seed);
    let mut derived = Zeroizing::new([0_u8; KEK_SEED_LEN]);
    // Write straight into the zeroizing buffer instead of returning a
    // non-zeroizing digest copy.
    hasher.finalize_into(GenericArray::from_mut_slice(&mut derived[..]));
    derived
}

/// Shared derivation: validate the root seed, resolve the KEM, derive the
/// per-KEM seed, build the private key from it, and compute the matching
/// hybrid public key.
fn derive_recipient(
    policy: &CryptoPolicy,
    seed: &SecretBox<[u8]>,
) -> SecretsResult<(RecipientPrivateKey, RecipientPublicKey)> {
    let seed = validated_seed(seed)?;
    let kem = policy.kem.hpke_kem()?;
    let kem_seed = derive_kem_seed(policy.kem, seed);
    let private_key = RecipientPrivateKey::from_seed_bytes(kem, &kem_seed[..])
        .map_err(|source| SecretsError::KekDerivation { source })?;
    let public_key = private_key
        .public_key()
        .map_err(|source| SecretsError::KekDerivation { source })?;
    Ok((private_key, public_key))
}

/// Check the root seed length without copying the seed bytes out of their
/// zeroizing container.
fn validated_seed(seed: &SecretBox<[u8]>) -> SecretsResult<&[u8]> {
    let bytes: &[u8] = seed.expose_secret();
    if bytes.len() != KEK_SEED_LEN {
        return Err(SecretsError::KekUnavailable {
            kind: "seed".to_owned(),
            reason: "expected exactly 32 bytes for deterministic hybrid PQ HPKE derivation"
                .to_owned(),
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};

    use secrecy::ExposeSecret;

    use crate::policy::{AeadAlgo, KemAlgo};

    use super::*;

    fn temp_seed_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after UNIX_EPOCH")
            .as_nanos();
        std::env::temp_dir().join(format!("harwness-kek-seed-{}-{nonce}", std::process::id()))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn key_file_loads_exactly_32_raw_bytes() {
        let path = temp_seed_path();
        let seed = [0xA5; 32];
        std::fs::write(&path, seed).expect("write temporary seed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("set temporary seed permissions");
        }

        let loaded =
            load_seed(&KekProvenance::KeyFile { path: path.clone() }).expect("load key-file seed");
        assert_eq!(loaded.expose_secret().as_ref(), seed.as_slice());
        std::fs::remove_file(path).expect("remove temporary seed");
    }

    #[test]
    fn key_file_rejects_wrong_length() {
        let path = temp_seed_path();
        std::fs::write(&path, [0u8; 31]).expect("write temporary seed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("set temporary seed permissions");
        }

        assert!(matches!(
            load_seed(&KekProvenance::KeyFile { path: path.clone() }),
            Err(SecretsError::KekUnavailable { .. })
        ));
        std::fs::remove_file(path).expect("remove temporary seed");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn key_file_rejects_a_symlink_instead_of_following_it() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let target = temp_seed_path();
        let link = temp_seed_path();
        std::fs::write(&target, [0xC3; 32]).expect("write temporary seed target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("set temporary seed target permissions");
        symlink(&target, &link).expect("create temporary seed symlink");

        assert!(matches!(
            load_seed(&KekProvenance::KeyFile { path: link.clone() }),
            Err(SecretsError::Io(_))
        ));

        std::fs::remove_file(link).expect("remove temporary seed symlink");
        std::fs::remove_file(target).expect("remove temporary seed target");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn key_file_rejects_group_or_other_permissions_from_opened_handle() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_seed_path();
        std::fs::write(&path, [0x6D; 32]).expect("write temporary seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
            .expect("set unsafe temporary seed permissions");

        assert!(matches!(
            load_seed(&KekProvenance::KeyFile { path: path.clone() }),
            Err(SecretsError::UnsafeKeyFilePermissions { mode, .. }) if mode & 0o777 == 0o640
        ));

        std::fs::remove_file(path).expect("remove temporary seed");
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    #[test]
    fn key_file_provenance_fails_closed_without_verified_handle_guarantees() {
        assert!(matches!(
            load_seed(&KekProvenance::KeyFile {
                path: PathBuf::from("unused-kek-seed"),
            }),
            Err(SecretsError::KekUnavailable { .. })
        ));
    }

    #[test]
    fn test_env_seed_from_os_value_loads_exactly_32_utf8_bytes() {
        let value = OsString::from("12345678901234567890123456789012");

        let loaded = env_seed_from_os_value("HARWNESS_TEST_KEK", &value).expect("load env seed");

        assert_eq!(
            loaded.expose_secret().as_ref(),
            b"12345678901234567890123456789012".as_slice()
        );
    }

    #[test]
    fn test_env_seed_from_os_value_rejects_wrong_length() {
        let value = OsString::from("too-short");

        assert!(matches!(
            env_seed_from_os_value("HARWNESS_TEST_KEK", &value),
            Err(SecretsError::KekUnavailable { reason, .. })
                if reason.contains("found 9") && !reason.contains("too-short")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_env_seed_from_os_value_rejects_non_utf8() {
        // Diesen Fall konnte der vorherige Test gar nicht erreichen: über
        // `set_var` mit einem `&str` ist der Wert immer gültiges UTF-8.
        use std::os::unix::ffi::OsStringExt;

        let value = OsString::from_vec(vec![0xFF; 32]);

        assert!(matches!(
            env_seed_from_os_value("HARWNESS_TEST_KEK", &value),
            Err(SecretsError::KekUnavailable { reason, .. }) if reason.contains("not valid UTF-8")
        ));
    }

    #[test]
    fn test_load_seed_reports_unavailable_for_unset_variable() {
        let var = format!("HARWNESS_TEST_KEK_UNSET_{}", std::process::id());

        assert!(matches!(
            load_seed(&KekProvenance::EnvSeed { var }),
            Err(SecretsError::KekUnavailable { reason, .. }) if reason.contains("unavailable")
        ));
    }

    #[test]
    fn exact_32_byte_secret_validator_places_secret_in_secret_box() {
        let secret = secret_box_from_exact_32_bytes("os-keyring", vec![0x7E; 32])
            .expect("32-byte secret is accepted");

        assert_eq!(secret.expose_secret().as_ref(), [0x7E; 32].as_slice());
    }

    #[test]
    fn exact_32_byte_secret_validator_rejects_wrong_length_without_reporting_secret() {
        let error = match secret_box_from_exact_32_bytes("os-keyring", vec![0x42; 31]) {
            Err(error) => error,
            Ok(_) => panic!("31-byte secret is rejected"),
        };

        assert!(matches!(
            error,
            SecretsError::KekUnavailable { kind, reason }
                if kind == "os-keyring"
                    && reason == "expected exactly 32 bytes, found 31"
        ));
    }

    #[cfg(not(feature = "keyring"))]
    #[test]
    fn os_keyring_provenance_reports_when_keyring_feature_is_disabled() {
        assert!(matches!(
            load_seed(&KekProvenance::OsKeyring {
                service: "harwness-test".to_owned(),
                account: "test-account".to_owned(),
            }),
            Err(SecretsError::KekUnavailable { kind, reason })
                if kind == "os-keyring"
                    && reason == "OS-keyring KEK loading was built without the `keyring` feature"
        ));
    }

    fn test_seed(byte: u8) -> SecretBox<[u8]> {
        SecretBox::new(vec![byte; 32].into_boxed_slice())
    }

    fn hybrid_policies() -> [CryptoPolicy; 3] {
        [
            CryptoPolicy::ml_kem_768_p256(),
            CryptoPolicy::ml_kem_768_x25519(),
            CryptoPolicy::strongest(),
        ]
    }

    /// crypt_guard public key for the domain-separated per-KEM seed, i.e. what
    /// production derivation must produce.
    fn crypt_guard_public_key(policy: &CryptoPolicy, seed: &[u8; 32]) -> Vec<u8> {
        crypt_guard_raw_public_key(policy, &derive_kem_seed(policy.kem, seed))
    }

    /// crypt_guard public key for `seed` used verbatim, without harw's domain
    /// separation (control value only).
    fn crypt_guard_raw_public_key(policy: &CryptoPolicy, seed: &[u8; 32]) -> Vec<u8> {
        RecipientPrivateKey::from_seed_bytes(policy.kem.hpke_kem().expect("hybrid KEM"), seed)
            .expect("hybrid recipient private key from seed")
            .public_key()
            .expect("hybrid recipient public key")
            .as_bytes()
            .to_vec()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn deterministic_derivation_is_stable_for_a_seed_and_policy() {
        let policy = CryptoPolicy::ml_kem_768_x25519();
        let seed = test_seed(0xA5);

        let public_first = derive_public_key(&policy, &seed).expect("derive first public key");
        let secret_first = derive_secret_key(&policy, &seed).expect("derive first secret key");
        let public_second = derive_public_key(&policy, &seed).expect("derive second public key");
        let secret_second = derive_secret_key(&policy, &seed).expect("derive second secret key");

        assert!(!public_first.is_empty());
        assert_eq!(public_first, crypt_guard_public_key(&policy, &[0xA5; 32]));
        // The secret key is the domain-separated per-KEM seed, never the root
        // KEK seed.
        assert_eq!(
            secret_first.expose_secret().as_ref(),
            derive_kem_seed(policy.kem, &[0xA5; 32]).as_slice()
        );
        assert_ne!(secret_first.expose_secret().as_ref(), [0xA5; 32].as_slice());
        assert_eq!(public_first, public_second);
        assert_eq!(secret_first.expose_secret(), secret_second.expose_secret());
    }

    #[test]
    fn changing_the_seed_changes_derived_key_material() {
        let policy = CryptoPolicy::ml_kem_768_p256();
        let first_seed = test_seed(0x11);
        let second_seed = test_seed(0x22);

        assert_ne!(
            derive_public_key(&policy, &first_seed).expect("derive first public key"),
            derive_public_key(&policy, &second_seed).expect("derive second public key"),
        );
        assert_ne!(
            derive_secret_key(&policy, &first_seed)
                .expect("derive first secret key")
                .expose_secret(),
            derive_secret_key(&policy, &second_seed)
                .expect("derive second secret key")
                .expose_secret(),
        );
    }

    #[test]
    fn every_hybrid_kem_policy_derives_stable_hpke_material() {
        let seed = test_seed(0x5A);
        let mut public_keys = Vec::new();

        for policy in hybrid_policies() {
            let public = derive_public_key(&policy, &seed).expect("derive public key");
            let secret = derive_secret_key(&policy, &seed).expect("derive secret key");

            assert!(!public.is_empty());
            assert_eq!(public, crypt_guard_public_key(&policy, &[0x5A; 32]));
            assert_eq!(
                secret.expose_secret().as_ref(),
                derive_kem_seed(policy.kem, &[0x5A; 32]).as_slice()
            );
            assert_eq!(
                public,
                derive_public_key(&policy, &seed).expect("re-derive public key")
            );
            assert_eq!(
                secret.expose_secret(),
                derive_secret_key(&policy, &seed)
                    .expect("re-derive secret key")
                    .expose_secret()
            );
            public_keys.push(public);
        }

        // Full keys differ per KEM. That the ML-KEM-768 halves of the P-256
        // and X25519 variants differ as well is asserted separately in
        // `ml_kem_768_p256_and_x25519_do_not_share_the_ml_kem_key`.
        assert_ne!(public_keys[0], public_keys[1]);
        assert_ne!(public_keys[0], public_keys[2]);
        assert_ne!(public_keys[1], public_keys[2]);
    }

    /// `ML_KEM_768_PUBLIC_KEY_BYTES` is `pub(crate)` in crypt_guard
    /// (`hpke_pq/mod.rs:363`), hence mirrored here. Hybrid public keys are
    /// `ML-KEM key ‖ classical point` (`hpke_pq/mod.rs:2238-2264`, `:2350-2367`).
    const ML_KEM_768_PUBLIC_KEY_BYTES: usize = 1_184;

    #[test]
    fn ml_kem_768_p256_and_x25519_do_not_share_the_ml_kem_key() {
        let seed = test_seed(0x5A);
        let p256_policy = CryptoPolicy::ml_kem_768_p256();
        let x25519_policy = CryptoPolicy::ml_kem_768_x25519();

        let p256 = derive_public_key(&p256_policy, &seed).expect("derive P-256 public key");
        let x25519 = derive_public_key(&x25519_policy, &seed).expect("derive X25519 public key");

        assert_eq!(p256.len(), ML_KEM_768_PUBLIC_KEY_BYTES + 65);
        assert_eq!(x25519.len(), ML_KEM_768_PUBLIC_KEY_BYTES + 32);
        assert_ne!(
            p256[..ML_KEM_768_PUBLIC_KEY_BYTES],
            x25519[..ML_KEM_768_PUBLIC_KEY_BYTES],
            "domain separation must give each hybrid KEM its own ML-KEM key"
        );

        // Determinism is preserved.
        assert_eq!(
            p256,
            derive_public_key(&p256_policy, &seed).expect("re-derive P-256 public key")
        );
        assert_eq!(
            x25519,
            derive_public_key(&x25519_policy, &seed).expect("re-derive X25519 public key")
        );

        // Control: without the domain separation crypt_guard 3.0.1 expands the
        // root seed to the same ML-KEM-768 key for both variants.
        let raw_p256 = crypt_guard_raw_public_key(&p256_policy, &[0x5A; 32]);
        let raw_x25519 = crypt_guard_raw_public_key(&x25519_policy, &[0x5A; 32]);
        assert_eq!(
            raw_p256[..ML_KEM_768_PUBLIC_KEY_BYTES],
            raw_x25519[..ML_KEM_768_PUBLIC_KEY_BYTES]
        );
        assert_ne!(p256, raw_p256);
        assert_ne!(x25519, raw_x25519);
    }

    /// Fixed 32-byte seed shared by both known-answer tests.
    const KAT_SEED: [u8; 32] = [0xA5; 32];

    #[test]
    fn kem_seed_domain_separation_matches_independent_sha256_known_answers() {
        // Computed outside Rust (Python `hashlib.sha256` and coreutils
        // `sha256sum`, identical results) over
        // `b"harw-secrets kek seed v2 " + wire_name + b"\x00" + bytes([0xA5]) * 32`.
        for (kem, expected) in [
            (
                KemAlgo::MlKem768P256,
                "3a2a668ae08e165d95311b4f7a903ae0b391dfef51a5a7c1124b593ff1d9525c",
            ),
            (
                KemAlgo::MlKem1024P384,
                "eb1373c4b5548c51e71575ce45502fc28e945dacb1e253f2d01a379d349db16e",
            ),
            (
                KemAlgo::MlKem768X25519,
                "0d18909288ff23572828dcd32579a7e8ef4d47e0f2b6ac173efa3fe9800559bf",
            ),
        ] {
            assert_eq!(hex(derive_kem_seed(kem, &KAT_SEED).as_slice()), expected);
        }
    }

    /// Blessed fixture pinning crypt_guard's hybrid seed-to-public-key
    /// derivation (R1-01). Format: one `<kem wire name> <sha256 hex>` line per
    /// hybrid KEM.
    const KEM_KAT_FIXTURE: &str = "tests/fixtures/kem_kat.txt";

    /// Set to `1` exactly once, after reviewing a crypt_guard (re)pin, to write
    /// [`KEM_KAT_FIXTURE`] from the current derivation.
    const BLESS_ENV: &str = "HARW_BLESS";

    #[test]
    fn hybrid_public_key_derivation_matches_blessed_known_answers() {
        let seed = SecretBox::new(KAT_SEED.to_vec().into_boxed_slice());
        let actual: String = hybrid_policies()
            .iter()
            .map(|policy| {
                let public = derive_public_key(policy, &seed).expect("derive KAT public key");
                format!(
                    "{} {}\n",
                    policy.kem.wire_name(),
                    hex(&crate::audit::chain::sha256(&public))
                )
            })
            .collect();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(KEM_KAT_FIXTURE);

        if std::env::var_os(BLESS_ENV).is_some_and(|value| value == "1") {
            std::fs::create_dir_all(path.parent().expect("fixture directory"))
                .expect("create KEM KAT fixture directory");
            std::fs::write(&path, &actual).expect("write blessed KEM KAT fixture");
            return;
        }

        let expected = match std::fs::read_to_string(&path) {
            Ok(expected) => expected,
            Err(error) => panic!(
                "KEM known-answer fixture '{}' is missing or unreadable ({error}). \
                 Bless it once, after reviewing the crypt_guard pin, with \
                 `{BLESS_ENV}=1 cargo test -p harw-secrets \
                 hybrid_public_key_derivation_matches_blessed_known_answers`, \
                 then commit the file.",
                path.display()
            ),
        };
        assert_eq!(
            expected.trim_end(),
            actual.trim_end(),
            "hybrid public-key derivation changed (crypt_guard upgrade or harw domain \
             separation): every persisted secret store would become unreadable"
        );
    }

    #[test]
    fn deterministic_derivation_rejects_a_non_32_byte_seed() {
        for length in [0, 31, 33, 64] {
            let seed = SecretBox::new(vec![0u8; length].into_boxed_slice());

            for policy in hybrid_policies() {
                assert!(matches!(
                    derive_public_key(&policy, &seed),
                    Err(SecretsError::KekUnavailable { kind, .. }) if kind == "seed"
                ));
                assert!(matches!(
                    derive_secret_key(&policy, &seed),
                    Err(SecretsError::KekUnavailable { kind, .. }) if kind == "seed"
                ));
            }
        }
    }

    #[test]
    fn deterministic_derivation_refuses_legacy_pure_ml_kem_policies() {
        let seed = test_seed(0x42);

        for (kem, wire) in [
            (KemAlgo::LegacyMlKem512, "ml_kem_512"),
            (KemAlgo::LegacyMlKem768, "ml_kem_768"),
            (KemAlgo::LegacyMlKem1024, "ml_kem_1024"),
        ] {
            let policy = CryptoPolicy {
                kem,
                aead: AeadAlgo::XChaCha20Poly1305,
            };

            assert!(matches!(
                derive_public_key(&policy, &seed),
                Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == wire
            ));
            assert!(matches!(
                derive_secret_key(&policy, &seed),
                Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == wire
            ));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn loaded_kek_material_constructs_native_hpke_material() {
        let seed_path = temp_seed_path();
        std::fs::write(&seed_path, [0x3C; 32]).expect("write temporary seed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&seed_path, std::fs::Permissions::from_mode(0o600))
                .expect("set temporary seed permissions");
        }

        let policy = CryptoPolicy::strongest();
        let provenance = KekProvenance::KeyFile {
            path: seed_path.clone(),
        };
        let first_material =
            load_kek_material(&policy, &provenance).expect("derive first configured KEK material");
        let second_material =
            load_kek_material(&policy, &provenance).expect("derive second configured KEK material");
        assert_eq!(
            derive_public_key(&policy, &test_seed(0x3C)).expect("derive native public key"),
            derive_public_key(&policy, &test_seed(0x3C)).expect("re-derive native public key")
        );
        drop((first_material, second_material));

        std::fs::remove_file(seed_path).expect("remove temporary seed");
    }

    #[test]
    fn loading_kek_material_rejects_an_invalid_configured_seed() {
        let path = temp_seed_path();
        std::fs::write(&path, [0u8; 31]).expect("write temporary invalid seed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("set temporary seed permissions");
        }

        assert!(matches!(
            load_kek_material(
                &CryptoPolicy::strongest(),
                &KekProvenance::KeyFile { path: path.clone() },
            ),
            Err(SecretsError::KekUnavailable { .. })
        ));

        std::fs::remove_file(path).expect("remove temporary invalid seed");
    }
}
