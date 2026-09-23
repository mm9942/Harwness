//! Fehlertyp für `harw-lens-store`.
//!
//! # Verantwortungsbereich
//! Ein einziges Enum [`LensStoreError`] für alle Fehler dieses Crates:
//! I/O-Fehler, JSON-(De-)Serialisierungsfehler, ungültige Indexnamen,
//! unerwartete Pfadtypen (Symlink-Abwehr beim Lesen, ein belegter Nicht-
//! Verzeichnis-Pfad beim Öffnen) und — der für dieses Crate wichtigste Fall
//! — eine fehlgeschlagene Digest-Nachprüfung beim Lesen eines Chunks.
//! `Display`, `std::error::Error` (inklusive `source()` für die
//! `#[from]`-Varianten) und der Typalias `LensStoreResult<T>` werden nicht
//! von Hand geschrieben, sondern von `#[derive(harw_macros::HarwError)]`
//! erzeugt (Muster: `harw-lens-types/src/error.rs`,
//! `harw-session-store/src/error.rs`). Kein `anyhow`, kein `thiserror`.
//!
//! Exportierte Typen: [`LensStoreError`], [`LensStoreResult`].
//!
//! # Nebenläufigkeit
//! `LensStoreError` ist reine Daten (`Debug`) ohne interne Veränderlichkeit
//! und ohne Einschränkung zwischen Threads teilbar. Es leitet weder `Clone`
//! noch `PartialEq` ab, weil die `Io`- und `Serde`-Varianten fremde
//! Fehlertypen wickeln, die selbst keines von beidem anbieten.
//!
//! # Examples
//! ```rust
//! use harw_lens_store::LensStoreError;
//!
//! let err = LensStoreError::InvalidIndexName {
//!     name: "../escape".to_owned(),
//! };
//! assert!(err.to_string().contains("../escape"));
//! ```

use harw_macros::HarwError;
use harw_types::ContentDigest;

/// Fehler dieses Crates (erzeugt den Typalias `LensStoreResult<T>`).
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zum Verstehen des
/// Fehlers ohne Quellcode-Lektüre nötig ist.
#[derive(Debug, HarwError)]
pub enum LensStoreError {
    /// Dateisystem-I/O-Fehler; deferiert `Display`/`source()` an den
    /// inneren Fehler.
    #[from]
    Io(std::io::Error),

    /// JSON-(De-)Serialisierungsfehler; deferiert `Display`/`source()` an
    /// den inneren Fehler.
    #[from]
    Serde(serde_json::Error),

    /// Ein Indexname verstößt gegen die Namensregel: nur `[A-Za-z0-9_.-]`,
    /// nicht leer, kein `..` als Teilzeichenkette. Entsteht in jeder
    /// Methode, die einen Indexnamen entgegennimmt.
    ///
    /// # Arguments
    /// - `name` (`String`): der abgelehnte, unveränderte Name.
    #[msg(
        "index name '{name}' is invalid: only [A-Za-z0-9_.-] is allowed, no path separator and no '..'"
    )]
    InvalidIndexName {
        /// Der abgelehnte, unveränderte Name.
        name: String,
    },

    /// Die wichtigste Zusage dieses Crates: ein gelesener Chunk hasht nicht
    /// auf den angefragten Digest. Entsteht ausschließlich in
    /// [`crate::LensStore::get_chunk`], das den tatsächlich gelesenen Text
    /// erneut hasht und mit dem angefragten Digest vergleicht, statt den
    /// Inhalt stillschweigend zurückzugeben.
    ///
    /// # Arguments
    /// - `requested` (`ContentDigest`): der Digest, unter dem der Chunk
    ///   angefragt wurde (der Fanout-Pfad, aus dem gelesen wurde).
    /// - `actual` (`ContentDigest`): der Digest, den `ContentDigest::of`
    ///   über den tatsächlich gelesenen `text` des Chunks berechnet hat.
    #[msg(
        "chunk content does not match requested digest {requested}: recomputed digest is {actual}"
    )]
    ChunkDigestMismatch {
        /// Digest, unter dem der Chunk angefragt wurde.
        requested: ContentDigest,
        /// Digest, den der tatsächlich gelesene Inhalt ergibt.
        actual: ContentDigest,
    },

    /// Ein Store-Pfad hat nicht den erwarteten Typ: entweder belegt ein
    /// Symlink oder eine Spezialdatei eine Stelle, an der eine reguläre
    /// Datei erwartet wird (Symlink-Abwehr beim Lesen), oder eine reguläre
    /// Datei belegt eine Stelle, an der [`crate::LensStore::open`] ein
    /// Verzeichnis erwartet.
    ///
    /// # Arguments
    /// - `path` (`String`): der betroffene Pfad, bereits über
    ///   `Path::display` formatiert (weder `Path` noch `PathBuf`
    ///   implementieren `Display` direkt).
    #[msg("store path has an unexpected type (not a regular file, or not a directory): {path}")]
    UnexpectedPathType {
        /// Der betroffene Pfad, bereits als Anzeigezeichenkette.
        path: String,
    },

    /// Eine schreibende Operation konnte die `fs4`-Advisory-Lock-Datei am
    /// Store-Root nicht nicht-blockierend erwerben, weil ein anderer
    /// Prozess sie hält.
    #[msg("lens store lock is contended")]
    LockContended,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_lens_store_error_display_invalid_index_name() {
        let err = LensStoreError::InvalidIndexName {
            name: "../escape".to_owned(),
        };
        assert!(err.to_string().contains("../escape"));
    }

    #[test]
    fn test_lens_store_error_display_chunk_digest_mismatch() {
        let requested = ContentDigest::of(b"expected");
        let actual = ContentDigest::of(b"actual");
        let err = LensStoreError::ChunkDigestMismatch { requested, actual };
        let text = err.to_string();
        assert!(text.contains(&requested.to_string()));
        assert!(text.contains(&actual.to_string()));
    }

    #[test]
    fn test_lens_store_error_display_unexpected_path_type() {
        let err = LensStoreError::UnexpectedPathType {
            path: "/tmp/example".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "store path has an unexpected type (not a regular file, or not a directory): /tmp/example"
        );
    }

    #[test]
    fn test_lens_store_error_display_lock_contended() {
        let err = LensStoreError::LockContended;
        assert_eq!(err.to_string(), "lens store lock is contended");
    }

    #[test]
    fn test_lens_store_error_from_io() {
        let io_err = std::io::Error::other("boom");
        let err: LensStoreError = io_err.into();
        assert!(matches!(err, LensStoreError::Io(_)));
    }

    #[test]
    fn test_lens_store_error_from_serde() -> TestResult {
        let Err(serde_err) = serde_json::from_str::<u32>("not json") else {
            return Err(TestError::Unexpected(
                "malformed JSON must fail to parse".to_owned(),
            ));
        };
        let err: LensStoreError = serde_err.into();
        assert!(matches!(err, LensStoreError::Serde(_)));
        Ok(())
    }
}
