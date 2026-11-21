//! Dauerhaftes Schreiben im StateDirectory (crate-intern): atomares Ersetzen
//! einer Datei, Anhängen mit `fsync`, Verzeichnis-`fsync`.
//!
//! # Verantwortungsbereich
//! Gemeinsame Mechanik für `nonce_ledger.rs`, `action_ledger.rs` und
//! `holds.rs`. Jede Datei wird mit Modus `0600` angelegt. Ersetzen läuft über
//! `<name>.tmp` im selben Verzeichnis (`write` → `fsync` → `rename` →
//! Verzeichnis-`fsync`); ein Absturz hinterlässt entweder die alte oder die
//! neue Fassung, nie eine halbe. Umbenennen im selben Verzeichnis ist auch
//! unter Landlock ABI V1 erlaubt (kein `REFER` nötig).
//!
//! # Genau ein Systemaufruf fürs Umbenennen
//! Umbenannt wird ausdrücklich über `renameat2` (flags `0`, über
//! [`rustix::fs::renameat_with`]), nicht über `std::fs::rename`: glibc
//! übersetzt `rename()` je nach Architektur in `rename` (x86_64),
//! `renameat` (aarch64) oder `renameat2` (riscv64). Die seccomp-Allowlist
//! des Wardens (`harw-warden`, `seccomp.rs`) erlaubt nur `renameat2`; ein
//! anderer Aufruf scheiterte dort nach dem Start mit `EPERM` und sperrte
//! Hold-Speicher und Nonce-Ledger ([`STATE_RENAME_SYSCALL`]).
//!
//! # Keine Symlinks
//! Jede Zustandsdatei wird mit `O_NOFOLLOW` geöffnet ([`open_state_file`]):
//! ein Symlink an ihrer Stelle ist ein Fehler (`ELOOP`), nie ein Umweg in
//! eine fremde Datei.
//!
//! # Nebenläufigkeit
//! Keine eigene Synchronisation; die Aufrufer halten ihren Mutex.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::Path;

use rustix::fs::{Mode, OFlags, RenameFlags};

/// Name des einzigen Systemaufrufs, mit dem diese Crate Zustandsdateien
/// umbenennt (auf jeder Architektur); die seccomp-Allowlist des Binarys muss
/// ihn erlauben.
pub const STATE_RENAME_SYSCALL: &str = "renameat2";

/// Opens `path` with `flags | O_NOFOLLOW | O_CLOEXEC` (new files get mode 0600).
pub(crate) fn open_state_file(path: &Path, flags: OFlags) -> std::io::Result<File> {
    let fd = rustix::fs::openat(
        rustix::fs::CWD,
        path,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    Ok(File::from(fd))
}

/// Replaces `dir/name` atomically with `contents` and makes it durable.
pub(crate) fn replace_file_atomically(
    dir: &Path,
    name: &str,
    contents: &[u8],
) -> std::io::Result<()> {
    let tmp_name = format!("{name}.tmp");
    let tmp_path = dir.join(tmp_name);
    {
        let mut tmp = open_state_file(&tmp_path, OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC)?;
        tmp.write_all(contents)?;
        tmp.sync_all()?;
    }
    // `renameat2` mit leeren Flags verhält sich wie `rename` (siehe
    // Moduldoku, Abschnitt „Genau ein Systemaufruf fürs Umbenennen“).
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        &tmp_path,
        rustix::fs::CWD,
        &dir.join(name),
        RenameFlags::empty(),
    )?;
    sync_dir(dir)
}

/// Opens `dir/name` for appending (created with mode 0600 if missing).
pub(crate) fn open_append(dir: &Path, name: &str) -> std::io::Result<File> {
    open_state_file(
        &dir.join(name),
        OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE,
    )
}

/// Appends one complete record and flushes it to stable storage.
pub(crate) fn append_durably(file: &mut File, record: &[u8]) -> std::io::Result<()> {
    file.write_all(record)?;
    file.sync_data()
}

/// Flushes directory metadata (new or renamed entries) to stable storage.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Reads `dir/name` without following a symlink; a missing file is `Ok(None)`.
pub(crate) fn read_optional(dir: &Path, name: &str) -> std::io::Result<Option<Vec<u8>>> {
    let mut file = match open_state_file(&dir.join(name), OFlags::RDONLY) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    Ok(Some(contents))
}

/// Splits file contents into complete lines and an optional torn tail.
///
/// # Description
/// Eine letzte Zeile ohne abschließendes `\n` stammt aus einem Schreibvorgang,
/// der vor seinem `fsync` abgebrochen ist; der zugehörige Aufruf hat nie
/// `Ok` geliefert. Sie wird getrennt zurückgegeben, damit der Aufrufer sie
/// verwerfen kann.
pub(crate) fn split_complete_lines(contents: &[u8]) -> (Vec<&[u8]>, Option<&[u8]>) {
    let mut lines: Vec<&[u8]> = contents.split(|byte| *byte == b'\n').collect();
    // `split` liefert nach einem abschließenden `\n` ein leeres letztes Stück.
    let tail = lines.pop().filter(|tail| !tail.is_empty());
    (lines, tail)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::{
        append_durably, open_append, read_optional, replace_file_atomically, split_complete_lines,
    };
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_replace_and_append_create_private_files() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        replace_file_atomically(dir.path(), "a.json", b"{}").map_err(ctx("replace"))?;
        let mode = std::fs::metadata(dir.path().join("a.json"))
            .map_err(ctx("metadata"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!dir.path().join("a.json.tmp").exists());

        let mut file = open_append(dir.path(), "b.log").map_err(ctx("open append"))?;
        append_durably(&mut file, b"one\n").map_err(ctx("append"))?;
        append_durably(&mut file, b"two\n").map_err(ctx("append"))?;
        let contents = read_optional(dir.path(), "b.log")
            .map_err(ctx("read"))?
            .unwrap_or_default();
        assert_eq!(contents, b"one\ntwo\n");
        assert_eq!(
            read_optional(dir.path(), "missing").map_err(ctx("read"))?,
            None
        );
        Ok(())
    }

    #[test]
    fn test_symlinked_state_files_are_never_followed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let outside = dir.path().join("outside.log");
        std::fs::write(&outside, b"foreign\n").map_err(ctx("write"))?;
        std::os::unix::fs::symlink(&outside, dir.path().join("b.log")).map_err(ctx("symlink"))?;
        std::os::unix::fs::symlink(&outside, dir.path().join("a.json.tmp"))
            .map_err(ctx("symlink tmp"))?;

        assert!(read_optional(dir.path(), "b.log").is_err());
        assert!(open_append(dir.path(), "b.log").is_err());
        assert!(replace_file_atomically(dir.path(), "a.json", b"{}").is_err());
        let untouched = std::fs::read(&outside).map_err(ctx("read outside"))?;
        assert_eq!(untouched, b"foreign\n");
        Ok(())
    }

    #[test]
    fn test_split_complete_lines_separates_a_torn_tail() {
        let (lines, tail) = split_complete_lines(b"a\nb\nhalf");
        assert_eq!(lines, [b"a".as_slice(), b"b".as_slice()]);
        assert_eq!(tail, Some(b"half".as_slice()));
        let (lines, tail) = split_complete_lines(b"a\n");
        assert_eq!(lines, [b"a".as_slice()]);
        assert_eq!(tail, None);
        let (lines, tail) = split_complete_lines(b"");
        assert!(lines.is_empty());
        assert_eq!(tail, None);
    }
}
