//! Capture-Manifest-Format und Materializer für echte Pi-Captures (Knoten
//! C-FIXT, Welle W3).
//!
//! # Verantwortungsbereich
//! Diese Datei besitzt ein zweites, eigenständiges Fixture-Format neben
//! `expect.json` ([`crate::fixture_io`]): eine [`CaptureManifest`] ist eine
//! JSON-Momentaufnahme eines echten `/sys`- bzw. `/proc`-Ausschnitts (siehe
//! `captures/rpi5-6.18/*.json`), die [`materialize`] in einen echten
//! Verzeichnisbaum zurückverwandelt — **inklusive echter Symlinks**
//! (`std::os::unix::fs::symlink`), nicht aufgelöst wie
//! [`crate::capture::capture`] es für das `tree/`-Format tut (siehe dortige
//! Moduldoku, Abschnitt „Symlinks", für die Begründung dieses Unterschieds:
//! [`crate::capture::capture`] baut portable Testbäume, dieses Modul bildet
//! die reale sysfs-Topologie ab, in der `thermal_zone0`, `mmcblk0` und
//! `card0` selbst Symlinks auf `../../devices/...` sind — ein Sensor, der
//! `readlink` auf diesen Pfaden prüft, braucht einen echten Symlink im
//! Testbaum, keine aufgelöste Kopie).
//!
//! [`CaptureEntry::path`] ist absolut geschrieben (z. B.
//! `/sys/class/thermal/thermal_zone0`), genau wie auf dem echten Host, den
//! `captures/rpi5-6.18/*.json` festhält. [`materialize`] bildet einen
//! führenden `/` beim Zusammensetzen mit `root` einfach auf `root` selbst ab
//! (siehe [`resolve_under_root`]) — der Aufrufer bekommt also einen Baum
//! unter einem beliebigen temporären Wurzelverzeichnis, dessen Struktur
//! relativ zu dieser Wurzel exakt der absoluten sysfs-/procfs-Struktur
//! entspricht, gegen die ein `ReadScope` in der Fixture-Harness zeigen kann.
//!
//! # Redaktion in `captures/rpi5-6.18/*.json`
//! Die eingecheckten Capture-Dateien enthalten keine Benutzernamen und keine
//! `/home`-Pfade. In `/proc/net/{tcp,udp}` wurden alle konkreten,
//! host-identifizierenden Adressen (privates LAN, Tailscale-CGNAT, ein
//! konkretes entferntes HTTPS-Ziel) durch Dokumentationsadressen aus
//! `192.0.2.0/24` (RFC 5737) ersetzt; `0.0.0.0`-Wildcards und `127.0.0.1`
//! blieben unverändert, da sie nichts über den aufgenommenen Host verraten.
//! Siehe `docs/remediation/ledger/W3/C-FIXT.md` für die vollständige
//! Zuordnungstabelle.
//!
//! # Nebenläufigkeit
//! [`materialize`] und [`load`] sind zustandslose freie Funktionen ohne
//! innere Veränderlichkeit: `Send + Sync`. [`materialize`] schreibt in den
//! ihr übergebenen `root`-Baum; parallele Aufrufe auf **unterschiedlichen**
//! `root`-Werten stören sich nicht, parallele Aufrufe auf demselben `root`
//! sind — wie bei [`crate::capture::capture`] — nicht vorgesehen.
//!
//! # Fehler
//! [`materialize`] gibt `std::io::Result<()>` zurück (siehe Signaturvorgabe
//! in `docs/remediation/AGENT-BRIEF.md`): ein abgelehnter Pfad (`..`-Segment)
//! wird als `io::Error` mit `ErrorKind::InvalidInput` gemeldet, alle anderen
//! Fehler sind durchgereichte Dateisystemfehler. [`load`] gibt
//! [`crate::error::FixturesResult`] zurück und nutzt dafür die bereits
//! vorhandenen Varianten [`crate::error::FixturesError::Io`] (Lesefehler) und
//! [`crate::error::FixturesError::Json`] (ungültiges Manifest) — dieses
//! Modul führt bewusst keinen dritten Fehlertyp ein.
//!
//! # Folgearbeit (nicht Teil dieses Auftrags)
//! `harw-dod-fixtures/src/lib.rs` ist nicht Eigentum dieses Agents und
//! bekommt deshalb weder `mod capture_manifest;` noch einen `pub use`
//! davon — siehe Ledger-Eintrag `docs/remediation/ledger/W3/C-FIXT.md`,
//! Abschnitt „Folgearbeit".
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_fixtures::capture_manifest::{load, materialize};
//! use std::path::Path;
//!
//! let manifest = load(Path::new("captures/rpi5-6.18/thermal.json"))?;
//! materialize(&manifest, Path::new("/tmp/rebuilt-sysfs"))?;
//! # Ok::<(), harw_dod_fixtures::error::FixturesError>(())
//! ```

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::FixturesResult;

/// Eine vollständige Capture-Momentaufnahme eines Pi-Hosts.
///
/// # Description
/// Wurzelstruktur von `captures/rpi5-6.18/*.json`: der Kernel, unter dem die
/// Aufnahme entstand, der Aufnahmezeitpunkt (RFC 3339, UTC) und die Liste der
/// aufgenommenen Pfade. Die Reihenfolge von [`Self::entries`] ist die
/// Schreibreihenfolge für [`materialize`] — ein `Dir`-Eintrag für ein
/// Verzeichnis steht deshalb in den eingecheckten Dateien vor den Dateien,
/// die es enthält, auch wenn [`materialize`] fehlende Elternverzeichnisse
/// ohnehin selbst anlegt.
///
/// # Arguments
/// Nicht zutreffend — reiner Datentyp, siehe Felder.
///
/// # Returns
/// Nicht zutreffend.
///
/// # Errors
/// Nicht zutreffend — Fehler entstehen erst beim (De-)Serialisieren über
/// [`load`] bzw. `serde_json`.
///
/// # Examples
/// ```rust
/// use harw_dod_fixtures::capture_manifest::CaptureManifest;
///
/// let json = r#"{"kernel":"6.18.34+rpt-rpi-2712","captured_at":"2026-09-13T18:33:35Z","entries":[]}"#;
/// let manifest: CaptureManifest = serde_json::from_str(json).expect("gueltiges Manifest");
/// assert_eq!(manifest.kernel, "6.18.34+rpt-rpi-2712");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureManifest {
    /// `uname -r` des aufgenommenen Hosts, z. B. `"6.18.34+rpt-rpi-2712"`.
    pub kernel: String,
    /// Aufnahmezeitpunkt als RFC-3339-Zeitstempel in UTC.
    pub captured_at: String,
    /// Die aufgenommenen Pfade, in Schreibreihenfolge (siehe Typdoku).
    pub entries: Vec<CaptureEntry>,
}

/// Ein einzelner aufgenommener Pfad innerhalb einer [`CaptureManifest`].
///
/// # Description
/// `path` ist absolut geschrieben, wie auf dem aufgenommenen Host (z. B.
/// `/sys/class/thermal/thermal_zone0`); [`materialize`] bildet ihn relativ
/// zu einem beliebigen `root` ab (siehe [`resolve_under_root`]). `kind`
/// bestimmt, was [`materialize`] an dieser Stelle erzeugt.
///
/// # Examples
/// ```rust
/// use harw_dod_fixtures::capture_manifest::{CaptureEntry, CaptureEntryKind};
///
/// let entry = CaptureEntry {
///     path: "/proc/loadavg".to_owned(),
///     kind: CaptureEntryKind::File { content: "0.00 0.00 0.00 1/1 1\n".to_owned() },
/// };
/// assert_eq!(entry.path, "/proc/loadavg");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEntry {
    /// Absoluter Pfad auf dem aufgenommenen Host (siehe Typdoku).
    pub path: String,
    /// Was [`materialize`] an `path` erzeugt.
    pub kind: CaptureEntryKind,
}

/// Was ein [`CaptureEntry`] beim Materialisieren erzeugt.
///
/// # Description
/// Intern getaggt über das JSON-Feld `type` (Werte `"file"`, `"symlink"`,
/// `"dir"`), damit `captures/rpi5-6.18/*.json` lesbar bleibt statt eine
/// extern getaggte `{"File": {...}}`-Form zu tragen.
///
/// # Errors
/// Nicht zutreffend — reiner Datentyp.
///
/// # Examples
/// ```rust
/// use harw_dod_fixtures::capture_manifest::CaptureEntryKind;
///
/// let kind = CaptureEntryKind::Symlink { target: "../../devices/virtual/thermal/thermal_zone0".to_owned() };
/// assert!(matches!(kind, CaptureEntryKind::Symlink { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureEntryKind {
    /// Eine gewöhnliche Datei mit dem aufgenommenen Inhalt.
    File {
        /// Der vollständige, bereits redigierte Dateiinhalt.
        content: String,
    },
    /// Ein Symlink, materialisiert als echter `std::os::unix::fs::symlink`.
    Symlink {
        /// Das Symlink-Ziel, unverändert wie von `readlink` gemeldet
        /// (typischerweise ein relativer Pfad wie
        /// `../../devices/virtual/thermal/thermal_zone0`).
        target: String,
    },
    /// Ein leeres Verzeichnis (kein Inhalt außer den weiteren Einträgen,
    /// die selbst `path` darunter tragen).
    Dir,
}

/// Bildet einen aufgenommenen Pfad relativ zu `root` ab und lehnt
/// `..`-Segmente ab.
///
/// # Description
/// Jede [`Component`] von `raw_path` wird einzeln verarbeitet:
/// `RootDir`/`Prefix` (ein führendes `/` bzw. ein Windows-Präfix) werden
/// verworfen statt an `root` angehängt — dadurch bildet ein absoluter
/// aufgenommener Pfad wie `/sys/class/thermal/thermal_zone0` auf
/// `root/sys/class/thermal/thermal_zone0` ab, nie auf einen Pfad außerhalb
/// von `root`. `CurDir` (`.`) wird ignoriert. `ParentDir` (`..`) wird mit
/// einem `io::Error` abgelehnt, bevor irgendein Dateisystemzugriff
/// stattfindet — das ist die einzige Stelle, an der ein aufgenommener Pfad
/// `root` verlassen könnte.
///
/// # Arguments
/// - `root` (`&Path`): das Zielwurzelverzeichnis, unter dem materialisiert
///   wird.
/// - `raw_path` (`&str`): [`CaptureEntry::path`], typischerweise absolut.
///
/// # Returns
/// Der aufgelöste Zielpfad unter `root`.
///
/// # Errors
/// [`io::ErrorKind::InvalidInput`], wenn `raw_path` ein `..`-Segment enthält.
fn resolve_under_root(root: &Path, raw_path: &str) -> io::Result<PathBuf> {
    let mut resolved = root.to_path_buf();
    for component in Path::new(raw_path).components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("capture entry path must not contain '..': {raw_path}"),
                ));
            }
        }
    }
    Ok(resolved)
}

/// Materialisiert eine [`CaptureManifest`] als echten Verzeichnisbaum unter
/// `root`.
///
/// # Description
/// Verarbeitet [`CaptureManifest::entries`] in Reihenfolge. Für jeden
/// Eintrag wird zunächst über [`resolve_under_root`] der Zielpfad bestimmt
/// (dabei werden `..`-Segmente abgelehnt, siehe dort) und dessen
/// Elternverzeichnis mit `fs::create_dir_all` angelegt. Danach, je nach
/// [`CaptureEntryKind`]:
/// - `Dir`: `fs::create_dir_all` auf dem Zielpfad selbst.
/// - `File { content }`: `fs::write` des Inhalts (überschreibt eine
///   vorhandene Datei an dieser Stelle).
/// - `Symlink { target }`: ein vorhandener Eintrag an der Zielstelle wird
///   zuerst per `fs::remove_file` entfernt (macht wiederholte Aufrufe auf
///   demselben `root` idempotent), danach
///   `std::os::unix::fs::symlink(target, zielpfad)` — ein echter Symlink,
///   dessen Ziel unverändert aus dem Manifest übernommen wird, nicht
///   aufgelöst (siehe Moduldoku für die Begründung).
///
/// # Arguments
/// - `manifest` (`&CaptureManifest`): die zu materialisierende Aufnahme.
/// - `root` (`&Path`): das Zielverzeichnis; muss bereits existieren.
///
/// # Returns
/// `Ok(())`, wenn jeder Eintrag erfolgreich geschrieben wurde.
///
/// # Errors
/// [`io::ErrorKind::InvalidInput`] für einen Eintrag mit `..`-Segment
/// (siehe [`resolve_under_root`]); jeder andere `io::Error`, den
/// `fs::create_dir_all`, `fs::write`, `fs::remove_file` oder
/// `std::os::unix::fs::symlink` liefern (z. B. fehlende Berechtigung).
///
/// # Concurrency
/// Nicht für parallele Aufrufe auf **demselben** `root` ausgelegt (siehe
/// Moduldoku); parallele Aufrufe auf unterschiedlichen `root`-Werten stören
/// sich nicht.
///
/// # Examples
/// ```rust,no_run
/// use harw_dod_fixtures::capture_manifest::{load, materialize};
/// use std::path::Path;
///
/// let manifest = load(Path::new("captures/rpi5-6.18/thermal.json"))?;
/// materialize(&manifest, Path::new("/tmp/rebuilt-sysfs"))?;
/// # Ok::<(), harw_dod_fixtures::error::FixturesError>(())
/// ```
pub fn materialize(manifest: &CaptureManifest, root: &Path) -> io::Result<()> {
    for entry in &manifest.entries {
        let target_path = resolve_under_root(root, &entry.path)?;
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        match &entry.kind {
            CaptureEntryKind::Dir => {
                fs::create_dir_all(&target_path)?;
            }
            CaptureEntryKind::File { content } => {
                fs::write(&target_path, content)?;
            }
            CaptureEntryKind::Symlink { target } => {
                match fs::symlink_metadata(&target_path) {
                    Ok(_) => fs::remove_file(&target_path)?,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                }
                std::os::unix::fs::symlink(target, &target_path)?;
            }
        }
    }
    Ok(())
}

/// Liest eine [`CaptureManifest`] aus einer JSON-Datei.
///
/// # Description
/// Liest `path` vollständig ein und dekodiert den Inhalt als
/// [`CaptureManifest`]. Gegenstück zu [`materialize`]: die
/// `captures/rpi5-6.18/*.json`-Dateien dieser Crate sind gültige Eingaben.
///
/// # Arguments
/// - `path` (`&Path`): Pfad zu einer Capture-Manifest-JSON-Datei.
///
/// # Returns
/// Die dekodierte [`CaptureManifest`].
///
/// # Errors
/// - [`crate::error::FixturesError::Io`]: `path` ist nicht lesbar.
/// - [`crate::error::FixturesError::Json`]: der Inhalt ist kein gültiges
///   Manifest gemäß [`CaptureManifest`] (unbekannte Felder eingeschlossen,
///   siehe `#[serde(deny_unknown_fields)]`).
///
/// # Examples
/// ```rust,no_run
/// use harw_dod_fixtures::capture_manifest::load;
/// use std::path::Path;
///
/// let manifest = load(Path::new("captures/rpi5-6.18/thermal.json"))?;
/// assert!(!manifest.entries.is_empty());
/// # Ok::<(), harw_dod_fixtures::error::FixturesError>(())
/// ```
pub fn load(path: &Path) -> FixturesResult<CaptureManifest> {
    let raw = fs::read_to_string(path)?;
    let manifest: CaptureManifest = serde_json::from_str(&raw)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn sample_manifest() -> CaptureManifest {
        CaptureManifest {
            kernel: "6.18.34+rpt-rpi-2712".to_owned(),
            captured_at: "2026-09-13T18:33:35Z".to_owned(),
            entries: vec![
                CaptureEntry {
                    path: "/sys/class/thermal/thermal_zone0".to_owned(),
                    kind: CaptureEntryKind::Symlink {
                        target: "../../devices/virtual/thermal/thermal_zone0".to_owned(),
                    },
                },
                CaptureEntry {
                    path: "/sys/class/thermal/thermal_zone0/type".to_owned(),
                    kind: CaptureEntryKind::File {
                        content: "cpu-thermal\n".to_owned(),
                    },
                },
            ],
        }
    }

    #[test]
    fn test_materialize_creates_real_symlink() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        materialize(&sample_manifest(), dir.path()).map_err(ctx("materialize muss gelingen"))?;

        let link_path = dir.path().join("sys/class/thermal/thermal_zone0");
        let meta = fs::symlink_metadata(&link_path).map_err(ctx("symlink_metadata"))?;
        assert!(meta.file_type().is_symlink());
        let target = fs::read_link(&link_path).map_err(ctx("read_link"))?;
        assert_eq!(
            target,
            PathBuf::from("../../devices/virtual/thermal/thermal_zone0")
        );

        let file_path = dir.path().join("sys/class/thermal/thermal_zone0/type");
        assert_eq!(
            fs::read_to_string(file_path).map_err(ctx("read type"))?,
            "cpu-thermal\n"
        );
        Ok(())
    }

    #[test]
    fn test_materialize_rejects_parent_dir_segment() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let manifest = CaptureManifest {
            kernel: "test".to_owned(),
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            entries: vec![CaptureEntry {
                path: "/sys/../etc/passwd".to_owned(),
                kind: CaptureEntryKind::File {
                    content: "escape".to_owned(),
                },
            }],
        };

        let result = materialize(&manifest, dir.path());
        let Err(err) = result else {
            return Err(TestError::Unexpected(
                "'..' muss abgelehnt werden".to_owned(),
            ));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let parent = dir
            .path()
            .parent()
            .ok_or(TestError::Missing("dir.path() hat kein parent"))?;
        assert!(
            !parent.join("etc/passwd").exists(),
            "es darf keine Datei außerhalb von root entstanden sein"
        );
        Ok(())
    }

    #[test]
    fn test_materialize_is_idempotent_for_repeated_symlink() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let manifest = sample_manifest();
        materialize(&manifest, dir.path()).map_err(ctx("erster materialize-Aufruf"))?;
        materialize(&manifest, dir.path())
            .map_err(ctx("zweiter materialize-Aufruf muss ebenfalls gelingen"))?;

        let link_path = dir.path().join("sys/class/thermal/thermal_zone0");
        assert!(
            fs::symlink_metadata(&link_path)
                .map_err(ctx("symlink_metadata"))?
                .file_type()
                .is_symlink()
        );
        Ok(())
    }

    #[test]
    fn test_load_reads_real_capture_file() -> TestResult {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("captures/rpi5-6.18/thermal.json");
        let manifest = load(&path).map_err(ctx("echtes Capture-File muss laden"))?;

        assert_eq!(manifest.kernel, "6.18.34+rpt-rpi-2712");
        assert!(
            manifest
                .entries
                .iter()
                .any(|entry| entry.path == "/sys/class/thermal/thermal_zone0/temp")
        );
        Ok(())
    }

    #[test]
    fn test_load_missing_file_is_io_error() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let result = load(&dir.path().join("does-not-exist.json"));
        let Err(err) = result else {
            return Err(TestError::Unexpected(
                "fehlende Datei muss scheitern".to_owned(),
            ));
        };
        assert!(matches!(err, crate::error::FixturesError::Io(_)));
        Ok(())
    }
}
