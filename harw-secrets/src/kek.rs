//! KEK provenance and key management (spec §3). The KEK secret half must
//! survive restarts without touching disk in plaintext, or be re-derivable
//! identically on every start. v1 default is a `0600` key file; OS keyring and
//! env-seed are alternatives.
//!
//! The `0600` refuse-to-start permission check is implemented in full (it is
//! the one behavior the doc marks as a hard startup gate). Native v3 PQ HPKE
//! recipient-key derivation from the seed goes through `crypt_guard`; OS
//! keyring retrieval goes through the optional `keyring` crate.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::fs::{File, OpenOptions};
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::io::Read;

use crypt_guard::pq_hpke::{derive_recipient_key_pair, Kem};
use secrecy::{ExposeSecret, SecretBox};

use crate::error::{SecretsError, SecretsResult};
use crate::policy::{CryptoPolicy, KemAlgo};
use crate::store::KekMaterial;

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
/// native v3 PQ HPKE material for `policy`.
///
/// This is the provenance-to-store boundary: the seed remains in its
/// zeroizing container while the native recipient pair is derived exactly once,
/// then the deterministic seed is retained in [`KekMaterial`]'s zeroizing
/// container.
pub fn load_kek_material(
    policy: &CryptoPolicy,
    provenance: &KekProvenance,
) -> SecretsResult<KekMaterial> {
    let seed = load_seed(provenance)?;
    let public_key = derive_public_key(policy, &seed)?;

    // `KekMaterial` needs the original 32-byte provenance seed to reproduce
    // the recipient keypair while opening v3 envelopes. Do not retain the
    // expanded recipient private-key representation here.
    KekMaterial::new(public_key, seed)
}

/// Derive the KEK's native PQ HPKE public key (needed to seal) from `seed` per
/// `policy`.
pub fn derive_public_key(policy: &CryptoPolicy, seed: &SecretBox<[u8]>) -> SecretsResult<Vec<u8>> {
    let seed = validated_seed(seed)?;
    let key_pair = derive_recipient_key_pair(kem_for_policy(policy), &seed)
        .map_err(|source| SecretsError::KekDerivation { source })?;
    Ok(key_pair.public_key().as_bytes().to_vec())
}

/// Derive the KEK's native PQ HPKE secret key (needed to unseal) from `seed` per
/// `policy`, returned in a zeroizing `SecretBox`.
pub fn derive_secret_key(
    policy: &CryptoPolicy,
    seed: &SecretBox<[u8]>,
) -> SecretsResult<SecretBox<[u8]>> {
    let seed = validated_seed(seed)?;
    let key_pair = derive_recipient_key_pair(kem_for_policy(policy), &seed)
        .map_err(|source| SecretsError::KekDerivation { source })?;
    Ok(SecretBox::new(
        key_pair
            .private_key()
            .as_seed_bytes()
            .to_vec()
            .into_boxed_slice(),
    ))
}

fn validated_seed(seed: &SecretBox<[u8]>) -> SecretsResult<[u8; 32]> {
    seed.expose_secret()
        .as_ref()
        .try_into()
        .map_err(|_| SecretsError::KekUnavailable {
            kind: "seed".to_owned(),
            reason: "expected exactly 32 bytes for deterministic native PQ HPKE derivation"
                .to_owned(),
        })
}

fn kem_for_policy(policy: &CryptoPolicy) -> Kem {
    match policy.kem {
        KemAlgo::MlKem512 => Kem::MlKem512,
        KemAlgo::MlKem768 => Kem::MlKem768,
        KemAlgo::MlKem1024 => Kem::MlKem1024,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};

    use secrecy::ExposeSecret;

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
        use std::os::unix::fs::{symlink, PermissionsExt};

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

    fn native_private_seed_representation(policy: &CryptoPolicy, seed: &[u8; 32]) -> Vec<u8> {
        derive_recipient_key_pair(kem_for_policy(policy), seed)
            .expect("derive native recipient keypair")
            .private_key()
            .as_seed_bytes()
            .to_vec()
    }

    #[test]
    fn deterministic_derivation_is_stable_for_a_seed_and_policy() {
        let policy = CryptoPolicy::ml_kem_768();
        let seed = test_seed(0xA5);

        let public_first = derive_public_key(&policy, &seed).expect("derive first public key");
        let secret_first = derive_secret_key(&policy, &seed).expect("derive first secret key");
        let public_second = derive_public_key(&policy, &seed).expect("derive second public key");
        let secret_second = derive_secret_key(&policy, &seed).expect("derive second secret key");
        let expected_private_seed = native_private_seed_representation(&policy, &[0xA5; 32]);

        assert!(!public_first.is_empty());
        assert_eq!(
            secret_first.expose_secret().as_ref(),
            expected_private_seed.as_slice()
        );
        assert_eq!(public_first, public_second);
        assert_eq!(secret_first.expose_secret(), secret_second.expose_secret());
    }

    #[test]
    fn changing_the_seed_changes_derived_key_material() {
        let policy = CryptoPolicy::ml_kem_512();
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
    fn every_kem_policy_derives_stable_native_hpke_material() {
        let seed = test_seed(0x5A);

        for policy in [
            CryptoPolicy::ml_kem_512(),
            CryptoPolicy::ml_kem_768(),
            CryptoPolicy::strongest(),
        ] {
            let public = derive_public_key(&policy, &seed).expect("derive public key");
            let secret = derive_secret_key(&policy, &seed).expect("derive secret key");
            let expected_private_seed = native_private_seed_representation(&policy, &[0x5A; 32]);

            assert!(!public.is_empty());
            assert_eq!(
                secret.expose_secret().as_ref(),
                expected_private_seed.as_slice()
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
        }
    }

    #[test]
    fn deterministic_derivation_rejects_a_non_32_byte_seed() {
        let short_seed = SecretBox::new(vec![0u8; 31].into_boxed_slice());

        assert!(matches!(
            derive_public_key(&CryptoPolicy::ml_kem_512(), &short_seed),
            Err(SecretsError::KekUnavailable { .. })
        ));
        assert!(matches!(
            derive_secret_key(&CryptoPolicy::ml_kem_512(), &short_seed),
            Err(SecretsError::KekUnavailable { .. })
        ));
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

        let policy = CryptoPolicy::ml_kem_768();
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
                &CryptoPolicy::ml_kem_512(),
                &KekProvenance::KeyFile { path: path.clone() },
            ),
            Err(SecretsError::KekUnavailable { .. })
        ));

        std::fs::remove_file(path).expect("remove temporary invalid seed");
    }
}
