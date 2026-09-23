//! Beleg für Sicherheitsbefunde: eingefrorene Beobachtungen mit Digest (Contract-Master §G).
//!
//! # Verantwortungsbereich
//! Trägt [`Hardness`], [`Severity`], [`SecurityEvidence`].
//!
//! # Warum ein Digest
//! Der Digest macht den Beleg zitierfähig: ohne ihn kann ein Befund
//! (`harw-dod-rules::Finding<S>`, siehe unten) auf nichts zeigen, und die
//! Vertrauensklasse `TrustClass::Evidence` (`harw-context`) bliebe im
//! Kontext dauerhaft leer — jedes Sensorfeld würde stattdessen als
//! `TrustClass::Data` eingestuft, weil es keinen überprüfbaren Anker hätte.
//!
//! # Digest-Bildungsregel (verbindlich)
//! `SecurityEvidence::digest` = `ContentDigest::of(canonical_bytes)`, wobei
//! `canonical_bytes` die JSON-Serialisierung von
//! `{ "samples": [..], "events": [..] }` ist — `samples` und `events` in
//! **exakt der Reihenfolge**, in der sie [`SecurityEvidence::capture`]
//! übergeben wurden, ohne Sortierung oder sonstige Normalisierung.
//!
//! Diese Regel ist **order-sensitiv**: dieselben Elemente in derselben
//! Reihenfolge ergeben immer denselben Digest; eine andere Reihenfolge
//! ergibt einen anderen Digest, weil ein JSON-Array ordnungserhaltend
//! serialisiert wird und hier nicht kanonisiert wird (siehe Tests unten für
//! beide Richtungen). Das ist eine bewusste Wahl, keine zufällige
//! Eigenschaft von `serde_json`: `SecurityEvidence` friert einen
//! *bestimmten* Beobachtungsablauf ein, nicht nur eine Menge von
//! Beobachtungen. Zwei Belege mit denselben Elementen in unterschiedlicher
//! Erfassungsreihenfolge sind unterschiedliche Belege — ein Aufrufer, der das
//! nicht will, sortiert vor dem Aufruf von `capture` selbst. Die Regel ist an
//! genau dieser einen Stelle festgelegt, damit ein zweiter Erzeuger sie nicht
//! abweichend erfindet.
//!
//! Es gibt **keinen** öffentlichen Weg, ein `SecurityEvidence` mit einem
//! nicht zu seinem Inhalt passenden `digest` zu bauen: der einzige `pub`
//! Konstruktor ist [`SecurityEvidence::capture`], der den Digest selbst
//! berechnet. (Serde-Deserialisierung eines bereits gespeicherten Belegs ist
//! die eine Ausnahme — sie verifiziert `digest` nicht nachträglich; das ist
//! Aufgabe des Regelwerks, nicht dieser Crate.)
//!
//! # Wo `Finding<S>` lebt
//! **Nicht hier.** Der Arbeitsplan sah `Finding<S>` in dieser Crate vor;
//! Contract-Master §G.1 korrigiert das: `Finding<S>` und alle drei
//! Typestate-Übergänge leben in `harw-dod-rules` (Knoten AW4-03), weil die
//! `pub(crate)`-Konstruktoren sonst für die elf Sensor-Crates unerreichbar
//! wären. Wer `Finding` sucht, findet es dort, nicht hier.
//!
//! # Nebenläufigkeit
//! Reine Datentypen; [`SecurityEvidence::capture`] ist eine zustandslose,
//! reine Funktion (kein Zugriff auf geteilten Zustand), sicher aus jedem
//! Thread aufrufbar.
//!
//! # Fehler
//! [`crate::error::SignalsError::DigestEncoding`], wenn die kanonische
//! Kodierung der Samples und Events fehlschlägt (siehe dortige Doku für die
//! praktische Erreichbarkeit dieses Pfads).
//!
//! # Examples
//! ```rust
//! use harw_dod_signals::SecurityEvidence;
//!
//! let evidence = SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
//!     .expect("empty evidence always encodes");
//! assert_eq!(evidence.samples.len(), 0);
//! ```

use jiff::Timestamp;

use crate::error::SignalsError;
use crate::event::SecurityEvent;
use crate::sample::HostSample;

/// Wie hart ein Nachweis ist.
///
/// # Description
/// Ordnung nach steigender epistemischer Distanz vom direkt Beobachteten:
/// `Observed` (die Beobachtung selbst, keine Verknüpfung nötig) ist die
/// direkteste und daher kleinste Stufe; `Correlated` verknüpft mehrere
/// Beobachtungen miteinander; `Inferred` zieht einen Schluss, der über die
/// vorliegenden Beobachtungen hinausgeht. `Observed < Correlated <
/// Inferred` — die Ordnung stimmt mit dieser Bedeutung überein: je größer
/// der Wert, desto indirekter der Nachweis.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::Hardness;
///
/// assert!(Hardness::Observed < Hardness::Correlated);
/// assert!(Hardness::Correlated < Hardness::Inferred);
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Hardness {
    /// Direkt beobachtet, ohne Verknüpfung oder Schlussfolgerung.
    Observed,
    /// Aus mehreren Beobachtungen verknüpft.
    Correlated,
    /// Über die Beobachtungen hinaus geschlossen.
    Inferred,
}

/// Wie schwer ein Befund wiegt.
///
/// # Description
/// Ordnung nach steigendem Gewicht: `Info < Low < Medium < High < Critical`.
/// Die Ordnung stimmt mit der Bedeutung überein — ein höherer Wert bedeutet
/// unmittelbar ein schwereres Gewicht, ohne Sonderfälle.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::Severity;
///
/// assert!(Severity::Info < Severity::Critical);
/// assert!(Severity::Medium < Severity::High);
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Rein informativ, kein Handlungsbedarf.
    Info,
    /// Geringes Gewicht.
    Low,
    /// Mittleres Gewicht.
    Medium,
    /// Hohes Gewicht.
    High,
    /// Kritisch — höchstes Gewicht.
    Critical,
}

/// Eingefrorene Beobachtungen als Beleg.
///
/// # Description
/// Siehe Moduldoku für die verbindliche Digest-Bildungsregel. Felder sind
/// `pub`, weil ein aus Speicher oder Wire deserialisierter Beleg seinen
/// `digest` bereits mitbringt und ihn nicht neu berechnen soll — die Regel
/// gilt für die *Erzeugung* über [`SecurityEvidence::capture`], nicht für
/// jedes Lesen.
///
/// # Errors
/// Keine eigenen Fehler bei reinem Datenzugriff; siehe
/// [`SecurityEvidence::capture`] für den einzigen fehlschlagbaren Pfad.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::SecurityEvidence;
///
/// let evidence = SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
///     .expect("empty evidence always encodes");
/// assert_eq!(evidence.events.len(), 0);
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityEvidence {
    /// Digest über `samples` und `events` in Erfassungsreihenfolge (siehe
    /// Moduldoku für die Bildungsregel).
    pub digest: harw_types::ContentDigest,
    /// Wann der Beleg eingefroren wurde.
    pub captured_at: Timestamp,
    /// Die eingefrorenen Messwerte, in Erfassungsreihenfolge.
    pub samples: Vec<HostSample>,
    /// Die eingefrorenen Ereignisse, in Erfassungsreihenfolge.
    pub events: Vec<SecurityEvent>,
}

/// Interne, ausschließlich für die Digestbildung serialisierte Hülle.
///
/// Kein `pub`: ihr einziger Zweck ist, `samples` und `events` gemeinsam und
/// ordnungserhaltend zu kodieren, ohne `SecurityEvidence` selbst (das schon
/// ein `digest`-Feld trägt, das ja erst berechnet werden soll) zu
/// serialisieren.
#[derive(serde::Serialize)]
struct DigestPayload<'a> {
    samples: &'a [HostSample],
    events: &'a [SecurityEvent],
}

impl SecurityEvidence {
    /// Bildet einen Beleg aus Samples und Events und berechnet seinen Digest.
    ///
    /// # Description
    /// Der einzige Konstruktionsweg, der die Bildungsregel aus der
    /// Moduldoku durchsetzt: `digest` wird hier berechnet, nie vom Aufrufer
    /// übergeben. `samples` und `events` werden unverändert und in der
    /// übergebenen Reihenfolge übernommen — diese Reihenfolge bestimmt den
    /// Digest.
    ///
    /// # Arguments
    /// - `samples` (`Vec<HostSample>`): die einzufrierenden Messwerte, in
    ///   der Reihenfolge, die den Digest bestimmt.
    /// - `events` (`Vec<SecurityEvent>`): die einzufrierenden Ereignisse, in
    ///   der Reihenfolge, die den Digest bestimmt.
    /// - `captured_at` (`jiff::Timestamp`): injizierte Erfassungszeit.
    ///
    /// # Returns
    /// Ein `SecurityEvidence` mit korrekt gebildetem `digest`.
    ///
    /// # Errors
    /// - [`SignalsError::DigestEncoding`]: die kanonische JSON-Kodierung von
    ///   `samples` und `events` ist fehlgeschlagen.
    ///
    /// # Concurrency
    /// Reine Funktion ohne geteilten Zustand, sicher aus jedem Thread
    /// aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_signals::SecurityEvidence;
    ///
    /// let a = SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
    ///     .expect("empty evidence always encodes");
    /// let b = SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
    ///     .expect("empty evidence always encodes");
    /// assert_eq!(a.digest, b.digest);
    /// ```
    pub fn capture(
        samples: Vec<HostSample>,
        events: Vec<SecurityEvent>,
        captured_at: Timestamp,
    ) -> Result<Self, SignalsError> {
        let digest = Self::digest_of(&samples, &events)?;
        Ok(Self {
            digest,
            captured_at,
            samples,
            events,
        })
    }

    /// Berechnet den Digest für eine gegebene Samples-/Events-Reihenfolge.
    ///
    /// Privater Helfer hinter [`SecurityEvidence::capture`] — siehe
    /// Moduldoku für die Bildungsregel, die dieser Helfer umsetzt.
    fn digest_of(
        samples: &[HostSample],
        events: &[SecurityEvent],
    ) -> Result<harw_types::ContentDigest, SignalsError> {
        let bytes = serde_json::to_vec(&DigestPayload { samples, events })?;
        Ok(harw_types::ContentDigest::of(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_types::SensorId;

    /// 64 Hex-Zeichen — ein syntaktisch gültiger, inhaltlich beliebiger
    /// Digest für Fixtures, die keinen echten `ContentDigest::of`-Aufruf
    /// brauchen.
    const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn sample(metric: &'static str, value: f64) -> HostSample {
        HostSample {
            sensor: SensorId::from_str("thermal-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: std::borrow::Cow::Borrowed(metric),
            value,
        }
    }

    #[test]
    fn test_hardness_ordering_matches_meaning() {
        assert!(Hardness::Observed < Hardness::Correlated);
        assert!(Hardness::Correlated < Hardness::Inferred);
    }

    #[test]
    fn test_severity_ordering_matches_meaning() {
        let ascending = [
            Severity::Info,
            Severity::Low,
            Severity::Medium,
            Severity::High,
            Severity::Critical,
        ];
        for pair in ascending.windows(2) {
            assert!(pair[0] < pair[1]);
        }
    }

    #[test]
    fn test_capture_digest_is_deterministic_for_same_order() -> TestResult {
        let samples = vec![
            sample("cpu_util_percent", 1.0),
            sample("mem_used_bytes", 2.0),
        ];
        let a = SecurityEvidence::capture(samples.clone(), vec![], Timestamp::UNIX_EPOCH)
            .map_err(ctx("capture succeeds"))?;
        let b = SecurityEvidence::capture(samples, vec![], Timestamp::UNIX_EPOCH)
            .map_err(ctx("capture succeeds"))?;
        assert_eq!(a.digest, b.digest);
        Ok(())
    }

    #[test]
    fn test_capture_digest_differs_for_reordered_samples() -> TestResult {
        let first = sample("cpu_util_percent", 1.0);
        let second = sample("mem_used_bytes", 2.0);

        let forward = SecurityEvidence::capture(
            vec![first.clone(), second.clone()],
            vec![],
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("capture succeeds"))?;
        let reversed =
            SecurityEvidence::capture(vec![second, first], vec![], Timestamp::UNIX_EPOCH)
                .map_err(ctx("capture succeeds"))?;

        assert_ne!(forward.digest, reversed.digest);
        Ok(())
    }

    #[test]
    fn test_capture_preserves_samples_and_events_unchanged() -> TestResult {
        let samples = vec![sample("cpu_util_percent", 1.0)];
        let evidence = SecurityEvidence::capture(samples.clone(), vec![], Timestamp::UNIX_EPOCH)
            .map_err(ctx("capture succeeds"))?;
        assert_eq!(evidence.samples, samples);
        assert_eq!(evidence.captured_at, Timestamp::UNIX_EPOCH);
        Ok(())
    }

    #[test]
    fn test_security_evidence_deserialize_accepts_well_formed_static_fixture() -> TestResult {
        let fixture: &'static str = r#"{
            "digest": "0000000000000000000000000000000000000000000000000000000000000000",
            "captured_at": "1970-01-01T00:00:00Z",
            "samples": [],
            "events": []
        }"#;
        let parsed: SecurityEvidence =
            serde_json::from_str(fixture).map_err(ctx("fixture deserializes"))?;
        assert_eq!(parsed.digest.to_string(), ZERO_DIGEST);
        assert_eq!(parsed.samples.len(), 0);
        assert_eq!(parsed.events.len(), 0);
        Ok(())
    }

    #[test]
    fn test_security_evidence_deserialize_rejects_unknown_field() {
        let fixture: &'static str = r#"{
            "digest": "0000000000000000000000000000000000000000000000000000000000000000",
            "captured_at": "1970-01-01T00:00:00Z",
            "samples": [],
            "events": [],
            "unexpected": true
        }"#;
        assert!(serde_json::from_str::<SecurityEvidence>(fixture).is_err());
    }
}
