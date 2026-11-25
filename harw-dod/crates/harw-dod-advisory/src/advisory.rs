//! Eine RustSec-Advisory-Datei lesen: Front-Matter, Titel, Bereiche.
//!
//! # Format (geprüft gegen `rustsec/advisory-db` und `rustsec::advisory`)
//! Jede Advisory liegt unter `crates/<crate>/RUSTSEC-JJJJ-NNNN.md`:
//!
//! ````text
//! ```toml
//! [advisory]
//! id = "RUSTSEC-2021-0124"
//! package = "tokio"
//! date = "2021-11-16"
//! cvss = "CVSS:3.1/…"            # optional
//! informational = "unmaintained"  # optional: notice | unmaintained | unsound
//! withdrawn = "2023-01-01"        # optional
//!
//! [versions]
//! patched = [">= 1.8.4, < 1.9.0", ">= 1.13.1"]
//! unaffected = ["< 0.1.14"]       # optional
//! ```
//!
//! # Data race when sending and receiving after closing a `oneshot` channel
//!
//! Beschreibung …
//! ````
//!
//! Die Zerlegung folgt `rustsec::advisory::Parts::parse`: die Datei beginnt
//! mit ```` ```toml ````, die Front-Matter endet an der ersten Zeile, die
//! mit ```` ``` ```` beginnt, danach folgt (nach Leerraum) die Überschrift
//! `# Titel`. Fehlt der `[advisory]`-Kopf (Format „V4“), wird er ergänzt.
//! Wie in `rustsec` sind ein `title` in der Front-Matter einer
//! Markdown-Datei und ein fehlender `[versions]`-Abschnitt Formfehler;
//! `patched` ist darin Pflicht, `unaffected` nicht.
//!
//! Das ältere reine TOML-Format (`RUSTSEC-*.toml`, Titel als
//! `[advisory] title`) wird ebenfalls gelesen.
//!
//! Gelesen werden nur die Felder, die der Abgleich braucht; alle übrigen
//! (`date`, `url`, `categories`, `aliases`, `[affected]`, …) werden
//! ignoriert.
//!
//! # Was eine Advisory meldbar macht
//! [`Advisory::is_reportable`]: nicht zurückgezogen (`withdrawn`), für
//! crates.io (`source` fehlt oder ist der crates.io-Index) und, falls
//! informativ, von einer der drei Arten, die `cargo audit` standardmäßig
//! meldet (`notice`, `unmaintained`, `unsound`).
//!
//! # Angreiferkontrollierter Inhalt
//! Die Datenbank ist ein Git-Checkout; wer ihn beschreiben kann, steuert
//! Titel und Felder. Deshalb:
//! - Die ID muss eine schlichte Kennung sein ([`MAX_ID_LEN`],
//!   `[A-Za-z0-9-]`).
//! - Der Titel läuft sofort durch [`harw_untrusted_text::escape_line`] und
//!   wird auf [`MAX_TITLE_CHARS`] Zeichen gekappt; gespeichert wird nur die
//!   entschärfte Form.
//! - Kein Fehler nennt Inhalt ([`SourceFault`] ist feldlos).
//!
//! # Nebenläufigkeit
//! Reine Funktionen und Werttypen; `Send + Sync`.
//!
//! # Fehler
//! [`SourceFault::FrontMatter`], [`SourceFault::Title`],
//! [`SourceFault::AdvisoryShape`], [`SourceFault::AdvisoryId`],
//! [`SourceFault::VersionRange`].
//!
//! # Examples
//! ```rust,ignore
//! // Crate-intern.
//! let advisory = parse_advisory(&content, AdvisoryFormat::Markdown)?;
//! assert!(advisory.is_reportable());
//! ```

use harw_dod_signals::DriftSeverity;
use serde::Deserialize;

use crate::cvss;
use crate::fault::SourceFault;
use crate::lockfile::is_crates_io_source;
use crate::versions::AffectedVersions;

/// Höchstlänge einer Advisory-ID (real: 17 Zeichen, `RUSTSEC-2021-0124`).
pub(crate) const MAX_ID_LEN: usize = 64;

/// Höchstlänge des gespeicherten, bereits entschärften Titels in Zeichen.
pub(crate) const MAX_TITLE_CHARS: usize = 256;

/// Das Dateiformat einer Advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdvisoryFormat {
    /// `RUSTSEC-*.md`: Markdown mit TOML-Front-Matter.
    Markdown,
    /// `RUSTSEC-*.toml`: älteres, reines TOML.
    Toml,
}

impl AdvisoryFormat {
    /// Das Format zu einem Dateinamen.
    ///
    /// # Arguments
    /// - `file_name` (`&str`): der letzte Pfadbestandteil.
    ///
    /// # Returns
    /// `Some` für `.md` und `.toml`, sonst `None`.
    pub(crate) fn from_file_name(file_name: &str) -> Option<Self> {
        if file_name.ends_with(".md") {
            Some(Self::Markdown)
        } else if file_name.ends_with(".toml") {
            Some(Self::Toml)
        } else {
            None
        }
    }
}

/// Die Art einer informativen Advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Informational {
    /// `notice`
    Notice,
    /// `unmaintained`
    Unmaintained,
    /// `unsound`
    Unsound,
    /// Jede andere Angabe; `cargo audit` meldet sie nicht.
    Other,
}

impl Informational {
    // Deutet den Rohwert; unbekannte Werte werden `Other`.
    fn parse(text: &str) -> Self {
        match text {
            "notice" => Self::Notice,
            "unmaintained" => Self::Unmaintained,
            "unsound" => Self::Unsound,
            _ => Self::Other,
        }
    }

    /// Der feste, ASCII-saubere Name für die Ereignisbeschreibung.
    ///
    /// # Returns
    /// `notice`, `unmaintained`, `unsound` oder `other`.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Notice => "notice",
            Self::Unmaintained => "unmaintained",
            Self::Unsound => "unsound",
            Self::Other => "other",
        }
    }

    // Ob `cargo audit` diese Art standardmäßig meldet.
    fn is_reported(&self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// Eine gelesene, geprüfte Advisory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Advisory {
    /// Die geprüfte ID (`[A-Za-z0-9-]`, höchstens [`MAX_ID_LEN`]).
    pub(crate) id: String,
    /// Das betroffene Paket laut Datei.
    pub(crate) package: String,
    /// Der entschärfte, gekappte Titel.
    pub(crate) title: String,
    /// Die Schwere, siehe [`severity_of`]: aus `cvss`
    /// ([`cvss::severity_for_vector`]), wann immer ein Vektor da ist; ohne
    /// Vektor `Low` für `notice`/`unmaintained` und sonst `Unknown`.
    pub(crate) severity: DriftSeverity,
    /// Die Art, falls die Advisory informativ ist.
    pub(crate) informational: Option<Informational>,
    /// Ob die Advisory zurückgezogen wurde.
    pub(crate) withdrawn: bool,
    /// Ob die Advisory für crates.io-Pakete gilt.
    pub(crate) crates_io: bool,
    /// Die betroffenen Versionen.
    pub(crate) versions: AffectedVersions,
}

impl Advisory {
    /// Ob diese Advisory gemeldet werden soll (siehe Moduldoku).
    ///
    /// # Returns
    /// `false` für zurückgezogene, nicht-crates.io- und nicht gemeldete
    /// informative Advisories.
    pub(crate) fn is_reportable(&self) -> bool {
        !self.withdrawn
            && self.crates_io
            && self.informational.is_none_or(|kind| kind.is_reported())
    }
}

// Die Datei als TOML; unbekannte Schlüssel und Tabellen werden ignoriert.
#[derive(Deserialize)]
struct RawAdvisoryFile {
    advisory: RawMetadata,
    #[serde(default)]
    versions: Option<RawVersions>,
}

// Der `[advisory]`-Abschnitt; nur die gebrauchten Felder.
#[derive(Deserialize)]
struct RawMetadata {
    id: String,
    package: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    cvss: Option<String>,
    #[serde(default)]
    informational: Option<String>,
    // Datum als Zeichenkette oder TOML-Datum; nur die Anwesenheit zählt.
    #[serde(default)]
    withdrawn: Option<toml::Value>,
    #[serde(default)]
    source: Option<String>,
}

// Der `[versions]`-Abschnitt; `patched` ist Pflicht wie in rustsec.
#[derive(Deserialize)]
struct RawVersions {
    patched: Vec<String>,
    #[serde(default)]
    unaffected: Vec<String>,
}

/// Liest eine Advisory-Datei.
///
/// # Arguments
/// - `content` (`&str`): der vollständige Dateiinhalt.
/// - `format` (`AdvisoryFormat`): Markdown oder reines TOML.
///
/// # Returns
/// Die geprüfte [`Advisory`].
///
/// # Errors
/// - [`SourceFault::FrontMatter`]: kein geschlossener ```` ```toml ````-Block.
/// - [`SourceFault::Title`]: keine `# `-Überschrift bzw. leerer Titel.
/// - [`SourceFault::AdvisoryShape`]: TOML-Fehler, fehlendes Pflichtfeld,
///   fehlender `[versions]`-Abschnitt oder `title` in der Front-Matter.
/// - [`SourceFault::AdvisoryId`]: ID ist keine schlichte Kennung.
/// - [`SourceFault::VersionRange`]: siehe [`AffectedVersions::parse`].
///
/// # Concurrency
/// Rein; von jedem Thread aufrufbar.
pub(crate) fn parse_advisory(
    content: &str,
    format: AdvisoryFormat,
) -> Result<Advisory, SourceFault> {
    match format {
        AdvisoryFormat::Markdown => {
            let (front_matter, title) = split_markdown(content)?;
            let raw = parse_front_matter(front_matter)?;
            if raw
                .advisory
                .title
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
            {
                return Err(SourceFault::AdvisoryShape);
            }
            build(raw, title)
        }
        AdvisoryFormat::Toml => {
            let mut raw: RawAdvisoryFile =
                toml::from_str(content).map_err(|_| SourceFault::AdvisoryShape)?;
            let title = raw.advisory.title.take().unwrap_or_default();
            build(raw, &title)
        }
    }
}

// Zerlegt eine Markdown-Advisory in Front-Matter und Titelzeile.
fn split_markdown(content: &str) -> Result<(&str, &str), SourceFault> {
    let rest = content
        .strip_prefix("```toml")
        .ok_or(SourceFault::FrontMatter)?;
    let (front_matter, after) = rest.split_once("\n```").ok_or(SourceFault::FrontMatter)?;
    let heading = after
        .trim_start()
        .strip_prefix("# ")
        .ok_or(SourceFault::Title)?;
    let title = heading.lines().next().unwrap_or_default();
    Ok((front_matter.trim(), title))
}

// Liest die Front-Matter; ergänzt den `[advisory]`-Kopf des V4-Formats.
fn parse_front_matter(front_matter: &str) -> Result<RawAdvisoryFile, SourceFault> {
    let wrapped;
    let source = if front_matter.starts_with("[advisory]") {
        front_matter
    } else {
        wrapped = format!("[advisory]\n{front_matter}");
        wrapped.as_str()
    };
    toml::from_str::<RawAdvisoryFile>(source).map_err(|_| SourceFault::AdvisoryShape)
}

// Prüft die Rohdaten und baut die Advisory.
fn build(raw: RawAdvisoryFile, title: &str) -> Result<Advisory, SourceFault> {
    let RawAdvisoryFile { advisory, versions } = raw;
    if !is_plain_id(&advisory.id) {
        return Err(SourceFault::AdvisoryId);
    }
    let title = title.trim();
    if title.is_empty() {
        return Err(SourceFault::Title);
    }
    let versions = versions.ok_or(SourceFault::AdvisoryShape)?;
    let versions = AffectedVersions::parse(&versions.patched, &versions.unaffected)?;

    let informational = advisory.informational.as_deref().map(Informational::parse);
    let severity = severity_of(advisory.cvss.as_deref(), informational);

    Ok(Advisory {
        id: advisory.id,
        package: advisory.package,
        title: harw_untrusted_text::escape_line(title, MAX_TITLE_CHARS),
        severity,
        informational,
        withdrawn: advisory.withdrawn.is_some(),
        crates_io: advisory.source.as_deref().is_none_or(is_crates_io_source),
        versions,
    })
}

/// Die Schwere einer Advisory.
///
/// # Description
/// Ein CVSS-Vektor bestimmt die Schwere immer, auch bei informativen
/// Advisories: ein `unsound` mit Vektor 9.8 ist `High`, nicht `Low`. Ohne
/// Vektor sind `notice` und `unmaintained` `Low` (kein bekannter
/// Ausnutzungsweg, nur ein Wartungs- oder Hinweisbefund), `unsound` dagegen
/// `Unknown`: undefiniertes Verhalten, erreichbar aus sicherem Code, wird
/// nie kleingeredet — wer eine unbekannte Schwere wie eine geringe
/// behandelt, redet einen Befund klein (`DriftSeverity`, und wie
/// `harw-dod-scanreport`, das ebenfalls auf `Unknown` zurückfällt).
///
/// # Arguments
/// - `cvss` (`Option<&str>`): der `cvss`-Wert der Advisory.
/// - `informational` (`Option<Informational>`): die Art, falls informativ.
///
/// # Returns
/// Die [`DriftSeverity`].
pub(crate) fn severity_of(
    cvss: Option<&str>,
    informational: Option<Informational>,
) -> DriftSeverity {
    match (cvss, informational) {
        (Some(vector), _) => cvss::severity_for_vector(vector),
        (None, Some(Informational::Notice | Informational::Unmaintained)) => DriftSeverity::Low,
        (None, Some(Informational::Unsound | Informational::Other) | None) => {
            DriftSeverity::Unknown
        }
    }
}

// Ob `id` eine schlichte Kennung ist: 1 bis `MAX_ID_LEN` Zeichen aus
// `[A-Za-z0-9-]`.
fn is_plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, advisory_md, ctx};
    use semver::Version;

    const TOKIO: &str = "```toml\n[advisory]\nid = \"RUSTSEC-2021-0124\"\npackage = \"tokio\"\naliases = [\"CVE-2021-45710\", \"GHSA-fg7r-2g4j-5cgr\"]\ndate = \"2021-11-16\"\nurl = \"https://github.com/tokio-rs/tokio/issues/4225\"\ncategories = [\"memory-corruption\", \"thread-safety\"]\nkeywords = [\"race condition\"]\n\n[versions]\npatched = [\">= 1.8.4, < 1.9.0\", \">= 1.13.1\"]\nunaffected = [\"< 0.1.14\"]\n\n[affected.functions]\n\"tokio::sync::oneshot::Receiver::close\" = [\"<= 1.13.0, >= 0.1.14\"]\n```\n\n# Data race when sending and receiving after closing a `oneshot` channel\n\nIf a `tokio::sync::oneshot` channel is closed ...\n";

    fn markdown(content: &str) -> Result<Advisory, SourceFault> {
        parse_advisory(content, AdvisoryFormat::Markdown)
    }

    fn is_vulnerable(advisory: &Advisory, text: &str) -> TestResult<bool> {
        let version = Version::parse(text).map_err(ctx("Version parsen"))?;
        Ok(advisory.versions.is_vulnerable(&version))
    }

    #[test]
    fn test_parses_real_shaped_markdown_advisory() -> TestResult {
        let advisory = markdown(TOKIO).map_err(ctx("tokio-Advisory parsen"))?;
        assert_eq!(advisory.id, "RUSTSEC-2021-0124");
        assert_eq!(advisory.package, "tokio");
        assert_eq!(
            advisory.title,
            "Data race when sending and receiving after closing a `oneshot` channel"
        );
        assert_eq!(advisory.severity, DriftSeverity::Unknown);
        assert_eq!(advisory.informational, None);
        assert!(!advisory.withdrawn);
        assert!(advisory.crates_io);
        assert!(advisory.is_reportable());
        assert!(is_vulnerable(&advisory, "1.8.0")?);
        assert!(!is_vulnerable(&advisory, "1.13.1")?);
        Ok(())
    }

    #[test]
    fn test_v4_front_matter_without_advisory_header() -> TestResult {
        let content = "```toml\nid = \"RUSTSEC-2099-0001\"\npackage = \"demo\"\ndate = \"2099-01-01\"\n\n[versions]\npatched = [\">= 2.0.0\"]\n```\n\n# Titel\n";
        let advisory = markdown(content).map_err(ctx("V4-Advisory parsen"))?;
        assert_eq!(advisory.id, "RUSTSEC-2099-0001");
        assert!(is_vulnerable(&advisory, "1.0.0")?);
        Ok(())
    }

    #[test]
    fn test_cvss_sets_the_severity() -> TestResult {
        let content = advisory_md(
            "RUSTSEC-2099-0002",
            "demo",
            "patched = []",
            "cvss = \"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H\"",
            "Kritisch",
        );
        let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
        assert_eq!(advisory.severity, DriftSeverity::High);
        Ok(())
    }

    #[test]
    fn test_unreadable_cvss_keeps_the_advisory_with_unknown_severity() -> TestResult {
        let content = advisory_md(
            "RUSTSEC-2099-0003",
            "demo",
            "patched = []",
            "cvss = \"CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N\"",
            "Neuer Vektor",
        );
        let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
        assert_eq!(advisory.severity, DriftSeverity::Unknown);
        assert!(advisory.is_reportable());
        Ok(())
    }

    #[test]
    fn test_informational_advisories() -> TestResult {
        // Mit Vektor: der Vektor bestimmt die Schwere, auch für informative.
        for (kind, expected) in [
            ("unmaintained", Informational::Unmaintained),
            ("unsound", Informational::Unsound),
            ("notice", Informational::Notice),
        ] {
            let content = advisory_md(
                "RUSTSEC-2099-0004",
                "demo",
                "patched = []",
                &format!(
                    "informational = \"{kind}\"\ncvss = \"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H\""
                ),
                "Info",
            );
            let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
            assert_eq!(advisory.informational, Some(expected));
            assert_eq!(advisory.severity, DriftSeverity::High);
            assert!(advisory.is_reportable());
        }
        // Ohne Vektor: `unsound` bleibt `Unknown`, die beiden anderen `Low`.
        for (kind, expected) in [
            ("unmaintained", DriftSeverity::Low),
            ("notice", DriftSeverity::Low),
            ("unsound", DriftSeverity::Unknown),
        ] {
            let content = advisory_md(
                "RUSTSEC-2099-0007",
                "demo",
                "patched = []",
                &format!("informational = \"{kind}\""),
                "Info ohne Vektor",
            );
            let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
            assert_eq!(advisory.severity, expected, "{kind}");
            assert!(advisory.is_reportable());
        }
        let other = advisory_md(
            "RUSTSEC-2099-0005",
            "demo",
            "patched = []",
            "informational = \"something-else\"",
            "Anders",
        );
        let advisory = markdown(&other).map_err(ctx("Advisory parsen"))?;
        assert_eq!(advisory.informational, Some(Informational::Other));
        assert!(!advisory.is_reportable());
        Ok(())
    }

    #[test]
    fn test_severity_of_prefers_the_vector_and_never_downgrades_unsound() {
        let critical = "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H";
        assert_eq!(
            severity_of(Some(critical), Some(Informational::Unmaintained)),
            DriftSeverity::High
        );
        assert_eq!(severity_of(Some(critical), None), DriftSeverity::High);
        assert_eq!(
            severity_of(Some("kein vektor"), Some(Informational::Notice)),
            DriftSeverity::Unknown
        );
        assert_eq!(
            severity_of(None, Some(Informational::Unsound)),
            DriftSeverity::Unknown
        );
        assert_eq!(
            severity_of(None, Some(Informational::Notice)),
            DriftSeverity::Low
        );
        assert_eq!(severity_of(None, None), DriftSeverity::Unknown);
    }

    #[test]
    fn test_withdrawn_advisory_is_not_reportable() -> TestResult {
        for withdrawn in ["withdrawn = \"2023-03-01\"", "withdrawn = 2023-03-01"] {
            let content = advisory_md(
                "RUSTSEC-2099-0006",
                "demo",
                "patched = []",
                withdrawn,
                "Zurückgezogen",
            );
            let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
            assert!(advisory.withdrawn);
            assert!(!advisory.is_reportable());
        }
        Ok(())
    }

    #[test]
    fn test_foreign_source_is_not_reportable() -> TestResult {
        let content = advisory_md(
            "RUSTSEC-2099-0007",
            "demo",
            "patched = []",
            "source = \"registry+https://example.invalid/index\"",
            "Fremd",
        );
        let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
        assert!(!advisory.crates_io);
        assert!(!advisory.is_reportable());

        let content = advisory_md(
            "RUSTSEC-2099-0008",
            "demo",
            "patched = []",
            "source = \"registry+https://github.com/rust-lang/crates.io-index\"",
            "crates.io ausdrücklich",
        );
        let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
        assert!(advisory.crates_io);
        Ok(())
    }

    #[test]
    fn test_missing_versions_section_is_a_shape_error() {
        let content =
            "```toml\n[advisory]\nid = \"RUSTSEC-2099-0009\"\npackage = \"demo\"\n```\n\n# Titel\n";
        assert_eq!(markdown(content), Err(SourceFault::AdvisoryShape));
    }

    #[test]
    fn test_versions_without_patched_is_a_shape_error() {
        let content = advisory_md(
            "RUSTSEC-2099-0010",
            "demo",
            "unaffected = [\"< 1.0.0\"]",
            "",
            "Titel",
        );
        assert_eq!(markdown(&content), Err(SourceFault::AdvisoryShape));
    }

    #[test]
    fn test_missing_title_is_rejected() {
        let no_heading = "```toml\n[advisory]\nid = \"RUSTSEC-2099-0011\"\npackage = \"demo\"\n[versions]\npatched = []\n```\n\nKein Titel hier.\n";
        assert_eq!(markdown(no_heading), Err(SourceFault::Title));
        let empty_heading = "```toml\n[advisory]\nid = \"RUSTSEC-2099-0011\"\npackage = \"demo\"\n[versions]\npatched = []\n```\n\n#    \n";
        assert_eq!(markdown(empty_heading), Err(SourceFault::Title));
        let at_end = "```toml\n[advisory]\nid = \"RUSTSEC-2099-0011\"\npackage = \"demo\"\n[versions]\npatched = []\n```";
        assert_eq!(markdown(at_end), Err(SourceFault::Title));
    }

    #[test]
    fn test_title_in_markdown_front_matter_is_a_shape_error() {
        let content = advisory_md(
            "RUSTSEC-2099-0012",
            "demo",
            "patched = []",
            "title = \"doppelt\"",
            "Titel",
        );
        assert_eq!(markdown(&content), Err(SourceFault::AdvisoryShape));
    }

    #[test]
    fn test_missing_or_unterminated_front_matter_is_rejected() {
        assert_eq!(markdown("# Nur ein Titel\n"), Err(SourceFault::FrontMatter));
        assert_eq!(
            markdown("```toml\n[advisory]\nid = \"RUSTSEC-2099-0013\"\n"),
            Err(SourceFault::FrontMatter)
        );
        assert_eq!(markdown(""), Err(SourceFault::FrontMatter));
    }

    #[test]
    fn test_malformed_toml_is_rejected_without_content() {
        let marker = "GEHEIM-0815";
        let content = format!("```toml\n[advisory\nid = \"{marker}\"\n```\n\n# Titel\n");
        let parsed = markdown(&content);
        assert_eq!(parsed, Err(SourceFault::AdvisoryShape));
        assert!(!SourceFault::AdvisoryShape.to_string().contains(marker));
    }

    #[test]
    fn test_ids_must_be_plain() {
        for id in [
            "",
            "RUSTSEC 2099",
            "RUSTSEC-2099-0014/../x",
            "RUSTSEC-\u{202E}2099",
            // Ein Tabulator ist in einem TOML-String erlaubt, in einer ID nicht.
            "RUSTSEC-2099-0014\tx",
        ] {
            let content = advisory_md(id, "demo", "patched = []", "", "Titel");
            assert_eq!(
                markdown(&content),
                Err(SourceFault::AdvisoryId),
                "ID {id:?}"
            );
        }
        let long = "R".repeat(MAX_ID_LEN.saturating_add(1));
        let content = advisory_md(&long, "demo", "patched = []", "", "Titel");
        assert_eq!(markdown(&content), Err(SourceFault::AdvisoryId));
    }

    #[test]
    fn test_unsupported_range_rejects_the_advisory() {
        let content = advisory_md(
            "RUSTSEC-2099-0015",
            "demo",
            "patched = [\"1.*\"]",
            "",
            "Titel",
        );
        assert_eq!(markdown(&content), Err(SourceFault::VersionRange));
    }

    #[test]
    fn test_title_control_and_bidi_characters_are_escaped() -> TestResult {
        let content = advisory_md(
            "RUSTSEC-2099-0016",
            "demo",
            "patched = []",
            "",
            "admin\u{202E}txt \u{1b}[31mrot\u{200B} \u{E0041}",
        );
        let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
        assert!(harw_untrusted_text::is_clean(&advisory.title));
        assert!(advisory.title.contains("\\u{202e}"));
        assert!(advisory.title.contains("\\u{001b}"));
        assert!(advisory.title.contains("\\u{200b}"));
        assert!(advisory.title.contains("\\u{e0041}"));
        Ok(())
    }

    #[test]
    fn test_long_title_is_bounded() -> TestResult {
        let content = advisory_md(
            "RUSTSEC-2099-0017",
            "demo",
            "patched = []",
            "",
            &"x".repeat(10_000),
        );
        let advisory = markdown(&content).map_err(ctx("Advisory parsen"))?;
        assert!(advisory.title.chars().count() <= MAX_TITLE_CHARS);
        Ok(())
    }

    #[test]
    fn test_crlf_line_endings() -> TestResult {
        let content = TOKIO.replace('\n', "\r\n");
        let advisory = markdown(&content).map_err(ctx("CRLF-Advisory parsen"))?;
        assert_eq!(
            advisory.title,
            "Data race when sending and receiving after closing a `oneshot` channel"
        );
        Ok(())
    }

    #[test]
    fn test_plain_toml_format() -> TestResult {
        let content = "[advisory]\nid = \"RUSTSEC-2018-0001\"\npackage = \"demo\"\ntitle = \"Altes Format\"\ndate = \"2018-01-01\"\n\n[versions]\npatched = [\">= 0.2.0\"]\n";
        let advisory =
            parse_advisory(content, AdvisoryFormat::Toml).map_err(ctx("TOML-Advisory parsen"))?;
        assert_eq!(advisory.title, "Altes Format");
        assert!(is_vulnerable(&advisory, "0.1.9")?);
        let untitled = "[advisory]\nid = \"RUSTSEC-2018-0002\"\npackage = \"demo\"\n\n[versions]\npatched = []\n";
        assert_eq!(
            parse_advisory(untitled, AdvisoryFormat::Toml),
            Err(SourceFault::Title)
        );
        Ok(())
    }

    #[test]
    fn test_format_from_file_name() {
        assert_eq!(
            AdvisoryFormat::from_file_name("RUSTSEC-2021-0124.md"),
            Some(AdvisoryFormat::Markdown)
        );
        assert_eq!(
            AdvisoryFormat::from_file_name("RUSTSEC-2018-0001.toml"),
            Some(AdvisoryFormat::Toml)
        );
        assert_eq!(
            AdvisoryFormat::from_file_name("RUSTSEC-2021-0124.md.orig"),
            None
        );
        assert_eq!(AdvisoryFormat::from_file_name("README"), None);
    }
}
