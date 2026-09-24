//! Inhaltsadressierter, ablaufender Zwischenspeicher für Telegram-Anhänge.
//!
//! # Verantwortung
//! Legt heruntergeladene Anhänge eines Gesprächs unter
//! `<root>/<hex(scope)>/<sha256>` ab, begleitet von einer JSON-Beschreibung
//! `<root>/<hex(scope)>/<sha256>.json` ([`CachedAttachment`]). Der Scope ist
//! der kanonische [`SessionKey`] des Chats (bei sehr langen Schlüsseln
//! `h-<sha256(canonical_key)>`), sodass Anhänge verschiedener Gespräche nie
//! denselben Pfad teilen.
//!
//! # Sicherheit
//! - Dateien entstehen unter Unix mit Modus 0600, Verzeichnisse mit 0700.
//! - Der vom Absender gelieferte Dateiname wird nur als bereinigte Metadaten
//!   gespeichert und nie als Pfadbestandteil verwendet.
//! - [`AttachmentCache::purge_expired`] löscht ausschließlich Dateien, deren
//!   Name ein 64-stelliger Hex-Digest ist, und bestimmt den Datenpfad aus dem
//!   Ort der Beschreibung, nicht aus deren Inhalt.
//!
//! # Nebenläufigkeit
//! Ein prozessinterner [`Mutex`] serialisiert `store` und `purge_expired`.

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use harw_channel::SessionKey;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{TelegramChannelError, TelegramChannelResult};

/// Maximale Länge des hex-kodierten Scopes als Verzeichnisname.
const MAX_HEX_DIR_LEN: usize = 200;
/// Maximale Länge eines bereinigten Dateinamens (in Zeichen).
const MAX_FILE_NAME_CHARS: usize = 128;

/// Ein zwischengespeicherter Anhang (zugleich Inhalt der `.json`-Beschreibung).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedAttachment {
    /// SHA-256 des Inhalts, klein-hex (64 Zeichen); zugleich Dateiname.
    pub digest_sha256: String,
    /// Pfad der Inhaltsdatei.
    pub path: PathBuf,
    /// Deklarierter bzw. geprüfter MIME-Typ.
    pub mime: String,
    /// Bereinigter ursprünglicher Dateiname, sofern vorhanden.
    pub file_name: Option<String>,
    /// Größe des Inhalts in Bytes.
    pub size_bytes: u64,
    /// Zeitpunkt der (letzten) Ablage.
    pub stored_at: Timestamp,
    /// Ab diesem Zeitpunkt entfernt [`AttachmentCache::purge_expired`] ihn.
    pub expires_at: Timestamp,
}

/// Inhaltsadressierter Anhang-Speicher, siehe Moduldokumentation.
pub struct AttachmentCache {
    root: PathBuf,
    ttl: SignedDuration,
    guard: Mutex<()>,
}

impl fmt::Debug for AttachmentCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentCache")
            .field("root", &self.root)
            .field("ttl", &self.ttl)
            .finish()
    }
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn scope_dir_name(scope: &SessionKey) -> String {
    let canonical = scope.canonical_key();
    let encoded = hex(canonical.as_bytes());
    if encoded.len() <= MAX_HEX_DIR_LEN {
        encoded
    } else {
        format!("h-{}", hex(&Sha256::digest(canonical.as_bytes())))
    }
}

fn is_digest_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Bereinigt einen vom Absender gelieferten Dateinamen: nur die letzte
/// Pfadkomponente, nur `[A-Za-z0-9._-]` (Leerzeichen → `_`, sonst verworfen),
/// ohne führende Punkte, höchstens 128 Zeichen; leer → `None`.
#[must_use]
pub(crate) fn sanitize_file_name(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last
        .chars()
        .filter_map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => Some(c),
            ' ' => Some('_'),
            _ => None,
        })
        .collect();
    let trimmed: String = cleaned
        .trim_start_matches('.')
        .chars()
        .take(MAX_FILE_NAME_CHARS)
        .collect();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Eindeutiger Suffix für temporäre Dateien dieses Prozesses.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Legt `dir` (rekursiv) an; unter Unix mit Modus 0700.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Schreibt `bytes` atomar nach `target`: exklusiv angelegte temporäre Datei
/// (Unix: Modus 0600) im selben Verzeichnis, `sync_all`, dann `rename`.
fn write_private_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target
        .parent()
        .ok_or_else(|| std::io::Error::other("Zielpfad ohne Elternverzeichnis"))?;
    let base = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment");
    let mut attempt = 0u32;
    let (temp_path, mut file) = loop {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = dir.join(format!(".{base}.{}.{n}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => break (candidate, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt < 16 => {
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written.and_then(|()| std::fs::rename(&temp_path, target));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl AttachmentCache {
    /// Erzeugt einen Cache unter `root` (wird bei Bedarf angelegt), dessen
    /// Einträge `ttl` nach der Ablage ablaufen.
    #[must_use]
    pub fn new(root: &Path, ttl: SignedDuration) -> Self {
        Self {
            root: root.to_path_buf(),
            ttl,
            guard: Mutex::new(()),
        }
    }

    /// Legt `bytes` für `scope` ab und liefert die Beschreibung.
    ///
    /// Gleicher Inhalt im selben Scope landet in derselben Datei; eine
    /// erneute Ablage aktualisiert Metadaten und Ablaufzeit.
    ///
    /// # Errors
    /// [`TelegramChannelError::Io`] / [`TelegramChannelError::Serde`] bei
    /// Schreib- bzw. Serialisierungsfehlern.
    pub fn store(
        &self,
        scope: &SessionKey,
        bytes: &[u8],
        mime: &str,
        file_name: Option<&str>,
        now: Timestamp,
    ) -> TelegramChannelResult<CachedAttachment> {
        let digest = hex(&Sha256::digest(bytes));
        let dir = self.root.join(scope_dir_name(scope));
        let path = dir.join(&digest);
        let sidecar = dir.join(format!("{digest}.json"));
        let expires_at = now.checked_add(self.ttl).unwrap_or(Timestamp::MAX);
        let attachment = CachedAttachment {
            digest_sha256: digest,
            path: path.clone(),
            mime: mime.trim().to_owned(),
            file_name: file_name.and_then(sanitize_file_name),
            size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            stored_at: now,
            expires_at,
        };
        let description = serde_json::to_vec(&attachment)?;

        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        create_private_dir(&dir)?;
        let existing_matches = std::fs::metadata(&path)
            .map(|meta| meta.is_file() && meta.len() == attachment.size_bytes)
            .unwrap_or(false);
        if !existing_matches {
            write_private_atomic(&path, bytes)?;
        }
        write_private_atomic(&sidecar, &description)?;
        Ok(attachment)
    }

    /// Entfernt alle Einträge, deren `expires_at <= now`, und liefert deren
    /// Anzahl. Beschädigte Beschreibungen werden protokolliert und
    /// übersprungen; leere Scope-Verzeichnisse werden (best effort) entfernt.
    ///
    /// # Errors
    /// [`TelegramChannelError::Io`], wenn Verzeichnisse nicht gelesen oder
    /// abgelaufene Dateien nicht gelöscht werden können. Ein fehlendes
    /// Wurzelverzeichnis ergibt `Ok(0)`.
    pub fn purge_expired(&self, now: Timestamp) -> TelegramChannelResult<usize> {
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        let scopes = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(TelegramChannelError::from(error)),
        };
        let mut removed = 0usize;
        for scope in scopes {
            let scope = scope?;
            if !scope.file_type()?.is_dir() {
                continue;
            }
            let scope_dir = scope.path();
            for entry in std::fs::read_dir(&scope_dir)? {
                let sidecar = entry?.path();
                let Some(digest) = sidecar
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".json"))
                    .filter(|stem| is_digest_name(stem))
                    .map(str::to_owned)
                else {
                    continue;
                };
                let description: CachedAttachment = match std::fs::read(&sidecar)
                    .map_err(TelegramChannelError::from)
                    .and_then(|raw| serde_json::from_slice(&raw).map_err(Into::into))
                {
                    Ok(description) => description,
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "Anhang-Beschreibung konnte nicht gelesen werden"
                        );
                        continue;
                    }
                };
                if description.expires_at > now {
                    continue;
                }
                remove_if_present(&scope_dir.join(&digest))?;
                remove_if_present(&sidecar)?;
                removed += 1;
            }
            // Nur leere Verzeichnisse lassen sich entfernen; Fehler sind hier
            // erwartbar (noch belegt) und werden ignoriert.
            let _ = std::fs::remove_dir(&scope_dir);
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{ChannelId, PeerId, TenantId};

    fn scope(peer: &str) -> SessionKey {
        SessionKey::new(
            TenantId::from_str("ops"),
            ChannelId::from_str("telegram:ops"),
            PeerId::from_str(peer),
            None,
        )
    }

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    #[test]
    fn store_is_content_addressed_per_scope_with_sidecar() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let cache = AttachmentCache::new(dir.path(), SignedDuration::from_secs(60));
        let now = at(1_700_000_000)?;

        let first = cache
            .store(
                &scope("1"),
                b"hello",
                "text/plain",
                Some("../../etc/pass wd.txt"),
                now,
            )
            .map_err(ctx("store"))?;
        assert_eq!(first.digest_sha256, hex(&Sha256::digest(b"hello")));
        assert_eq!(first.size_bytes, 5);
        assert_eq!(first.file_name.as_deref(), Some("pass_wd.txt"));
        assert_eq!(first.expires_at, at(1_700_000_060)?);
        assert_eq!(
            first.path,
            dir.path()
                .join(scope_dir_name(&scope("1")))
                .join(&first.digest_sha256)
        );
        assert_eq!(
            std::fs::read(&first.path).map_err(ctx("read data"))?,
            b"hello"
        );
        let sidecar = first.path.with_extension("json");
        let described: CachedAttachment =
            serde_json::from_slice(&std::fs::read(&sidecar).map_err(ctx("read sidecar"))?)
                .map_err(ctx("parse sidecar"))?;
        assert_eq!(described, first);

        let again = cache
            .store(&scope("1"), b"hello", "text/plain", None, now)
            .map_err(ctx("store again"))?;
        assert_eq!(again.path, first.path);

        let other = cache
            .store(&scope("2"), b"hello", "text/plain", None, now)
            .map_err(ctx("other scope"))?;
        assert_ne!(other.path, first.path);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn stored_files_are_private() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let cache = AttachmentCache::new(dir.path(), SignedDuration::from_secs(60));
        let stored = cache
            .store(&scope("1"), b"secret", "application/pdf", None, at(0)?)
            .map_err(ctx("store"))?;
        for path in [stored.path.clone(), stored.path.with_extension("json")] {
            let mode = std::fs::metadata(&path)
                .map_err(ctx("metadata"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        Ok(())
    }

    #[test]
    fn purge_expired_removes_only_expired_entries() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let cache = AttachmentCache::new(dir.path(), SignedDuration::from_secs(100));
        assert_eq!(cache.purge_expired(at(0)?).map_err(ctx("empty"))?, 0);

        let old = cache
            .store(&scope("1"), b"old", "text/plain", None, at(1_000)?)
            .map_err(ctx("old"))?;
        let fresh = cache
            .store(&scope("2"), b"fresh", "text/plain", None, at(1_050)?)
            .map_err(ctx("fresh"))?;
        // Beschädigte Beschreibung wird übersprungen, nicht gelöscht.
        let corrupt = dir
            .path()
            .join(scope_dir_name(&scope("3")))
            .join(format!("{}.json", "0".repeat(64)));
        create_private_dir(corrupt.parent().ok_or(TestError::Missing("parent"))?)
            .map_err(ctx("corrupt dir"))?;
        std::fs::write(&corrupt, b"not json").map_err(ctx("corrupt write"))?;

        assert_eq!(cache.purge_expired(at(1_099)?).map_err(ctx("none"))?, 0);
        assert_eq!(cache.purge_expired(at(1_100)?).map_err(ctx("one"))?, 1);
        assert!(!old.path.exists());
        assert!(!old.path.with_extension("json").exists());
        assert!(fresh.path.exists());
        assert!(corrupt.exists());
        assert_eq!(cache.purge_expired(at(1_150)?).map_err(ctx("two"))?, 1);
        assert!(!fresh.path.exists());
        Ok(())
    }

    #[test]
    fn sanitize_file_name_strips_paths_and_controls() {
        assert_eq!(sanitize_file_name("a/b\\c.pdf").as_deref(), Some("c.pdf"));
        assert_eq!(sanitize_file_name("..hidden").as_deref(), Some("hidden"));
        assert_eq!(sanitize_file_name("ä\u{0}\n"), None);
        assert_eq!(sanitize_file_name("/"), None);
        assert_eq!(
            sanitize_file_name(&"x".repeat(500)).map(|name| name.len()),
            Some(MAX_FILE_NAME_CHARS)
        );
    }
}
