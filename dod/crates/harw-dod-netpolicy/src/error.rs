//! Fehler eines `NetBackend`: Backend nicht verfügbar oder Plan abgelehnt.
//!
//! # Verantwortungsbereich
//! [`NetPolicyError`] ist der einzige Fehlertyp dieser Crate. Er beschreibt
//! ausschließlich, warum [`crate::NetBackend::apply`] fehlschlagen kann —
//! nicht, warum ein [`crate::NetPlan`]-Wert ungültig *aussieht*: ein
//! `NetPlan` ist immer ein gültiger Wert (siehe die Moduldoc von `crate`).
//! `Display`, `Debug` und `std::error::Error` entstehen über
//! `#[derive(harw_macros::HarwError)]` (Muster: `harw-dod-cap/src/error.rs`).
//! Kein `anyhow`, kein `thiserror`.
//!
//! # Wiederholbarkeit
//! [`NetPolicyError::permanence`] ordnet jede Variante einer
//! [`harw_dod_cap::Permanence`] zu — demselben Vokabular, das die
//! Sensor-Crates des Ausbauprogramms für „lohnt sich ein erneuter Versuch"
//! verwenden. Ein Durchsetzer-Knoten kann dieselbe Retry-Logik auf
//! Netzplan-Fehler wie auf Sensorfehler anwenden.

use harw_dod_cap::Permanence;
use harw_macros::HarwError;

/// Fehler beim Anwenden eines [`crate::NetPlan`] durch ein
/// [`crate::NetBackend`].
///
/// # Description
/// Genau zwei Fälle, wie von [`crate::NetBackend::apply`] dokumentiert: das
/// Backend selbst ist nicht ansprechbar, oder es hat den Inhalt des Plans
/// geprüft und abgelehnt. Beide Varianten tragen einen `reason`-Text für
/// Diagnose/Logging; keine Variante enthält den vollständigen
/// [`crate::NetPlan`] selbst, damit ein Fehlerwert nicht versehentlich zu
/// einer zweiten, potenziell abweichenden Kopie der Regeln wird.
#[derive(Debug, HarwError)]
pub enum NetPolicyError {
    /// Das Backend kann derzeit keine Regeln anwenden, z. B. weil ein
    /// externer Prozess fehlt oder eine Ressource gesperrt ist.
    #[msg("network backend is unavailable: {reason}")]
    BackendUnavailable {
        /// Backend-seitige Beschreibung, warum es nicht verfügbar ist.
        reason: String,
    },

    /// Das Backend hat den Plan inhaltlich geprüft und abgelehnt — zum
    /// Beispiel, weil er eine Regelart enthält, die dieses Backend nicht
    /// durchsetzen kann (siehe die Moduldoc von `crate`, Abschnitt zu
    /// `DnsSuffix`/`Host`).
    #[msg("network backend rejected the plan: {reason}")]
    PlanRejected {
        /// Backend-seitige Begründung der Ablehnung.
        reason: String,
    },
}

impl NetPolicyError {
    /// Ob ein erneuter Anwendungsversuch sinnvoll sein kann.
    ///
    /// # Description
    /// [`Self::BackendUnavailable`] gilt als `Transient`: eine
    /// vorübergehend gesperrte Ressource oder ein kurzzeitig nicht
    /// erreichbarer Dienst kann sich bis zum nächsten Versuch geändert
    /// haben. [`Self::PlanRejected`] gilt als `Permanent`: derselbe Plan
    /// wird von demselben Backend beim nächsten Versuch aus demselben Grund
    /// wieder abgelehnt, solange sich weder der Plan noch das Backend
    /// ändern.
    ///
    /// # Returns
    /// Die [`Permanence`] dieser Fehlervariante.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Permanence;
    /// use harw_dod_netpolicy::NetPolicyError;
    ///
    /// let err = NetPolicyError::PlanRejected {
    ///     reason: "unsupported rule".to_owned(),
    /// };
    /// assert_eq!(err.permanence(), Permanence::Permanent);
    /// ```
    #[must_use]
    pub const fn permanence(&self) -> Permanence {
        match self {
            Self::BackendUnavailable { .. } => Permanence::Transient,
            Self::PlanRejected { .. } => Permanence::Permanent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_unavailable_is_transient() {
        let err = NetPolicyError::BackendUnavailable {
            reason: "locked".to_owned(),
        };
        assert_eq!(err.permanence(), Permanence::Transient);
    }

    #[test]
    fn test_plan_rejected_is_permanent() {
        let err = NetPolicyError::PlanRejected {
            reason: "bad rule".to_owned(),
        };
        assert_eq!(err.permanence(), Permanence::Permanent);
    }

    #[test]
    fn test_backend_unavailable_display_contains_reason() {
        let err = NetPolicyError::BackendUnavailable {
            reason: "no nft binary".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "network backend is unavailable: no nft binary"
        );
    }

    #[test]
    fn test_plan_rejected_display_contains_reason() {
        let err = NetPolicyError::PlanRejected {
            reason: "dns suffix unsupported".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "network backend rejected the plan: dns suffix unsupported"
        );
    }

    #[test]
    fn test_error_implements_std_error() {
        let err = NetPolicyError::BackendUnavailable {
            reason: "boom".to_owned(),
        };
        let _: &dyn std::error::Error = &err;
    }
}
