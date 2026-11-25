//! Laden des Proof-v2-Schlüsselrings aus `proof_key_dir`:
//! [`load_key_ring`] (PL-100 WP 3.1, THREAT-MODEL §1 „v2-Schlüssel“).
//!
//! # Ablage
//! Je Schlüssel eine Datei `<key-id>.key` in `proof_key_dir`
//! (Vorgabe `/etc/harw-dod/warden-keys`). Inhalt: genau 64 Hexzeichen in
//! Kleinbuchstaben (32 Bytes für keyed BLAKE3), optional gefolgt von einem
//! Zeilenumbruch. Der Dateiname ohne `.key` ist die [`KeyId`]. Andere
//! Dateien (z. B. `README`) werden übersprungen.
//!
//! # Regeln (fail closed)
//! - Ordner: Verzeichnis (kein Symlink), Eigentümer root, nicht gruppen- oder
//!   weltschreibbar.
//! - Schlüsseldatei: reguläres File, Eigentümer root, Modus ohne jedes
//!   Gruppen- oder Weltrecht (`0600`/`0400`), höchstens 128 Bytes.
//! - Mindestens ein, höchstens [`MAX_KEY_FILES`] Schlüssel.
//! - Jeder Verstoß verhindert den Start; ein einzelner unsicherer Schlüssel
//!   wird nicht still übersprungen.
//!
//! # Erzeugung
//! [`write_key_file`] legt einen neuen Schlüssel exklusiv an (`0600`, nie
//! ein Überschreiben) und prüft ihn wie beim Laden; das Binary ruft es über
//! `harw-warden keygen --key-id <id>` mit 32 Bytes aus `getrandom` auf.
//!
//! # Rotation
//! Der Escalator signiert mit dem neuen Schlüssel, der Warden akzeptiert
//! alle Schlüssel im Ordner. Ein alter Schlüssel gilt, bis seine Datei
//! entfernt und der Warden neu gestartet wurde (der Ring wird nur beim
//! Start geladen; der Warden ist socket-aktiviert, ein Neustart verliert
//! keine Verbindung, die nicht ohnehin eine Antwort bekäme). Ablauf:
//! neuen Schlüssel erzeugen → Warden neu starten (beide Key-IDs gültig) →
//! Escalator auf die neue Key-ID umstellen → alte Datei entfernen → Warden
//! neu starten.
//!
//! # Bedrohung: symmetrischer MAC
//! Der MAC ist keyed BLAKE3: wer eine Schlüsseldatei lesen kann, kann
//! Autorisierungen fälschen (THREAT-MODEL B1). Der Escalator braucht
//! dasselbe Material; seine Kopie gehört ausschließlich seinem Nutzer
//! (`0600`), die des Wardens root (`0600` hier). Kein Schlüssel erscheint
//! in Logs, Fehlermeldungen oder der Ausgabe von `keygen`.
//!
//! # Schlüsselmaterial im Speicher
//! Der gelesene Dateiinhalt und das dekodierte Array (an seinem Platz in der
//! `Option`, nicht in einer Kopie) werden nach dem Einlesen überschrieben
//! (best effort wie `ProofKey::drop`; ohne `zeroize`-Crate kann der Compiler
//! das Schreiben theoretisch entfernen, und Kopien, die beim Übergeben von
//! Werten auf dem Stack entstehen, erreicht das Überschreiben nicht).
//!
//! # Nebenläufigkeit
//! Reine Startfunktion; kein geteilter Zustand.
//!
//! # Fehler
//! [`WardenError::KeyStore`], [`WardenError::UntrustedFile`],
//! [`WardenError::Io`], [`WardenError::Proof`].
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_warden::keys::load_key_ring;
//! use harw_dod_warden::trust::TrustRules;
//!
//! let dir = std::path::Path::new("/etc/harw-dod/warden-keys");
//! if let Ok(ring) = load_key_ring(dir, TrustRules::root()) {
//!     // Ein geladener Ring enthält immer mindestens einen Schlüssel.
//!     assert!(!ring.is_empty());
//! }
//! ```

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harw_dod_warden_proto::{KeyId, KeyRing, ProofKey};
use rustix::fs::{Mode, OFlags};

use crate::error::{WardenError, WardenResult};
use crate::fsutil;
use crate::hex;
use crate::trust::{FileSecrecy, TrustRules, check_trusted_dir, read_trusted_file};

/// Dateiendung einer Schlüsseldatei.
pub const KEY_FILE_SUFFIX: &str = ".key";

/// Höchstzahl gleichzeitig gültiger Schlüssel (Rotation braucht zwei).
pub const MAX_KEY_FILES: usize = 16;

// Größenobergrenze einer Schlüsseldatei (64 Hexzeichen + Zeilenumbruch).
const MAX_KEY_FILE_LEN: u64 = 128;

/// Loads every `<key-id>.key` file from `dir` into a [`KeyRing`].
///
/// # Arguments
/// - `dir` (`&Path`): der Schlüsselordner.
/// - `rules` (`TrustRules`): erwarteter Eigentümer (Produktion: root).
///
/// # Returns
/// Den Ring mit mindestens einem Schlüssel.
///
/// # Errors
/// - [`WardenError::UntrustedFile`]: Ordner oder Datei verletzt die Regeln.
/// - [`WardenError::KeyStore`]: kein Schlüssel, zu viele, Dateiname kein
///   UTF-8 oder keine gültige Key-ID, Inhalt nicht genau 64 Hexzeichen.
/// - [`WardenError::Io`]: Ordner oder Datei nicht lesbar.
pub fn load_key_ring(dir: &Path, rules: TrustRules) -> WardenResult<KeyRing> {
    check_trusted_dir(dir, rules, "proof key directory")?;
    let mut ring = KeyRing::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            return Err(WardenError::KeyStore {
                reason: "a file name in the key directory is not UTF-8",
            });
        };
        let Some(id) = name.strip_suffix(KEY_FILE_SUFFIX) else {
            continue;
        };
        if ring.len() >= MAX_KEY_FILES {
            return Err(WardenError::KeyStore {
                reason: "too many key files",
            });
        }
        let key_id = KeyId::new(id).map_err(|_| WardenError::KeyStore {
            reason: "a key file name is not a valid key id",
        })?;
        let key = read_key_file(&entry.path(), rules)?;
        ring.insert(key_id, key)?;
    }
    if ring.is_empty() {
        return Err(WardenError::KeyStore {
            reason: "no proof keys found",
        });
    }
    Ok(ring)
}

/// Writes a new key file `<dir>/<key_id>.key` and checks that the warden would load it.
///
/// # Description
/// Für `harw-warden keygen` (Erzeugung, PL-100 WP 3.1 „Schlüssel“). Der
/// Ordner muss die Regeln von [`load_key_ring`] erfüllen. Die Datei wird
/// exklusiv angelegt (`O_CREAT | O_EXCL | O_NOFOLLOW`, Modus `0600`): ein
/// vorhandener Schlüssel derselben Key-ID wird nie überschrieben. Inhalt:
/// 64 Hexzeichen in Kleinbuchstaben und ein Zeilenumbruch, danach `fsync`
/// der Datei und des Ordners. Zum Schluss liest die Funktion die Datei über
/// denselben Weg wie [`load_key_ring`] zurück (Eigentümer, Modus, Format);
/// scheitert das, wird die neue Datei wieder entfernt. Der Textpuffer wird
/// nach dem Schreiben überschrieben (best effort, siehe Moduldoku).
///
/// # Arguments
/// - `dir` (`&Path`): der Schlüsselordner (`proof_key_dir`).
/// - `key_id` (`&KeyId`): die neue Key-ID (zugleich Dateiname).
/// - `key` (`&[u8; 32]`): das Schlüsselmaterial.
/// - `rules` (`TrustRules`): erwarteter Eigentümer (Produktion: root).
///
/// # Returns
/// Den Pfad der neuen Datei.
///
/// # Errors
/// - [`WardenError::UntrustedFile`]: Ordner oder neue Datei verletzt die
///   Regeln (z. B. nicht als root ausgeführt).
/// - [`WardenError::Io`]: `AlreadyExists` für eine vergebene Key-ID, sonst
///   ein Schreibfehler.
///
/// # Examples
/// ```rust
/// use std::os::unix::fs::MetadataExt as _;
///
/// use harw_dod_warden::keys::{load_key_ring, write_key_file};
/// use harw_dod_warden::trust::TrustRules;
/// use harw_dod_warden_proto::KeyId;
///
/// let Ok(dir) = tempfile::tempdir() else { return };
/// let Ok(meta) = std::fs::metadata(dir.path()) else { return };
/// let rules = TrustRules::owned_by(meta.uid());
/// let Ok(key_id) = KeyId::new("k-2026") else { return };
/// assert!(write_key_file(dir.path(), &key_id, &[0x5a; 32], rules).is_ok());
/// assert!(load_key_ring(dir.path(), rules).is_ok_and(|ring| ring.contains(&key_id)));
/// ```
pub fn write_key_file(
    dir: &Path,
    key_id: &KeyId,
    key: &[u8; 32],
    rules: TrustRules,
) -> WardenResult<PathBuf> {
    check_trusted_dir(dir, rules, "proof key directory")?;
    let path = dir.join(format!("{}{KEY_FILE_SUFFIX}", key_id.as_str()));
    let fd = rustix::fs::openat(
        rustix::fs::CWD,
        &path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|errno| WardenError::Io(std::io::Error::from(errno)))?;
    let mut file = File::from(fd);
    let mut line = hex::encode(key).into_bytes();
    line.push(b'\n');
    let written = file.write_all(&line).and_then(|()| file.sync_all());
    wipe(&mut line);
    written?;
    fsutil::sync_dir(dir)?;
    if let Err(error) = read_key_file(&path, rules) {
        // Best effort: eine Datei, die der Warden nicht laden würde, soll
        // nicht liegen bleiben; maßgeblich ist der Prüffehler.
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

// Liest und dekodiert eine Schlüsseldatei; überschreibt die Zwischenpuffer.
fn read_key_file(path: &Path, rules: TrustRules) -> WardenResult<ProofKey> {
    let mut contents = read_trusted_file(
        path,
        rules,
        FileSecrecy::Private,
        MAX_KEY_FILE_LEN,
        "proof key",
    )?;
    let mut decoded = std::str::from_utf8(&contents)
        .ok()
        .map(|text| text.strip_suffix('\n').unwrap_or(text))
        .and_then(hex::decode::<32>);
    wipe(&mut contents);
    // In der `Option` selbst überschreiben: `Option<[u8; 32]>` ist `Copy`,
    // ein `let Some(bytes) = decoded` überschriebe nur eine Kopie.
    let Some(bytes) = decoded.as_mut() else {
        return Err(WardenError::KeyStore {
            reason: "a key file does not hold exactly 64 lowercase hex characters",
        });
    };
    let key = ProofKey::from_bytes(*bytes);
    wipe(bytes);
    Ok(key)
}

// Überschreibt einen Puffer mit Schlüsselmaterial (best effort).
fn wipe(buffer: &mut [u8]) {
    buffer.fill(0);
    // Hält das Überschreiben für den Optimierer beobachtbar (best effort);
    // der zurückgegebene Verweis wird nicht gebraucht.
    let _ = std::hint::black_box(buffer);
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::Path;

    use super::load_key_ring;
    use crate::error::WardenError;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::trust::TrustRules;
    use harw_dod_warden_proto::KeyId;

    const HEX_A: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const HEX_B: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

    fn rules_for(dir: &Path) -> TestResult<TrustRules> {
        Ok(TrustRules::owned_by(
            std::fs::metadata(dir).map_err(ctx("metadata"))?.uid(),
        ))
    }

    fn write_key(dir: &Path, name: &str, contents: &str, mode: u32) -> TestResult {
        let path = dir.join(name);
        std::fs::write(&path, contents).map_err(ctx("write key"))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(ctx("chmod key"))?;
        Ok(())
    }

    fn key_id(id: &str) -> TestResult<KeyId> {
        KeyId::new(id).map_err(ctx("key id"))
    }

    #[test]
    fn test_loads_all_keys_and_skips_other_files() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_key(dir.path(), "k-old.key", HEX_A, 0o600)?;
        write_key(dir.path(), "k-new.key", &format!("{HEX_B}\n"), 0o400)?;
        write_key(dir.path(), "README", "not a key", 0o644)?;
        let ring = load_key_ring(dir.path(), rules_for(dir.path())?).map_err(ctx("ring loads"))?;
        assert_eq!(ring.len(), 2);
        assert!(ring.contains(&key_id("k-old")?));
        assert!(ring.contains(&key_id("k-new")?));
        Ok(())
    }

    #[test]
    fn test_removed_key_file_disappears_from_the_reloaded_ring() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_key(dir.path(), "k-old.key", HEX_A, 0o600)?;
        write_key(dir.path(), "k-new.key", HEX_B, 0o600)?;
        let rules = rules_for(dir.path())?;
        let before = load_key_ring(dir.path(), rules).map_err(ctx("ring loads"))?;
        assert!(before.contains(&key_id("k-old")?));
        std::fs::remove_file(dir.path().join("k-old.key")).map_err(ctx("remove old key"))?;
        let after = load_key_ring(dir.path(), rules).map_err(ctx("ring reloads"))?;
        assert!(!after.contains(&key_id("k-old")?));
        assert!(after.contains(&key_id("k-new")?));
        Ok(())
    }

    #[test]
    fn test_key_readable_by_group_or_world_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_key(dir.path(), "k1.key", HEX_A, 0o644)?;
        match load_key_ring(dir.path(), rules_for(dir.path())?) {
            Err(WardenError::UntrustedFile { .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected UntrustedFile, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_malformed_key_material_is_refused() -> TestResult {
        for (name, contents) in [
            ("short.key", "a1a1"),
            (
                "upper.key",
                "A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1",
            ),
            (
                "spaced.key",
                " a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a",
            ),
        ] {
            let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
            write_key(dir.path(), name, contents, 0o600)?;
            if !matches!(
                load_key_ring(dir.path(), rules_for(dir.path())?),
                Err(WardenError::KeyStore { .. })
            ) {
                return Err(TestError::Unexpected(format!("{name} must be refused")));
            }
        }
        Ok(())
    }

    #[test]
    fn test_empty_directory_and_bad_key_id_are_refused() -> TestResult {
        let empty = tempfile::tempdir().map_err(ctx("tempdir"))?;
        assert!(matches!(
            load_key_ring(empty.path(), rules_for(empty.path())?),
            Err(WardenError::KeyStore { .. })
        ));
        let bad = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_key(bad.path(), "bad id.key", HEX_A, 0o600)?;
        assert!(matches!(
            load_key_ring(bad.path(), rules_for(bad.path())?),
            Err(WardenError::KeyStore { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_written_key_file_is_private_and_loads() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = rules_for(dir.path())?;
        let id = key_id("k-2026")?;
        let path =
            super::write_key_file(dir.path(), &id, &[0xc3; 32], rules).map_err(ctx("write key"))?;
        assert_eq!(path, dir.path().join("k-2026.key"));
        let metadata = std::fs::metadata(&path).map_err(ctx("metadata"))?;
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let contents = std::fs::read_to_string(&path).map_err(ctx("read"))?;
        assert_eq!(contents, format!("{}\n", "c3".repeat(32)));
        let ring = load_key_ring(dir.path(), rules).map_err(ctx("load"))?;
        assert!(ring.contains(&id));
        Ok(())
    }

    #[test]
    fn test_write_key_file_never_overwrites_an_existing_key() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = rules_for(dir.path())?;
        let id = key_id("k1")?;
        super::write_key_file(dir.path(), &id, &[0x11; 32], rules).map_err(ctx("first"))?;
        match super::write_key_file(dir.path(), &id, &[0x22; 32], rules) {
            Err(WardenError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected AlreadyExists, got {other:?}"
                )));
            }
        }
        let contents = std::fs::read_to_string(dir.path().join("k1.key")).map_err(ctx("read"))?;
        assert_eq!(contents, format!("{}\n", "11".repeat(32)));
        Ok(())
    }

    #[test]
    fn test_write_key_file_refuses_an_untrusted_directory_or_owner() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = rules_for(dir.path())?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("chmod"))?;
        assert!(matches!(
            super::write_key_file(dir.path(), &key_id("k1")?, &[0x11; 32], rules),
            Err(WardenError::UntrustedFile { .. })
        ));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(ctx("chmod back"))?;
        let foreign = TrustRules::owned_by(rules.owner_uid().wrapping_add(1));
        assert!(matches!(
            super::write_key_file(dir.path(), &key_id("k1")?, &[0x11; 32], foreign),
            Err(WardenError::UntrustedFile { .. })
        ));
        assert!(!dir.path().join("k1.key").exists());
        Ok(())
    }
}
