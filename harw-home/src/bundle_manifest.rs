//! Aktualisierung der mitgelieferten Bundle-Dateien über ein Manifest
//! (Runde 7, Teil T6).
//!
//! # Verantwortung
//! Früher schrieb [`crate::scaffold::ensure_home`] jede Bundle-Datei nur,
//! wenn sie fehlte — eine neue Fassung eines Skills oder Agenten erreichte
//! bestehende Installationen nie. Dieses Modul führt deshalb
//! `<home>/.bundle-manifest.toml`: je Bundle-Pfad der SHA-256 des zuletzt
//! **installierten** Inhalts. Daraus folgt je Datei:
//!
//! | Lage | Wirkung |
//! |---|---|
//! | Datei fehlt | neu schreiben |
//! | Datei entspricht schon dem Bundle | nichts tun |
//! | Datei == installierter Hash, Bundle neu | überschreiben (Update) |
//! | Datei von der Nutzerin geändert, Bundle neu | nicht anfassen; neue Fassung als `<datei>.harw-neu` daneben, Hinweis im Bericht |
//! | Datei geändert, Bundle unverändert | nichts tun |
//! | kein Manifest-Eintrag (Alt-Installation) und Datei weicht ab | wie „geändert“: `<datei>.harw-neu` |
//!
//! Eine vorhandene Datei, die exakt dem aktuellen Bundle entspricht, wird
//! (auch bei Alt-Installationen ohne Manifest) ins Manifest aufgenommen.
//!
//! # Fehlerverhalten
//! Ein fehlendes **oder** unlesbares Manifest gilt als „kein Manifest“: das
//! ist die vorsichtigste Deutung, denn dann wird keine abweichende Datei
//! überschrieben, sondern höchstens eine `.harw-neu` daneben gelegt. I/O-Fehler
//! an Bundle-Dateien brechen mit [`HomeError::Io`] ab.
//!
//! # Nebenläufigkeit
//! Kein Locking; ein `harw`-Prozess pro Root-Space ist die erwartete Nutzung
//! (wie beim übrigen Scaffolding).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bundle::BundledFile;
use crate::error::{HomeError, HomeResult};

/// Dateiname des Manifests direkt unter dem Root-Space.
pub const BUNDLE_MANIFEST_FILE: &str = ".bundle-manifest.toml";

/// Endung, unter der eine neue Bundle-Fassung neben einer von der Nutzerin
/// geänderten Datei abgelegt wird.
pub const BUNDLE_NEW_SUFFIX: &str = ".harw-neu";

/// Kopfkommentar des Manifests.
const MANIFEST_HEADER: &str = "\
# Harwness — installierte Bundle-Dateien (automatisch gepflegt, nicht bearbeiten).
# Je Pfad der SHA-256 des zuletzt installierten Inhalts. Weicht eine Datei davon
# ab, gilt sie als von dir geändert und wird bei Updates nie überschrieben; die
# neue Fassung landet dann als <datei>.harw-neu daneben.
";

/// Serialisierte Form von `.bundle-manifest.toml`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BundleManifest {
    /// Formatversion; derzeit immer `1`.
    #[serde(default = "manifest_version")]
    version: u32,
    /// Bundle-Pfad (mit `/`) → SHA-256 (hex) des installierten Inhalts.
    #[serde(default)]
    files: BTreeMap<String, String>,
}

/// Vorgabe für [`BundleManifest::version`].
const fn manifest_version() -> u32 {
    1
}

/// Ergebnis eines Bundle-Abgleichs.
///
/// # Beschreibung
/// Wird von [`crate::scaffold`] in den [`crate::scaffold::Scaffolded`]-Bericht
/// übernommen.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct BundleSync {
    /// Neu angelegte Dateien (vorher nicht vorhanden).
    pub(crate) written: Vec<PathBuf>,
    /// Unveränderte Dateien, die auf die neue Bundle-Fassung gehoben wurden.
    pub(crate) updated: Vec<PathBuf>,
    /// Neu geschriebene oder aufgefrischte `<datei>.harw-neu` neben von der
    /// Nutzerin geänderten Dateien.
    pub(crate) conflicts: Vec<PathBuf>,
}

/// Gleicht die Bundle-Dateien `files` unter `home` mit dem Manifest ab.
///
/// # Beschreibung
/// Setzt die Regeln aus der Modul-Doku um und schreibt danach das Manifest
/// neu, falls sich etwas geändert hat.
///
/// # Argumente
/// - `home` (`&Path`): der Root-Space.
/// - `files` (`&[BundledFile]`): die Bundle-Dateien, üblicherweise
///   [`crate::bundle::bundled_files`].
///
/// # Rückgabe
/// Ein [`BundleSync`] mit neu geschriebenen, aktualisierten und
/// danebengelegten Dateien.
///
/// # Errors
/// [`HomeError::Io`], wenn eine Bundle-Datei, ihre `.harw-neu` oder das
/// Manifest nicht gelesen bzw. geschrieben werden kann.
pub(crate) fn sync_bundle(home: &Path, files: &[BundledFile]) -> HomeResult<BundleSync> {
    let manifest_path = home.join(BUNDLE_MANIFEST_FILE);
    let previous = read_manifest(&manifest_path);
    let mut next = BundleManifest::default();
    let mut sync = BundleSync::default();

    for file in files {
        let target = file.target_in(home);
        let bundle_hash = sha256_hex(file.contents.as_bytes());
        let installed = previous
            .as_ref()
            .and_then(|manifest| manifest.files.get(file.relative_path));
        if let Some(recorded) =
            sync_file(&target, file.contents, &bundle_hash, installed, &mut sync)?
        {
            next.files.insert(file.relative_path.to_owned(), recorded);
        }
    }

    if previous.as_ref() != Some(&next) {
        write_manifest(&manifest_path, &next)?;
    }
    Ok(sync)
}

/// Gleicht eine einzelne Bundle-Datei ab.
///
/// # Rückgabe
/// Den Hash, den das Manifest für diese Datei führen soll, oder `None`, wenn
/// die Datei nicht aus dem Bundle stammt (geändert, ohne früheren Eintrag).
fn sync_file(
    target: &Path,
    contents: &str,
    bundle_hash: &str,
    installed: Option<&String>,
    sync: &mut BundleSync,
) -> HomeResult<Option<String>> {
    if !target.exists() {
        write_file(target, contents)?;
        sync.written.push(target.to_path_buf());
        return Ok(Some(bundle_hash.to_owned()));
    }
    let current = std::fs::read(target).map_err(|error| HomeError::io(target, error))?;
    let current_hash = sha256_hex(&current);
    if current_hash == bundle_hash {
        // Schon auf Stand (auch: Alt-Installation mit identischer Datei).
        return Ok(Some(bundle_hash.to_owned()));
    }
    match installed {
        // Unverändert seit der letzten Installation, Bundle neu: Update.
        Some(installed) if *installed == current_hash => {
            write_file(target, contents)?;
            sync.updated.push(target.to_path_buf());
            Ok(Some(bundle_hash.to_owned()))
        }
        // Von der Nutzerin geändert, das Bundle aber nicht: nichts zu tun.
        Some(installed) if installed == bundle_hash => Ok(Some(installed.clone())),
        // Geändert und Bundle neu, oder Alt-Installation mit abweichender
        // Datei: nie überschreiben, neue Fassung daneben legen.
        _ => {
            let sidecar = sidecar_path(target);
            if write_sidecar(&sidecar, contents)? {
                sync.conflicts.push(sidecar);
            }
            Ok(installed.cloned())
        }
    }
}

/// Schreibt `<datei>.harw-neu`, sofern sie fehlt oder einen anderen Inhalt
/// trägt. Liefert `true`, wenn geschrieben wurde.
fn write_sidecar(sidecar: &Path, contents: &str) -> HomeResult<bool> {
    if sidecar.exists() {
        let existing = std::fs::read(sidecar).map_err(|error| HomeError::io(sidecar, error))?;
        if existing == contents.as_bytes() {
            return Ok(false);
        }
    }
    write_file(sidecar, contents)?;
    Ok(true)
}

/// Pfad der neuen Fassung neben `target` (`agent.toml` → `agent.toml.harw-neu`).
fn sidecar_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(BUNDLE_NEW_SUFFIX);
    PathBuf::from(name)
}

/// Schreibt `contents` nach `path` und legt fehlende Elternverzeichnisse an.
fn write_file(path: &Path, contents: &str) -> HomeResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| HomeError::io(parent, error))?;
    }
    std::fs::write(path, contents).map_err(|error| HomeError::io(path, error))
}

/// Liest das Manifest; `None`, wenn es fehlt oder nicht lesbar/parsbar ist
/// (siehe Modul-Doku, „Fehlerverhalten“).
fn read_manifest(path: &Path) -> Option<BundleManifest> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

/// Schreibt das Manifest samt Kopfkommentar.
fn write_manifest(path: &Path, manifest: &BundleManifest) -> HomeResult<()> {
    let body = toml::to_string(manifest).map_err(|error| {
        HomeError::io(
            path,
            std::io::Error::other(format!("Bundle-Manifest nicht serialisierbar: {error}")),
        )
    })?;
    write_file(path, &format!("{MANIFEST_HEADER}{body}"))
}

/// SHA-256 von `bytes` als Kleinbuchstaben-Hex.
fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    /// Ein frischer, leerer Root-Space im Temp-Verzeichnis.
    fn temp_home() -> TestResult<PathBuf> {
        let home =
            std::env::temp_dir().join(format!("harw-home-manifest-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&home).map_err(ctx("create temporary home"))?;
        Ok(home)
    }

    const V1: &[BundledFile] = &[BundledFile {
        relative_path: "skills/demo/instructions.md",
        contents: "Fassung 1\n",
    }];

    const V2: &[BundledFile] = &[BundledFile {
        relative_path: "skills/demo/instructions.md",
        contents: "Fassung 2\n",
    }];

    fn target(home: &Path) -> PathBuf {
        home.join("skills").join("demo").join("instructions.md")
    }

    fn read(path: &Path) -> TestResult<String> {
        std::fs::read_to_string(path).map_err(ctx("read file"))
    }

    #[test]
    fn test_first_install_writes_files_and_manifest() -> TestResult {
        let home = temp_home()?;
        let sync = sync_bundle(&home, V1).map_err(ctx("install v1"))?;
        assert_eq!(sync.written, vec![target(&home)]);
        assert!(sync.updated.is_empty() && sync.conflicts.is_empty());
        assert_eq!(read(&target(&home))?, "Fassung 1\n");
        let manifest = read(&home.join(BUNDLE_MANIFEST_FILE))?;
        assert!(
            manifest.contains("skills/demo/instructions.md"),
            "{manifest}"
        );
        assert!(manifest.contains(&sha256_hex(b"Fassung 1\n")), "{manifest}");

        // Zweiter Lauf ohne Bundle-Änderung: nichts passiert.
        let again = sync_bundle(&home, V1).map_err(ctx("rerun v1"))?;
        assert_eq!(again, BundleSync::default());
        std::fs::remove_dir_all(&home).map_err(ctx("cleanup"))?;
        Ok(())
    }

    #[test]
    fn test_unchanged_file_is_updated_to_the_new_bundle() -> TestResult {
        let home = temp_home()?;
        sync_bundle(&home, V1).map_err(ctx("install v1"))?;
        let sync = sync_bundle(&home, V2).map_err(ctx("install v2"))?;
        assert_eq!(sync.updated, vec![target(&home)]);
        assert!(sync.conflicts.is_empty());
        assert_eq!(read(&target(&home))?, "Fassung 2\n");
        assert!(!sidecar_path(&target(&home)).exists());
        let manifest = read(&home.join(BUNDLE_MANIFEST_FILE))?;
        assert!(manifest.contains(&sha256_hex(b"Fassung 2\n")), "{manifest}");
        std::fs::remove_dir_all(&home).map_err(ctx("cleanup"))?;
        Ok(())
    }

    #[test]
    fn test_user_modified_file_is_kept_and_new_version_lands_beside_it() -> TestResult {
        let home = temp_home()?;
        sync_bundle(&home, V1).map_err(ctx("install v1"))?;
        std::fs::write(target(&home), "meine Fassung\n").map_err(ctx("user edit"))?;

        // Bundle unverändert: die Änderung der Nutzerin bleibt, keine Sidecar.
        let same = sync_bundle(&home, V1).map_err(ctx("rerun v1"))?;
        assert_eq!(same, BundleSync::default());
        assert!(!sidecar_path(&target(&home)).exists());

        // Bundle neu: Datei bleibt, neue Fassung liegt als .harw-neu daneben.
        let sync = sync_bundle(&home, V2).map_err(ctx("install v2"))?;
        let sidecar = sidecar_path(&target(&home));
        assert_eq!(sync.conflicts, vec![sidecar.clone()]);
        assert!(sync.updated.is_empty());
        assert_eq!(read(&target(&home))?, "meine Fassung\n");
        assert_eq!(read(&sidecar)?, "Fassung 2\n");
        assert!(
            sidecar
                .to_string_lossy()
                .ends_with("instructions.md.harw-neu")
        );

        // Wiederholung: die Sidecar ist schon aktuell, kein neuer Hinweis.
        let again = sync_bundle(&home, V2).map_err(ctx("rerun v2"))?;
        assert!(again.conflicts.is_empty());
        assert_eq!(read(&target(&home))?, "meine Fassung\n");
        std::fs::remove_dir_all(&home).map_err(ctx("cleanup"))?;
        Ok(())
    }

    #[test]
    fn test_legacy_install_without_manifest_adopts_identical_and_protects_differing() -> TestResult
    {
        let home = temp_home()?;
        const LEGACY: &[BundledFile] = &[
            BundledFile {
                relative_path: "skills/same/instructions.md",
                contents: "gleich\n",
            },
            BundledFile {
                relative_path: "skills/other/instructions.md",
                contents: "neu\n",
            },
        ];
        let same = home.join("skills").join("same").join("instructions.md");
        let other = home.join("skills").join("other").join("instructions.md");
        for (path, text) in [(&same, "gleich\n"), (&other, "alt oder geändert\n")] {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(ctx("mkdir legacy"))?;
            }
            std::fs::write(path, text).map_err(ctx("write legacy"))?;
        }

        let sync = sync_bundle(&home, LEGACY).map_err(ctx("sync legacy"))?;
        assert!(sync.written.is_empty() && sync.updated.is_empty());
        assert_eq!(sync.conflicts, vec![sidecar_path(&other)]);
        assert_eq!(read(&other)?, "alt oder geändert\n");
        assert_eq!(read(&sidecar_path(&other))?, "neu\n");
        let manifest = read(&home.join(BUNDLE_MANIFEST_FILE))?;
        assert!(
            manifest.contains("skills/same/instructions.md"),
            "{manifest}"
        );
        assert!(
            !manifest.contains("skills/other/instructions.md"),
            "eine abweichende Alt-Datei stammt nicht nachweislich aus dem Bundle: {manifest}"
        );
        std::fs::remove_dir_all(&home).map_err(ctx("cleanup"))?;
        Ok(())
    }

    #[test]
    fn test_corrupt_manifest_is_treated_as_missing_and_never_overwrites() -> TestResult {
        let home = temp_home()?;
        sync_bundle(&home, V1).map_err(ctx("install v1"))?;
        std::fs::write(home.join(BUNDLE_MANIFEST_FILE), "das ist [kein toml")
            .map_err(ctx("corrupt manifest"))?;
        let sync = sync_bundle(&home, V2).map_err(ctx("install v2"))?;
        assert!(sync.updated.is_empty());
        assert_eq!(read(&target(&home))?, "Fassung 1\n");
        assert_eq!(read(&sidecar_path(&target(&home)))?, "Fassung 2\n");
        std::fs::remove_dir_all(&home).map_err(ctx("cleanup"))?;
        Ok(())
    }
}
