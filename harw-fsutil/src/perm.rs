//! Rechteprüfung für private Dateien (Secrets, Tokens, Zustand).

use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;

/// Prüft, dass `file` eine reguläre Datei ist, der effektiven UID gehört und
/// weder für Gruppe noch für Andere Rechte hat (`mode & 0o077 == 0`).
///
/// Die Prüfung läuft per `fstat` auf dem bereits geöffneten Deskriptor —
/// kein Pfad, also kein Wettlauf zwischen Prüfen und Lesen. Zusammen mit
/// [`crate::open_nofollow`] ist ausgeschlossen, dass ein Symlink auf eine
/// fremde Datei geprüft wird.
///
/// # Errors
/// - `InvalidInput`: keine reguläre Datei (Verzeichnis, FIFO, Gerät …).
/// - `PermissionDenied`: Eigentümer ≠ `geteuid()` oder Gruppe/Andere haben
///   Rechte.
/// - sonst der `errno` von `fstat`.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{OpenMode, ensure_private_regular, open_nofollow};
/// use std::path::Path;
///
/// let file = open_nofollow(Path::new("/home/mia/.harw/auth.toml"), OpenMode::read_only())?;
/// ensure_private_regular(&file)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn ensure_private_regular(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ensure_private_regular: keine reguläre Datei",
        ));
    }
    let euid = rustix::process::geteuid().as_raw();
    if meta.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "ensure_private_regular: Eigentümer-UID {} ≠ effektive UID {euid}",
                meta.uid()
            ),
        ));
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("ensure_private_regular: Rechte {mode:04o} erlauben Gruppe/Andere"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    fn file_with_mode(dir: &std::path::Path, name: &str, mode: u32) -> File {
        let path = dir.join(name);
        std::fs::write(&path, b"geheim").expect("write");
        std::fs::set_permissions(&path, Permissions::from_mode(mode)).expect("chmod");
        File::open(&path).expect("open")
    }

    #[test]
    fn akzeptiert_0600_und_0400() {
        let tmp = tempfile::tempdir().expect("tempdir");
        ensure_private_regular(&file_with_mode(tmp.path(), "a", 0o600)).expect("0600");
        ensure_private_regular(&file_with_mode(tmp.path(), "b", 0o400)).expect("0400");
    }

    #[test]
    fn lehnt_0644_und_0640_und_0606_ab() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for (name, mode) in [("a", 0o644), ("b", 0o640), ("c", 0o606)] {
            let err = ensure_private_regular(&file_with_mode(tmp.path(), name, mode)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{mode:o}");
        }
    }

    #[test]
    fn lehnt_verzeichnis_ab() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = File::open(tmp.path()).expect("open dir");
        let err = ensure_private_regular(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
