//! Atomares Ersetzen von Dateiinhalten.
//!
//! Ablauf von [`write_atomic`]:
//!
//! 1. Elternverzeichnis einmal als Deskriptor öffnen; alle weiteren Schritte
//!    laufen dirfd-relativ (kein Wettlauf durch Umbenennen des Pfads). Ist
//!    das Verzeichnis für alle beschreibbar **ohne** Sticky-Bit, wird
//!    abgebrochen (siehe „Voraussetzung“ bei [`write_atomic`]).
//! 2. Tempdatei `.<name-präfix>.<pid>.<zähler>.<zufall>.tmp` im selben
//!    Verzeichnis mit `O_CREAT | O_EXCL | O_NOFOLLOW`, Rechte `0600`. Der
//!    Name ist höchstens `NAME_MAX` (255) Bytes lang; bei langen Zielnamen
//!    wird das Präfix gekürzt. `<zufall>` sind 64 Bit aus einem zufällig
//!    geschlüsselten SipHash (`std`-`RandomState`) über PID, Zähler, Uhrzeit
//!    und Versuch — ohne zusätzliche Abhängigkeit nicht vorhersagbar.
//! 3. Inhalt schreiben, `fchmod(mode)` (umask-unabhängig), `fsync`.
//! 4. Prüfen, dass der Tempname noch auf dieselbe Inode zeigt (`fstatat`
//!    ohne Symlink-Folge gegen `fstat` der Tempdatei), dann
//!    `renameat(tmp, name)`. `rename(2)` ersetzt einen Symlink am Ziel als
//!    Verzeichniseintrag — dem Symlink wird **nie** gefolgt.
//! 5. Optional `fsync` des Verzeichnisses.
//!
//! Scheitert ein Schritt vor dem `rename`, wird die Tempdatei entfernt und
//! das Ziel bleibt unverändert.

use std::collections::hash_map::RandomState;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Permissions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{AtFlags, CWD, Mode, OFlags};
use rustix::io::{Errno, retry_on_intr};

use crate::open::{invalid_input, mode_from_bits};

/// Maximale Länge eines Verzeichniseintrags in Bytes (`NAME_MAX` unter Linux
/// und den gängigen Unix-Dateisystemen).
pub(crate) const NAME_MAX: usize = 255;

/// Obergrenze für Namenskollisionen (`EEXIST`) beim Anlegen der Tempdatei.
const MAX_TEMP_ATTEMPTS: u32 = 64;

/// Prozessweiter Zähler für eindeutige Tempnamen.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Optionen für [`write_atomic`].
///
/// # Examples
/// ```rust
/// use harw_fsutil::AtomicWriteOptions;
///
/// let opts = AtomicWriteOptions::private();
/// assert_eq!(opts, AtomicWriteOptions { mode: 0o600, fsync_dir: true });
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomicWriteOptions {
    /// Endgültige Rechte-Bits der Zieldatei (`& 0o7777`, per `fchmod` gesetzt,
    /// also unabhängig vom `umask`).
    pub mode: u32,
    /// Nach dem `rename` das Elternverzeichnis `fsync`en, damit der neue
    /// Verzeichniseintrag einen Absturz übersteht.
    pub fsync_dir: bool,
}

impl AtomicWriteOptions {
    /// `0600` mit Verzeichnis-`fsync` — Default für Secrets und Zustand.
    #[must_use]
    pub const fn private() -> Self {
        Self {
            mode: 0o600,
            fsync_dir: true,
        }
    }

    /// Beliebige Rechte mit Verzeichnis-`fsync`.
    #[must_use]
    pub const fn with_mode(mode: u32) -> Self {
        Self {
            mode,
            fsync_dir: true,
        }
    }
}

/// Ersetzt den Inhalt von `path` atomar durch `bytes`.
///
/// Nur das **letzte** Pfadglied ist symlinkfest: Elternverzeichnisse werden
/// normal aufgelöst (einmalig, danach dirfd-relativ). Ist `path` selbst ein
/// Symlink, wird der Symlink durch eine reguläre Datei ersetzt; sein Ziel
/// bleibt unberührt.
///
/// # Voraussetzung
/// Das Elternverzeichnis darf **nicht von Dritten beschreibbar** sein
/// (typisch `0700`/`0755` des eigenen Benutzers). Wer dort Einträge anlegen
/// oder entfernen darf, kann Tempnamen vorbelegen (Abbruch nach 64
/// Kollisionen) oder die Tempdatei zwischen Anlegen und `rename` durch einen
/// Symlink ersetzen, den `rename` dann ans Ziel stellt. Gegenmaßnahmen im
/// Rahmen dieser Voraussetzung:
/// - Für alle beschreibbare Verzeichnisse ohne Sticky-Bit (`o+w`, kein
///   `S_ISVTX`) werden mit `PermissionDenied` abgelehnt. Mit Sticky-Bit (z. B.
///   `/tmp`, `1777`) darf niemand außer dem Eigentümer die Tempdatei
///   entfernen oder umbenennen; Vorbelegen bleibt dort möglich (nur DoS).
/// - Der Tempname enthält 64 Zufallsbits, und `O_EXCL | O_NOFOLLOW` verhindert,
///   dass ein vorbelegter Name oder Symlink übernommen wird.
/// - Unmittelbar vor dem `rename` wird geprüft, dass der Tempname noch die
///   eigene Inode bezeichnet; sonst Fehler (`ErrorKind::Other`), das Ziel
///   bleibt unverändert. Ein Restfenster zwischen dieser Prüfung und dem
///   `rename` bleibt — daher die Voraussetzung.
///
/// Gruppen-Schreibrechte und ACLs werden **nicht** geprüft; sie fallen unter
/// die Voraussetzung.
///
/// # Fehlersemantik
/// - Fehler **vor** dem `rename` (Öffnen, Anlegen, Schreiben, `fchmod`,
///   Datei-`fsync`, `rename` selbst): Tempdatei wird best-effort entfernt,
///   das Ziel ist unverändert.
/// - Fehler **nach** dem `rename` gibt es nur mit `fsync_dir == true`: Der
///   Verzeichnis-`fsync` ist gescheitert. Der neue Inhalt ist dann bereits
///   sichtbar (Ziel ersetzt), aber seine Dauerhaftigkeit über einen Absturz
///   ist nicht garantiert; der Aufrufer darf weder den alten noch den neuen
///   Stand als persistiert annehmen. `EINVAL` beim Verzeichnis-`fsync`
///   (Dateisystem unterstützt ihn nicht) wird ignoriert.
/// - Mit `fsync_dir == false` wird kein Verzeichnis-`fsync` ausgeführt.
///
/// # Errors
/// `InvalidInput`, wenn `path` kein letztes normales Glied hat (z. B. `/`
/// oder `..`); `PermissionDenied`, wenn das Elternverzeichnis für alle
/// beschreibbar ist und kein Sticky-Bit hat; `Other`, wenn die Tempdatei vor
/// dem `rename` ersetzt wurde; `EISDIR` u. ä., wenn das Ziel ein Verzeichnis
/// ist; sonst der `errno` des jeweiligen Syscalls.
///
/// # Examples
/// ```rust,no_run
/// use harw_fsutil::{AtomicWriteOptions, write_atomic};
/// use std::path::Path;
///
/// write_atomic(
///     Path::new("/home/mia/.harw/state.json"),
///     br#"{"version":1}"#,
///     AtomicWriteOptions { mode: 0o644, fsync_dir: false },
/// )?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn write_atomic(path: &Path, bytes: &[u8], opts: AtomicWriteOptions) -> io::Result<()> {
    let name = match path.components().next_back() {
        Some(Component::Normal(name)) => name,
        _ => return Err(invalid_input("write_atomic: Pfad ohne Dateinamen")),
    };
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let dir = retry_on_intr(|| rustix::fs::openat(CWD, parent, dir_flags, Mode::empty()))?;
    let dir = dir.as_fd();
    ensure_not_shared_writable(dir)?;

    let (temp_name, file) = create_temp(dir, name)?;
    let temp_stat = match rustix::fs::fstat(&file) {
        Ok(stat) => stat,
        Err(errno) => {
            remove_temp(dir, &temp_name);
            return Err(errno.into());
        }
    };
    if let Err(err) = fill_temp(file, bytes, opts.mode) {
        remove_temp(dir, &temp_name);
        return Err(err);
    }
    // Wurde der Tempname inzwischen ersetzt, gehört der Eintrag nicht mehr uns:
    // nicht entfernen, nicht umbenennen.
    match retry_on_intr(|| rustix::fs::statat(dir, &temp_name, AtFlags::SYMLINK_NOFOLLOW)) {
        Ok(stat) if stat.st_dev == temp_stat.st_dev && stat.st_ino == temp_stat.st_ino => {}
        Ok(_) => {
            return Err(io::Error::other(
                "write_atomic: Tempdatei wurde vor dem rename ersetzt",
            ));
        }
        Err(errno) => return Err(errno.into()),
    }
    if let Err(errno) = retry_on_intr(|| rustix::fs::renameat(dir, &temp_name, dir, name)) {
        remove_temp(dir, &temp_name);
        return Err(errno.into());
    }
    if opts.fsync_dir {
        match retry_on_intr(|| rustix::fs::fsync(dir)) {
            Ok(()) | Err(Errno::INVAL) => {}
            Err(errno) => return Err(errno.into()),
        }
    }
    Ok(())
}

/// Lehnt ein für alle beschreibbares Verzeichnis ohne Sticky-Bit ab.
fn ensure_not_shared_writable(dir: BorrowedFd<'_>) -> io::Result<()> {
    let mode = Mode::from_raw_mode(rustix::fs::fstat(dir)?.st_mode);
    if mode.contains(Mode::WOTH) && !mode.contains(Mode::SVTX) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "write_atomic: Zielverzeichnis ist für alle beschreibbar und hat kein Sticky-Bit",
        ));
    }
    Ok(())
}

/// Legt die Tempdatei exklusiv an; wiederholt bei Namenskollision.
fn create_temp(dir: BorrowedFd<'_>, target: &OsStr) -> io::Result<(OsString, File)> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let create_mode = mode_from_bits(0o600);
    let mut attempt = 0;
    loop {
        let candidate = temp_name(target, attempt);
        match retry_on_intr(|| rustix::fs::openat(dir, &candidate, flags, create_mode)) {
            Ok(fd) => return Ok((candidate, File::from(fd))),
            Err(Errno::EXIST) if attempt + 1 < MAX_TEMP_ATTEMPTS => attempt += 1,
            Err(errno) => return Err(errno.into()),
        }
    }
}

/// Schreibt, setzt Rechte und synchronisiert die Tempdatei.
fn fill_temp(mut file: File, bytes: &[u8], mode: u32) -> io::Result<()> {
    file.write_all(bytes)?;
    file.set_permissions(Permissions::from_mode(mode & 0o7777))?;
    file.sync_all()
}

/// Entfernt die Tempdatei best-effort; der ursprüngliche Fehler hat Vorrang.
fn remove_temp(dir: BorrowedFd<'_>, temp_name: &OsStr) {
    // Ein Fehler hier ist nicht behebbar und nicht wichtiger als der
    // Ursprungsfehler, deshalb wird er bewusst verworfen.
    let _ = rustix::fs::unlinkat(dir, temp_name, AtFlags::empty());
}

/// Erzeugt `.<präfix>.<pid>.<zähler>.<zufall>.tmp` mit höchstens
/// [`NAME_MAX`] Bytes; das Präfix ist der (ggf. gekürzte) Zielname, `<zufall>`
/// genau 16 Hex-Ziffern (siehe [`random_salt`]). Der Suffix ist höchstens
/// 47 Bytes lang.
pub(crate) fn temp_name(target: &OsStr, attempt: u32) -> OsString {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let salt = random_salt(pid, counter, attempt);
    let suffix = format!(".{pid:x}.{counter:x}.{salt:016x}.tmp");
    let budget = NAME_MAX.saturating_sub(1 + suffix.len());
    let target = target.as_bytes();
    let prefix = &target[..target.len().min(budget)];

    let mut name = Vec::with_capacity(1 + prefix.len() + suffix.len());
    name.push(b'.');
    name.extend_from_slice(prefix);
    name.extend_from_slice(suffix.as_bytes());
    OsString::from_vec(name)
}

/// 64 nicht vorhersagbare Bits ohne zusätzliche Abhängigkeit.
///
/// `RandomState::new()` wird aus Betriebssystem-Zufall geschlüsselt (je Thread
/// einmal, danach mit verändertem Schlüssel je Aufruf); der daraus gebaute
/// SipHash über PID, Zähler, Uhrzeit und Versuch ist ohne Kenntnis des
/// Schlüssels nicht vorhersagbar. Keine kryptographische Zusage — die
/// Sicherheit ruht auf `O_EXCL | O_NOFOLLOW` und der dokumentierten
/// Voraussetzung an das Verzeichnis.
fn random_salt(pid: u32, counter: u64, attempt: u32) -> u64 {
    let (secs, nanos) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or((0, 0), |elapsed| {
            (elapsed.as_secs(), elapsed.subsec_nanos())
        });
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(pid);
    hasher.write_u64(counter);
    hasher.write_u64(secs);
    hasher.write_u32(nanos);
    hasher.write_u32(attempt);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::os::unix::fs::symlink;

    fn dir_names(dir: &Path) -> TestResult<Vec<OsString>> {
        let mut names: Vec<OsString> = std::fs::read_dir(dir)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }

    #[test]
    fn schreibt_neue_datei_mit_mode() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("state.json");
        write_atomic(&path, b"eins", AtomicWriteOptions::private())?;
        assert_eq!(std::fs::read(&path)?, b"eins");
        let meta = std::fs::symlink_metadata(&path)?;
        assert_eq!(meta.permissions().mode() & 0o7777, 0o600);

        write_atomic(
            &path,
            b"zwei",
            AtomicWriteOptions {
                mode: 0o644,
                fsync_dir: false,
            },
        )
        .map_err(ctx("overwrite"))?;
        assert_eq!(std::fs::read(&path)?, b"zwei");
        let meta = std::fs::symlink_metadata(&path)?;
        assert_eq!(meta.permissions().mode() & 0o7777, 0o644);
        assert_eq!(dir_names(tmp.path())?, vec![OsString::from("state.json")]);
        Ok(())
    }

    #[test]
    fn ersetzt_ziel_symlink_statt_ihm_zu_folgen() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, b"original")?;
        let target = tmp.path().join("target");
        symlink(&victim, &target)?;

        write_atomic(&target, b"neu", AtomicWriteOptions::private())?;

        let meta = std::fs::symlink_metadata(&target)?;
        assert!(meta.file_type().is_file(), "Symlink muss ersetzt sein");
        assert_eq!(std::fs::read(&target)?, b"neu");
        assert_eq!(std::fs::read(&victim)?, b"original");
        Ok(())
    }

    #[test]
    fn ersetzt_dangling_symlink_ohne_ziel_anzulegen() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let target = tmp.path().join("target");
        symlink(tmp.path().join("fehlt"), &target)?;
        write_atomic(&target, b"neu", AtomicWriteOptions::private())?;
        assert!(std::fs::symlink_metadata(&target)?.is_file());
        assert!(!tmp.path().join("fehlt").exists());
        Ok(())
    }

    #[test]
    fn tempname_bleibt_unter_name_max() {
        let long = OsString::from("a".repeat(NAME_MAX));
        let name = temp_name(&long, 7);
        assert!(
            name.as_bytes().len() <= NAME_MAX,
            "{}",
            name.as_bytes().len()
        );
        assert!(name.as_bytes().starts_with(b".aaa"));
        assert!(name.as_bytes().ends_with(b".tmp"));

        let short = temp_name(OsStr::new("x"), 0);
        assert!(short.as_bytes().starts_with(b".x."));
        assert_ne!(
            temp_name(OsStr::new("x"), 0),
            short,
            "Zähler macht Namen eindeutig"
        );
    }

    /// R2-09: Der Tempname trägt 16 Hex-Ziffern Zufall; zwei Namen
    /// unterscheiden sich darin (Kollision mit Wahrscheinlichkeit 2^-64).
    #[test]
    fn tempname_hat_zufallsanteil() -> TestResult {
        let salt_of = |name: &OsStr| -> TestResult<String> {
            let text = name.to_str().ok_or(TestError::Missing("ascii"))?.to_owned();
            let parts: Vec<&str> = text.rsplitn(3, '.').collect();
            // rsplitn: ["tmp", "<zufall>", ".x.<pid>.<zähler>"]
            assert_eq!(parts.first(), Some(&"tmp"), "{text}");
            parts
                .get(1)
                .map(|salt| (*salt).to_owned())
                .ok_or(TestError::Missing("zufall"))
        };
        let first = salt_of(temp_name(OsStr::new("x"), 0).as_os_str())?;
        let second = salt_of(temp_name(OsStr::new("x"), 0).as_os_str())?;
        for salt in [&first, &second] {
            assert_eq!(salt.len(), 16, "{salt}");
            assert!(salt.bytes().all(|byte| byte.is_ascii_hexdigit()), "{salt}");
        }
        assert_ne!(first, second);
        let long = temp_name(OsStr::new(&"y".repeat(NAME_MAX)), u32::MAX);
        assert_eq!(long.as_bytes().len(), NAME_MAX);
        Ok(())
    }

    /// R2-09: Für alle beschreibbares Verzeichnis ohne Sticky-Bit wird
    /// abgelehnt; mit Sticky-Bit oder ohne `o+w` geschrieben.
    #[test]
    fn weltbeschreibbares_verzeichnis_ohne_sticky_wird_abgelehnt() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let shared = tmp.path().join("shared");
        std::fs::create_dir(&shared)?;
        let target = shared.join("state");
        let chmod = |mode: u32| -> io::Result<()> {
            std::fs::set_permissions(&shared, Permissions::from_mode(mode))
        };

        chmod(0o777)?;
        let Err(err) = write_atomic(&target, b"x", AtomicWriteOptions::private()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        let after_reject = dir_names(&shared)?;
        chmod(0o1777)?;
        let sticky = write_atomic(&target, b"sticky", AtomicWriteOptions::private());
        chmod(0o770)?;
        let group = write_atomic(&target, b"gruppe", AtomicWriteOptions::private());
        chmod(0o700)?;

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(after_reject.is_empty(), "keine Tempreste: {after_reject:?}");
        sticky.map_err(ctx("Sticky-Bit erlaubt"))?;
        group.map_err(ctx("ohne o+w erlaubt"))?;
        assert_eq!(std::fs::read(&target)?, b"gruppe");
        assert_eq!(dir_names(&shared)?, vec![OsString::from("state")]);
        Ok(())
    }

    #[test]
    fn langer_dateiname_wird_geschrieben() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let name = "b".repeat(NAME_MAX);
        let path = tmp.path().join(&name);
        write_atomic(&path, b"lang", AtomicWriteOptions::private())?;
        assert_eq!(std::fs::read(&path)?, b"lang");
        assert_eq!(dir_names(tmp.path())?, vec![OsString::from(name)]);
        Ok(())
    }

    #[test]
    fn ziel_verzeichnis_scheitert_ohne_reste() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("sub");
        std::fs::create_dir(&path)?;
        let Err(_) = write_atomic(&path, b"x", AtomicWriteOptions::private()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(dir_names(tmp.path())?, vec![OsString::from("sub")]);
        assert!(std::fs::symlink_metadata(&path)?.is_dir());
        Ok(())
    }

    #[test]
    fn ungueltige_pfade() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let Err(err) = write_atomic(Path::new("/"), b"x", AtomicWriteOptions::private()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let parent = tmp.path().join("..");
        let Err(err) = write_atomic(&parent, b"x", AtomicWriteOptions::private()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let missing = tmp.path().join("fehlt/datei");
        let Err(err) = write_atomic(&missing, b"x", AtomicWriteOptions::private()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        Ok(())
    }
}
