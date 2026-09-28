//! Release-Auswahl und -Prüfung für `harw update` — rein, ohne Netzwerk.
//!
//! # Zweck
//! `harw update` holt eine neue Version bevorzugt als GitHub-Release
//! (Tarball plus `SHA256SUMS`, siehe `docs/setup/install.md`, Pfad a) und
//! fällt sonst auf ein Quell-Update zurück. Dieses Modul enthält alles daran,
//! was sich ohne Netz und Dateisystem entscheiden lässt:
//! - [`parse_latest_release`] liest die Antwort von
//!   `GET /repos/<owner>/<repo>/releases/latest`.
//! - [`compare_versions`] / [`is_newer`] vergleichen `X.Y.Z[-pre]`.
//! - [`tarball_name`] nennt das Asset, das `release.yml` baut.
//! - [`verify_sha256sums`] prüft einen heruntergeladenen Tarball gegen
//!   `SHA256SUMS`.
//! - [`update_notice`] entscheidet, ob der Chat-Start auf eine neue Version
//!   hinweist.
//!
//! Netzwerkzugriff, Entpacken und Ersetzen der Binaries liegen in `harw-cli`.
//!
//! # Nebenläufigkeit
//! Nur freie Funktionen und Werttypen; `Send + Sync`.
//!
//! # Fehler
//! [`ReleaseError`], handgeschrieben; Meldungen sind deutsch und enthalten
//! keine Geheimnisse.

use std::cmp::Ordering;
use std::fmt;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::update::VersionInfo;

/// Eine veröffentlichte Release: ihr Tag, die daraus gelesene Version und die
/// Download-Assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Git-Tag der Release, z. B. `v0.8.0`.
    pub tag: String,
    /// Version ohne führendes `v`, z. B. `0.8.0`.
    pub version: String,
    /// Die Dateien der Release.
    pub assets: Vec<ReleaseAsset>,
}

/// Ein Download einer Release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    /// Dateiname, z. B. `harw-v0.8.0-x86_64-unknown-linux-gnu.tar.gz`.
    pub name: String,
    /// Direkte Download-Adresse.
    pub url: String,
}

impl ReleaseInfo {
    /// Das Asset mit genau diesem Namen.
    ///
    /// # Errors
    /// [`ReleaseError::MissingAsset`], wenn die Release es nicht enthält.
    pub fn asset(&self, name: &str) -> Result<&ReleaseAsset, ReleaseError> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| ReleaseError::MissingAsset {
                tag: self.tag.clone(),
                name: name.to_owned(),
            })
    }
}

/// Fehler bei Auswahl oder Prüfung einer Release.
#[derive(Clone, PartialEq, Eq)]
pub enum ReleaseError {
    /// Die API-Antwort ist kein gültiges Release-JSON.
    Parse(String),
    /// Der Tag ist keine Version der Form `X.Y.Z[-pre]`.
    Version(String),
    /// Die Release enthält das benötigte Asset nicht.
    MissingAsset {
        /// Tag der Release.
        tag: String,
        /// Fehlender Dateiname.
        name: String,
    },
    /// `SHA256SUMS` nennt die Datei nicht.
    ChecksumMissing(String),
    /// Die Prüfsumme der Datei stimmt nicht.
    ChecksumMismatch {
        /// Geprüfte Datei.
        name: String,
        /// Laut `SHA256SUMS`.
        expected: String,
        /// Tatsächlich berechnet.
        actual: String,
    },
    /// Das Release-Archiv enthält einen unzulässigen Eintrag oder ist kein
    /// gültiges `.tar.gz` (siehe [`validate_release_archive`]).
    UnsafeArchive(String),
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(detail) => write!(f, "Release-Antwort nicht lesbar: {detail}"),
            Self::Version(tag) => write!(f, "Release-Tag `{tag}` ist keine Version X.Y.Z"),
            Self::MissingAsset { tag, name } => {
                write!(f, "Release {tag} enthält `{name}` nicht")
            }
            Self::ChecksumMissing(name) => write!(f, "SHA256SUMS nennt `{name}` nicht"),
            Self::ChecksumMismatch {
                name,
                expected,
                actual,
            } => write!(
                f,
                "Prüfsumme von `{name}` stimmt nicht (erwartet {expected}, berechnet {actual})"
            ),
            Self::UnsafeArchive(detail) => {
                write!(f, "Release-Archiv abgelehnt, nichts entpackt: {detail}")
            }
        }
    }
}

// Debug delegiert an Display, damit Fehlermeldungen lesbar bleiben.
impl fmt::Debug for ReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ReleaseError {}

/// Die Felder der GitHub-Antwort, die `harw update` braucht.
#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// Liest die Antwort von `GET /repos/<owner>/<repo>/releases/latest`.
///
/// # Description
/// `releases/latest` liefert nie Entwürfe oder Vorabversionen; kommt trotzdem
/// eine solche an (z. B. über einen umgelenkten Endpunkt), wird sie
/// abgelehnt statt installiert.
///
/// # Errors
/// - [`ReleaseError::Parse`]: kein gültiges JSON, oder Entwurf/Vorabversion.
/// - [`ReleaseError::Version`]: der Tag ist keine Version.
pub fn parse_latest_release(json: &[u8]) -> Result<ReleaseInfo, ReleaseError> {
    let api: ApiRelease =
        serde_json::from_slice(json).map_err(|error| ReleaseError::Parse(error.to_string()))?;
    if api.draft || api.prerelease {
        return Err(ReleaseError::Parse(format!(
            "`{}` ist ein Entwurf oder eine Vorabversion",
            api.tag_name
        )));
    }
    let version = api.tag_name.trim_start_matches('v').to_owned();
    if parse_version(&version).is_none() {
        return Err(ReleaseError::Version(api.tag_name));
    }
    Ok(ReleaseInfo {
        tag: api.tag_name,
        version,
        assets: api
            .assets
            .into_iter()
            .map(|asset| ReleaseAsset {
                name: asset.name,
                url: asset.browser_download_url,
            })
            .collect(),
    })
}

/// `X.Y.Z` als Zahlen plus optionale Vorabkennung (`-rc.1`).
fn parse_version(text: &str) -> Option<([u64; 3], Option<&str>)> {
    let text = text.trim().trim_start_matches('v');
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (text, None),
    };
    // Build-Metadaten (`+…`) zählen beim Vergleich nicht.
    let core = core.split('+').next().unwrap_or(core);
    let mut parts = core.split('.');
    let mut numbers = [0_u64; 3];
    for slot in &mut numbers {
        *slot = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((numbers, pre))
}

/// Vergleicht zwei Versionen `X.Y.Z[-pre]` (ein führendes `v` ist erlaubt).
///
/// # Description
/// Die Zahlen werden numerisch verglichen; bei gleichen Zahlen ist eine
/// Vorabversion älter als die Release (`0.8.0-rc.1 < 0.8.0`). Zwei
/// Vorabkennungen werden als Text verglichen.
///
/// # Returns
/// `None`, wenn eine der beiden keine Version ist.
#[must_use]
pub fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    let (left_core, left_pre) = parse_version(left)?;
    let (right_core, right_pre) = parse_version(right)?;
    Some(
        left_core
            .cmp(&right_core)
            .then_with(|| match (left_pre, right_pre) {
                (None, None) => Ordering::Equal,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(left), Some(right)) => left.cmp(right),
            }),
    )
}

/// `true`, wenn `latest` eine neuere Version als `current` ist; eine nicht
/// lesbare Version gilt nie als neuer.
#[must_use]
pub fn is_newer(current: &str, latest: &str) -> bool {
    compare_versions(latest, current) == Some(Ordering::Greater)
}

/// Name des Tarballs, den `release.yml` für `tag` und `target` baut:
/// `harw-<tag>-<target>.tar.gz`.
#[must_use]
pub fn tarball_name(tag: &str, target: &str) -> String {
    format!("harw-{tag}-{target}.tar.gz")
}

/// Name der Prüfsummendatei jeder Release.
pub const CHECKSUMS_FILE: &str = "SHA256SUMS";

/// Prüft `bytes` gegen den Eintrag für `name` in `SHA256SUMS`.
///
/// # Description
/// Versteht das Format von `sha256sum`: `<hex>  <name>` bzw. `<hex> *<name>`
/// (Binärmodus); Pfadpräfixe wie `./` zählen nicht. Die Prüfsumme wird ohne
/// Rücksicht auf Groß-/Kleinschreibung verglichen.
///
/// # Errors
/// - [`ReleaseError::ChecksumMissing`]: kein Eintrag für `name`.
/// - [`ReleaseError::ChecksumMismatch`]: die Prüfsumme weicht ab.
pub fn verify_sha256sums(sums: &str, name: &str, bytes: &[u8]) -> Result<(), ReleaseError> {
    let expected = sums
        .lines()
        .filter_map(|line| {
            let (hash, file) = line.trim().split_once(char::is_whitespace)?;
            let file = file
                .trim_start()
                .trim_start_matches('*')
                .trim_start_matches("./");
            (file == name).then_some(hash)
        })
        .next()
        .ok_or_else(|| ReleaseError::ChecksumMissing(name.to_owned()))?;
    let actual = hex(&Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(ReleaseError::ChecksumMismatch {
            name: name.to_owned(),
            expected: expected.to_ascii_lowercase(),
            actual,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Obergrenze für den entpackten Inhalt eines Release-Archivs (512 MiB).
const MAX_UNPACKED_BYTES: u64 = 512 << 20;

/// Prüft ein Release-Tarball (`.tar.gz`) vor dem Entpacken.
///
/// # Description
/// `SHA256SUMS` stammt aus derselben Release wie das Archiv und belegt nur,
/// dass die Bytes zueinander passen, nicht dass die Einträge harmlos sind.
/// Deshalb liest diese Funktion jeden ustar-Kopf und lässt nur zu:
/// reguläre Dateien (`0`/`\0`) und Verzeichnisse (`5`), deren Pfad relativ
/// ist, keine `..`- oder leeren Segmente enthält und unter genau einem
/// Wurzelverzeichnis `root` liegt (`harw-<tag>-<ziel>/`). Links, Geräte,
/// FIFOs, GNU-Langnamen und pax-Köpfe werden abgelehnt (fail closed), ebenso
/// ein entpackter Inhalt über 512 MiB.
///
/// # Errors
/// [`ReleaseError::UnsafeArchive`] mit dem ersten unzulässigen Eintrag bzw.
/// einer Beschreibung des Formatfehlers.
pub fn validate_release_archive(gz: &[u8], root: &str) -> Result<(), ReleaseError> {
    use std::io::Read as _;

    let unsafe_archive = |reason: String| ReleaseError::UnsafeArchive(reason);
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .take(MAX_UNPACKED_BYTES + 1)
        .read_to_end(&mut tar)
        .map_err(|error| unsafe_archive(format!("kein gültiges gzip: {error}")))?;
    if tar.len() as u64 > MAX_UNPACKED_BYTES {
        return Err(unsafe_archive("entpackt größer als 512 MiB".to_owned()));
    }
    let mut offset = 0usize;
    let mut entries = 0usize;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let field = |range: std::ops::Range<usize>| {
            let raw = &header[range];
            let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
            String::from_utf8_lossy(&raw[..end]).into_owned()
        };
        let name = field(0..100);
        let prefix = field(345..500);
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let size_text = field(124..136);
        let size = u64::from_str_radix(size_text.trim(), 8)
            .map_err(|_| unsafe_archive(format!("`{path}`: ungültige Größe `{size_text}`")))?;
        let kind = header[156];
        match kind {
            b'0' | 0 | b'5' => {}
            other => {
                return Err(unsafe_archive(format!(
                    "`{path}`: Eintragstyp `{}` ist nicht erlaubt (nur Dateien und Verzeichnisse)",
                    char::from(other)
                )));
            }
        }
        let trimmed = path.strip_suffix('/').unwrap_or(&path);
        let mut segments = trimmed.split('/');
        let first = segments.next().unwrap_or_default();
        if path.starts_with('/') || first != root {
            return Err(unsafe_archive(format!(
                "`{path}` liegt nicht unter `{root}/`"
            )));
        }
        if segments.any(|segment| segment.is_empty() || segment == "." || segment == "..") {
            return Err(unsafe_archive(format!(
                "`{path}` enthält ein unzulässiges Segment"
            )));
        }
        entries += 1;
        let data_blocks = usize::try_from(size.div_ceil(512))
            .map_err(|_| unsafe_archive(format!("`{path}`: Größe zu groß")))?;
        offset = offset
            .checked_add(512 + data_blocks * 512)
            .ok_or_else(|| unsafe_archive(format!("`{path}`: Größe zu groß")))?;
    }
    if offset > tar.len() {
        return Err(unsafe_archive("Archiv ist abgeschnitten".to_owned()));
    }
    if entries == 0 {
        return Err(unsafe_archive("Archiv ist leer".to_owned()));
    }
    Ok(())
}

/// Hinweis für den Chat-Start, falls eine neuere Version bekannt ist.
///
/// # Description
/// Liest nur den gespeicherten Stand (`version.json`), nie das Netz. Keinen
/// Hinweis gibt es, wenn die bekannte Version nicht neuer als `current` ist
/// oder der Nutzer genau diese Version mit `harw update --dismiss` verworfen
/// hat.
#[must_use]
pub fn update_notice(info: &VersionInfo, current: &str) -> Option<String> {
    if !is_newer(current, &info.latest_version) {
        return None;
    }
    if info.dismissed_version.as_deref() == Some(info.latest_version.as_str()) {
        return None;
    }
    Some(format!(
        "harw {} ist verfügbar (installiert: {current}). `harw update` installiert sie, \
         `harw update --dismiss` blendet diesen Hinweis aus.",
        info.latest_version
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    /// Ein ustar-Eintrag: Kopf (Name, Größe, Typ) plus auf 512 Byte
    /// aufgefüllte Daten. Die Prüfsumme bleibt leer, der Validator liest sie
    /// nicht.
    fn tar_entry(path: &str, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        let size = format!("{:011o}", data.len());
        header[124..135].copy_from_slice(size.as_bytes());
        header[156] = kind;
        let mut out = header;
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    fn gz_archive(entries: &[Vec<u8>]) -> TestResult<Vec<u8>> {
        use std::io::Write as _;
        let mut tar: Vec<u8> = entries.concat();
        tar.extend_from_slice(&[0u8; 1024]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder
            .write_all(&tar)
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        encoder
            .finish()
            .map_err(|error| TestError::Unexpected(error.to_string()))
    }

    const ROOT: &str = "harw-v0.8.0-x86_64-unknown-linux-gnu";

    #[test]
    fn validate_release_archive_accepts_files_and_dirs_under_root() -> TestResult {
        let archive = gz_archive(&[
            tar_entry(&format!("{ROOT}/"), b'5', b""),
            tar_entry(&format!("{ROOT}/harw"), b'0', b"\x7fELF binary"),
            tar_entry(&format!("{ROOT}/README.md"), 0, b"text"),
        ])?;
        validate_release_archive(&archive, ROOT)
            .map_err(|error| TestError::Unexpected(error.to_string()))
    }

    #[test]
    fn validate_release_archive_rejects_traversal_links_and_foreign_roots() -> TestResult {
        let cases = [
            tar_entry(&format!("{ROOT}/../evil"), b'0', b"x"),
            tar_entry("/etc/passwd", b'0', b"x"),
            tar_entry("other-root/harw", b'0', b"x"),
            tar_entry(&format!("{ROOT}/link"), b'2', b""),
            tar_entry(&format!("{ROOT}/hard"), b'1', b""),
            tar_entry(&format!("{ROOT}//x"), b'0', b"x"),
            tar_entry("././@LongLink", b'L', b"x"),
        ];
        for entry in cases {
            let archive = gz_archive(&[entry])?;
            let result = validate_release_archive(&archive, ROOT);
            assert!(
                matches!(result, Err(ReleaseError::UnsafeArchive(_))),
                "{result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn validate_release_archive_rejects_garbage_and_empty() -> TestResult {
        assert!(matches!(
            validate_release_archive(b"not gzip", ROOT),
            Err(ReleaseError::UnsafeArchive(_))
        ));
        let empty = gz_archive(&[])?;
        assert!(matches!(
            validate_release_archive(&empty, ROOT),
            Err(ReleaseError::UnsafeArchive(_))
        ));
        Ok(())
    }

    fn release_json(tag: &str, prerelease: bool) -> String {
        serde_json::json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": prerelease,
            "assets": [
                {"name": "SHA256SUMS", "browser_download_url": "https://example.test/SHA256SUMS"},
                {"name": format!("harw-{tag}-x86_64-unknown-linux-gnu.tar.gz"),
                 "browser_download_url": "https://example.test/harw.tar.gz"}
            ]
        })
        .to_string()
    }

    fn info(latest: &str, dismissed: Option<&str>) -> TestResult<VersionInfo> {
        Ok(VersionInfo {
            latest_version: latest.to_owned(),
            last_checked_at: jiff::Timestamp::from_second(1_790_000_000)
                .map_err(|error| TestError::Unexpected(error.to_string()))?,
            dismissed_version: dismissed.map(str::to_owned),
        })
    }

    #[test]
    fn test_parse_latest_release_reads_tag_version_and_assets() -> TestResult {
        let release = parse_latest_release(release_json("v0.9.0", false).as_bytes())
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert_eq!(release.tag, "v0.9.0");
        assert_eq!(release.version, "0.9.0");
        let tarball = tarball_name(&release.tag, "x86_64-unknown-linux-gnu");
        let asset = release
            .asset(&tarball)
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert_eq!(asset.url, "https://example.test/harw.tar.gz");
        assert!(release.asset(CHECKSUMS_FILE).is_ok());
        Ok(())
    }

    #[test]
    fn test_parse_latest_release_rejects_prerelease_and_bad_tags() {
        assert!(matches!(
            parse_latest_release(release_json("v0.9.0-rc.1", true).as_bytes()),
            Err(ReleaseError::Parse(_))
        ));
        assert!(matches!(
            parse_latest_release(release_json("nightly", false).as_bytes()),
            Err(ReleaseError::Version(_))
        ));
        assert!(matches!(
            parse_latest_release(b"{not json"),
            Err(ReleaseError::Parse(_))
        ));
    }

    #[test]
    fn test_missing_asset_names_tag_and_file() -> TestResult {
        let release = parse_latest_release(release_json("v0.9.0", false).as_bytes())
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        match release.asset("harw-v0.9.0-aarch64-unknown-linux-gnu.tar.gz") {
            Err(ReleaseError::MissingAsset { tag, name }) => {
                assert_eq!(tag, "v0.9.0");
                assert!(name.contains("aarch64"));
                Ok(())
            }
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_compare_versions_orders_numbers_and_prereleases() {
        assert_eq!(compare_versions("0.8.0", "0.10.0"), Some(Ordering::Less));
        assert_eq!(
            compare_versions("v1.0.0", "0.99.99"),
            Some(Ordering::Greater)
        );
        assert_eq!(compare_versions("0.8.0", "v0.8.0"), Some(Ordering::Equal));
        assert_eq!(
            compare_versions("0.8.0-rc.1", "0.8.0"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("0.8.0+build.7", "0.8.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(compare_versions("0.8", "0.8.0"), None);
        assert_eq!(compare_versions("0.8.0.1", "0.8.0"), None);
        assert!(is_newer("0.8.0", "0.8.1"));
        assert!(!is_newer("0.8.1", "0.8.0"));
        assert!(!is_newer("0.8.0", "garbage"));
    }

    #[test]
    fn test_verify_sha256sums_accepts_match_and_rejects_mismatch() {
        // sha256("abc")
        let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let sums = format!("{hash}  harw.tar.gz\n0000  other.tar.gz\n");
        assert_eq!(verify_sha256sums(&sums, "harw.tar.gz", b"abc"), Ok(()));
        let binary_mode = format!("{}  *./harw.tar.gz\n", hash.to_ascii_uppercase());
        assert_eq!(
            verify_sha256sums(&binary_mode, "harw.tar.gz", b"abc"),
            Ok(())
        );
        assert!(matches!(
            verify_sha256sums(&sums, "harw.tar.gz", b"abd"),
            Err(ReleaseError::ChecksumMismatch { .. })
        ));
        assert_eq!(
            verify_sha256sums(&sums, "missing.tar.gz", b"abc"),
            Err(ReleaseError::ChecksumMissing("missing.tar.gz".to_owned()))
        );
    }

    #[test]
    fn test_update_notice_only_for_newer_undismissed_versions() -> TestResult {
        let notice = update_notice(&info("0.9.0", None)?, "0.8.0")
            .ok_or(TestError::Missing("notice for a newer version"))?;
        assert!(notice.contains("0.9.0") && notice.contains("harw update --dismiss"));
        assert_eq!(update_notice(&info("0.8.0", None)?, "0.8.0"), None);
        assert_eq!(update_notice(&info("0.7.0", None)?, "0.8.0"), None);
        assert_eq!(update_notice(&info("0.9.0", Some("0.9.0"))?, "0.8.0"), None);
        // Ein älteres Verwerfen blendet eine neuere Version nicht aus.
        assert!(update_notice(&info("0.9.1", Some("0.9.0"))?, "0.8.0").is_some());
        Ok(())
    }
}
