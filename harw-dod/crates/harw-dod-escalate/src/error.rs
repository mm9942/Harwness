//! Fehlertyp von `harw-dod-escalate` (Contract-Master §H.1: ein Fehlertyp je
//! Crate).
//!
//! # Verantwortungsbereich
//! [`EscalateError`] deckt jeden Fehlerpfad ab, der beim Autorisieren einer
//! vorgeschlagenen Aktion entstehen kann: eine Triage, die keine Eskalation
//! rechtfertigt ([`EscalateError::VerdictNotConfirmed`],
//! [`EscalateError::NotEscalatable`]), eine Aktion, die ab der ermittelten
//! Stufe nicht zulässig ist ([`EscalateError::NotAdmissibleAtStage`]), eine
//! vorgeschlagene Aktion, die nicht zur aufgerufenen Operation passt
//! ([`EscalateError::NotAFreezeAction`], [`EscalateError::NotAReleaseAction`]),
//! sowie die beiden Fremdfehlerpfade — Digestbildung
//! ([`EscalateError::ActionEncoding`]) und `FreezeStore`-Zugriff
//! ([`EscalateError::Store`]).
//!
//! # Inhaltsfreie Fehlermeldungen
//! Diese Crate liegt auf dem Weg zum Warden (Brief, Abschnitt „Die Auflagen,
//! die diese Crate tragen"): was sie meldet, kann in ein Protokoll wandern,
//! das den Vertrauensbereich dieses Prozesses verlässt. Jede `Display`-Meldung
//! ist deshalb eine feste, nicht interpolierte Zeichenkette — auch für
//! [`EscalateError::Store`] und [`EscalateError::ActionEncoding`], deren
//! gewrapptes Detail über `source()` erreichbar bleibt (für lokalen, im
//! Prozess bleibenden Code), aber nie in die eigene `Display`-Meldung
//! einfließt. Dieselbe Disziplin, die
//! [`harw_dod_warden_proto::denial::Denial`] strukturell erzwingt: mehr
//! Information zu *besitzen* als zu *sagen*.
//!
//! # Nebenläufigkeit
//! Trägt keinen inneren Zustand außer den Fehlerdaten selbst; kein Locking,
//! keine geteilten Ressourcen.
//!
//! # Examples
//! ```rust
//! use harw_dod_escalate::error::EscalateError;
//!
//! fn describe(err: &EscalateError) -> String {
//!     err.to_string()
//! }
//! ```

use std::fmt;

use harw_dod_warden_proto::WardenProtoError;
use harw_session_store::SessionStoreError;

/// Fehler dieser Crate.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung jeder Variante und für
/// die Regel, dass keine Meldung Inhalt preisgibt.
///
/// # Errors
/// Wird von [`crate::action::Action::authorize`] (`pub(crate)`) und den
/// öffentlichen Verdrahtungsfunktionen in [`crate::freeze_ops`] erzeugt.
#[derive(harw_macros::HarwError)]
pub enum EscalateError {
    /// [`crate::ladder::Ladder::stage_for`]/[`crate::action::Action::authorize`]
    /// verlangen `Verdict::Confirmed` — ein Fehlalarm oder ein noch nicht
    /// geklärter Befund darf keine Aktion autorisieren.
    #[msg("finding verdict is not confirmed; no action may be authorized for it")]
    VerdictNotConfirmed,

    /// [`crate::ladder::Ladder::stage_for`] hat keine Eskalationsstufe
    /// ermittelt — der Befund ist entweder keine ausgelöste Regel
    /// (`FindingKind::RuleTriggered`) oder nicht bestätigt.
    #[msg("finding is not eligible for any escalation stage")]
    NotEscalatable,

    /// [`harw_dod_warden_proto::WardenAction::is_admissible_from`] hat die
    /// vorgeschlagene Aktion ab der ermittelten Stufe abgelehnt.
    #[msg("action is not admissible at the determined escalation stage")]
    NotAdmissibleAtStage,

    /// [`crate::freeze_ops::authorize_freeze`] wurde mit einer
    /// `ProposedAction` aufgerufen, die keine `FreezeCgroup`-Variante ist.
    #[msg("proposed action does not target a cgroup freeze")]
    NotAFreezeAction,

    /// [`crate::freeze_ops::authorize_release`] wurde mit einer
    /// `ProposedAction` aufgerufen, die keine `ReleaseCgroup`-Variante ist.
    #[msg("proposed action does not target a cgroup release")]
    NotAReleaseAction,

    /// Die kanonische JSON-Kodierung der Aktion für die Belegbindung (siehe
    /// [`harw_dod_warden_proto::WardenAction::content_digest`]) ist
    /// fehlgeschlagen. Praktisch unerreichbar für die hier verwendeten
    /// Feldtypen (ausschließlich `harw-types`-Kennungen).
    #[msg("failed to encode the proposed action for content-addressed binding")]
    #[from]
    ActionEncoding(WardenProtoError),

    /// Ein `FreezeStore`-Zugriff ist fehlgeschlagen (I/O, Serialisierung,
    /// bereits existierender oder bereits aufgelöster Datensatz, Lock).
    #[msg("underlying freeze store operation failed")]
    #[from]
    Store(SessionStoreError),
}

/// Formatiert `EscalateError` über seine [`std::fmt::Display`]-Meldung.
///
/// # Description
/// `#[derive(harw_macros::HarwError)]` erzeugt kein `Debug` — diese Impl
/// schließt die Lücke, indem sie an `Display` delegiert, statt eine zweite,
/// potenziell auseinanderlaufende Formatierung zu pflegen (dasselbe Muster
/// wie `harw_dod_warden_proto::error::WardenProtoError`).
impl fmt::Debug for EscalateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::EscalateError;
    use crate::test_support::{TestError, TestResult};
    use harw_dod_warden_proto::WardenProtoError;
    use harw_session_store::SessionStoreError;

    /// Liefert einen echten `serde_json`-Fehler (ersetzt das frühere
    /// `.unwrap_err()`): ungültiges JSON muss beim Parsen scheitern.
    fn sample_json_error() -> TestResult<serde_json::Error> {
        let Err(err) = serde_json::from_str::<serde_json::Value>("not json") else {
            return Err(TestError::Unexpected(
                "ungültiges JSON muss einen Fehler liefern".into(),
            ));
        };
        Ok(err)
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let err = EscalateError::NotEscalatable;
        assert_eq!(format!("{err:?}"), err.to_string());
    }

    #[test]
    fn test_unit_variant_messages_carry_no_embedded_identifiers() {
        // "Inhaltsfrei" heißt hier konkret: keine Ziffern und keine der
        // Kennungs-Präfixe, die diese Crate an anderer Stelle verwendet
        // (`cgroup-`, `finding-`) — eine ID-artige Zeichenkette hier wäre
        // ein Leck.
        for err in [
            EscalateError::VerdictNotConfirmed,
            EscalateError::NotEscalatable,
            EscalateError::NotAdmissibleAtStage,
            EscalateError::NotAFreezeAction,
            EscalateError::NotAReleaseAction,
        ] {
            let text = err.to_string();
            assert!(!text.chars().any(|c| c.is_ascii_digit()));
            assert!(!text.contains("cgroup-"));
            assert!(!text.contains("finding-"));
        }
    }

    #[test]
    fn test_from_wrapped_variants_are_content_free_but_keep_source() -> TestResult {
        let action_encoding: EscalateError = WardenProtoError::from(sample_json_error()?).into();
        assert_eq!(
            action_encoding.to_string(),
            "failed to encode the proposed action for content-addressed binding"
        );
        assert!(action_encoding.source().is_some());

        let store: EscalateError = SessionStoreError::from(sample_json_error()?).into();
        assert_eq!(
            store.to_string(),
            "underlying freeze store operation failed"
        );
        assert!(store.source().is_some());
        Ok(())
    }
}
