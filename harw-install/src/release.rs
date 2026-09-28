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
