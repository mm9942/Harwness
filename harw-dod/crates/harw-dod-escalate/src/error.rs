//! Fehlertyp von `harw-dod-escalate` (Contract-Master §H.1: ein Fehlertyp je
//! Crate).
//!
//! # Verantwortungsbereich
//! [`EscalateError`] deckt jeden Fehlerpfad ab, der beim Autorisieren einer
//! vorgeschlagenen Aktion entstehen kann: eine Triage, die keine Eskalation
//! rechtfertigt ([`EscalateError::VerdictNotConfirmed`],
//! [`EscalateError::NotEscalatable`]), eine stillgelegte Aktion
//! ([`EscalateError::RetiredAction`]), eine Aktion, die ab der ermittelten
//! Stufe nicht zulässig ist ([`EscalateError::NotAdmissibleAtStage`]), eine
//! vorgeschlagene Aktion, die nicht zur aufgerufenen Operation passt
//! ([`EscalateError::NotAFreezeAction`], [`EscalateError::NotAThawAction`],
//! [`EscalateError::NotAStageGatedAction`]),
//! sowie der einzige Fremdfehlerpfad, der `FreezeStore`-Zugriff
//! ([`EscalateError::Store`]).
//!
//! Seit PL-100 WP 3.0 baut diese Crate keinen v1-Beleg mehr und bildet keinen
//! JSON-Digest; die frühere Variante `ActionEncoding` (wrappte
//! `harw_dod_warden_proto::WardenProtoError`) ist mit diesem Pfad entfallen.
//!
//! # Inhaltsfreie Fehlermeldungen
//! Diese Crate liegt auf dem Weg zum Warden (Brief, Abschnitt „Die Auflagen,
//! die diese Crate tragen"): was sie meldet, kann in ein Protokoll wandern,
//! das den Vertrauensbereich dieses Prozesses verlässt. Jede `Display`-Meldung
//! ist deshalb eine feste, nicht interpolierte Zeichenkette — auch für
//! [`EscalateError::Store`], dessen gewrapptes Detail über `source()`
//! erreichbar bleibt (für lokalen, im Prozess bleibenden Code), aber nie in
//! die eigene `Display`-Meldung einfließt. Dieselbe Disziplin, die
//! [`harw_dod_warden_types::denial::Denial`] strukturell erzwingt: mehr
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

use harw_dod_ladder::LadderError;
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
    /// (`FindingKind::RuleTriggered`) oder nicht bestätigt. Auch das Ziel von
    /// `From<LadderError>` (PL-100 WP 3.0): dieselbe Meldung wie
    /// [`LadderError::NotEscalatable`].
    #[msg("finding is not eligible for any escalation stage")]
    NotEscalatable,

    /// [`harw_dod_warden_types::WardenAction::is_admissible_from`] hat die
    /// vorgeschlagene Aktion ab der ermittelten Stufe abgelehnt.
    #[msg("action is not admissible at the determined escalation stage")]
    NotAdmissibleAtStage,

    /// [`crate::action::Action::authorize`] wurde mit einer stillgelegten
    /// Aktion aufgerufen ([`harw_dod_warden_types::WardenAction::is_retired`],
    /// heute nur `ReleaseCgroup`, PL-100 WP 3.1). Ersatz: `ThawCgroup` bzw.
    /// `LiftIsolation`.
    #[msg("proposed action is retired and cannot be authorized")]
    RetiredAction,

    /// [`crate::freeze_ops::authorize_freeze`] wurde mit einer
    /// `ProposedAction` aufgerufen, die keine `FreezeCgroup`-Variante ist.
    #[msg("proposed action does not target a cgroup freeze")]
    NotAFreezeAction,

    /// [`crate::freeze_ops::authorize_thaw`] wurde mit einer
    /// `ProposedAction` aufgerufen, die keine `ThawCgroup`-Variante ist
    /// (seit PL-100 WP 3.0 Nachfolger von `NotAReleaseAction`).
    #[msg("proposed action does not target a cgroup thaw")]
    NotAThawAction,

    /// [`crate::freeze_ops::authorize_stage_gated`] wurde mit
    /// `FreezeCgroup` oder `ThawCgroup` aufgerufen. Beide verändern den
    /// `FreezeStore`-Datensatz und laufen deshalb nur über
    /// [`crate::freeze_ops::authorize_freeze`] bzw.
    /// [`crate::freeze_ops::authorize_thaw`] („keine Freigabe ohne
    /// Datensatz", siehe `freeze_ops`-Moduldoku).
    #[msg("proposed action changes a cgroup freeze and needs the freeze store path")]
    NotAStageGatedAction,

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
/// wie `harw_dod_ladder::LadderError`).
impl fmt::Debug for EscalateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Faltet die Ablehnung der Leiter in die gleichbedeutende Variante dieser
/// Crate (PL-100 WP 3.0: `Ladder` liegt seitdem in `harw-dod-ladder`).
///
/// # Description
/// Erschöpfend: eine neue Variante in `LadderError` macht diese Stelle zum
/// Compile-Fehler. Aufrufer, die bis WP 3.0 von
/// `Ladder::stage_for_or_reject` einen `EscalateError` bekamen, erhalten mit
/// `.map_err(EscalateError::from)` dieselbe Variante und Meldung.
///
/// # Examples
/// ```rust
/// use harw_dod_escalate::{EscalateError, LadderError};
///
/// let err = EscalateError::from(LadderError::NotEscalatable);
/// assert!(matches!(err, EscalateError::NotEscalatable));
/// assert_eq!(err.to_string(), LadderError::NotEscalatable.to_string());
/// ```
impl From<LadderError> for EscalateError {
    fn from(err: LadderError) -> Self {
        match err {
            LadderError::NotEscalatable => Self::NotEscalatable,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::EscalateError;
    use crate::test_support::{TestError, TestResult};
    use harw_dod_ladder::LadderError;
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
    fn test_from_ladder_error_keeps_variant_and_message() {
        let err = EscalateError::from(LadderError::NotEscalatable);
        assert!(matches!(err, EscalateError::NotEscalatable));
        assert_eq!(err.to_string(), LadderError::NotEscalatable.to_string());
        assert!(err.source().is_none());
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
            EscalateError::RetiredAction,
            EscalateError::NotAFreezeAction,
            EscalateError::NotAThawAction,
            EscalateError::NotAStageGatedAction,
        ] {
            let text = err.to_string();
            assert!(!text.chars().any(|c| c.is_ascii_digit()));
            assert!(!text.contains("cgroup-"));
            assert!(!text.contains("finding-"));
        }
    }

    #[test]
    fn test_unit_variant_messages_are_fixed() {
        assert_eq!(
            EscalateError::RetiredAction.to_string(),
            "proposed action is retired and cannot be authorized"
        );
        assert_eq!(
            EscalateError::NotAThawAction.to_string(),
            "proposed action does not target a cgroup thaw"
        );
        assert_eq!(
            EscalateError::NotAStageGatedAction.to_string(),
            "proposed action changes a cgroup freeze and needs the freeze store path"
        );
        assert!(EscalateError::RetiredAction.source().is_none());
        assert!(EscalateError::NotAStageGatedAction.source().is_none());
    }

    /// Erschöpfend ohne Platzhalter: eine neue Variante — etwa ein
    /// zurückkehrender Signier- oder Digestpfad wie das frühere
    /// `ActionEncoding` (PL-100 WP 3.0) — macht diesen Test zum
    /// Compile-Fehler und zwingt zur bewussten Entscheidung.
    #[test]
    fn test_error_set_has_only_unsigned_variants() {
        fn variant_name(err: &EscalateError) -> &'static str {
            match err {
                EscalateError::VerdictNotConfirmed => "verdict-not-confirmed",
                EscalateError::NotEscalatable => "not-escalatable",
                EscalateError::NotAdmissibleAtStage => "not-admissible-at-stage",
                EscalateError::RetiredAction => "retired-action",
                EscalateError::NotAFreezeAction => "not-a-freeze-action",
                EscalateError::NotAThawAction => "not-a-thaw-action",
                EscalateError::NotAStageGatedAction => "not-a-stage-gated-action",
                EscalateError::Store(_) => "store",
            }
        }
        assert_eq!(
            variant_name(&EscalateError::RetiredAction),
            "retired-action"
        );
        assert_eq!(
            variant_name(&EscalateError::NotAThawAction),
            "not-a-thaw-action"
        );
    }

    #[test]
    fn test_from_wrapped_variants_are_content_free_but_keep_source() -> TestResult {
        let store: EscalateError = SessionStoreError::from(sample_json_error()?).into();
        assert_eq!(
            store.to_string(),
            "underlying freeze store operation failed"
        );
        assert!(store.source().is_some());
        Ok(())
    }
}
