//! Der `[versions]`-Abschnitt einer RustSec-Advisory: welche Versionen
//! **nicht** betroffen sind, und daraus, welche es sind.
//!
//! # Zweck
//! RustSec beschreibt nicht die verwundbaren Versionen, sondern deren
//! Gegenteil: `patched` (behoben) und `unaffected` (nie betroffen), je eine
//! Liste von Cargo-Versionsanforderungen. Eine Version ist verwundbar, wenn
//! sie in **keinem** dieser Bereiche liegt. Ohne einen einzigen Bereich
//! (`patched = []`, typisch für „unmaintained“) ist jede Version betroffen.
//!
//! # Warum nicht `semver::VersionReq::matches`
//! `VersionReq::matches` schließt Vorabversionen aus, sobald der Vergleich
//! selbst keine trägt: `>= 0.9.5` passt nicht auf `0.10.0-pre.3`. Ein
//! gesperrtes `0.10.0-pre.3` wäre damit „in keinem behobenen Bereich“ und
//! würde fälschlich als verwundbar gemeldet. `rustsec` (die Bibliothek
//! hinter `cargo audit`) wandelt jede Anforderung deshalb in ein Intervall
//! mit SemVer-Präzedenz um (`osv::unaffected_range`); dieses Modul tut
//! dasselbe:
//!
//! - `>`/`>=`/`<`/`<=` setzen je höchstens eine untere bzw. obere Grenze;
//!   höchstens zwei Vergleiche je Anforderung.
//! - `=V` ist das Intervall `[V, V]`.
//! - `^V` und `~V` (auch die nackte Form `V`, die `semver` als `^V` liest)
//!   enden exklusiv bei der nächsten inkompatiblen Version mit der
//!   kleinstmöglichen Vorabkennung `-0` — `^1.2.3` schließt also
//!   `2.0.0-alpha.1` nicht ein.
//! - Wildcards (`1.*`) und jede andere Form gelten als nicht unterstützt
//!   ([`SourceFault::VersionRange`]), wie in `rustsec`.
//!
//! Verglichen wird mit [`Version::cmp_precedence`]: Build-Metadaten
//! (`1.1.6+spec-1.1.0`) zählen nach SemVer §10 nicht zur Präzedenz.
//! Damit ist der Vergleich zahlenbasiert, nie lexikographisch: `1.10.0`
//! liegt über `1.9.0`.
//!
//! Überlappende Bereiche lehnt `rustsec` beim Laden ab; hier ändern sie das
//! Ergebnis nicht (eine Version liegt in einem Bereich oder nicht) und
//! werden deshalb hingenommen.
//!
//! # Grenzen
//! Höchstens [`MAX_RANGES`] Anforderungen je Advisory; mehr gilt als
//! [`SourceFault::VersionRange`].
//!
//! # Nebenläufigkeit
//! Unveränderliche Werte und reine Funktionen; `Send + Sync`.
//!
//! # Fehler
//! [`SourceFault::VersionRange`], inhaltsfrei.
//!
//! # Examples
//! ```rust,ignore
//! // Crate-intern.
//! let versions = AffectedVersions::parse(&[">= 1.9.0".to_owned()], &[])?;
//! assert!(!versions.is_vulnerable(&Version::parse("1.10.0")?));
//! ```

use std::cmp::Ordering;

use semver::{BuildMetadata, Comparator, Op, Prerelease, Version, VersionReq};

use crate::fault::SourceFault;

/// Höchstzahl der Versionsanforderungen (`patched` plus `unaffected`) je
/// Advisory. Reale Advisories haben selten mehr als fünf.
pub(crate) const MAX_RANGES: usize = 64;

// Eine Intervallgrenze; `Unbounded` heißt „keine Grenze in diese Richtung“.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bound {
    Unbounded,
    Inclusive(Version),
    Exclusive(Version),
}

impl Bound {
    // Die Version der Grenze, ohne Rücksicht auf die Art.
    fn version(&self) -> Option<&Version> {
        match self {
            Self::Unbounded => None,
            Self::Inclusive(version) | Self::Exclusive(version) => Some(version),
        }
    }
}

// Ein nicht betroffener Bereich; `start` liegt nie über `end`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnaffectedRange {
    start: Bound,
    end: Bound,
}

impl UnaffectedRange {
    // Baut den Bereich; ein leerer Bereich (Start über Ende, oder gleiche
    // Versionen mit einer exklusiven Grenze) ist ein Fehler, wie in rustsec.
    fn new(start: Bound, end: Bound) -> Result<Self, SourceFault> {
        let ordered = match (start.version(), end.version()) {
            (Some(low), Some(high)) => match low.cmp_precedence(high) {
                Ordering::Less => true,
                Ordering::Equal => {
                    matches!((&start, &end), (Bound::Inclusive(_), Bound::Inclusive(_)))
                }
                Ordering::Greater => false,
            },
            _ => true,
        };
        if ordered {
            Ok(Self { start, end })
        } else {
            Err(SourceFault::VersionRange)
        }
    }

    // Ob `version` im Bereich liegt (Präzedenz, Build-Metadaten ignoriert).
    fn contains(&self, version: &Version) -> bool {
        let above_start = match &self.start {
            Bound::Unbounded => true,
            Bound::Inclusive(start) => version.cmp_precedence(start) != Ordering::Less,
            Bound::Exclusive(start) => version.cmp_precedence(start) == Ordering::Greater,
        };
        let below_end = match &self.end {
            Bound::Unbounded => true,
            Bound::Inclusive(end) => version.cmp_precedence(end) != Ordering::Greater,
            Bound::Exclusive(end) => version.cmp_precedence(end) == Ordering::Less,
        };
        above_start && below_end
    }
}

/// Die betroffenen Versionen einer Advisory, als Komplement ihrer nicht
/// betroffenen Bereiche.
///
/// # Description
/// Siehe Moduldoku. Hält die bereits gedeuteten Bereiche aus `patched` und
/// `unaffected`; [`Self::is_vulnerable`] ist eine reine Mengenabfrage.
///
/// # Concurrency
/// Unveränderlich; `Send + Sync`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AffectedVersions {
    unaffected: Vec<UnaffectedRange>,
}

impl AffectedVersions {
    /// Deutet `patched` und `unaffected` einer Advisory.
    ///
    /// # Arguments
    /// - `patched` (`&[String]`): die behobenen Bereiche.
    /// - `unaffected` (`&[String]`): die nie betroffenen Bereiche.
    ///
    /// # Returns
    /// Die gedeuteten Bereiche.
    ///
    /// # Errors
    /// [`SourceFault::VersionRange`]: mehr als [`MAX_RANGES`] Einträge, eine
    /// unlesbare oder nicht unterstützte Anforderung, oder ein leerer
    /// Bereich.
    pub(crate) fn parse(patched: &[String], unaffected: &[String]) -> Result<Self, SourceFault> {
        if patched.len().saturating_add(unaffected.len()) > MAX_RANGES {
            return Err(SourceFault::VersionRange);
        }
        let unaffected = unaffected
            .iter()
            .chain(patched)
            .map(|text| range_from_text(text.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { unaffected })
    }

    /// Ob `version` von der Advisory betroffen ist.
    ///
    /// # Arguments
    /// - `version` (`&Version`): die gesperrte Version.
    ///
    /// # Returns
    /// `true`, wenn `version` in keinem nicht betroffenen Bereich liegt.
    pub(crate) fn is_vulnerable(&self, version: &Version) -> bool {
        !self.unaffected.iter().any(|range| range.contains(version))
    }
}

// Liest eine Anforderung wie `">= 1.8.4, < 1.9.0"` als Bereich.
fn range_from_text(text: &str) -> Result<UnaffectedRange, SourceFault> {
    let requirement = VersionReq::parse(text).map_err(|_| SourceFault::VersionRange)?;
    range_from_requirement(&requirement)
}

// Wandelt eine Anforderung in einen Bereich (Regeln siehe Moduldoku).
fn range_from_requirement(requirement: &VersionReq) -> Result<UnaffectedRange, SourceFault> {
    let count = requirement.comparators.len();
    if count > 2 {
        return Err(SourceFault::VersionRange);
    }

    let mut start = Bound::Unbounded;
    let mut end = Bound::Unbounded;
    for comparator in &requirement.comparators {
        let version = comparator_version(comparator);
        match comparator.op {
            Op::Greater => set_once(&mut start, Bound::Exclusive(version))?,
            Op::GreaterEq => set_once(&mut start, Bound::Inclusive(version))?,
            Op::Less => set_once(&mut end, Bound::Exclusive(version))?,
            Op::LessEq => set_once(&mut end, Bound::Inclusive(version))?,
            Op::Exact => {
                require_alone(count)?;
                start = Bound::Inclusive(version.clone());
                end = Bound::Inclusive(version);
            }
            Op::Caret => {
                require_alone(count)?;
                end = Bound::Exclusive(caret_upper(comparator)?);
                start = Bound::Inclusive(version);
            }
            Op::Tilde => {
                require_alone(count)?;
                end = Bound::Exclusive(tilde_upper(comparator)?);
                start = Bound::Inclusive(version);
            }
            // `Op::Wildcard` und künftige Operatoren (`Op` ist
            // `#[non_exhaustive]`): nicht unterstützt, wie in rustsec.
            _ => return Err(SourceFault::VersionRange),
        }
    }
    UnaffectedRange::new(start, end)
}

// Setzt eine Grenze, die noch frei sein muss (`>= 1, > 2` ist ein Fehler).
fn set_once(slot: &mut Bound, bound: Bound) -> Result<(), SourceFault> {
    if !matches!(slot, Bound::Unbounded) {
        return Err(SourceFault::VersionRange);
    }
    *slot = bound;
    Ok(())
}

// `=`, `^` und `~` legen beide Grenzen fest und müssen allein stehen.
fn require_alone(count: usize) -> Result<(), SourceFault> {
    if count == 1 {
        Ok(())
    } else {
        Err(SourceFault::VersionRange)
    }
}

// Die Version eines Vergleichs; fehlende Teile zählen als 0.
fn comparator_version(comparator: &Comparator) -> Version {
    Version {
        major: comparator.major,
        minor: comparator.minor.unwrap_or(0),
        patch: comparator.patch.unwrap_or(0),
        pre: comparator.pre.clone(),
        build: BuildMetadata::EMPTY,
    }
}

// Obere, exklusive Grenze von `^V`: die nächste inkompatible Version mit
// Vorabkennung `-0` (Cargo-Regeln für Hauptversion 0 eingeschlossen).
fn caret_upper(comparator: &Comparator) -> Result<Version, SourceFault> {
    let mut upper = if comparator.major == 0 {
        match (comparator.minor, comparator.patch) {
            (Some(0), Some(patch)) => Version::new(0, 0, increment(patch)?),
            (Some(minor), _) => Version::new(0, increment(minor)?, 0),
            (None, None) => Version::new(1, 0, 0),
            (None, Some(_)) => return Err(SourceFault::VersionRange),
        }
    } else {
        Version::new(increment(comparator.major)?, 0, 0)
    };
    upper.pre = lowest_prerelease()?;
    Ok(upper)
}

// Obere, exklusive Grenze von `~V`.
fn tilde_upper(comparator: &Comparator) -> Result<Version, SourceFault> {
    let mut upper = match (comparator.minor, comparator.patch) {
        (None, None) => Version::new(increment(comparator.major)?, 0, 0),
        (Some(minor), _) => Version::new(comparator.major, increment(minor)?, 0),
        (None, Some(_)) => return Err(SourceFault::VersionRange),
    };
    upper.pre = lowest_prerelease()?;
    Ok(upper)
}

// `value + 1` ohne Überlauf.
fn increment(value: u64) -> Result<u64, SourceFault> {
    value.checked_add(1).ok_or(SourceFault::VersionRange)
}

// Die kleinstmögliche Vorabkennung `-0`.
fn lowest_prerelease() -> Result<Prerelease, SourceFault> {
    Prerelease::new("0").map_err(|_| SourceFault::VersionRange)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn versions(patched: &[&str], unaffected: &[&str]) -> Result<AffectedVersions, SourceFault> {
        let patched: Vec<String> = patched.iter().map(|text| (*text).to_owned()).collect();
        let unaffected: Vec<String> = unaffected.iter().map(|text| (*text).to_owned()).collect();
        AffectedVersions::parse(&patched, &unaffected)
    }

    fn vulnerable(affected: &AffectedVersions, text: &str) -> TestResult<bool> {
        let version = Version::parse(text).map_err(ctx("Version parsen"))?;
        Ok(affected.is_vulnerable(&version))
    }

    #[test]
    fn test_numeric_not_lexicographic_comparison() -> TestResult {
        // "1.10.0" < "1.9.0" als Zeichenkette — nach SemVer liegt 1.10.0 darüber.
        let affected = versions(&[">= 1.9.0"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&affected, "1.10.0")?);
        assert!(!vulnerable(&affected, "1.9.0")?);
        assert!(vulnerable(&affected, "1.8.9")?);
        Ok(())
    }

    #[test]
    fn test_tokio_style_patched_and_unaffected_ranges() -> TestResult {
        // RUSTSEC-2021-0124 (tokio), wörtlich aus der advisory-db.
        let affected = versions(&[">= 1.8.4, < 1.9.0", ">= 1.13.1"], &["< 0.1.14"])
            .map_err(ctx("Bereiche parsen"))?;
        assert!(!vulnerable(&affected, "0.1.13")?);
        assert!(vulnerable(&affected, "0.1.14")?);
        assert!(vulnerable(&affected, "1.8.3")?);
        assert!(!vulnerable(&affected, "1.8.4")?);
        assert!(!vulnerable(&affected, "1.8.9")?);
        assert!(vulnerable(&affected, "1.9.0")?);
        assert!(vulnerable(&affected, "1.13.0")?);
        assert!(!vulnerable(&affected, "1.13.1")?);
        assert!(!vulnerable(&affected, "1.40.0")?);
        Ok(())
    }

    #[test]
    fn test_no_ranges_means_every_version_is_affected() -> TestResult {
        let affected = versions(&[], &[]).map_err(ctx("leere Bereiche"))?;
        assert!(vulnerable(&affected, "0.0.1")?);
        assert!(vulnerable(&affected, "99.0.0")?);
        Ok(())
    }

    #[test]
    fn test_exclusive_lower_bound() -> TestResult {
        let affected = versions(&["> 1.0.0"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(vulnerable(&affected, "1.0.0")?);
        assert!(!vulnerable(&affected, "1.0.1")?);
        Ok(())
    }

    #[test]
    fn test_inclusive_upper_bound() -> TestResult {
        let affected = versions(&[], &["<= 0.5.0"]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&affected, "0.5.0")?);
        assert!(vulnerable(&affected, "0.5.1")?);
        Ok(())
    }

    #[test]
    fn test_bare_and_caret_requirements_stop_before_next_major() -> TestResult {
        let affected = versions(&["1.2.3"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(vulnerable(&affected, "1.2.2")?);
        assert!(!vulnerable(&affected, "1.2.3")?);
        assert!(!vulnerable(&affected, "1.99.0")?);
        assert!(vulnerable(&affected, "2.0.0")?);
        // `-0` ist die kleinste Vorabkennung: 2.0.0-alpha.1 liegt nicht mehr im Bereich.
        assert!(vulnerable(&affected, "2.0.0-alpha.1")?);
        Ok(())
    }

    #[test]
    fn test_caret_rules_for_major_zero() -> TestResult {
        let minor = versions(&["^0.2.3"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&minor, "0.2.9")?);
        assert!(vulnerable(&minor, "0.3.0")?);
        let patch = versions(&["^0.0.3"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&patch, "0.0.3")?);
        assert!(vulnerable(&patch, "0.0.4")?);
        let zero = versions(&["^0"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&zero, "0.9.0")?);
        assert!(vulnerable(&zero, "1.0.0")?);
        Ok(())
    }

    #[test]
    fn test_tilde_requirement() -> TestResult {
        let affected = versions(&["~1.2"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&affected, "1.2.9")?);
        assert!(vulnerable(&affected, "1.3.0")?);
        assert!(vulnerable(&affected, "1.1.9")?);
        Ok(())
    }

    #[test]
    fn test_exact_requirement() -> TestResult {
        let affected = versions(&["=1.0.5"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&affected, "1.0.5")?);
        assert!(vulnerable(&affected, "1.0.6")?);
        assert!(vulnerable(&affected, "1.0.4")?);
        Ok(())
    }

    #[test]
    fn test_prerelease_inside_patched_range_is_not_vulnerable() -> TestResult {
        // `VersionReq::matches` sagte hier `false` — und meldete fälschlich.
        let affected = versions(&[">= 0.9.5"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&affected, "0.10.0-pre.3")?);
        assert!(vulnerable(&affected, "0.9.5-rc.1")?);
        Ok(())
    }

    #[test]
    fn test_build_metadata_does_not_change_precedence() -> TestResult {
        let affected = versions(&[], &["<= 1.1.6"]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&affected, "1.1.6+spec-1.1.0")?);
        assert!(vulnerable(&affected, "1.1.7+spec-1.1.0")?);
        Ok(())
    }

    #[test]
    fn test_star_requirement_covers_every_version() -> TestResult {
        let affected = versions(&["*"], &[]).map_err(ctx("Bereich parsen"))?;
        assert!(!vulnerable(&affected, "0.0.1")?);
        assert!(!vulnerable(&affected, "7.0.0")?);
        Ok(())
    }

    #[test]
    fn test_unsupported_or_malformed_requirements_are_rejected() {
        for text in [
            "1.*",
            ">= 1.0.0, < 2.0.0, < 3.0.0",
            ">= 1.0.0, > 2.0.0",
            "< 1.0.0, <= 2.0.0",
            "^1.0, < 2.0.0",
            "=1.0.0, < 2.0.0",
            ">= 2.0.0, < 1.0.0",
            ">= 1.0.0, < 1.0.0",
            "keine version",
            "",
        ] {
            assert_eq!(
                versions(&[text], &[]),
                Err(SourceFault::VersionRange),
                "Anforderung {text:?} hätte abgelehnt werden müssen"
            );
        }
    }

    #[test]
    fn test_overflowing_caret_is_rejected() {
        let text = format!("^{}", u64::MAX);
        assert_eq!(
            versions(&[text.as_str()], &[]),
            Err(SourceFault::VersionRange)
        );
    }

    #[test]
    fn test_too_many_ranges_are_rejected() {
        let many: Vec<&str> =
            std::iter::repeat_n(">= 1.0.0", MAX_RANGES.saturating_add(1)).collect();
        assert_eq!(versions(&many, &[]), Err(SourceFault::VersionRange));
        let enough: Vec<&str> = std::iter::repeat_n(">= 1.0.0", MAX_RANGES).collect();
        assert!(versions(&enough, &[]).is_ok());
    }
}
