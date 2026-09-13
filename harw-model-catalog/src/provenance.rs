//! Provenance-Typen für den Harw Model Catalog (Wave B Umbau).
//!
//! # Zweck
//! Dieses Modul trennt strikt zwischen der **Herkunft von Policies**
//! ([`PolicyOrigin`]) und dem **Zustand empirischer Messwerte**
//! ([`EvidenceState`], [`MetricEstimate`]).  Es beseitigt den
//! semantischen Missbrauch von `Score(50)` als „neutralem Bootstrap",
//! der wie ein gemessener Wert aussah.
//!
//! Gemäß `coding-philosophy.md §17` (Provider-Reality durch Abstraktion)
//! und §16 (Dependencies mit Evidenz).
//!
//! # Exportierte Typen
//! - [`PolicyOrigin`] — Herkunft einer Policy oder eines Fähigkeits-Eintrags.
//! - [`PrimarySource`] — URL-basierte Primärquelle mit optionalem Fingerprint.
//! - [`EvidenceState`] — Zustand einer empirischen Messung.
//! - [`Confidence`] — Grob-quantifizierte Konfidenz einer Aussage.
//! - [`MetricEstimate`] — Schätzwert mit ehrlicher Evidenz-Angabe.
//!
//! # Nebenläufigkeit
//! Alle Typen sind reine Wert-Typen ohne inneren Zustand und implementieren
//! `Send + Sync` durch ihre Felder (ausschließlich `String`, `u8`, `u32`,
//! `Option<OffsetDateTime>`, und `Copy`-Enums).
//!
//! # Fehler
//! Dieses Modul erzeugt keine eigenen Fehlertypen; Konstruktoren klemmen
//! Werte oder geben direkt einen validen Typ zurück.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_model_catalog::provenance::{MetricEstimate, Confidence};
//! use time::OffsetDateTime;
//!
//! // Ehrlicher Zustand: noch nie gemessen.
//! let unbekannt = MetricEstimate::unmeasured();
//! assert!(unbekannt.value.is_none());
//! assert!(!unbekannt.is_reliable());
//!
//! // Echte Messung mit ausreichend Samples.
//! let gemessen = MetricEstimate::measured(
//!     82, 20, Confidence::High, OffsetDateTime::now_utc()
//! );
//! assert!(gemessen.is_reliable());
//! ```

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// ─────────────────────────────────────────────────────────────────────────────
// PolicyOrigin
// ─────────────────────────────────────────────────────────────────────────────

/// Woher stammt eine Policy oder ein Fähigkeits-Eintrag?
///
/// # Beschreibung
/// Trennt klar zwischen harw-eigenen Defaults, Operator-Konfiguration,
/// Provider-Dokumentation und echten Harness-Evaluationen.
/// Kein Eintrag kann „neutral" oder „unbekannt" sein — jede Policy
/// hat eine nachvollziehbare Herkunft.
///
/// Gemäß `coding-philosophy.md §17`: Provider-spezifische Realität muss
/// durch Abstraktion erhalten bleiben; Policies dürfen nicht als Messwerte
/// maskiert werden.
///
/// # Serialisierung
/// Tagged union mit `"kind"`-Feld, snake_case-Varianten:
/// `"conservative_default"`, `"operator_configured"`,
/// `"provider_guidance"`, `"evaluation_derived"`.
///
/// # Nebenläufigkeit
/// Reine Wert-Typen; `Send + Sync`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::provenance::PolicyOrigin;
/// let p = PolicyOrigin::ConservativeDefault;
/// let json = serde_json::to_string(&p).unwrap();
/// assert!(json.contains("conservative_default"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyOrigin {
    /// Harw-eigener konservativer Default, kein externer Anspruch.
    ConservativeDefault,
    /// Vom Operator per Config gesetzt.
    OperatorConfigured {
        /// Bezeichner des Operators (z. B. Hostname oder Tenant-ID).
        operator: String,
    },
    /// Aus Provider-Dokumentation extrahiert.
    ProviderGuidance {
        /// Primärquelle der Dokumentation.
        source: PrimarySource,
        /// Zeitpunkt der Verifikation (RFC 3339).
        #[serde(with = "time::serde::rfc3339")]
        verified_at: OffsetDateTime,
    },
    /// Aus echten Harness-Evaluationen abgeleitet.
    EvaluationDerived {
        /// Name der Evaluations-Suite.
        suite: String,
        /// Anzahl der ausgewerteten Samples.
        sample_size: u32,
        /// Zeitpunkt der Berechnung (RFC 3339).
        #[serde(with = "time::serde::rfc3339")]
        computed_at: OffsetDateTime,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// PrimarySource
// ─────────────────────────────────────────────────────────────────────────────

/// Primärquelle mit URL + optionalem Hash/Titel für Nachvollziehbarkeit.
///
/// # Beschreibung
/// Kapselt eine Dokumentations-URL zusammen mit optionalem Titel und
/// optionalem SHA-256-Fingerprint der abgerufenen Ressource. Damit ist
/// eine Policy-Herkunft auch nach Link-Rot reproduzierbar.
///
/// Gemäß `coding-philosophy.md §16`: Dependencies (hier: externe Quellen)
/// werden mit Evidenz belegt.
///
/// # Nebenläufigkeit
/// `String`-Felder; `Send + Sync`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::provenance::PrimarySource;
/// let src = PrimarySource {
///     url: "https://docs.example.com/limits".to_owned(),
///     title: Some("Rate Limits".to_owned()),
///     content_sha256: None,
/// };
/// assert_eq!(src.url, "https://docs.example.com/limits");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimarySource {
    /// Vollständige URL der Quelle.
    pub url: String,
    /// Optionaler menschenlesbarer Titel der Seite.
    pub title: Option<String>,
    /// Optionaler Fingerprint (SHA-256 hex der abgerufenen Ressource), falls verfügbar.
    pub content_sha256: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// EvidenceState
// ─────────────────────────────────────────────────────────────────────────────

/// Zustand einer empirischen Messung.
///
/// # Beschreibung
/// Gibt ehrlich an, ob und in welcher Qualität eine Messung vorliegt.
/// Kein Placeholder-Wert wie `Score(50)` ist nötig; stattdessen
/// drückt `Unmeasured` aus, dass noch kein Datenpunkt vorliegt.
///
/// # Übergänge
/// `Unmeasured` → `Insufficient` → `Measured` → `Stale`
/// (via [`MetricEstimate::mark_stale`]).
///
/// # Nebenläufigkeit
/// `Copy`-Enum; `Send + Sync`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::provenance::EvidenceState;
/// assert_ne!(EvidenceState::Measured, EvidenceState::Stale);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    /// Noch nie gemessen.
    Unmeasured,
    /// Gemessen, aber Sample-Größe unter dem Schwellwert.
    Insufficient,
    /// Ausreichend gemessen.
    Measured,
    /// Gemessen, aber älter als der Staleness-Threshold.
    Stale,
}

// ─────────────────────────────────────────────────────────────────────────────
// Confidence
// ─────────────────────────────────────────────────────────────────────────────

// Umgezogen nach `harw_types::Confidence` (Knoten AW0-03b), zu Ende geführt
// als Fortsetzung der ausführlichen Analyse, die Knoten AW2-20 an dieser
// Stelle hinterlassen hat: dieser Typ war formgleich (Varianten, Reihenfolge,
// Derives, Serde-Form) zu `harw_memory::epistemic::Confidence` — ein
// Duplikat, kein ehrlich getrenntes Paar nach der Regel „pro Vokabularpaar
// genau ein Symbol". Knoten AW2-20 beließ beide Definitionen bewusst
// eigenständig, weil `harw_types::Confidence` noch nicht existierte und ein
// halb durchgeführter Umzug der schlechtere Zustand gewesen wäre. Dieser
// Knoten hat `harw_types::Confidence` angelegt und führt den Umzug hier zu
// Ende. Reexport unter dem historischen Pfad, damit alle bestehenden
// Verwendungen (`crate::provenance::Confidence`) unverändert auflösen. Die
// vollständige Aufzählung aller `Confidence`-Typen im Baum — sie verhindert
// einen fünften — ist umgezogen in die Moduldokumentation von
// `harw_types::confidence`; lege hier keine lokale Definition wieder an.
pub use harw_types::Confidence;

// ─────────────────────────────────────────────────────────────────────────────
// MetricEstimate
// ─────────────────────────────────────────────────────────────────────────────

/// Ein Schätzwert, der ehrlich angibt, ob und wie viele Messungen dahinterstehen.
///
/// # Beschreibung
/// `value = None` bedeutet: nicht gemessen. Es gibt keinen
/// „neutralen 50"-Placeholder mehr. Jede Instanz trägt ihren
/// [`EvidenceState`] offen nach außen.
///
/// Konstruiert über:
/// - [`MetricEstimate::unmeasured`]: expliziter Null-Zustand.
/// - [`MetricEstimate::measured`]: echte Messung mit Sample-Anzahl.
///
/// # Felder
/// - `value` — 0..=100 Skala; `None` wenn nie gemessen.
/// - `confidence` — Konfidenz-Stufe.
/// - `samples` — Anzahl der zugrunde liegenden Messungen.
/// - `state` — Aktueller Evidenz-Zustand.
/// - `last_updated` — Zeitpunkt der letzten Messung; `None` wenn nie gemessen.
///
/// # Nebenläufigkeit
/// Reine Wert-Typen (kein innerer Mutex, kein Rc); `Send + Sync`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::provenance::{MetricEstimate, Confidence, EvidenceState};
/// use time::OffsetDateTime;
///
/// let u = MetricEstimate::unmeasured();
/// assert_eq!(u.state, EvidenceState::Unmeasured);
///
/// let m = MetricEstimate::measured(75, 10, Confidence::High, OffsetDateTime::now_utc());
/// assert!(m.is_reliable());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricEstimate {
    /// 0..=100 Skala; `None`, wenn nie gemessen.
    pub value: Option<u8>,
    /// Grob-quantifizierte Konfidenz der Aussage.
    pub confidence: Confidence,
    /// Anzahl der Messungen, die diesem Schätzwert zugrunde liegen.
    pub samples: u32,
    /// Aktueller Zustand der Evidenz.
    pub state: EvidenceState,
    /// Letzter Messzeitpunkt; `None`, wenn nie gemessen.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_updated: Option<OffsetDateTime>,
}

impl MetricEstimate {
    /// Ehrlicher „nichts gewusst"-Zustand ohne jede Messung.
    ///
    /// # Beschreibung
    /// Ersetzt den alten `Score(50)`-Bootstrap-Placeholder. Der zurückgegebene
    /// Wert signalisiert dem Harness: „Es wurde noch kein Datenpunkt erhoben."
    ///
    /// # Rückgabe
    /// `MetricEstimate` mit `value = None`, `samples = 0`,
    /// `confidence = VeryLow`, `state = Unmeasured`, `last_updated = None`.
    ///
    /// # Nebenläufigkeit
    /// `const`-Funktion; vollständig deterministisch.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::provenance::{MetricEstimate, EvidenceState};
    /// let u = MetricEstimate::unmeasured();
    /// assert_eq!(u.state, EvidenceState::Unmeasured);
    /// assert!(u.value.is_none());
    /// ```
    pub const fn unmeasured() -> Self {
        Self {
            value: None,
            confidence: Confidence::VeryLow,
            samples: 0,
            state: EvidenceState::Unmeasured,
            last_updated: None,
        }
    }

    /// Aufgezeichnete Messung mit N Samples.
    ///
    /// # Beschreibung
    /// Konstruiert einen `MetricEstimate` aus einer echten Messung.
    /// Werte > 100 werden auf 100 gesättigt. Bei weniger als 5 Samples
    /// wird der State auf `Insufficient` gesetzt, sonst auf `Measured`.
    ///
    /// # Argumente
    /// - `value` (`u8`): Rohwert 0..=255; wird auf 100 gedeckelt.
    /// - `samples` (`u32`): Anzahl der Messungen. < 5 → `Insufficient`.
    /// - `confidence` (`Confidence`): Konfidenz-Stufe.
    /// - `at` (`OffsetDateTime`): Zeitstempel der Messung.
    ///
    /// # Rückgabe
    /// `MetricEstimate` mit gesättigtem `value` und korrekt gesetztem `state`.
    ///
    /// # Nebenläufigkeit
    /// Keine Seiteneffekte; rein deterministisch.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::provenance::{MetricEstimate, Confidence, EvidenceState};
    /// use time::OffsetDateTime;
    ///
    /// let m = MetricEstimate::measured(200, 10, Confidence::High, OffsetDateTime::now_utc());
    /// assert_eq!(m.value, Some(100));
    /// assert_eq!(m.state, EvidenceState::Measured);
    ///
    /// let insuf = MetricEstimate::measured(80, 3, Confidence::Low, OffsetDateTime::now_utc());
    /// assert_eq!(insuf.state, EvidenceState::Insufficient);
    /// ```
    pub fn measured(value: u8, samples: u32, confidence: Confidence, at: OffsetDateTime) -> Self {
        let clamped = if value > 100 { 100 } else { value };
        let state = if samples < 5 {
            EvidenceState::Insufficient
        } else {
            EvidenceState::Measured
        };
        Self {
            value: Some(clamped),
            confidence,
            samples,
            state,
            last_updated: Some(at),
        }
    }

    /// Markiert eine bestehende Messung als veraltet.
    ///
    /// # Beschreibung
    /// Setzt `state` auf `Stale`, wenn der aktuelle State `Measured` oder
    /// `Insufficient` ist. `Unmeasured` und bereits `Stale` bleiben unverändert.
    ///
    /// # Argumente
    /// `&mut self` — mutiert den Zustand in-place.
    ///
    /// # Nebenläufigkeit
    /// Erfordert exklusiven Zugriff (`&mut self`); keine Locks nötig.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::provenance::{MetricEstimate, Confidence, EvidenceState};
    /// use time::OffsetDateTime;
    ///
    /// let mut m = MetricEstimate::measured(70, 10, Confidence::Medium, OffsetDateTime::now_utc());
    /// m.mark_stale();
    /// assert_eq!(m.state, EvidenceState::Stale);
    /// ```
    pub fn mark_stale(&mut self) {
        if matches!(
            self.state,
            EvidenceState::Measured | EvidenceState::Insufficient
        ) {
            self.state = EvidenceState::Stale;
        }
    }

    /// Gibt zurück, ob der Schätzwert als zuverlässig gilt.
    ///
    /// # Beschreibung
    /// Eine Messung gilt als zuverlässig, wenn `state == Measured` **und**
    /// `value.is_some()`. `Insufficient`, `Stale` und `Unmeasured` gelten
    /// ausdrücklich nicht als zuverlässig.
    ///
    /// # Rückgabe
    /// `true` genau dann, wenn `state == Measured` und `value != None`.
    ///
    /// # Nebenläufigkeit
    /// Nur Lesezugriff; thread-safe.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::provenance::{MetricEstimate, Confidence};
    /// use time::OffsetDateTime;
    ///
    /// let u = MetricEstimate::unmeasured();
    /// assert!(!u.is_reliable());
    ///
    /// let m = MetricEstimate::measured(90, 15, Confidence::VeryHigh, OffsetDateTime::now_utc());
    /// assert!(m.is_reliable());
    /// ```
    pub fn is_reliable(&self) -> bool {
        matches!(self.state, EvidenceState::Measured) && self.value.is_some()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    // ── Test 1 ────────────────────────────────────────────────────────────────

    /// `ConservativeDefault` serialisiert als `{"kind":"conservative_default"}`.
    #[test]
    fn policy_origin_serde_kebab() {
        let origin = PolicyOrigin::ConservativeDefault;
        let json = serde_json::to_string(&origin).expect("Serialisierung fehlgeschlagen");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("JSON-Parse fehlgeschlagen");
        assert_eq!(parsed["kind"], "conservative_default");
    }

    // ── Test 2 ────────────────────────────────────────────────────────────────

    /// `PrimarySource` ohne optionale Felder deserialisiert korrekt.
    #[test]
    fn primary_source_optional_fields() {
        let json = r#"{"url":"https://example.com/docs"}"#;
        let src: PrimarySource =
            serde_json::from_str(json).expect("Deserialisierung fehlgeschlagen");
        assert_eq!(src.url, "https://example.com/docs");
        assert!(src.title.is_none());
        assert!(src.content_sha256.is_none());
    }

    // ── Test 3 ────────────────────────────────────────────────────────────────

    /// `EvidenceState::Unmeasured` übersteht einen JSON-Roundtrip.
    #[test]
    fn evidence_state_serde() {
        let state = EvidenceState::Unmeasured;
        let json = serde_json::to_string(&state).expect("Serialisierung fehlgeschlagen");
        let restored: EvidenceState =
            serde_json::from_str(&json).expect("Deserialisierung fehlgeschlagen");
        assert_eq!(state, restored);
        assert_eq!(json, "\"unmeasured\"");
    }

    // ── Test 4 ────────────────────────────────────────────────────────────────

    /// Ordnungsrelation: VeryLow < Low < Medium < High < VeryHigh.
    #[test]
    fn confidence_ordering() {
        assert!(Confidence::VeryLow < Confidence::Low);
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
        assert!(Confidence::High < Confidence::VeryHigh);
        assert!(Confidence::VeryLow < Confidence::VeryHigh);
    }

    // ── Test 5 ────────────────────────────────────────────────────────────────

    /// `unmeasured()` liefert value=None, samples=0, state=Unmeasured.
    #[test]
    fn metric_unmeasured_defaults() {
        let m = MetricEstimate::unmeasured();
        assert!(m.value.is_none());
        assert_eq!(m.samples, 0);
        assert_eq!(m.state, EvidenceState::Unmeasured);
        assert_eq!(m.confidence, Confidence::VeryLow);
        assert!(m.last_updated.is_none());
    }

    // ── Test 6 ────────────────────────────────────────────────────────────────

    /// `measured(150, …)` sättigt auf `value = Some(100)`.
    #[test]
    fn metric_measured_clamps_over_100() {
        let m = MetricEstimate::measured(150, 10, Confidence::High, OffsetDateTime::now_utc());
        assert_eq!(m.value, Some(100));
        assert_eq!(m.state, EvidenceState::Measured);
    }

    // ── Test 7 ────────────────────────────────────────────────────────────────

    /// `measured(…, samples=3, …)` → state = Insufficient.
    #[test]
    fn metric_measured_marks_insufficient_below_5_samples() {
        let m = MetricEstimate::measured(80, 3, Confidence::Low, OffsetDateTime::now_utc());
        assert_eq!(m.state, EvidenceState::Insufficient);
        assert_eq!(m.value, Some(80));
    }

    // ── Test 8 ────────────────────────────────────────────────────────────────

    /// `mark_stale`: Measured → Stale; Unmeasured bleibt Unmeasured.
    #[test]
    fn metric_mark_stale_transitions() {
        // Measured → Stale
        let mut m = MetricEstimate::measured(60, 10, Confidence::Medium, OffsetDateTime::now_utc());
        assert_eq!(m.state, EvidenceState::Measured);
        m.mark_stale();
        assert_eq!(m.state, EvidenceState::Stale);

        // Unmeasured bleibt Unmeasured
        let mut u = MetricEstimate::unmeasured();
        u.mark_stale();
        assert_eq!(u.state, EvidenceState::Unmeasured);

        // Stale bleibt Stale (idempotent)
        m.mark_stale();
        assert_eq!(m.state, EvidenceState::Stale);
    }

    // ── Test 9 ────────────────────────────────────────────────────────────────

    /// `is_reliable()` ist nur bei State=Measured+value=Some(…) true.
    #[test]
    fn metric_is_reliable_true_only_when_measured() {
        let now = OffsetDateTime::now_utc();

        // Unmeasured → false
        assert!(!MetricEstimate::unmeasured().is_reliable());

        // Insufficient → false
        let insuf = MetricEstimate::measured(70, 3, Confidence::Low, now);
        assert!(!insuf.is_reliable());

        // Measured → true
        let measured = MetricEstimate::measured(70, 10, Confidence::High, now);
        assert!(measured.is_reliable());

        // Stale → false
        let mut stale = MetricEstimate::measured(70, 10, Confidence::High, now);
        stale.mark_stale();
        assert!(!stale.is_reliable());
    }

    // ── Test 10 ───────────────────────────────────────────────────────────────

    /// `MetricEstimate` übersteht einen vollständigen JSON-Roundtrip.
    #[test]
    fn metric_serde_roundtrip() {
        let now = OffsetDateTime::now_utc();
        let original = MetricEstimate::measured(55, 20, Confidence::VeryHigh, now);
        let json = serde_json::to_string(&original).expect("Serialisierung fehlgeschlagen");
        let restored: MetricEstimate =
            serde_json::from_str(&json).expect("Deserialisierung fehlgeschlagen");
        assert_eq!(original.value, restored.value);
        assert_eq!(original.confidence, restored.confidence);
        assert_eq!(original.samples, restored.samples);
        assert_eq!(original.state, restored.state);
        // OffsetDateTime-Vergleich auf Sekunden-Granularität (RFC 3339 verliert Sub-Sekunden)
        assert_eq!(
            original.last_updated.map(|t| t.unix_timestamp()),
            restored.last_updated.map(|t| t.unix_timestamp())
        );
    }
}
