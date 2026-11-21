//! Bereichsgeprüftes, nicht blockierendes Lesen einer regulären Datei.
//!
//! # Zweck
//! Lockfiles liegen typischerweise in Projektverzeichnissen, die ein
//! gewöhnlicher Nutzer beschreiben kann. Ersetzt er sein `Cargo.lock` durch
//! eine FIFO ohne Schreiber, blockiert ein schlichtes `File::open` (so wie
//! es `harw_dod_readfs::read_to_string` intern über `ReadScope::open`
//! ausführt) unbegrenzt — und mit ihm jeder weitere Sensor im selben
//! Sentinel-Zyklus (Befund F-064, siehe `harw-dod-scanreport`).
//! [`read_regular_file`] schließt das ohne Zeitfenster:
//!
//! 1. [`harw_dod_cap::ReadScope::resolve`] löst alle Symlinks auf und prüft
//!    das kanonische Ziel gegen den Lesebereich — dieselbe Prüfung, die
//!    `ReadScope::open` vornimmt.
//! 2. Das kanonische Ziel wird mit `O_NONBLOCK` geöffnet: das Öffnen kehrt
//!    auf jedem Dateityp sofort zurück.
//! 3. `fstat` auf **demselben** Deskriptor bestätigt eine reguläre Datei.
//! 4. Gelesen wird aus **demselben** Deskriptor, begrenzt auf
//!    [`harw_dod_readfs::MAX_READ_BYTES`] (1 MiB). Für reguläre Dateien hat
//!    `O_NONBLOCK` keine Wirkung.
//!
//! Anders als `harw-dod-scanreport` (Typprüfung über einen zweiten,
//! unabhängigen Deskriptor) bleibt hier keine Lücke zwischen Typprüfung und
//! Lesen.
//!
//! # Bewusste Ausnahme von „kein direkter `std::fs`-Zugriff“
//! Das Öffnen geschieht über `std::fs::OpenOptions`, nicht über
//! `ReadScope::open`, weil Letzteres keine Öffnungsflags annimmt. Die
//! Bereichsentscheidung bleibt vollständig bei `ReadScope::resolve`;
//! geöffnet wird nur dessen geprüftes, kanonisches Ergebnis — genau wie in
//! `ReadScope::open`.
//!
//! # Warum das Literal `0o4000`
//! `O_NONBLOCK` ist auf Linux (x86_64, aarch64; `asm-generic`) ABI-stabil
//! `0o4000`. Dieselbe Begründung wie in `harw-dod-scanreport`: keine neue
//! Abhängigkeit für eine plattformweit feste Konstante.
//!
//! # Restrisiko
//! Zwischen `resolve` und dem Öffnen kann ein Angreifer mit Schreibrecht auf
//! das Verzeichnis den Pfad austauschen (dieselbe Restlücke wie
//! `ReadScope::open`, Moduldoku von `harw-dod-cap`). Er erreicht damit
//! höchstens eine andere Datei, nie einen blockierenden Leser.
//!
//! # Nebenläufigkeit
//! Zustandslos; jeder Aufruf öffnet und schließt seinen eigenen Deskriptor.
//!
//! # Fehler
//! [`SourceFault`] (inhaltsfrei): `OutsideScope`, `Unreadable`,
//! `NotRegularFile`, `TooLarge`, `NotUtf8`.
//!
//! # Examples
//! ```rust,ignore
//! // Crate-intern.
//! let scope = harw_dod_cap::ReadScope::from_roots([PathBuf::from("/srv/projekt/Cargo.lock")]);
//! let content = read_regular_file(&scope, Path::new("/srv/projekt/Cargo.lock"))?;
//! ```

use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use harw_dod_cap::{ReadScope, SensorError};

use crate::fault::SourceFault;

/// `O_NONBLOCK` (Linux), siehe Moduldoku für das Literal.
const O_NONBLOCK: i32 = 0o4000;

/// Liest eine reguläre Datei im Lesebereich vollständig als UTF-8.
///
/// # Description
/// Siehe Moduldoku für die vier Schritte. Blockiert nie auf einer FIFO und
/// liest höchstens `MAX_READ_BYTES + 1` Bytes.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich des Sensors.
/// - `path` (`&Path`): der angefragte Pfad, vor der Auflösung.
///
/// # Returns
/// Den Dateiinhalt.
///
/// # Errors
/// - [`SourceFault::OutsideScope`]: das kanonische Ziel liegt außerhalb.
/// - [`SourceFault::Unreadable`]: Auflösen, Öffnen, `fstat` oder Lesen
///   scheitert (auch: der Pfad fehlt).
/// - [`SourceFault::NotRegularFile`]: Verzeichnis, FIFO, Gerät, Socket.
/// - [`SourceFault::TooLarge`]: mehr als `MAX_READ_BYTES` Bytes.
/// - [`SourceFault::NotUtf8`]: kein gültiges UTF-8.
///
/// # Concurrency
/// Zustandslos; von jedem Thread aufrufbar.
pub(crate) fn read_regular_file(scope: &ReadScope, path: &Path) -> Result<String, SourceFault> {
    let canonical = scope.resolve(path).map_err(|err| match err {
        SensorError::OutsideScope => SourceFault::OutsideScope,
        _ => SourceFault::Unreadable,
    })?;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(canonical)
        .map_err(|_| SourceFault::Unreadable)?;
    let metadata = file.metadata().map_err(|_| SourceFault::Unreadable)?;
    if !metadata.file_type().is_file() {
        return Err(SourceFault::NotRegularFile);
    }

    let limit = harw_dod_readfs::MAX_READ_BYTES;
    let mut buf = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|_| SourceFault::Unreadable)?;
    let too_large = match u64::try_from(buf.len()) {
        Ok(len) => len > limit,
        Err(_) => true,
    };
    if too_large {
        return Err(SourceFault::TooLarge);
    }

    String::from_utf8(buf).map_err(|_| SourceFault::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, ctx};
    use std::fs;

    fn scope_for(root: &Path) -> ReadScope {
        ReadScope::from_roots([root.to_path_buf()])
    }

    #[test]
    fn test_reads_regular_file_in_scope() -> TestResult {
        let fixture = Fixture::new()?;
        let path = fixture.root.join("Cargo.lock");
        fs::write(&path, "version = 4\n").map_err(ctx("Datei schreiben"))?;
        let content = read_regular_file(&scope_for(&fixture.root), &path);
        assert_eq!(content, Ok("version = 4\n".to_owned()));
        Ok(())
    }

    #[test]
    fn test_directory_is_not_a_regular_file() -> TestResult {
        let fixture = Fixture::new()?;
        let path = fixture.root.join("Cargo.lock");
        fs::create_dir(&path).map_err(ctx("Verzeichnis anlegen"))?;
        let content = read_regular_file(&scope_for(&fixture.root), &path);
        assert_eq!(content, Err(SourceFault::NotRegularFile));
        Ok(())
    }

    #[test]
    fn test_missing_file_is_unreadable() -> TestResult {
        let fixture = Fixture::new()?;
        let path = fixture.root.join("fehlt.lock");
        let content = read_regular_file(&scope_for(&fixture.root), &path);
        assert_eq!(content, Err(SourceFault::Unreadable));
        Ok(())
    }

    #[test]
    fn test_file_outside_scope_is_rejected() -> TestResult {
        let fixture = Fixture::new()?;
        let inside = fixture.root.join("innen");
        fs::create_dir(&inside).map_err(ctx("Verzeichnis anlegen"))?;
        let outside = fixture.root.join("aussen.lock");
        fs::write(&outside, "version = 4\n").map_err(ctx("Datei schreiben"))?;
        let content = read_regular_file(&scope_for(&inside), &outside);
        assert_eq!(content, Err(SourceFault::OutsideScope));
        Ok(())
    }

    #[test]
    fn test_symlink_pointing_outside_scope_is_rejected() -> TestResult {
        let fixture = Fixture::new()?;
        let inside = fixture.root.join("innen");
        fs::create_dir(&inside).map_err(ctx("Verzeichnis anlegen"))?;
        let target = fixture.root.join("geheim.txt");
        fs::write(&target, "geheim").map_err(ctx("Ziel schreiben"))?;
        let link = inside.join("Cargo.lock");
        std::os::unix::fs::symlink(target, &link).map_err(ctx("Symlink anlegen"))?;
        let content = read_regular_file(&scope_for(&inside), &link);
        assert_eq!(content, Err(SourceFault::OutsideScope));
        Ok(())
    }

    #[test]
    fn test_oversized_file_is_rejected() -> TestResult {
        let fixture = Fixture::new()?;
        let path = fixture.root.join("Cargo.lock");
        let size = usize::try_from(harw_dod_readfs::MAX_READ_BYTES)
            .map_err(ctx("Grenze passt in usize"))?
            .saturating_add(1);
        fs::write(&path, "#".repeat(size)).map_err(ctx("Datei schreiben"))?;
        let content = read_regular_file(&scope_for(&fixture.root), &path);
        assert_eq!(content, Err(SourceFault::TooLarge));
        Ok(())
    }

    #[test]
    fn test_file_at_the_size_limit_is_accepted() -> TestResult {
        let fixture = Fixture::new()?;
        let path = fixture.root.join("Cargo.lock");
        let size = usize::try_from(harw_dod_readfs::MAX_READ_BYTES)
            .map_err(ctx("Grenze passt in usize"))?;
        fs::write(&path, "#".repeat(size)).map_err(ctx("Datei schreiben"))?;
        let content = read_regular_file(&scope_for(&fixture.root), &path)
            .map_err(ctx("Datei an der Grenze lesen"))?;
        assert_eq!(content.len(), size);
        Ok(())
    }

    #[test]
    fn test_invalid_utf8_is_rejected() -> TestResult {
        let fixture = Fixture::new()?;
        let path = fixture.root.join("Cargo.lock");
        fs::write(&path, [0x66_u8, 0x6f, 0xff, 0xfe]).map_err(ctx("Datei schreiben"))?;
        let content = read_regular_file(&scope_for(&fixture.root), &path);
        assert_eq!(content, Err(SourceFault::NotUtf8));
        Ok(())
    }
}
