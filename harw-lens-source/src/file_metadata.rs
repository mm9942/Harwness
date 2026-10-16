//! Datei-Metadaten mit Inhalts-Hash als Identität (Knoten h16, Teil 1).
//!
//! # Verantwortungsbereich
//! [`FileMetadata`] beschreibt eine Datei auf der Platte durch Pfad,
//! Inhalts-Hash ([`fnv1a64_hex`] über die Rohbytes), Größe und
//! Text/Binär-Klassifikation. Der Inhalts-Hash dient als *Identität* des
//! Inhalts, nicht des Pfades: [`rename_source`] ordnet eine umbenannte
//! Quelle ihren Kandidaten über gleichen Hash zu — Pfadänderungen sind
//! keine inhaltliche Änderung.
//!
//! # Klassifikation
//! [`classify`] liest die Datei ganz. Größer als [`MAX_TEXT_BYTES`] (1 MiB)
//! oder kein gültiges UTF-8 → `is_binary = true`; der Hash wird in jedem
//! Fall über die **Rohbytes** gebildet, damit Text- und Binärdatei
//! identisch behandelt werden.
//!
//! # Secret-Ausschluss
//! Dateien, deren Name auf `.env`, `auth.toml` oder `.key` endet, werden
//! **vor dem Hashen** ausgeschlossen: [`classify`] liefert dafür
//! `Option::None`. Das ist bewusst kein `Err` — Überspringen einer Datei
//! ist das erwartete Verhalten, kein Fehler; `None` zwingt jeden Aufrufer,
//! den Fall explizit zu behandeln, statt ihn in einer Fehlerbehandlung zu
//! begraben. Vor dem Hashen deshalb, weil der Hash selbst kein Leck ist,
//! aber die Konstruktion eines `FileMetadata` mit `path` die Existenz und
//! Position einer Secret-Datei in Indexberichten sichtbar machen würde.
//!
//! # Hash
//! FNV-1a (64 bit) als Hex — ein eigener, std-only Helfer. Es gibt im
//! Workspace keinen geteilten Hash-Helfer (`harw-job-executor-k8s/src/pod.rs`
//! trägt nur ein privates `fnv64`, nicht exportiert, nicht für Dateien).
//! FNV-1a ist deterministisch, kollisionsarm für Gleichheitsprüfung
//! einzelner Dateien und ohne externe Abhängigkeit. Kryptografisch ist er
//! **nicht** — für Identitätsvergleich von Dateiversionen reicht und soll
//! er das.
//!
//! # Nebenläufigkeit
//! Reine Daten (`Debug`, `Clone`, `PartialEq`, `Eq`), keine interne
//! Veränderlichkeit, frei zwischen Threads teilbar.
//!
//! # Stand
//! Knoten **h16** (Teil 1). Konsumenten (document.rs/build.rs-Anbindung,
//! Rename-Zuordnung im Sammlungspfad) landen in Folgepaketen.

use std::path::Path;

/// Obergrenze, ab der eine Datei als Binär-Artefakt klassifiziert wird
/// (1 MiB). Text über dieser Größe wird nicht mehr als Text indexiert;
/// die Datei bleibt aber als Binary-Artefakt mit Hash erfasst statt still
/// verworfen zu werden.
pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;

/// Metadaten einer Datei; `content_hash` ist die inhaltliche Identität.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    /// Pfad, unter dem die Datei zum Zeitpunkt der Klassifikation lag.
    pub path: std::path::PathBuf,
    /// FNV-1a (64 bit) über die Rohbytes, als kleingeschriebener Hex-String.
    pub content_hash: String,
    /// Größe in Bytes.
    pub size: u64,
    /// `true`, wenn größer als [`MAX_TEXT_BYTES`] oder kein gültiges UTF-8.
    pub is_binary: bool,
}

/// Klassifiziert die Datei unter `path`.
///
/// Liefert `None`, wenn der Dateiname auf `.env`, `auth.toml` oder `.key`
/// endet (Secret-Ausschluss, vor dem Hashen) oder die Datei nicht gelesen
/// werden kann (Grund: Lesefehler sind im Sammlungspfad keinen Abbruch
/// wert; das Verhalten ist dokumentiert und in den Tests festgepinnt).
///
/// # Examples
/// ```rust,no_run
/// # fn main() {
/// let meta = harw_lens_source::file_metadata::classify(
///     std::path::Path::new("Cargo.toml"),
/// ).expect("Cargo.toml vorhanden");
/// assert!(!meta.is_binary);
/// assert_eq!(meta.content_hash.len(), 16);
/// # }
/// ```
pub fn classify(path: &Path) -> Option<FileMetadata> {
    if is_secret_path(path) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let size = bytes.len() as u64;
    let is_binary = size > MAX_TEXT_BYTES || std::str::from_utf8(&bytes).is_err();
    Some(FileMetadata {
        path: path.to_path_buf(),
        content_hash: fnv1a64_hex(&bytes),
        size,
        is_binary,
    })
}

/// Prüft den Dateinamen gegen die Secret-Muster `.env`, `auth.toml`,
/// `.key` (Suffix-Match, vor jedem Hashing).
pub fn is_secret_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        // Nicht-UTF-8-Dateiname ist kein bekanntes Secret-Muster.
        return false;
    };
    name.ends_with(".env") || name.ends_with("auth.toml") || name.ends_with(".key")
}

/// Ordnet eine (umbenannte) Quelle ihren Kandidaten über gleichen
/// Inhalts-Hash zu: liefert den Index des ersten Kandidaten mit
/// `content_hash == old.content_hash`, sonst `None`.
///
/// Der Pfad wird absichtlich nicht verglichen — gleich der Hash, gleich
/// der Inhalt, egal wo er jetzt liegt.
pub fn rename_source(old: &FileMetadata, candidates: &[FileMetadata]) -> Option<usize> {
    candidates
        .iter()
        .position(|c| c.content_hash == old.content_hash)
}

/// FNV-1a (64 bit) über die Rohbytes, als 16-stelliger Hex-String.
fn fnv1a64_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_file(name: &str, content: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, content).expect("write");
        (dir, path)
    }

    #[test]
    fn hash_is_stable_across_reads_and_paths() {
        let (dir_a, path_a) = temp_file("a.txt", b"hello world");
        let (dir_b, path_b) = temp_file("b.txt", b"hello world");
        let meta_a = classify(&path_a).expect("a.txt klassifiziert");
        let meta_b = classify(&path_b).expect("b.txt klassifiziert");
        assert_eq!(meta_a.content_hash, meta_b.content_hash);
        assert_eq!(meta_a.content_hash, fnv1a64_hex(b"hello world"));
        assert_eq!(meta_a.content_hash.len(), 16);
        // Erneutes Lesen liefert denselben Hash.
        assert_eq!(classify(&path_a).unwrap().content_hash, meta_a.content_hash);
        drop((dir_a, dir_b));
    }

    #[test]
    fn binary_detection_for_invalid_utf8() {
        let (dir, path) = temp_file("blob.bin", &[0xff, 0x00, 0xfe, 0xff]);
        let meta = classify(&path).expect("bin klassifiziert");
        assert!(meta.is_binary);
        assert_eq!(meta.size, 4);
        drop(dir);
    }

    #[test]
    fn binary_detection_for_size_limit() {
        let big = vec![b'a'; (MAX_TEXT_BYTES + 1) as usize];
        let (dir, path) = temp_file("big.txt", &big);
        let meta = classify(&path).expect("big klassifiziert");
        assert!(meta.is_binary);
        assert_eq!(meta.size, MAX_TEXT_BYTES + 1);
        // Genau an der Grenze ist noch Text.
        let (dir2, path2) = temp_file("edge.txt", &[b'a'; MAX_TEXT_BYTES as usize]);
        assert!(!classify(&path2).expect("edge klassifiziert").is_binary);
        drop((dir, dir2));
    }

    #[test]
    fn secret_paths_are_excluded_before_hashing() {
        for name in [
            ".env",
            "prod.env",
            "auth.toml",
            "server.key",
            "id_ed25519.key",
        ] {
            let (dir, path) = temp_file(name, b"secret");
            assert!(
                classify(&path).is_none(),
                "{name} muss ausgeschlossen werden"
            );
            drop(dir);
        }
        // Normalname wird nicht ausgeschlossen.
        let (dir, path) = temp_file("keys.md", b"dokumentation");
        assert!(classify(&path).is_some());
        drop(dir);
    }

    #[test]
    fn rename_matches_by_hash_not_path() {
        let (dir_old, old_path) = temp_file("old_name.md", b"# Inhalt\n");
        let (dir_c1, c1_path) = temp_file("anderer.md", b"# Anderes\n");
        let (dir_c2, c2_path) = temp_file("new_name.md", b"# Inhalt\n");
        let old = classify(&old_path).expect("old");
        let candidates = vec![
            classify(&c1_path).expect("c1"),
            classify(&c2_path).expect("c2"),
        ];
        assert_eq!(rename_source(&old, &candidates), Some(1));
        assert_eq!(rename_source(&old, &candidates[..1]), None);
        drop((dir_old, dir_c1, dir_c2));
    }
}
