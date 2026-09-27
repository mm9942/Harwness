//! Token-Store: schreibt einen Setup-Token als 0600-Datei und liefert die
//! zugehörige `file:`-[`harw_config::SecretRef`] zurück.
//!
//! Der Token wird niemals geloggt; die Datei erhält unter Unix `0o600`.
//!
//! Außerdem liegt hier der crate-weite atomare Schreiber
//! [`write_private_file_atomically`], den sowohl [`save_token`] als auch der
//! Codex-Refresh (`codex_refresh.rs`) für das Zurückschreiben von
//! `auth.json` nutzen.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret as _, SecretString};

use crate::error::{OAuthError, OAuthResult};

/// Returns whether `provider` is safe to embed in a token filename.
///
/// Provider identifiers are deliberately restricted to portable ASCII filename
/// components. This prevents both path traversal and platform-specific path
/// separators from reaching the filesystem boundary.
fn is_safe_provider_name(provider: &str) -> bool {
    !provider.is_empty()
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Dateiname des gespeicherten Setup-Tokens je Provider.
fn token_path(secrets_dir: &Path, provider: &str) -> OAuthResult<PathBuf> {
    if !is_safe_provider_name(provider) {
        return Err(OAuthError::UnsafeProviderFilenameComponent);
    }

    Ok(secrets_dir.join(format!("{provider}-oauth.token")))
}

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Erzeugt einen eindeutigen Temp-Pfad neben `path` (PID + Nanosekunden +
/// Zähler), damit `create_new` keine kollidierenden Namen sieht.
fn temporary_path_beside(path: &Path) -> PathBuf {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("harw-oauth");
    path.with_file_name(format!(
        ".{file_name}.tmp-{}-{timestamp}-{counter}",
        std::process::id()
    ))
}

fn create_restricted_temp(path: &Path) -> OAuthResult<(File, PathBuf)> {
    for _ in 0..16 {
        let temporary_path = temporary_path_beside(path);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }

        match options.open(&temporary_path) {
            Ok(file) => return Ok((file, temporary_path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary token file",
    )
    .into())
}

/// Schreibt `bytes` atomar nach `path`: eindeutige Temp-Datei daneben
/// (`create_new`, unter Unix `0o600`), `fsync`, `rename` auf das Ziel und
/// danach `fsync` des Elternverzeichnisses.
///
/// Das Ziel trägt nach Erfolg unter Unix immer `0o600`, auch wenn vorher eine
/// Datei mit weiteren Rechten (z. B. `0o644`) dort lag, denn `rename` ersetzt
/// den Verzeichniseintrag statt die Altdatei zu öffnen. `create_new` folgt
/// keinem untergeschobenen Symlink auf dem Temp-Pfad. Einziger crate-weiter
/// Schreiber für Token-Dateien ([`save_token`]) und `auth.json`
/// (Codex-Refresh).
///
/// # Arguments
/// - `path` (`&Path`): Zieldatei; ein leeres Elternteil bedeutet `.`.
/// - `bytes` (`&[u8]`): der vollständige neue Dateiinhalt.
///
/// # Errors
/// - [`OAuthError::TokenStoreIo`]: wenn `path` keinen Dateinamen hat
///   (`InvalidInput`) oder Anlegen, Rechte setzen, Schreiben, Synchronisieren
///   oder `rename` fehlschlagen. Eine bereits angelegte Temp-Datei wird dann
///   nach bestem Bemühen wieder entfernt.
///
/// # Concurrency
/// Kein geteilter Zustand außer dem atomaren Zähler für Temp-Namen. Parallele
/// Schreiber auf dasselbe Ziel überschreiben sich gegenseitig vollständig
/// (letzter `rename` gewinnt), nie teilweise; wer Lesen-Ändern-Schreiben
/// braucht, serialisiert selbst (der Codex-Refresh über seinen Datei-Lock).
pub(crate) fn write_private_file_atomically(path: &Path, bytes: &[u8]) -> OAuthResult<()> {
    if path.file_name().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "atomic write target has no file name",
        )
        .into());
    }
    #[cfg(unix)]
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let (mut temporary_file, temporary_path) = create_restricted_temp(path)?;
    let write_result = (|| -> OAuthResult<()> {
        use std::io::Write as _;

        // Rechte explizit auf dem offenen Handle setzen (doppelte Absicherung,
        // falls Open-Flags/Umask abweichen). Innerhalb des Closures, damit ein
        // Fehlschlag die Temp-Datei ebenfalls wieder aufräumt.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            temporary_file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }

        temporary_file.write_all(bytes)?;
        temporary_file.flush()?;
        temporary_file.sync_all()?;
        drop(temporary_file);
        std::fs::rename(&temporary_path, path)?;

        #[cfg(unix)]
        {
            File::open(parent)?.sync_all()?;
        }

        Ok(())
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    write_result
}

/// Speichert `token` unter `<home>/secrets/<provider>-oauth.token` (0600) und
/// gibt die passende `file:`-[`harw_config::SecretRef`] zurück.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space (`~/.harw`).
/// - `provider` (`&str`): Provider-Name (z. B. `"anthropic"`).
/// - `token` (`&SecretString`): der zu speichernde Setup-Token.
///
/// # Returns
/// Die `file:`-Referenz auf die geschriebene Token-Datei.
///
/// # Errors
/// - [`OAuthError::TokenStoreIo`]: bei Verzeichnis-/Datei-/Synchronisationsfehlern.
/// - [`OAuthError::SecretRef`]: wenn der Pfad nicht als `SecretRef` parst.
/// - [`OAuthError::UnsafeProviderFilenameComponent`]: wenn der Provider kein sicherer Dateiname ist
///   oder das Secrets-Verzeichnis aus dem Home-Verzeichnis herauszeigt.
///
/// # Concurrency
/// Kein geteilter Zustand; sicher aus einem einzelnen Prozess.
pub fn save_token(
    home: &Path,
    provider: &str,
    token: &SecretString,
) -> OAuthResult<harw_config::SecretRef> {
    // Validate before creating any directory or temporary file.
    if !is_safe_provider_name(provider) {
        return Err(OAuthError::UnsafeProviderFilenameComponent);
    }

    std::fs::create_dir_all(home)?;
    let canonical_home = home.canonicalize()?;
    let secrets_dir = home.join("secrets");
    std::fs::create_dir_all(&secrets_dir)?;
    let canonical_secrets_dir = secrets_dir.canonicalize()?;
    if !canonical_secrets_dir.starts_with(&canonical_home) {
        return Err(OAuthError::UnsafeProviderFilenameComponent);
    }

    let path = token_path(&canonical_secrets_dir, provider)?;
    write_private_file_atomically(&path, token.expose_secret().as_bytes())?;

    format!("file:{}", path.display())
        .parse()
        .map_err(|error: harw_config::ConfigError| OAuthError::SecretRef(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn test_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("harw-oauth-store-{name}-{}", std::process::id()))
    }

    #[test]
    fn test_save_token_writes_file_and_returns_ref() -> TestResult {
        let home = test_home("save");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("create home"))?;

        let secret = SecretString::new("sk-ant-oat-EXAMPLE".to_owned().into_boxed_str());
        let secret_ref = save_token(&home, "anthropic", &secret).map_err(ctx("save"))?;

        let path =
            token_path(&home.join("secrets"), "anthropic").map_err(ctx("safe token path"))?;
        assert!(path.is_file());
        let stored = std::fs::read(&path).map_err(ctx("read stored token"))?;
        assert!(
            stored == secret.expose_secret().as_bytes(),
            "stored token bytes mismatch"
        );
        assert!(secret_ref.as_ref_string().starts_with("file:"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .map_err(ctx("read metadata"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn test_save_token_replaces_existing_file_without_temp_artifacts() -> TestResult {
        let home = test_home("replace");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("create home"))?;

        let first = SecretString::new("first-token".to_owned().into_boxed_str());
        let second = SecretString::new("second-token".to_owned().into_boxed_str());
        save_token(&home, "anthropic", &first).map_err(ctx("save first token"))?;
        save_token(&home, "anthropic", &second).map_err(ctx("save second token"))?;

        let path =
            token_path(&home.join("secrets"), "anthropic").map_err(ctx("safe token path"))?;
        let stored = std::fs::read(&path).map_err(ctx("read stored token"))?;
        assert!(
            stored == second.expose_secret().as_bytes(),
            "stored token bytes mismatch"
        );
        let entries =
            std::fs::read_dir(home.join("secrets")).map_err(ctx("read secrets directory"))?;
        assert!(entries.filter_map(Result::ok).all(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".anthropic-oauth.token.tmp-")
        }));

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    /// Dateinamen aller Einträge in `dir` (für den Nachweis "keine Temp-Reste").
    fn directory_entry_names(dir: &Path) -> TestResult<Vec<std::ffi::OsString>> {
        std::fs::read_dir(dir)
            .map_err(ctx("read directory"))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(ctx("read directory entry"))
    }

    #[test]
    fn test_write_private_file_atomically_writes_0600_and_leaves_no_temp_artifacts() -> TestResult {
        let dir = test_home("atomic-fresh");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(ctx("create directory"))?;
        let target = dir.join("private.json");

        write_private_file_atomically(&target, b"{\"fresh\":true}")
            .map_err(ctx("atomic write"))?;

        let stored = std::fs::read(&target).map_err(ctx("read target"))?;
        assert!(stored == b"{\"fresh\":true}", "stored bytes mismatch");
        assert_eq!(
            directory_entry_names(&dir)?,
            vec![std::ffi::OsString::from("private.json")]
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&target)
                .map_err(ctx("read metadata"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn test_write_private_file_atomically_replaces_existing_0644_file_with_0600() -> TestResult {
        let dir = test_home("atomic-replace");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(ctx("create directory"))?;
        let target = dir.join("auth.json");
        std::fs::write(&target, b"old-content").map_err(ctx("pre-create target"))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
                .map_err(ctx("set 0644 on pre-existing target"))?;
        }

        write_private_file_atomically(&target, b"new-content").map_err(ctx("atomic write"))?;

        let stored = std::fs::read(&target).map_err(ctx("read target"))?;
        assert!(stored == b"new-content", "stored bytes mismatch");
        assert_eq!(
            directory_entry_names(&dir)?,
            vec![std::ffi::OsString::from("auth.json")]
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&target)
                .map_err(ctx("read metadata"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn test_write_private_file_atomically_rejects_path_without_file_name() {
        let result = write_private_file_atomically(Path::new("/"), b"secret");

        assert!(
            matches!(
                &result,
                Err(OAuthError::TokenStoreIo(error))
                    if error.kind() == std::io::ErrorKind::InvalidInput
            ),
            "expected TokenStoreIo(InvalidInput), got {result:?}"
        );
    }

    #[test]
    fn test_save_token_rejects_unsafe_provider_names_before_writing() {
        let secret = SecretString::new("token".to_owned().into_boxed_str());

        for (index, provider) in [
            "../outside",
            "provider/name",
            "provider\\name",
            ".",
            "..",
            "control\u{0000}name",
            "control\u{001f}name",
        ]
        .into_iter()
        .enumerate()
        {
            let home = test_home(&format!("unsafe-{index}"));
            let _ = std::fs::remove_dir_all(&home);

            let result = save_token(&home, provider, &secret);
            assert!(
                matches!(result, Err(OAuthError::UnsafeProviderFilenameComponent)),
                "provider {provider:?} should be rejected"
            );
            assert!(
                !home.exists(),
                "provider {provider:?} must be rejected before filesystem writes"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_save_token_rejects_secrets_symlink_outside_home() -> TestResult {
        use std::os::unix::fs::symlink;

        let home = test_home("symlink-escape");
        let outside = test_home("symlink-outside");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&home).map_err(ctx("create home"))?;
        std::fs::create_dir_all(&outside).map_err(ctx("create outside directory"))?;
        symlink(&outside, home.join("secrets")).map_err(ctx("create secrets symlink"))?;

        let secret = SecretString::new("token".to_owned().into_boxed_str());
        let result = save_token(&home, "anthropic", &secret);

        assert!(matches!(
            result,
            Err(OAuthError::UnsafeProviderFilenameComponent)
        ));
        assert!(
            !outside.join("anthropic-oauth.token").exists(),
            "must not write through a secrets-directory symlink"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&outside);
        Ok(())
    }
}
