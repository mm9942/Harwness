//! Eigentums- und Modusprüfungen für Dateien, denen der Warden vertrauen muss
//! (PL-100 WP 3.1): [`TrustRules`], [`read_trusted_file`],
//! [`check_trusted_dir`].
//!
//! # Zweck
//! Die Warden-Policy (`/etc/harw-dod/warden.toml`) und die Proof-Schlüssel
//! (`proof_key_dir`) bestimmen, was der Warden ausführt. Wer sie schreiben
//! kann, steuert den Warden. Deshalb liest der Warden sie nur, wenn
//!
//! - die Datei ein reguläres File ist (kein Symlink, kein Gerät),
//! - sie dem erwarteten Eigentümer gehört (Produktion: `root`, uid 0),
//! - sie nicht gruppen- oder weltschreibbar ist (Schlüssel: überhaupt keine
//!   Gruppen- oder Weltrechte, also `0600`/`0400`),
//! - das Elternverzeichnis demselben Eigentümer gehört und nicht gruppen-
//!   oder weltschreibbar ist (sonst ließe sich die Datei austauschen),
//! - die Datei zwischen Prüfung und Öffnen nicht ersetzt wurde (Gerät und
//!   Inode des geöffneten Deskriptors stimmen mit der Prüfung überein),
//! - sie die Größenobergrenze einhält.
//!
//! # Warum der Eigentümer konfigurierbar ist
//! Produktion verlangt `root` ([`TrustRules::root`]). Tests laufen nicht als
//! root und prüfen mit dem Eigentümer ihres Tempdirs
//! ([`TrustRules::owned_by`]); die Regeln selbst sind identisch.
//!
//! # Nebenläufigkeit
//! Reine Funktionen über `std::fs`; kein geteilter Zustand.
//!
//! # Fehler
//! [`WardenError::UntrustedFile`] mit festem Grund, [`WardenError::Io`] für
//! Lesefehler. Kein Pfad erscheint in einer Meldung.

use std::fs::{File, Metadata};
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use crate::error::{WardenError, WardenResult};

// Gruppen- oder Weltschreibrecht.
const GROUP_OR_WORLD_WRITABLE: u32 = 0o022;
// Irgendein Gruppen- oder Weltrecht.
const GROUP_OR_WORLD_ANY: u32 = 0o077;

/// Eigentumsregel für vertrauenswürdige Dateien.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::trust::TrustRules;
///
/// assert_eq!(TrustRules::root().owner_uid(), 0);
/// assert_eq!(TrustRules::owned_by(1000).owner_uid(), 1000);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustRules {
    owner_uid: u32,
}

impl TrustRules {
    /// Production rule: files and their directory must be owned by root.
    #[must_use]
    pub const fn root() -> Self {
        Self { owner_uid: 0 }
    }

    /// Rule with an explicit owner (tests, non-root fixtures).
    #[must_use]
    pub const fn owned_by(owner_uid: u32) -> Self {
        Self { owner_uid }
    }

    /// Returns the required owner uid.
    #[must_use]
    pub const fn owner_uid(self) -> u32 {
        self.owner_uid
    }
}

/// Wie streng die Modusbits einer Datei geprüft werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSecrecy {
    /// Öffentlich lesbar erlaubt, aber nicht gruppen- oder weltschreibbar
    /// (Policy-Datei).
    Public,
    /// Keinerlei Gruppen- oder Weltrechte (`0600`/`0400`; Schlüssel).
    Private,
}

/// Checks that `dir` is a directory owned per `rules` and not group/world writable.
///
/// # Arguments
/// - `dir` (`&Path`): das Verzeichnis.
/// - `rules` (`TrustRules`): erwarteter Eigentümer.
/// - `what` (`&'static str`): fester Name für Fehlermeldungen.
///
/// # Errors
/// - [`WardenError::Io`]: Metadaten nicht lesbar.
/// - [`WardenError::UntrustedFile`]: kein Verzeichnis (auch Symlink),
///   falscher Eigentümer, gruppen- oder weltschreibbar.
pub fn check_trusted_dir(dir: &Path, rules: TrustRules, what: &'static str) -> WardenResult<()> {
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.file_type().is_dir() {
        return Err(WardenError::UntrustedFile {
            what,
            reason: "directory is not a plain directory",
        });
    }
    check_owner_and_mode(&metadata, rules, GROUP_OR_WORLD_WRITABLE, what)
}

/// Reads a file the warden must trust, after all ownership and mode checks.
///
/// # Description
/// Siehe Moduldoku für die Liste der Prüfungen. Liest höchstens `max_len`
/// Bytes; eine längere Datei ist ein Fehler, keine stille Kürzung.
///
/// # Arguments
/// - `path` (`&Path`): die Datei.
/// - `rules` (`TrustRules`): erwarteter Eigentümer (Datei und Elternverzeichnis).
/// - `secrecy` (`FileSecrecy`): erlaubte Modusbits.
/// - `max_len` (`u64`): Größenobergrenze in Bytes.
/// - `what` (`&'static str`): fester Name für Fehlermeldungen.
///
/// # Returns
/// Den Dateiinhalt.
///
/// # Errors
/// - [`WardenError::Io`]: Datei oder Elternverzeichnis nicht lesbar.
/// - [`WardenError::UntrustedFile`]: siehe Moduldoku.
pub fn read_trusted_file(
    path: &Path,
    rules: TrustRules,
    secrecy: FileSecrecy,
    max_len: u64,
    what: &'static str,
) -> WardenResult<Vec<u8>> {
    let parent = path.parent().ok_or(WardenError::UntrustedFile {
        what,
        reason: "path has no parent directory",
    })?;
    check_trusted_dir(parent, rules, what)?;

    let before = std::fs::symlink_metadata(path)?;
    if !before.file_type().is_file() {
        return Err(WardenError::UntrustedFile {
            what,
            reason: "not a regular file",
        });
    }
    let forbidden_bits = match secrecy {
        FileSecrecy::Public => GROUP_OR_WORLD_WRITABLE,
        FileSecrecy::Private => GROUP_OR_WORLD_ANY,
    };
    check_owner_and_mode(&before, rules, forbidden_bits, what)?;

    let file = File::open(path)?;
    let opened = file.metadata()?;
    if opened.dev() != before.dev() || opened.ino() != before.ino() {
        return Err(WardenError::UntrustedFile {
            what,
            reason: "file was replaced while opening",
        });
    }
    check_owner_and_mode(&opened, rules, forbidden_bits, what)?;
    if opened.len() > max_len {
        return Err(WardenError::UntrustedFile {
            what,
            reason: "file exceeds the size limit",
        });
    }

    let mut contents = Vec::new();
    file.take(max_len.saturating_add(1))
        .read_to_end(&mut contents)?;
    let read_len = u64::try_from(contents.len()).unwrap_or(u64::MAX);
    if read_len > max_len {
        return Err(WardenError::UntrustedFile {
            what,
            reason: "file exceeds the size limit",
        });
    }
    Ok(contents)
}

// Eigentümer und verbotene Modusbits prüfen.
fn check_owner_and_mode(
    metadata: &Metadata,
    rules: TrustRules,
    forbidden_bits: u32,
    what: &'static str,
) -> WardenResult<()> {
    if metadata.uid() != rules.owner_uid() {
        return Err(WardenError::UntrustedFile {
            what,
            reason: "owner is not the required owner",
        });
    }
    if metadata.mode() & forbidden_bits != 0 {
        return Err(WardenError::UntrustedFile {
            what,
            reason: if forbidden_bits == GROUP_OR_WORLD_ANY {
                "mode grants group or world access"
            } else {
                "group or world writable"
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use super::{FileSecrecy, TrustRules, check_trusted_dir, read_trusted_file};
    use crate::error::WardenError;
    use crate::test_support::{TestError, TestResult, ctx};

    fn owner_of(path: &std::path::Path) -> TestResult<TrustRules> {
        let metadata = std::fs::metadata(path).map_err(ctx("metadata"))?;
        Ok(TrustRules::owned_by(metadata.uid()))
    }

    fn write_with_mode(path: &std::path::Path, contents: &[u8], mode: u32) -> TestResult {
        std::fs::write(path, contents).map_err(ctx("write"))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(ctx("chmod"))?;
        Ok(())
    }

    fn expect_untrusted(result: crate::error::WardenResult<Vec<u8>>, reason: &str) -> TestResult {
        match result {
            Err(WardenError::UntrustedFile { reason: got, .. }) if got == reason => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected UntrustedFile({reason}), got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_reads_owner_file_without_write_bits_for_others() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = owner_of(dir.path())?;
        let path = dir.path().join("warden.toml");
        write_with_mode(&path, b"schema = 1\n", 0o644)?;
        let contents = read_trusted_file(&path, rules, FileSecrecy::Public, 1024, "policy")
            .map_err(ctx("trusted read"))?;
        assert_eq!(contents, b"schema = 1\n");
        Ok(())
    }

    #[test]
    fn test_group_or_world_writable_file_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = owner_of(dir.path())?;
        let path = dir.path().join("warden.toml");
        for mode in [0o664, 0o646, 0o666] {
            write_with_mode(&path, b"x", mode)?;
            expect_untrusted(
                read_trusted_file(&path, rules, FileSecrecy::Public, 1024, "policy"),
                "group or world writable",
            )?;
        }
        Ok(())
    }

    #[test]
    fn test_private_file_must_not_grant_any_group_or_world_access() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = owner_of(dir.path())?;
        let path = dir.path().join("k1.key");
        write_with_mode(&path, b"x", 0o640)?;
        expect_untrusted(
            read_trusted_file(&path, rules, FileSecrecy::Private, 1024, "key"),
            "mode grants group or world access",
        )?;
        write_with_mode(&path, b"x", 0o600)?;
        assert!(read_trusted_file(&path, rules, FileSecrecy::Private, 1024, "key").is_ok());
        write_with_mode(&path, b"x", 0o400)?;
        assert!(read_trusted_file(&path, rules, FileSecrecy::Private, 1024, "key").is_ok());
        Ok(())
    }

    #[test]
    fn test_foreign_owner_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let own = owner_of(dir.path())?;
        let foreign = TrustRules::owned_by(own.owner_uid().wrapping_add(1));
        let path = dir.path().join("warden.toml");
        write_with_mode(&path, b"x", 0o644)?;
        // Schon das Elternverzeichnis gehört nicht dem verlangten Eigentümer.
        expect_untrusted(
            read_trusted_file(&path, foreign, FileSecrecy::Public, 1024, "policy"),
            "owner is not the required owner",
        )
    }

    #[test]
    fn test_symlink_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = owner_of(dir.path())?;
        let target = dir.path().join("real.toml");
        write_with_mode(&target, b"x", 0o644)?;
        let link = dir.path().join("warden.toml");
        std::os::unix::fs::symlink(target, &link).map_err(ctx("symlink"))?;
        expect_untrusted(
            read_trusted_file(&link, rules, FileSecrecy::Public, 1024, "policy"),
            "not a regular file",
        )
    }

    #[test]
    fn test_oversized_file_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = owner_of(dir.path())?;
        let path = dir.path().join("warden.toml");
        write_with_mode(&path, &[b'#'; 64], 0o644)?;
        expect_untrusted(
            read_trusted_file(&path, rules, FileSecrecy::Public, 16, "policy"),
            "file exceeds the size limit",
        )
    }

    #[test]
    fn test_writable_parent_directory_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let rules = owner_of(dir.path())?;
        let sub = dir.path().join("etc");
        std::fs::create_dir(&sub).map_err(ctx("mkdir"))?;
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("chmod dir"))?;
        let path = sub.join("warden.toml");
        write_with_mode(&path, b"x", 0o644)?;
        expect_untrusted(
            read_trusted_file(&path, rules, FileSecrecy::Public, 1024, "policy"),
            "group or world writable",
        )?;
        assert!(matches!(
            check_trusted_dir(&sub, rules, "dir"),
            Err(WardenError::UntrustedFile { .. })
        ));
        Ok(())
    }
}
