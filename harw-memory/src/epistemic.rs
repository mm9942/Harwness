//! Epistemische Typen für das Memory-Subsystem.
//!
//! # Verantwortungsbereich
//! Dieses Modul implementiert die epistemische Schicht aus
//! `coding-philosophy.md §18` (Context+Memory follow same discipline):
//! Provenance, Confidence, Contradiction, Correction, Scope, Validity, Outcome.
//!
//! Promotion ist rücknehmbar: Beobachtung → Hypothese → Evidenz →
//! begrenzte Lesson → Outcome-Prüfung → bestätigen / korrigieren / verwerfen.
//!
//! # Schlüsseltypen
//! - [`Provenance`] — Herkunft einer Behauptung (User, Agent, extern).
//! - [`Confidence`] — Geordnete Konfidenz-Stufe (VeryLow..VeryHigh).
//! - [`Validity`] — Zeitliche Gültigkeit (dauerhaft / zeitgebunden / abgelaufen).
//! - [`MemoryScope`] — Geltungsbereich (UserGlobal, Project, Agent, Repo, Session).
//! - [`OutcomeVerdict`] — Ergebnis einer Outcome-Prüfung (Confirmed/Inconclusive/Refuted).
//! - [`EpistemicSignal`] — Memory-Eintrag mit vollständiger epistemischer Herkunft.
//! - [`PromotionScore`] — Zusammengesetzter Score für Promotion-Entscheidung.
//!
//! # Nebenläufigkeit
//! Alle Typen sind reine Wert-Typen. Sie implementieren `Send + Sync` automatisch
//! und benötigen keine eigene Synchronisation. Der Aufrufer ist für Locking
//! verantwortlich, wenn Instanzen über Threads hinweg mutiert werden.
//!
//! # Fehler
//! Dieses Modul produziert keine eigenen Fehler. Serde-Fehler entstehen
//! beim Deserialisieren und werden an den Aufrufer propagiert.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_memory::epistemic::{
//!     EpistemicSignal, Provenance, Confidence, Validity, MemoryScope,
//! };
//! use time::OffsetDateTime;
//!
//! let mut sig = EpistemicSignal {
//!     id: "sig-1".to_owned(),
//!     statement: "Immer clippy -D warnings nutzen.".to_owned(),
//!     origin: Provenance::UserCorrection {
//!         session_id: "s42".to_owned(),
//!         replaces: None,
//!     },
//!     confidence: Confidence::High,
//!     validity: Validity::Permanent,
//!     scope: MemoryScope::UserGlobal,
//!     contradictions: vec![],
//!     outcome: None,
//!     created_at: OffsetDateTime::now_utc(),
//!     last_used: None,
//!     independent_confirmations: 2,
//!     salience: 80,
//! };
//! sig.confirm();
//! let score = sig.promotion_score();
//! assert!(score.should_promote());
//! ```

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// ─────────────────────────────────────────────────────────────────────────────
// Provenance
// ─────────────────────────────────────────────────────────────────────────────

/// Herkunft einer Memory-Behauptung.
///
/// # Beschreibung
/// Jede Behauptung im epistemischen Speicher muss rückverfolgbar sein.
/// Die Variante bestimmt sowohl das Vertrauen als auch das Gewicht bei der
/// Promotion: `UserCorrection` ist gewichtiger als `AgentObservation`.
///
/// # Serialisierung
/// Serde-Tag `"kind"` mit snake_case-Varianten, z. B. `"user_correction"`.
///
/// # Concurrency
/// Reiner Wert-Typ, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// Vom User explizit ausgesprochen.
    UserStatement {
        /// Session-ID des auslösenden Gesprächs.
        session_id: String,
    },
    /// Vom User explizit korrigiert (höheres Gewicht als `UserStatement`).
    UserCorrection {
        /// Session-ID des auslösenden Gesprächs.
        session_id: String,
        /// Optionale ID der ersetzten Behauptung.
        replaces: Option<String>,
    },
    /// Vom Agenten aus Beobachtung abgeleitet.
    AgentObservation {
        /// Task-ID des auslösenden Turns.
        task_id: String,
    },
    /// Vom Agenten nach abgeschlossenem Turn reflektiert.
    AgentReflection {
        /// Task-ID des auslösenden Turns.
        task_id: String,
    },
    /// Aus einem externen Skill/Tool/Import übernommen.
    ExternalImport {
        /// Bezeichner der Quelle (z. B. Skill-Name, URL, Dateiname).
        source: String,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Confidence
// ─────────────────────────────────────────────────────────────────────────────

// Umgezogen nach `harw_types::Confidence` (Knoten AW0-03b): dieser Typ war
// formgleich (Varianten, Reihenfolge, Derives, Serde-Form) zu
// `harw_model_catalog::provenance::Confidence` — ein Duplikat, kein
// ehrlich getrenntes Paar nach der Regel „pro Vokabularpaar genau ein
// Symbol". Reexport unter dem historischen Pfad, damit alle bestehenden
// Verwendungen (`crate::epistemic::Confidence`) unverändert auflösen.
// Lege hier keine lokale Definition wieder an — Inventar aller
// `Confidence`-Typen im Baum steht in `harw_types::confidence`.
pub use harw_types::Confidence;

// ─────────────────────────────────────────────────────────────────────────────
// Validity
// ─────────────────────────────────────────────────────────────────────────────

/// Zeitliche Gültigkeit einer Memory-Behauptung.
///
/// # Beschreibung
/// - `Permanent` — dauerhaft gültig, kein Ablauf.
/// - `ExpiresAt` — gültig bis zu einem bestimmten Zeitpunkt.
/// - `Expired` — abgelaufen; sollte nicht mehr aktiv promoviert werden.
///
/// Zur Laufzeit: [`EpistemicSignal::is_expired`] vergleicht `ExpiresAt`
/// gegen einen übergebenen `now`-Wert, ohne `OffsetDateTime::now_utc()`
/// intern aufzurufen (testbar).
///
/// # Serialisierung
/// Serde-Tag `"kind"`, RFC-3339-kodierte Zeitstempel.
///
/// # Concurrency
/// Reiner Wert-Typ, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Validity {
    /// Dauerhaft gültig — kein Ablauf.
    Permanent,
    /// Gültig bis `at` (UTC, RFC 3339).
    ExpiresAt {
        /// Ablaufzeitpunkt.
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
    },
    /// Bereits abgelaufen.
    Expired,
}

// ─────────────────────────────────────────────────────────────────────────────
// MemoryScope
// ─────────────────────────────────────────────────────────────────────────────

/// Geltungsbereich einer Memory-Behauptung.
///
/// # Beschreibung
/// Definiert, welcher Kontext eine Behauptung trägt. Schmälere Scopes
/// (Session, Repo) sind flüchtiger; `UserGlobal` ist der breiteste Scope.
///
/// # Serialisierung
/// Serde-Tag `"kind"`, snake_case-Varianten.
///
/// # Concurrency
/// Reiner Wert-Typ, `Send + Sync`. Implementiert `Hash` für Map-Keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryScope {
    /// Gilt für den User global — kein Projektbezug.
    UserGlobal,
    /// Gilt für ein bestimmtes Projekt.
    Project {
        /// Projekt-ID oder -Name.
        id: String,
    },
    /// Gilt für einen bestimmten Agenten.
    Agent {
        /// Agenten-ID.
        id: String,
    },
    /// Gilt für ein bestimmtes Repository (Pfad-basiert).
    Repo {
        /// Absoluter oder relativer Pfad des Repos.
        path: String,
    },
    /// Gilt nur für eine einzelne Session.
    Session {
        /// Session-ID.
        id: String,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// OutcomeVerdict
// ─────────────────────────────────────────────────────────────────────────────

/// Ergebnis einer Outcome-Prüfung.
///
/// # Beschreibung
/// Wird nach Anwendung einer Strategie eingetragen. Beeinflusst den
/// `outcome_value` im [`PromotionScore`]: `Refuted` zieht stark ab,
/// `Confirmed` hebt an, `Inconclusive` ist neutral.
///
/// # Serialisierung
/// snake_case-Varianten.
///
/// # Concurrency
/// `Copy`-Typ, `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeVerdict {
    /// Strategie hat gewirkt — Bestätigung.
    Confirmed,
    /// Neutral oder nicht messbar.
    Inconclusive,
    /// Strategie hat nicht gewirkt — Widerlegung.
    Refuted,
}

// ─────────────────────────────────────────────────────────────────────────────
// EpistemicSignal
// ─────────────────────────────────────────────────────────────────────────────

/// Ein Memory-Eintrag mit epistemischer Herkunft.
///
/// # Beschreibung
/// Kapselt eine Einzelaussage (`statement`) mit vollständiger epistemischer
/// Metadaten: Herkunft, Konfidenz, Gültigkeit, Geltungsbereich, Widersprüche,
/// Outcome und Nutzungsstatistiken.
///
/// `contradictions` enthält IDs anderer `EpistemicSignal`-Einträge, die dieser
/// Aussage widersprechen. Damit ist Konflikt-Erkennung ohne globale Suche möglich.
///
/// # Promotion-Pipeline
/// `Beobachtung → Hypothese → Evidenz → Lesson → Outcome → bestätigen/korrigieren/verwerfen`
///
/// Promotion ist rücknehmbar: [`OutcomeVerdict::Refuted`] führt via
/// [`PromotionScore::should_demote`] zur Demotion.
///
/// # Serialisierung
/// RFC-3339-Zeitstempel für `created_at` und `last_used`.
///
/// # Concurrency
/// Reiner Wert-Typ, `Send + Sync`. Der Aufrufer ist für Locking verantwortlich.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpistemicSignal {
    /// Eindeutiger Bezeichner dieses Signals.
    pub id: String,
    /// Die epistemische Behauptung im Klartext.
    pub statement: String,
    /// Herkunft der Behauptung.
    pub origin: Provenance,
    /// Konfidenz-Stufe.
    pub confidence: Confidence,
    /// Zeitliche Gültigkeit.
    pub validity: Validity,
    /// Geltungsbereich.
    pub scope: MemoryScope,
    /// IDs widersprechender Signale (leer = kein bekannter Widerspruch).
    #[serde(default)]
    pub contradictions: Vec<String>,
    /// Outcome nach Anwendung der Strategie.
    #[serde(default)]
    pub outcome: Option<OutcomeVerdict>,
    /// Zeitpunkt der Erstellung (UTC, RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Zeitpunkt des letzten Abrufs (UTC, RFC 3339), `None` = nie abgerufen.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_used: Option<OffsetDateTime>,
    /// Anzahl unabhängiger Bestätigungen aus verschiedenen Quellen.
    #[serde(default)]
    pub independent_confirmations: u32,
    /// Salience-Score 0..=100 (höher = relevanter).
    #[serde(default)]
    pub salience: u8,
}

impl EpistemicSignal {
    /// Berechnet einen [`PromotionScore`] aus dem aktuellen Signal-Zustand.
    ///
    /// # Beschreibung
    /// Bildet alle epistemischen Felder auf skalare Score-Komponenten ab:
    /// - `salience` — direkt übernommen (0..=100).
    /// - `independent_confirmations` — direkt übernommen.
    /// - `correction_weight` — 30 bei `UserCorrection`, sonst 0.
    /// - `outcome_value` — +30 (Confirmed), 0 (Inconclusive/None), -50 (Refuted).
    /// - `contradiction_penalty` — `min(60, contradictions.len() * 15)`.
    /// - `staleness_penalty` — 20 bei `Validity::Expired`, sonst 0.
    /// - `uncertainty_penalty` — VeryLow→40, Low→20, Medium→10, High/VeryHigh→0.
    ///
    /// # Rückgabe
    /// [`PromotionScore`] — kann über [`PromotionScore::total`] ausgewertet werden.
    ///
    /// # Concurrency
    /// Nur lesender Zugriff auf `self`; thread-safe.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use harw_memory::epistemic::*;
    /// # use time::OffsetDateTime;
    /// let sig = EpistemicSignal {
    ///     id: "x".to_owned(), statement: "s".to_owned(),
    ///     origin: Provenance::UserCorrection { session_id: "s1".to_owned(), replaces: None },
    ///     confidence: Confidence::High, validity: Validity::Permanent,
    ///     scope: MemoryScope::UserGlobal, contradictions: vec![], outcome: None,
    ///     created_at: OffsetDateTime::now_utc(), last_used: None,
    ///     independent_confirmations: 3, salience: 70,
    /// };
    /// let score = sig.promotion_score();
    /// assert_eq!(score.correction_weight, 30);
    /// ```
    #[must_use]
    pub fn promotion_score(&self) -> PromotionScore {
        let correction_weight: u8 = match &self.origin {
            Provenance::UserCorrection { .. } => 30,
            _ => 0,
        };

        let outcome_value: i8 = match self.outcome {
            Some(OutcomeVerdict::Confirmed) => 30,
            Some(OutcomeVerdict::Refuted) => -50,
            Some(OutcomeVerdict::Inconclusive) | None => 0,
        };

        let contradiction_penalty: u8 =
            (self.contradictions.len().saturating_mul(15)).min(60) as u8;

        let staleness_penalty: u8 = match &self.validity {
            Validity::Expired => 20,
            _ => 0,
        };

        let uncertainty_penalty: u8 = match self.confidence {
            Confidence::VeryLow => 40,
            Confidence::Low => 20,
            Confidence::Medium => 10,
            Confidence::High | Confidence::VeryHigh => 0,
        };

        PromotionScore {
            salience: self.salience,
            independent_confirmations: self.independent_confirmations,
            correction_weight,
            outcome_value,
            contradiction_penalty,
            staleness_penalty,
            uncertainty_penalty,
        }
    }

    /// Gibt `true` zurück, wenn das Signal abgelaufen ist.
    ///
    /// # Beschreibung
    /// Gibt `true` genau dann, wenn:
    /// - `validity == Validity::Expired`, oder
    /// - `validity == Validity::ExpiresAt { at }` und `at <= now`.
    ///
    /// # Argumente
    /// - `now` ([`OffsetDateTime`]): Referenzzeitpunkt (UTC); wird nicht intern
    ///   abgerufen, um Testbarkeit zu garantieren.
    ///
    /// # Rückgabe
    /// `true` wenn abgelaufen, `false` sonst.
    ///
    /// # Concurrency
    /// Nur lesender Zugriff; thread-safe.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use harw_memory::epistemic::*;
    /// # use time::OffsetDateTime;
    /// # use time::Duration;
    /// let past = OffsetDateTime::now_utc() - Duration::days(1);
    /// let sig = EpistemicSignal {
    ///     id: "x".to_owned(), statement: "s".to_owned(),
    ///     origin: Provenance::AgentObservation { task_id: "t1".to_owned() },
    ///     confidence: Confidence::Medium, validity: Validity::ExpiresAt { at: past },
    ///     scope: MemoryScope::UserGlobal, contradictions: vec![], outcome: None,
    ///     created_at: OffsetDateTime::now_utc(), last_used: None,
    ///     independent_confirmations: 0, salience: 50,
    /// };
    /// assert!(sig.is_expired(OffsetDateTime::now_utc()));
    /// ```
    #[must_use]
    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        match &self.validity {
            Validity::Expired => true,
            Validity::ExpiresAt { at } => *at <= now,
            Validity::Permanent => false,
        }
    }

    /// Trägt [`OutcomeVerdict::Confirmed`] als Outcome ein.
    ///
    /// # Beschreibung
    /// Convenience-Wrapper für `self.outcome = Some(OutcomeVerdict::Confirmed)`.
    /// Erhöht den `outcome_value` im nächsten [`PromotionScore`] um +30.
    ///
    /// # Concurrency
    /// Erfordert `&mut self`; der Aufrufer ist für Synchronisation verantwortlich.
    pub fn confirm(&mut self) {
        self.outcome = Some(OutcomeVerdict::Confirmed);
    }

    /// Trägt [`OutcomeVerdict::Refuted`] als Outcome ein.
    ///
    /// # Beschreibung
    /// Convenience-Wrapper für `self.outcome = Some(OutcomeVerdict::Refuted)`.
    /// Senkt den `outcome_value` im nächsten [`PromotionScore`] um -50.
    ///
    /// # Concurrency
    /// Erfordert `&mut self`; der Aufrufer ist für Synchronisation verantwortlich.
    pub fn refute(&mut self) {
        self.outcome = Some(OutcomeVerdict::Refuted);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PromotionScore
// ─────────────────────────────────────────────────────────────────────────────

/// Zusammengesetzter Score für die Promotion-Entscheidung.
///
/// # Beschreibung
/// Aggregiert alle epistemischen Felder eines [`EpistemicSignal`] zu einer
/// einzigen Zahl über [`PromotionScore::total`]. Positive Werte sprechen für
/// Promotion, negative für Demotion.
///
/// Skalare Komponenten:
/// - `salience` (0..=100) — direkte Bedeutsamkeit.
/// - `independent_confirmations` — unabhängige Bestätigungen (gedeckelt bei 50).
/// - `correction_weight` (0 oder 30) — explizite User-Korrektur.
/// - `outcome_value` (-100..=100) — Ergebnis einer Outcome-Prüfung.
/// - `contradiction_penalty` — Abzug für bekannte Widersprüche.
/// - `staleness_penalty` — Abzug für abgelaufene Validity.
/// - `uncertainty_penalty` — Abzug für niedrige Konfidenz.
///
/// # Concurrency
/// `Copy`-Typ, `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PromotionScore {
    /// Salience 0..=100.
    pub salience: u8,
    /// Anzahl unabhängiger Bestätigungen.
    pub independent_confirmations: u32,
    /// Gewicht für User-Korrektur (0 oder 30).
    pub correction_weight: u8,
    /// Outcome-Wert (-100..=100).
    pub outcome_value: i8,
    /// Abzug für Widersprüche (0..=60).
    pub contradiction_penalty: u8,
    /// Abzug für abgelaufene Validity (0 oder 20).
    pub staleness_penalty: u8,
    /// Abzug für niedrige Konfidenz (0, 10, 20 oder 40).
    pub uncertainty_penalty: u8,
}

impl PromotionScore {
    /// Threshold für Promotion in das HOT-Tier (verbindlich: 80).
    pub const PROMOTE_HOT_THRESHOLD: i32 = 80;

    /// Threshold für Demotion (verbindlich: 20).
    pub const DEMOTE_THRESHOLD: i32 = 20;

    /// Berechnet den Gesamt-Score.
    ///
    /// # Beschreibung
    /// Verbindliche Formel:
    /// ```text
    /// score =  salience
    ///       +  min(50, independent_confirmations * 10)
    ///       +  correction_weight
    ///       +  outcome_value as i32
    ///       -  contradiction_penalty
    ///       -  staleness_penalty
    ///       -  uncertainty_penalty
    /// ```
    ///
    /// # Rückgabe
    /// `i32` — positiv = eher promoten, negativ = eher demoten.
    ///
    /// # Concurrency
    /// Rein funktional, thread-safe.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use harw_memory::epistemic::PromotionScore;
    /// let s = PromotionScore {
    ///     salience: 80, independent_confirmations: 3, correction_weight: 30,
    ///     outcome_value: 30, contradiction_penalty: 0, staleness_penalty: 0,
    ///     uncertainty_penalty: 0,
    /// };
    /// assert!(s.total() >= 80);
    /// assert!(s.should_promote());
    /// ```
    #[must_use]
    pub fn total(&self) -> i32 {
        let conf_bonus = (self.independent_confirmations.saturating_mul(10)).min(50) as i32;
        i32::from(self.salience)
            + conf_bonus
            + i32::from(self.correction_weight)
            + i32::from(self.outcome_value)
            - i32::from(self.contradiction_penalty)
            - i32::from(self.staleness_penalty)
            - i32::from(self.uncertainty_penalty)
    }

    /// Gibt `true` zurück, wenn der Score die Promotion-Schwelle erreicht.
    ///
    /// # Beschreibung
    /// Equivalent zu `self.total() >= Self::PROMOTE_HOT_THRESHOLD`.
    ///
    /// # Concurrency
    /// Rein funktional, thread-safe.
    #[must_use]
    pub fn should_promote(&self) -> bool {
        self.total() >= Self::PROMOTE_HOT_THRESHOLD
    }

    /// Gibt `true` zurück, wenn der Score die Demotions-Schwelle unterschreitet.
    ///
    /// # Beschreibung
    /// Equivalent zu `self.total() <= Self::DEMOTE_THRESHOLD`.
    ///
    /// # Concurrency
    /// Rein funktional, thread-safe.
    #[must_use]
    pub fn should_demote(&self) -> bool {
        self.total() <= Self::DEMOTE_THRESHOLD
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use time::{Duration, OffsetDateTime};

    /// Erzeugt ein minimales `EpistemicSignal` für Tests.
    fn base_signal() -> EpistemicSignal {
        EpistemicSignal {
            id: "test-id".to_owned(),
            statement: "Immer Clippy nutzen.".to_owned(),
            origin: Provenance::AgentObservation {
                task_id: "task-1".to_owned(),
            },
            confidence: Confidence::High,
            validity: Validity::Permanent,
            scope: MemoryScope::UserGlobal,
            contradictions: vec![],
            outcome: None,
            created_at: OffsetDateTime::now_utc(),
            last_used: None,
            independent_confirmations: 0,
            salience: 50,
        }
    }

    // 1. Serde: Provenance-Variante → snake_case-Tag
    #[test]
    fn provenance_kebab_serde() -> TestResult {
        let p = Provenance::UserCorrection {
            session_id: "s1".to_owned(),
            replaces: Some("old-id".to_owned()),
        };
        let json = serde_json::to_string(&p).map_err(ctx("serialize"))?;
        assert!(
            json.contains("\"user_correction\""),
            "erwarteter Tag 'user_correction' fehlt in: {json}"
        );
        let back: Provenance = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, p);
        Ok(())
    }

    // 2. Serde: Validity::Expired Roundtrip
    #[test]
    fn validity_expired_serde() -> TestResult {
        let v = Validity::Expired;
        let json = serde_json::to_string(&v).map_err(ctx("serialize"))?;
        let back: Validity = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, v);
        Ok(())
    }

    // 3. Serde: MemoryScope::Project Roundtrip
    #[test]
    fn scope_project_serde() -> TestResult {
        let s = MemoryScope::Project {
            id: "harwness".to_owned(),
        };
        let json = serde_json::to_string(&s).map_err(ctx("serialize"))?;
        let back: MemoryScope = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, s);
        Ok(())
    }

    // 4. Hohe salience + confirmations + correction → total >= 80
    #[test]
    fn promotion_score_total_positive() {
        let score = PromotionScore {
            salience: 70,
            independent_confirmations: 3,
            correction_weight: 30,
            outcome_value: 0,
            contradiction_penalty: 0,
            staleness_penalty: 0,
            uncertainty_penalty: 0,
        };
        // 70 + 30 + 30 = 130
        assert!(
            score.total() >= 80,
            "score.total()={} sollte >= 80 sein",
            score.total()
        );
        assert!(score.should_promote());
    }

    // 5. Starke Widersprüche → total < 20
    #[test]
    fn promotion_score_penalties_reduce_total() {
        let score = PromotionScore {
            salience: 10,
            independent_confirmations: 0,
            correction_weight: 0,
            outcome_value: -50,
            contradiction_penalty: 60,
            staleness_penalty: 20,
            uncertainty_penalty: 40,
        };
        // 10 + 0 + 0 - 50 - 60 - 20 - 40 = -160
        assert!(
            score.total() < 20,
            "score.total()={} sollte < 20 sein",
            score.total()
        );
        assert!(score.should_demote());
    }

    // 6. Exakte Threshold-Werte
    #[test]
    fn promotion_thresholds_exact() {
        let at_80 = PromotionScore {
            salience: 80,
            independent_confirmations: 0,
            correction_weight: 0,
            outcome_value: 0,
            contradiction_penalty: 0,
            staleness_penalty: 0,
            uncertainty_penalty: 0,
        };
        assert_eq!(at_80.total(), 80);
        assert!(at_80.should_promote(), "total==80 → should_promote");

        let at_20 = PromotionScore {
            salience: 20,
            independent_confirmations: 0,
            correction_weight: 0,
            outcome_value: 0,
            contradiction_penalty: 0,
            staleness_penalty: 0,
            uncertainty_penalty: 0,
        };
        assert_eq!(at_20.total(), 20);
        assert!(at_20.should_demote(), "total==20 → should_demote");
    }

    // 7. UserCorrection → correction_weight == 30
    #[test]
    fn epistemic_signal_promotion_correction_boosts() {
        let mut sig = base_signal();
        sig.origin = Provenance::UserCorrection {
            session_id: "s2".to_owned(),
            replaces: None,
        };
        let score = sig.promotion_score();
        assert_eq!(
            score.correction_weight, 30,
            "UserCorrection muss correction_weight=30 liefern"
        );
    }

    // 8. Outcome Refuted → total sinkt spürbar gegenüber None
    #[test]
    fn epistemic_signal_promotion_refuted_penalizes() {
        let mut sig = base_signal();
        sig.salience = 60;
        sig.independent_confirmations = 2;

        let score_none = sig.promotion_score();
        let total_none = score_none.total();

        sig.refute();
        let score_refuted = sig.promotion_score();
        let total_refuted = score_refuted.total();

        assert!(
            total_refuted < total_none,
            "Refuted ({total_refuted}) sollte kleiner als None ({total_none}) sein"
        );
        // outcome_value -50 → Differenz exakt 50
        assert_eq!(total_none - total_refuted, 50);
    }

    // 9. Validity::Expired → is_expired true
    #[test]
    fn epistemic_signal_is_expired_by_validity_variant() {
        let mut sig = base_signal();
        sig.validity = Validity::Expired;
        let now = OffsetDateTime::now_utc();
        assert!(
            sig.is_expired(now),
            "Expired-Variante muss is_expired=true liefern"
        );
    }

    // 10. ExpiresAt in Vergangenheit → is_expired true
    #[test]
    fn epistemic_signal_is_expired_by_time() {
        let now = OffsetDateTime::now_utc();
        let past = now - Duration::hours(1);

        let mut sig = base_signal();
        sig.validity = Validity::ExpiresAt { at: past };

        assert!(
            sig.is_expired(now),
            "ExpiresAt in Vergangenheit muss is_expired=true liefern"
        );
    }

    // 11. Vollständiges EpistemicSignal JSON-Roundtrip
    #[test]
    fn signal_serde_roundtrip() -> TestResult {
        let now = OffsetDateTime::now_utc();
        let sig = EpistemicSignal {
            id: "roundtrip-1".to_owned(),
            statement: "Keine unwrap() in Produktion.".to_owned(),
            origin: Provenance::UserCorrection {
                session_id: "session-42".to_owned(),
                replaces: Some("old-sig".to_owned()),
            },
            confidence: Confidence::VeryHigh,
            validity: Validity::Permanent,
            scope: MemoryScope::Project {
                id: "harwness".to_owned(),
            },
            contradictions: vec!["sig-conflict-1".to_owned()],
            outcome: Some(OutcomeVerdict::Confirmed),
            created_at: now,
            last_used: Some(now),
            independent_confirmations: 5,
            salience: 90,
        };

        let json = serde_json::to_string_pretty(&sig).map_err(ctx("serialize"))?;
        let back: EpistemicSignal = serde_json::from_str(&json).map_err(ctx("deserialize"))?;

        assert_eq!(back.id, sig.id);
        assert_eq!(back.statement, sig.statement);
        assert_eq!(back.confidence, sig.confidence);
        assert_eq!(back.salience, sig.salience);
        assert_eq!(
            back.independent_confirmations,
            sig.independent_confirmations
        );
        assert_eq!(back.contradictions, sig.contradictions);
        assert_eq!(back.outcome, sig.outcome);
        assert_eq!(back.scope, sig.scope);
        Ok(())
    }

    // 12. Confidence-Ordnung
    #[test]
    fn confidence_ordering() {
        assert!(Confidence::VeryLow < Confidence::Low);
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
        assert!(Confidence::High < Confidence::VeryHigh);
        assert!(Confidence::VeryLow < Confidence::VeryHigh);
    }

    // Zusatz: is_expired false für zukünftiges ExpiresAt
    #[test]
    fn epistemic_signal_not_expired_future() {
        let now = OffsetDateTime::now_utc();
        let future = now + Duration::days(1);

        let mut sig = base_signal();
        sig.validity = Validity::ExpiresAt { at: future };

        assert!(
            !sig.is_expired(now),
            "ExpiresAt in Zukunft muss is_expired=false liefern"
        );
    }

    // Zusatz: confirm() setzt Outcome korrekt
    #[test]
    fn epistemic_signal_confirm_sets_outcome() {
        let mut sig = base_signal();
        assert_eq!(sig.outcome, None);
        sig.confirm();
        assert_eq!(sig.outcome, Some(OutcomeVerdict::Confirmed));
    }

    // Zusatz: contradiction_penalty ist auf 60 begrenzt
    #[test]
    fn promotion_score_contradiction_penalty_capped() {
        let mut sig = base_signal();
        // 10 Widersprüche × 15 = 150, aber cap bei 60
        for i in 0..10_u8 {
            sig.contradictions.push(format!("contra-{i}"));
        }
        let score = sig.promotion_score();
        assert_eq!(
            score.contradiction_penalty, 60,
            "contradiction_penalty muss bei 60 gecapped sein"
        );
    }

    // Zusatz: independent_confirmations bonus ist auf 50 begrenzt
    #[test]
    fn promotion_score_confirmations_capped_at_50() {
        let score = PromotionScore {
            salience: 0,
            independent_confirmations: 100,
            correction_weight: 0,
            outcome_value: 0,
            contradiction_penalty: 0,
            staleness_penalty: 0,
            uncertainty_penalty: 0,
        };
        // min(50, 100*10) = 50
        assert_eq!(score.total(), 50);
    }
}
