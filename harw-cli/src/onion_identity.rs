//! Dauerhafte Onion-Identität: automatische Sicherung, Wiederherstellung und bewusste Rotation.
//!
//! Der Schlüssel der Onion-Adresse liegt im arti-Keystore unter `<onion_dir>/state`. Er wird bei jedem
//! Start des Onion-Dienstes gesichert (neben `<onion_dir>`, nicht darin) und beim Start zurückgespielt,
//! falls der Zustandsordner fehlt. Nur `harw admin onion-rotate --yes` ändert die Adresse absichtlich.

use std::fs;
use std::path::{Path, PathBuf};

const NICKNAME: &str = "harw-session";
const KEY_FILE: &str = "ks_hs_id.ed25519_expanded_private";

fn key_path(onion_dir: &Path) -> PathBuf {
    onion_dir.join("state").join("keystore").join("hss").join(NICKNAME).join(KEY_FILE)
}

/// Sicherungsordner neben dem Onion-Ordner: `<onion_dir>.identity-backup`.
pub(crate) fn backup_dir(onion_dir: &Path) -> PathBuf {
    let mut name = onion_dir.file_name().map_or_else(|| "onion".into(), std::ffi::OsString::from);
    name.push(".identity-backup");
    onion_dir.with_file_name(name)
}

#[cfg(unix)]
fn private_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| format!("{}: {e}", path.display()))
}
#[cfg(not(unix))]
fn private_mode(_: &Path, _: u32) -> Result<(), String> {
    Ok(())
}

fn private_dirs(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    private_mode(path, 0o700)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    harw_fsutil::write_atomic(path, bytes, harw_fsutil::AtomicWriteOptions::private())
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    private_mode(&tmp, 0o600)?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Spielt den gesicherten Schlüssel zurück, wenn er im Keystore fehlt. `Ok(true)` = wiederhergestellt.
pub(crate) fn restore_if_missing(onion_dir: &Path) -> Result<bool, String> {
    let live = key_path(onion_dir);
    let saved = backup_dir(onion_dir).join(KEY_FILE);
    if live.exists() || !saved.is_file() {
        return Ok(false);
    }
    let bytes = fs::read(&saved).map_err(|e| format!("{}: {e}", saved.display()))?;
    if let Some(parent) = live.parent() {
        let mut dir = onion_dir.to_path_buf();
        for part in parent.strip_prefix(onion_dir).map_err(|e| e.to_string())?.components() {
            dir.push(part);
            private_dirs(&dir)?;
        }
    }
    write_private(&live, &bytes)?;
    Ok(true)
}

/// Reads the persisted locator before launching Arti. Once a locator is
/// recorded, Arti must never generate a replacement key on ordinary startup.
#[cfg(feature = "onion")]
pub(crate) fn expected_locator(
    onion_dir: &Path,
) -> Result<Option<harw_onion::OnionLocator>, String> {
    let mut expected = None;
    for path in [onion_dir.join("locator"), backup_dir(onion_dir).join("locator")] {
        let locator = match fs::read_to_string(&path) {
            Ok(text) => text.trim().parse::<harw_onion::OnionLocator>().map_err(|error| {
                format!("invalid stored Onion locator at {}: {error}", path.display())
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        if expected.as_ref().is_some_and(|previous| previous != &locator) {
            return Err("live and backup Onion locators disagree".to_owned());
        }
        expected = Some(locator);
    }
    if expected.is_some() && !key_path(onion_dir).is_file() {
        return Err("the recorded Onion identity has no usable private key".to_owned());
    }
    Ok(expected)
}

/// Sichert den aktuellen Schlüssel, wenn er sich von der Sicherung unterscheidet. `Ok(true)` = geschrieben.
pub(crate) fn backup_now(onion_dir: &Path) -> Result<bool, String> {
    let live = key_path(onion_dir);
    let Ok(bytes) = fs::read(&live) else { return Ok(false) };
    let dir = backup_dir(onion_dir);
    let saved = dir.join(KEY_FILE);
    if fs::read(&saved).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    private_dirs(&dir)?;
    write_private(&saved, &bytes)?;
    if let Ok(locator) = fs::read(onion_dir.join("locator")) {
        write_private(&dir.join("locator"), &locator)?;
    }
    Ok(true)
}

/// Rotation: legt Schlüssel und Keystore-Ordner beiseite (`retired/<unix>`), löscht die aktuelle Sicherung
/// und die Adressdatei. Beim nächsten Start des Gateways entsteht eine neue Adresse.
pub(crate) fn rotate(onion_dir: &Path) -> Result<PathBuf, String> {
    let dir = backup_dir(onion_dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let retired = dir.join("retired").join(stamp.to_string());
    private_dirs(&retired)?;
    let keystore = onion_dir.join("state").join("keystore").join("hss").join(NICKNAME);
    if keystore.exists() {
        fs::rename(&keystore, retired.join("keystore")).map_err(|e| format!("{}: {e}", keystore.display()))?;
    }
    let hss = onion_dir.join("state").join("hss").join(NICKNAME);
    if hss.exists() {
        fs::rename(&hss, retired.join("hss")).map_err(|e| format!("{}: {e}", hss.display()))?;
    }
    for name in [KEY_FILE, "locator"] {
        let _ = fs::remove_file(dir.join(name));
    }
    let _ = fs::remove_file(onion_dir.join("locator"));
    Ok(retired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(root: &Path) -> PathBuf {
        let onion = root.join("onion");
        let key = key_path(&onion);
        fs::create_dir_all(key.parent().unwrap()).unwrap();
        fs::write(&key, b"secret").unwrap();
        fs::write(onion.join("locator"), b"x.onion:443\n").unwrap();
        onion
    }

    #[test]
    fn backup_restore_and_rotate() {
        let tmp = tempfile::tempdir().unwrap();
        let onion = seeded(tmp.path());
        assert!(backup_now(&onion).unwrap());
        assert!(!backup_now(&onion).unwrap(), "unchanged key is not rewritten");
        fs::remove_dir_all(onion.join("state")).unwrap();
        assert!(restore_if_missing(&onion).unwrap());
        assert_eq!(fs::read(key_path(&onion)).unwrap(), b"secret");
        let retired = rotate(&onion).unwrap();
        assert!(retired.join("keystore").exists());
        assert!(!key_path(&onion).exists());
        assert!(!restore_if_missing(&onion).unwrap(), "rotation drops the backup");
    }

    #[cfg(feature = "onion")]
    #[test]
    fn recorded_locator_requires_a_restored_key() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let onion = seeded(tmp.path());
        let text = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443";
        fs::write(onion.join("locator"), text)?;
        assert!(backup_now(&onion)?);
        fs::remove_file(key_path(&onion))?;
        assert!(expected_locator(&onion).is_err());
        assert!(restore_if_missing(&onion)?);
        assert_eq!(expected_locator(&onion)?, Some(text.parse()?));
        Ok(())
    }
}
