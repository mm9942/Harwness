//! CVSS-3.x-Basiswert aus dem Vektor einer RustSec-Advisory.
//!
//! # Zweck
//! Eine RustSec-Datei trägt keine Schwerestufe, sondern höchstens einen
//! CVSS-Vektor (`cvss = "CVSS:3.1/AV:N/AC:L/…"`); `cargo audit` rechnet die
//! Stufe daraus. Ohne diese Rechnung wäre jede Meldung
//! [`DriftSeverity::Unknown`] — und die Regel `structure-drift` stufte eine
//! kritische Lücke genauso ein wie eine geringe. Dieses Modul berechnet den
//! Basiswert nach der CVSS-v3.1-Spezifikation (Abschnitt 7.1 und
//! Anhang A, `Roundup`) und bildet ihn auf [`DriftSeverity`] ab
//! ([`severity_for_score`]).
//!
//! # Was gelesen wird
//! - Präfix `CVSS:3.0/` oder `CVSS:3.1/`. Die Basisformel ist für beide
//!   gleich; 3.1 hat nur `Roundup` gegen Gleitkomma-Rauschen gehärtet, und
//!   diese Fassung wird für beide verwendet.
//! - Die acht Basismetriken `AV`, `AC`, `PR`, `UI`, `S`, `C`, `I`, `A`,
//!   jede genau einmal.
//! - Temporale und Umgebungsmetriken (`E`, `RL`, `RC`, `CR`, `IR`, `AR`,
//!   `MAV` … `MA`) werden erkannt und ignoriert: sie ändern den Basiswert
//!   nicht.
//!
//! Alles andere — CVSS 4.0, eine fehlende, doppelte oder unbekannte Metrik,
//! ein unbekannter Wert, ein Vektor über [`MAX_VECTOR_LEN`] Zeichen — ergibt
//! `None` bzw. [`DriftSeverity::Unknown`]. Die Advisory selbst bleibt
//! gültig: ein unlesbarer Schwerewert ist kein Grund, eine Lücke zu
//! verschweigen.
//!
//! # Abbildung
//! | Basiswert | CVSS-Stufe | [`DriftSeverity`] |
//! |---|---|---|
//! | 9.0–10.0 | Critical | `High` |
//! | 7.0–8.9 | High | `High` |
//! | 4.0–6.9 | Medium | `Medium` |
//! | 0.1–3.9 | Low | `Low` |
//! | 0.0 | None | `Low` |
//!
//! # Nebenläufigkeit
//! Reine Funktionen; von jedem Thread aufrufbar.
//!
//! # Fehler
//! Keine; unlesbare Vektoren ergeben `None`.
//!
//! # Examples
//! ```rust,ignore
//! // Crate-intern.
//! let score = base_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H");
//! assert!(score.is_some_and(|value| (value - 9.8).abs() < 1e-9));
//! ```

use harw_dod_signals::DriftSeverity;

/// Höchstlänge eines Vektors; reale Vektoren haben rund 45 Zeichen, mit
/// allen Umgebungsmetriken gut 130.
pub(crate) const MAX_VECTOR_LEN: usize = 256;

// Metriken, die erkannt, aber für den Basiswert ignoriert werden.
const NON_BASE_METRICS: [&str; 14] = [
    "E", "RL", "RC", "CR", "IR", "AR", "MAV", "MAC", "MPR", "MUI", "MS", "MC", "MI", "MA",
];

// Die Rohwerte der acht Basismetriken, je höchstens einmal gesetzt.
#[derive(Default)]
struct BaseMetrics<'a> {
    attack_vector: Option<&'a str>,
    attack_complexity: Option<&'a str>,
    privileges_required: Option<&'a str>,
    user_interaction: Option<&'a str>,
    scope: Option<&'a str>,
    confidentiality: Option<&'a str>,
    integrity: Option<&'a str>,
    availability: Option<&'a str>,
}

/// Bildet einen CVSS-Vektor auf die grobe Schwere des Signalstroms ab.
///
/// # Arguments
/// - `vector` (`&str`): der `cvss`-Wert der Advisory.
///
/// # Returns
/// Die Stufe nach der Tabelle der Moduldoku, oder
/// [`DriftSeverity::Unknown`], wenn der Vektor nicht lesbar ist.
pub(crate) fn severity_for_vector(vector: &str) -> DriftSeverity {
    base_score(vector).map_or(DriftSeverity::Unknown, severity_for_score)
}

/// Bildet einen Basiswert (0.0 bis 10.0) auf [`DriftSeverity`] ab.
///
/// # Arguments
/// - `score` (`f64`): der gerundete Basiswert.
///
/// # Returns
/// `High` ab 7.0, `Medium` ab 4.0, sonst `Low`.
pub(crate) fn severity_for_score(score: f64) -> DriftSeverity {
    if score >= 7.0 {
        DriftSeverity::High
    } else if score >= 4.0 {
        DriftSeverity::Medium
    } else {
        DriftSeverity::Low
    }
}

/// Berechnet den CVSS-3.x-Basiswert eines Vektors.
///
/// # Arguments
/// - `vector` (`&str`): etwa `CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H`.
///
/// # Returns
/// Den auf eine Nachkommastelle aufgerundeten Basiswert, oder `None` für
/// einen nicht lesbaren Vektor (siehe Moduldoku).
pub(crate) fn base_score(vector: &str) -> Option<f64> {
    if vector.len() > MAX_VECTOR_LEN {
        return None;
    }
    let metrics_text = vector
        .strip_prefix("CVSS:3.1/")
        .or_else(|| vector.strip_prefix("CVSS:3.0/"))?;

    let mut metrics = BaseMetrics::default();
    for part in metrics_text.split('/') {
        let (key, value) = part.split_once(':')?;
        let slot = match key {
            "AV" => &mut metrics.attack_vector,
            "AC" => &mut metrics.attack_complexity,
            "PR" => &mut metrics.privileges_required,
            "UI" => &mut metrics.user_interaction,
            "S" => &mut metrics.scope,
            "C" => &mut metrics.confidentiality,
            "I" => &mut metrics.integrity,
            "A" => &mut metrics.availability,
            _ if NON_BASE_METRICS.contains(&key) => continue,
            _ => return None,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(value);
    }

    let attack_vector = match metrics.attack_vector? {
        "N" => 0.85,
        "A" => 0.62,
        "L" => 0.55,
        "P" => 0.2,
        _ => return None,
    };
    let attack_complexity = match metrics.attack_complexity? {
        "L" => 0.77,
        "H" => 0.44,
        _ => return None,
    };
    let scope_changed = match metrics.scope? {
        "U" => false,
        "C" => true,
        _ => return None,
    };
    let privileges_required = match (metrics.privileges_required?, scope_changed) {
        ("N", _) => 0.85,
        ("L", false) => 0.62,
        ("L", true) => 0.68,
        ("H", false) => 0.27,
        ("H", true) => 0.5,
        _ => return None,
    };
    let user_interaction = match metrics.user_interaction? {
        "N" => 0.85,
        "R" => 0.62,
        _ => return None,
    };
    let confidentiality = impact_weight(metrics.confidentiality?)?;
    let integrity = impact_weight(metrics.integrity?)?;
    let availability = impact_weight(metrics.availability?)?;

    let impact_sub_score =
        1.0 - ((1.0 - confidentiality) * (1.0 - integrity) * (1.0 - availability));
    let impact = if scope_changed {
        7.52 * (impact_sub_score - 0.029) - 3.25 * (impact_sub_score - 0.02).powi(15)
    } else {
        6.42 * impact_sub_score
    };
    let exploitability =
        8.22 * attack_vector * attack_complexity * privileges_required * user_interaction;

    if impact <= 0.0 {
        return Some(0.0);
    }
    let raw = if scope_changed {
        (1.08 * (impact + exploitability)).min(10.0)
    } else {
        (impact + exploitability).min(10.0)
    };
    Some(round_up(raw))
}

// Gewicht einer Auswirkungsmetrik (C, I, A).
fn impact_weight(value: &str) -> Option<f64> {
    match value {
        "H" => Some(0.56),
        "L" => Some(0.22),
        "N" => Some(0.0),
        _ => None,
    }
}

// `Roundup` aus CVSS v3.1, Anhang A: kleinster Wert mit einer
// Nachkommastelle, der nicht unter `value` liegt — über eine Ganzzahl mit
// fünf Nachkommastellen, damit Gleitkomma-Rauschen (4.000000000002) nicht
// eine Stufe höher rundet.
fn round_up(value: f64) -> f64 {
    let scaled = (value * 100_000.0).round();
    if (scaled % 10_000.0).abs() < 0.5 {
        scaled / 100_000.0
    } else {
        ((scaled / 10_000.0).floor() + 1.0) / 10.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    // Prüft einen Vektor gegen den Referenzwert (NVD-Rechner).
    fn expect_score(vector: &str, expected: f64) -> TestResult {
        let Some(score) = base_score(vector) else {
            return Err(TestError::Unexpected(format!(
                "Vektor {vector} nicht gelesen"
            )));
        };
        if (score - expected).abs() > 1e-9 {
            return Err(TestError::Unexpected(format!(
                "Vektor {vector}: {score} statt {expected}"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_reference_scores() -> TestResult {
        expect_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H", 9.8)?;
        expect_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N", 6.1)?;
        expect_score("CVSS:3.1/AV:L/AC:L/PR:L/UI:N/S:U/C:H/I:H/A:H", 7.8)?;
        expect_score("CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:L/I:N/A:N", 3.7)?;
        expect_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H", 7.5)?;
        expect_score("CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:N/I:N/A:H", 5.9)?;
        expect_score("CVSS:3.1/AV:N/AC:L/PR:L/UI:N/S:C/C:H/I:H/A:H", 9.9)?;
        expect_score("CVSS:3.1/AV:L/AC:L/PR:N/UI:R/S:U/C:N/I:H/A:N", 5.5)?;
        expect_score("CVSS:3.0/AV:L/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N", 1.8)?;
        expect_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N", 0.0)?;
        Ok(())
    }

    #[test]
    fn test_metric_order_and_temporal_metrics_do_not_matter() -> TestResult {
        expect_score("CVSS:3.1/A:H/I:H/C:H/S:U/UI:N/PR:N/AC:L/AV:N", 9.8)?;
        expect_score(
            "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H/E:P/RL:O/RC:C/CR:H/MAV:L",
            9.8,
        )?;
        Ok(())
    }

    #[test]
    fn test_unreadable_vectors_yield_none() {
        for vector in [
            "",
            "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N",
            "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H",
            "CVSS:3.1/AV:N/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
            "CVSS:3.1/AV:X/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
            "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H/ZZ:1",
            "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H/",
            "AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
        ] {
            assert!(base_score(vector).is_none(), "Vektor {vector:?}");
            assert_eq!(severity_for_vector(vector), DriftSeverity::Unknown);
        }
        let long = format!(
            "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H{}",
            "/E:P".repeat(MAX_VECTOR_LEN)
        );
        assert!(base_score(&long).is_none());
    }

    #[test]
    fn test_severity_bands() {
        assert_eq!(
            severity_for_vector("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            DriftSeverity::High
        );
        assert_eq!(
            severity_for_vector("CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N"),
            DriftSeverity::Medium
        );
        assert_eq!(
            severity_for_vector("CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:L/I:N/A:N"),
            DriftSeverity::Low
        );
        assert_eq!(
            severity_for_vector("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N"),
            DriftSeverity::Low
        );
        assert_eq!(severity_for_score(7.0), DriftSeverity::High);
        assert_eq!(severity_for_score(6.9), DriftSeverity::Medium);
        assert_eq!(severity_for_score(4.0), DriftSeverity::Medium);
        assert_eq!(severity_for_score(3.9), DriftSeverity::Low);
    }

    #[test]
    fn test_round_up_ignores_float_noise() {
        assert!((round_up(4.000_000_000_002) - 4.0).abs() < 1e-12);
        assert!((round_up(4.02) - 4.1).abs() < 1e-12);
        assert!((round_up(9.0) - 9.0).abs() < 1e-12);
    }
}
