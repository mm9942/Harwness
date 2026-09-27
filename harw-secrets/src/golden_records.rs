//! Frozen golden secret records (Crypto-Masterplan v2 §16/§17).
//!
//! # Verantwortungsbereich
//! Belegt, dass bereits persistierte Datensätze nach einem `crypt_guard`-Wechsel
//! (3.0.2 → 3.1) bitgenau weiter öffnen. Für jeden hybriden KEM liegt je ein
//! V1-Datensatz ([`SecretEnvelopeFormat::LegacyDirectHpke`]) und ein
//! V2-Datensatz ([`SecretEnvelopeFormat::DekWrappedV2`]) als JSON unter
//! `tests/fixtures/records/{v1,v2}_<kem wire name>.json`.
//!
//! Siegeln ist randomisiert (frischer DEK, Nonce, HPKE-Kapselung); deshalb
//! werden die Datensätze **einmal** eingefroren und danach nur noch gelesen.
//!
//! # Blessing
//! `HARW_BLESS=1 cargo test -p harw-secrets golden` schreibt nur **fehlende**
//! Fixture-Dateien — vorhandene werden nie überschrieben, auch nicht bei einem
//! Workspace-weiten `HARW_BLESS=1`-Lauf nach einem Upgrade (sonst würde ein
//! Re-Blessing unter der neuen Bibliothek genau den Kompatibilitätsbeweis
//! zerstören). Wer bewusst neu einfrieren will, löscht die Datei zuerst.
//! Ohne `HARW_BLESS=1` schlägt der Test bei fehlender Datei fehl.

use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretBox};

use crate::dek_wrapper::LocalHpkeDekWrapper;
use crate::envelope::{open, open_v3, rewrap_to_v3, seal, seal_legacy_v1_for_tests};
use crate::id::{KeyVersion, SecretId};
use crate::kek::derive_public_key;
use crate::policy::{AeadAlgo, CryptoPolicy, KemAlgo};
use crate::record::{SecretEnvelopeFormat, SecretRecord};
use crate::test_support::{TestError, TestResult};

/// Deterministic root KEK seed, identical to the `kek.rs` KAT seed.
const GOLDEN_SEED: [u8; 32] = [0xA5; 32];

/// Fixed plaintext sealed into every golden record.
const GOLDEN_PLAINTEXT: &[u8] = b"harw golden secret record v1";

/// Fixture directory relative to the crate root.
const FIXTURE_DIR: &str = "tests/fixtures/records";

/// Set to `1` to write missing golden fixtures (never overwrites).
const BLESS_ENV: &str = "HARW_BLESS";

/// Every hybrid KEM a record may name.
const HYBRID_KEMS: [KemAlgo; 3] = [
    KemAlgo::MlKem768P256,
    KemAlgo::MlKem1024P384,
    KemAlgo::MlKem768X25519,
];

fn blessing() -> bool {
    std::env::var_os(BLESS_ENV).is_some_and(|value| value == "1")
}

fn seed() -> SecretBox<[u8]> {
    SecretBox::new(GOLDEN_SEED.to_vec().into_boxed_slice())
}

fn policy_for(kem: KemAlgo) -> CryptoPolicy {
    CryptoPolicy {
        kem,
        aead: AeadAlgo::XChaCha20Poly1305,
    }
}

fn fixture_path(prefix: &str, kem: KemAlgo) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE_DIR)
        .join(format!("{prefix}_{}.json", kem.wire_name()))
}

/// Seal a fresh V1 record exactly as the retired direct-HPKE path wrote it.
fn seal_v1(kem: KemAlgo) -> TestResult<SecretRecord> {
    let policy = policy_for(kem);
    let public = derive_public_key(&policy, &seed())?;
    Ok(seal_legacy_v1_for_tests(
        &policy,
        SecretId::new(),
        KeyVersion::initial(),
        &public,
        GOLDEN_PLAINTEXT,
    )?)
}

/// Seal a fresh V2 record through the production seal path.
fn seal_v2(kem: KemAlgo) -> TestResult<SecretRecord> {
    let policy = policy_for(kem);
    let public = derive_public_key(&policy, &seed())?;
    let id = SecretId::new();
    let key_version = KeyVersion::initial();
    let sealed = seal(&policy, id, key_version, &public, GOLDEN_PLAINTEXT)?;
    Ok(SecretRecord {
        id,
        envelope_format: sealed.envelope_format,
        ciphertext: sealed.ciphertext,
        nonce: sealed.nonce,
        wrapped_dek: sealed.wrapped_dek,
        kem_algo: sealed.kem_algo,
        aead_algo: sealed.aead_algo,
        key_version,
        key_id: None,
        key_generation: None,
        crypto_profile_id: None,
    })
}

/// Write a missing fixture when blessing, then read and open every fixture.
fn check_golden(
    prefix: &str,
    format: SecretEnvelopeFormat,
    seal_fresh: fn(KemAlgo) -> TestResult<SecretRecord>,
) -> TestResult {
    let bless = blessing();
    for kem in HYBRID_KEMS {
        let path = fixture_path(prefix, kem);

        if bless && !path.exists() {
            let record = seal_fresh(kem)?;
            let parent = path
                .parent()
                .ok_or(TestError::Missing("golden record fixture directory"))?;
            std::fs::create_dir_all(parent)?;
            let mut json = serde_json::to_string(&record)?;
            json.push('\n');
            std::fs::write(&path, json)?;
        }

        let json = match std::fs::read_to_string(&path) {
            Ok(json) => json,
            Err(error) => {
                return Err(TestError::Context {
                    context: "golden record fixture is missing or unreadable; run HARW_BLESS=1 \
                        once (`HARW_BLESS=1 cargo test -p harw-secrets golden`) under the \
                        currently pinned crypt_guard, then commit tests/fixtures/records/",
                    source: format!("{}: {error}", path.display()),
                });
            }
        };
        let record: SecretRecord = serde_json::from_str(&json)?;

        if record.envelope_format != format || record.kem_algo != kem {
            return Err(TestError::Unexpected(format!(
                "{}: fixture names {:?}/{:?}, expected {format:?}/{kem:?}",
                path.display(),
                record.envelope_format,
                record.kem_algo
            )));
        }

        let plaintext = open(&GOLDEN_SEED, &record).map_err(|error| TestError::Context {
            context: "frozen golden record no longer opens: persisted secret stores would \
                become unreadable",
            source: format!("{}: {error}", path.display()),
        })?;
        assert_eq!(
            plaintext.expose_secret().as_ref(),
            GOLDEN_PLAINTEXT,
            "{}: golden record opened to a different plaintext",
            path.display()
        );
    }
    Ok(())
}

#[test]
fn golden_v1_direct_hpke_records_still_open() -> TestResult {
    check_golden("v1", SecretEnvelopeFormat::LegacyDirectHpke, seal_v1)
}

#[test]
fn golden_v2_dek_wrapped_records_still_open() -> TestResult {
    check_golden("v2", SecretEnvelopeFormat::DekWrappedV2, seal_v2)
}

#[test]
fn golden_fresh_seals_open_under_the_golden_seed() -> TestResult {
    // Guards the blessing path itself: whatever `check_golden` would freeze
    // must open with the same seed right now.
    for kem in HYBRID_KEMS {
        for record in [seal_v1(kem)?, seal_v2(kem)?] {
            assert_eq!(
                open(&GOLDEN_SEED, &record)?.expose_secret().as_ref(),
                GOLDEN_PLAINTEXT
            );
        }
    }
    Ok(())
}

#[test]
fn golden_v1_and_v2_records_migrate_to_v3_and_open() -> TestResult {
    // Read-only: the frozen fixtures are parsed, never rewritten.
    let wrapper =
        LocalHpkeDekWrapper::new("secrets/dek-wrap", 0, CryptoPolicy::strongest(), seed())?;
    for prefix in ["v1", "v2"] {
        for kem in HYBRID_KEMS {
            let path = fixture_path(prefix, kem);
            let json = std::fs::read_to_string(&path).map_err(|error| TestError::Context {
                context: "golden record fixture is missing or unreadable",
                source: format!("{}: {error}", path.display()),
            })?;
            let record: SecretRecord = serde_json::from_str(&json)?;
            let v3 = rewrap_to_v3(&GOLDEN_SEED, &record, &wrapper)?;

            assert_eq!(v3.envelope_format, SecretEnvelopeFormat::KmsWrappedV3);
            if record.envelope_format == SecretEnvelopeFormat::DekWrappedV2 {
                assert_eq!(v3.ciphertext, record.ciphertext, "{}", path.display());
                assert_eq!(v3.nonce, record.nonce, "{}", path.display());
            }
            assert_eq!(
                open_v3(&wrapper, &v3)?.expose_secret().as_ref(),
                GOLDEN_PLAINTEXT,
                "{}: migrated golden record opened to a different plaintext",
                path.display()
            );
        }
    }
    Ok(())
}

#[test]
fn golden_v1_and_v2_records_reserialize_byte_identically() -> TestResult {
    // The optional V3 fields (`key_id`, `key_generation`, `crypto_profile_id`)
    // must never appear in V1/V2 JSON.
    for prefix in ["v1", "v2"] {
        for kem in HYBRID_KEMS {
            let path = fixture_path(prefix, kem);
            let json = std::fs::read_to_string(&path).map_err(|error| TestError::Context {
                context: "golden record fixture is missing or unreadable",
                source: format!("{}: {error}", path.display()),
            })?;
            let record: SecretRecord = serde_json::from_str(&json)?;
            assert_eq!(
                serde_json::to_string(&record)?,
                json.trim_end_matches('\n'),
                "{}: V1/V2 JSON changed on re-serialization",
                path.display()
            );
        }
    }
    Ok(())
}
