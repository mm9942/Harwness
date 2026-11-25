//! `harw-warden keygen`: einen neuen Proof-v2-Schlüssel erzeugen (PL-100
//! WP 3.1, „Schlüssel: Erzeugung, Rotation, Modi“).
//!
//! # Zweck
//! Erzeugt 32 Bytes aus der Zufallsquelle des Kernels (`getrandom`) und legt
//! sie über [`harw_dod_warden::keys::write_key_file`] als
//! `<key-dir>/<key-id>.key` an: exklusiv (nie ein Überschreiben), Modus
//! `0600`, danach mit denselben Prüfungen zurückgelesen, die der Warden beim
//! Start anwendet (Eigentümer root, keine Gruppen- oder Weltrechte). Läuft
//! vor jedem systemd-, Landlock- oder seccomp-Schritt und beendet sich.
//!
//! # Was ausgegeben wird
//! Nur die Key-ID und der Hinweis auf den nötigen Neustart. Schlüsselmaterial
//! erscheint nie in der Ausgabe, in Logs oder in Fehlermeldungen; der Puffer
//! wird nach dem Schreiben überschrieben (best effort).
//!
//! # Rotation und Escalator
//! Siehe `harw_dod_warden::keys`-Moduldoku für den Ablauf. Der Escalator
//! braucht dasselbe Material (symmetrischer MAC); wie seine Kopie entsteht
//! und wo sie liegt, legt das Escalator-Arbeitspaket fest (README §4,
//! 3.2). Diese Datei schreibt nur die Kopie des Wardens.
//!
//! # Nebenläufigkeit
//! Einmaliger, synchroner Lauf im Hauptthread.
//!
//! # Fehler
//! [`WardenBinError::KeyGeneration`] (Zufallsquelle, Ausgabe),
//! [`WardenBinError::Setup`] (ungültige Key-ID, Ordner oder Datei
//! verletzt die Regeln, Key-ID bereits vergeben).
//!
//! # Examples
//! ```text
//! # harw-warden keygen --key-id k-2026-11
//! proof key k-2026-11 written; restart the warden to load it
//! ```

use std::io::Write;

use harw_dod_warden::trust::TrustRules;
use harw_dod_warden_proto::KeyId;

use crate::cli::KeygenArgs;
use crate::error::WardenBinError;

/// Länge eines Proof-v2-Schlüssels in Bytes (keyed BLAKE3).
const KEY_LEN: usize = 32;

/// Runs `harw-warden keygen` with the production ownership rule (root).
///
/// # Arguments
/// - `args` (`&KeygenArgs`): Key-ID und Schlüsselordner.
/// - `out` (`&mut dyn Write`): Ziel der inhaltsfreien Meldung (stdout).
///
/// # Errors
/// Siehe Moduldoku, Abschnitt „Fehler“.
pub fn run(args: &KeygenArgs, out: &mut dyn Write) -> Result<(), WardenBinError> {
    generate(args, TrustRules::root(), out)
}

// Wie `run`, mit wählbarer Eigentumsregel (Tests laufen nicht als root).
fn generate(
    args: &KeygenArgs,
    rules: TrustRules,
    out: &mut dyn Write,
) -> Result<(), WardenBinError> {
    let key_id = KeyId::new(args.key_id.as_str()).map_err(harw_dod_warden::WardenError::Proof)?;
    let mut key = [0u8; KEY_LEN];
    let filled = fill_random(&mut key);
    let written = filled.and_then(|()| {
        harw_dod_warden::keys::write_key_file(&args.key_dir, &key_id, &key, rules)
            .map_err(WardenBinError::from)
    });
    wipe(&mut key);
    written?;
    writeln!(
        out,
        "proof key {} written; restart the warden to load it",
        key_id.as_str()
    )
    .map_err(WardenBinError::KeyGeneration)
}

// Füllt `key` vollständig aus `getrandom` (blockiert nur, bis der Pool des
// Kernels initialisiert ist).
fn fill_random(key: &mut [u8; KEY_LEN]) -> Result<(), WardenBinError> {
    let filled = rustix::rand::getrandom(&mut *key, rustix::rand::GetRandomFlags::empty())
        .map_err(|errno| WardenBinError::KeyGeneration(std::io::Error::from(errno)))?;
    if filled != KEY_LEN {
        return Err(WardenBinError::KeyGeneration(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the kernel returned fewer random bytes than requested",
        )));
    }
    Ok(())
}

// Überschreibt das Schlüsselmaterial (best effort, wie `ProofKey::drop`).
fn wipe(key: &mut [u8; KEY_LEN]) {
    key.fill(0);
    // Hält das Überschreiben für den Optimierer beobachtbar; der
    // zurückgegebene Verweis wird nicht gebraucht.
    let _ = std::hint::black_box(key);
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

    use super::{KEY_LEN, fill_random, generate};
    use crate::cli::KeygenArgs;
    use crate::error::WardenBinError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden::trust::TrustRules;
    use harw_dod_warden_proto::KeyId;

    fn args(dir: &std::path::Path, key_id: &str) -> KeygenArgs {
        KeygenArgs {
            key_id: key_id.to_owned(),
            key_dir: dir.to_path_buf(),
        }
    }

    fn rules_for(dir: &std::path::Path) -> TestResult<TrustRules> {
        Ok(TrustRules::owned_by(
            std::fs::metadata(dir).map_err(ctx("metadata"))?.uid(),
        ))
    }

    #[test]
    fn test_keygen_writes_a_loadable_key_and_prints_only_the_key_id() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = rules_for(dir.path())?;
        let mut out = Vec::new();
        generate(&args(dir.path(), "k-2026"), rules, &mut out).map_err(ctx("keygen"))?;

        let printed = String::from_utf8(out).map_err(ctx("utf8"))?;
        assert_eq!(
            printed,
            "proof key k-2026 written; restart the warden to load it\n"
        );
        let contents =
            std::fs::read_to_string(dir.path().join("k-2026.key")).map_err(ctx("read key"))?;
        let hex = contents.trim_end();
        assert_eq!(hex.len(), KEY_LEN.saturating_mul(2));
        // Das Schlüsselmaterial steht nicht in der Ausgabe.
        assert!(!printed.contains(hex));
        let ring = harw_dod_warden::load_key_ring(dir.path(), rules).map_err(ctx("load"))?;
        assert!(ring.contains(&KeyId::new("k-2026").map_err(ctx("key id"))?));
        Ok(())
    }

    #[test]
    fn test_keygen_refuses_an_existing_or_invalid_key_id() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = rules_for(dir.path())?;
        let mut out = Vec::new();
        generate(&args(dir.path(), "k1"), rules, &mut out).map_err(ctx("first keygen"))?;
        let before = std::fs::read(dir.path().join("k1.key")).map_err(ctx("read"))?;
        assert!(matches!(
            generate(&args(dir.path(), "k1"), rules, &mut out),
            Err(WardenBinError::Setup(_))
        ));
        assert_eq!(
            std::fs::read(dir.path().join("k1.key")).map_err(ctx("read again"))?,
            before
        );
        for invalid in ["", "a/b", "k 1"] {
            if !matches!(
                generate(&args(dir.path(), invalid), rules, &mut out),
                Err(WardenBinError::Setup(_))
            ) {
                return Err(TestError::Unexpected(format!(
                    "key id {invalid:?} must be refused"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_two_generated_keys_differ() -> TestResult {
        let mut first = [0u8; KEY_LEN];
        let mut second = [0u8; KEY_LEN];
        fill_random(&mut first).map_err(ctx("random"))?;
        fill_random(&mut second).map_err(ctx("random"))?;
        assert_ne!(first, second);
        Ok(())
    }
}
