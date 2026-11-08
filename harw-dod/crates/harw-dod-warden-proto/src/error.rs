//! Fehlertyp von `harw-dod-warden-proto` (Contract-Master §H.1: ein
//! Fehlertyp je Crate).
//!
//! # Verantwortungsbereich
//! [`WardenProtoError`] deckt die beiden Fehlerpfade ab, die innerhalb
//! dieser Crate selbst entstehen können: die kanonische Kodierung einer
//! [`crate::action::WardenAction`] für die Belegbindung
//! ([`WardenProtoError::ActionEncoding`], siehe `action.rs`,
//! `content_digest`), und eine fehlgeschlagene Beleg-/Zulässigkeitsprüfung
//! ([`WardenProtoError::ProofMismatch`], [`WardenProtoError::NotAdmissibleAtStage`],
//! siehe `proof.rs`, `authorizes`/`verify`).
//!
//! Seit Proof v2 (C-WPROTO, W3) zusätzlich [`ProofError`]: je Prüfschritt von
//! [`crate::signed::SignedAuthorization::verify`] eine Variante, mit
//! [`ProofError::as_denial`] als verlustbehafteter Brücke auf
//! [`crate::denial::Denial`] (siehe `signed.rs`-Moduldoku).
//!
//! # Dieser Typ verlässt den Prozess nie
//! `WardenProtoError` implementiert absichtlich **kein** `Serialize` — er
//! geht nie über die Leitung. Er ist außerdem strukturell dafür ungeeignet:
//! [`WardenProtoError::ActionEncoding`] trägt einen `serde_json::Error`, der
//! selbst kein `Serialize` implementiert. Was über die Leitung geht, ist
//! [`crate::denial::Denial`] — eine feldlose, inhaltsfreie Kategorie.
//! [`WardenProtoError::as_denial`] ist die einzige Brücke zwischen beiden:
//! sie bildet einen `WardenProtoError` auf die passende `Denial`-Kategorie
//! ab (oder `None`, wenn der Fehler keine geschäftslogische Ablehnung ist,
//! sondern eine interne Kodierungspanne) und lässt dabei jedes Detail
//! fallen. Diese Abbildung ist der Ort, an dem „inhaltsfrei" tatsächlich
//! durchgesetzt wird — nicht (nur) die Wortwahl der `Display`-Meldung.
//!
//! # Warum `ProofMismatch` trotzdem ein Feld trägt, das nirgends erscheint
//! [`MismatchAspect`] sagt, WELCHER Teil eines Belegs nicht passte (Befund
//! oder Aktion) — aber `WardenProtoError::ProofMismatch`s `#[msg(...)]`
//! interpoliert es nicht. Das ist beabsichtigt, nicht übersehen: die
//! `Display`-Meldung bleibt so formuliert, dass ein Aufrufer, der sie
//! unbedacht weiterreicht (z. B. in eine Log-Zeile, die den
//! Vertrauensbereich verlässt), trotzdem nichts Feinkörniges preisgibt. Der
//! Wert bleibt für lokalen, im Prozess bleibenden Code erreichbar — über
//! Pattern-Matching auf die Variante, nie über die Meldung selbst. Diese
//! Crate bindet sich damit an dieselbe Disziplin, die
//! [`crate::denial::Denial`] strukturell erzwingt: mehr Information zu
//! *besitzen* als zu *sagen*.
//!
//! # Nebenläufigkeit
//! `WardenProtoError` trägt keinen inneren Zustand außer den Fehlerdaten
//! selbst; kein Locking, keine geteilten Ressourcen. Nicht `Clone` (ein
//! `serde_json::Error` ist es selbst nicht), ansonsten unproblematisch aus
//! jedem Thread erreichbar.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden_proto::error::WardenProtoError;
//!
//! fn describe(err: &WardenProtoError) -> String {
//!     err.to_string()
//! }
//! ```

use std::fmt;

use crate::denial::Denial;

/// Welcher Teil eines [`crate::proof::AuthorizationProof`] nicht zur
/// angeforderten Aktion passte.
///
/// # Description
/// Rein interne Diagnoseinformation (siehe `error.rs`-Moduldoku, Abschnitt
/// „Warum `ProofMismatch` trotzdem ein Feld trägt"). Nie serialisiert, nie
/// in eine `Display`-Meldung interpoliert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MismatchAspect {
    /// [`crate::proof::AuthorizationProof::finding`] stimmt nicht mit dem
    /// geprüften Befund überein.
    Finding,
    /// [`crate::proof::AuthorizationProof::bound_action`] stimmt nicht mit
    /// dem Inhaltsdigest der geprüften Aktion überein.
    Action,
}

/// Fehler dieser Crate.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung jeder Variante und dafür,
/// warum dieser Typ nie serialisiert wird.
///
/// # Errors
/// Wird über `?`/[`std::convert::From`] (für [`Self::ActionEncoding`]) oder
/// von Hand (für [`Self::ProofMismatch`], [`Self::NotAdmissibleAtStage`]) in
/// [`crate::proof::AuthorizationProof`] und [`crate::action::WardenAction`]
/// erzeugt.
#[derive(harw_macros::HarwError)]
pub enum WardenProtoError {
    /// Die kanonische JSON-Kodierung einer [`crate::action::WardenAction`]
    /// für die Digest-Bildung (siehe `action.rs`, `content_digest`) ist
    /// fehlgeschlagen.
    #[msg("failed to encode a warden action for content-addressed binding: {0}")]
    #[from]
    ActionEncoding(serde_json::Error),

    /// Der Autorisierungsbeleg ist nicht an die geprüfte Aktion/den
    /// geprüften Befund gebunden (siehe `proof.rs`, `authorizes`). Welcher
    /// Aspekt genau abweicht, steht in [`MismatchAspect`] — bewusst nicht in
    /// dieser Meldung (siehe Moduldoku).
    #[msg("authorization proof does not authorize the requested action")]
    ProofMismatch(MismatchAspect),

    /// Die Aktion ist ab der im Beleg genannten Eskalationsstufe nicht
    /// zulässig (siehe [`crate::action::WardenAction::is_admissible_from`]).
    #[msg("action is not admissible at the proof's escalation stage")]
    NotAdmissibleAtStage,

    /// Proof v2 (C-WPROTO): eine Prüfung von
    /// [`crate::signed::SignedAuthorization::verify`] ist fehlgeschlagen.
    /// Das Detail steht in [`ProofError`].
    #[msg("signed authorization rejected: {0}")]
    #[from]
    Proof(ProofError),
}

/// Fehler von Proof v2 ([`crate::signed`]).
///
/// # Description
/// Eine Variante je Prüfschritt von
/// [`crate::signed::SignedAuthorization::verify`] (Reihenfolge: Version →
/// Key-ID → MAC → Zeitfenster → Action-Digest → stillgelegte Aktion → Stufe →
/// `irreversible_ack` → Hold → Freigebender → cgroup → Nonce), dazu das
/// Dekodieren der Anfragehülle ([`crate::response::WardenRequest::decode`],
/// v1-Ablehnung), die Zielprüfung vor dem Eingriff
/// ([`crate::signed::VerifiedAuthorization::confirm_target`]) und die
/// Konstruktionsfehler von [`crate::signed::KeyId`],
/// [`crate::signed::ProofKey`], [`crate::signed::KeyRing`],
/// [`crate::signed::ProofPolicy`] und [`crate::target::CgroupKernelId`]. Kein
/// Text interpoliert Schlüssel, MACs, Nonces, Kennungen oder cgroup-Namen.
/// Nie serialisiert; über die Leitung geht nur [`ProofError::as_denial`].
///
/// # Concurrency
/// `Send + Sync`; nicht `Clone` (wegen [`std::io::Error`] und
/// [`serde_json::Error`]).
#[derive(harw_macros::HarwError)]
pub enum ProofError {
    /// Eine Key-ID verletzt die Grammatik (1–64 Zeichen aus `[A-Za-z0-9._-]`).
    #[msg("proof key id is not well-formed")]
    InvalidKeyId,
    /// Schlüsselmaterial hat nicht exakt 32 Bytes.
    #[msg("proof key material must be exactly 32 bytes")]
    InvalidKeyMaterial,
    /// Die Key-ID ist im [`crate::signed::KeyRing`] bereits vorhanden.
    #[msg("proof key id is already present in the key ring")]
    DuplicateKeyId,
    /// [`crate::signed::ProofPolicy`] liegt außerhalb der festen Grenzen
    /// (TTL 1..=120 s, Uhrenversatz ≤ 30 s, wohlgeformte Präfixe).
    #[msg("proof policy is out of bounds")]
    InvalidPolicy,
    /// Eine cgroup-Kernel-ID ist `0` (siehe [`crate::target::CgroupKernelId`]).
    #[msg("cgroup kernel id must not be zero")]
    InvalidKernelId,
    /// Hülle: die Anfrage ist ein v1-Auftrag (fälschbarer
    /// [`crate::proof::AuthorizationProof`], F-001) und wird ausdrücklich
    /// abgelehnt, bevor irgendein Feld als Autorisierung gelesen wird.
    #[msg("legacy v1 warden authorization is refused")]
    LegacyV1Refused,
    /// Hülle: die Anfrage ist kein dekodierbares v2-JSON-Objekt (unbekanntes
    /// Feld, falscher Typ, fehlende Version, kein JSON). Das Detail steht nur
    /// in `source()`, nie in der Meldung (es kann Bytes des Absenders enthalten).
    #[msg("warden request could not be decoded")]
    MalformedRequest(#[source] serde_json::Error),
    /// Schritt 1: Proof-Version ist nicht [`crate::signed::WARDEN_PROTOCOL_VERSION`]
    /// (v1 wird nicht mehr akzeptiert). Der Wert bleibt nur per Match lesbar.
    #[msg("unsupported warden proof version")]
    UnsupportedVersion(u16),
    /// Schritt 2: Key-ID nicht im Schlüsselring.
    #[msg("signed authorization names an unknown key id")]
    UnknownKeyId,
    /// Schritt 3: MAC stimmt nicht (konstante Zeit verglichen).
    #[msg("signed authorization MAC does not verify")]
    BadMac,
    /// Schritt 4: TTL ist 0 oder größer als die Policy erlaubt.
    #[msg("signed authorization TTL is outside the policy")]
    TtlOutOfPolicy,
    /// Schritt 4: `issued_at` liegt weiter als der erlaubte Uhrenversatz in der Zukunft.
    #[msg("signed authorization is not yet valid")]
    NotYetValid,
    /// Schritt 4: `now >= issued_at + ttl`.
    #[msg("signed authorization has expired")]
    Expired,
    /// Schritt 4: Zeitarithmetik außerhalb des darstellbaren Bereichs.
    #[msg("signed authorization timestamp is out of range")]
    TimestampOutOfRange,
    /// Schritt 5: Bindungsdigest der Aktion weicht vom signierten ab.
    #[msg("signed authorization is bound to a different action")]
    ActionMismatch,
    /// Schritt 5: die Aktion ist stillgelegt (`ReleaseCgroup`, ersetzt durch
    /// `ThawCgroup` und `LiftIsolation`) und wird von Proof v2 nie akzeptiert.
    #[msg("action is retired and not accepted by this warden protocol")]
    RetiredAction,
    /// Schritt 6: Aktion ist ab der signierten Stufe nicht zulässig.
    #[msg("action is not admissible at the signed escalation stage")]
    NotAdmissibleAtStage,
    /// Schritt 7: Kill oder Isolation ohne signiertes `irreversible_ack`
    /// (Entscheidung O4).
    #[msg("irreversible action lacks the operator's irreversible acknowledgement")]
    IrreversibleAckMissing,
    /// Schritt 8: `hold_secs` ist `Some(0)` oder an einer Aktion gesetzt, die
    /// keinen Hold kennt (nur `FreezeCgroup` darf einen Hold tragen).
    #[msg("signed hold duration is not valid for this action")]
    InvalidHold,
    /// Schritt 9: der Freigebende ist kein lokaler Operator mit wohlgeformter
    /// Kennung (Kanal-Peers dürfen keinen Eingriff freigeben, Leitprinzip I1).
    #[msg("signed approver is not an acceptable operator")]
    InvalidApprover,
    /// Schritt 10: signierte cgroup weicht von der cgroup der Aktion ab.
    #[msg("signed authorization is bound to a different cgroup")]
    CgroupMismatch,
    /// Schritt 10: cgroup liegt unter keinem erlaubten Präfix (Pfadkomponenten).
    #[msg("target cgroup is outside the allowed prefixes")]
    CgroupNotAllowed,
    /// Schritt 11: Nonce wurde bereits verbraucht (Replay).
    #[msg("signed authorization nonce was already used")]
    NonceReplayed,
    /// Schritt 11: der [`crate::signed::NonceLedger`] konnte nicht dauerhaft
    /// prüfen/schreiben — fail closed.
    #[msg("nonce ledger is unavailable")]
    #[from]
    LedgerIo(std::io::Error),
    /// Zielprüfung vor dem Eingriff: Kernel-ID, Pfad oder `first_seen` der
    /// beobachteten cgroup weichen vom signierten Ziel ab (B21).
    #[msg("target cgroup no longer matches the authorized identity")]
    TargetChanged,
}

/// Formatiert `ProofError` über seine [`std::fmt::Display`]-Meldung.
impl fmt::Debug for ProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl ProofError {
    /// Maps this error onto the content-free wire denial category.
    ///
    /// # Description
    /// Alle Authentisierungs-/Bindungsfehler werden auf
    /// [`Denial::ProofMismatch`] zusammengefaltet (ein Absender erfährt nicht,
    /// welcher Schritt scheiterte). Getrennt bleiben:
    /// - [`Denial::UnsupportedVersion`] (Rollout-Diagnose vor jeder
    ///   Kryptographie, auch für einen abgelehnten v1-Auftrag);
    /// - [`Denial::Malformed`] (Hülle nicht dekodierbar);
    /// - Kategorien, die erst **nach** gültigem MAC erreichbar sind und dem
    ///   echten Escalator sagen, was er ändern muss:
    ///   [`Denial::NotAdmissibleAtStage`], [`Denial::Unsupported`]
    ///   (stillgelegte Aktion), [`Denial::IrreversibleAckRequired`],
    ///   [`Denial::PolicyDenied`] (Präfix-Allowlist, Freigebender) und
    ///   [`Denial::TargetChanged`] (Zielprüfung);
    /// - [`Denial::Unavailable`] (Ledger-Ausfall, kein Fehler des Absenders).
    ///
    /// Konstruktionsfehler (Key-ID, Schlüssel, Policy) sind lokale
    /// Konfigurationsfehler und werden als [`Denial::Unavailable`] gemeldet.
    ///
    /// # Returns
    /// Die [`Denial`]-Kategorie.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::{Denial, ProofError};
    ///
    /// assert_eq!(ProofError::BadMac.as_denial(), Denial::ProofMismatch);
    /// assert_eq!(ProofError::Expired.as_denial(), Denial::ProofMismatch);
    /// assert_eq!(ProofError::LegacyV1Refused.as_denial(), Denial::UnsupportedVersion);
    /// ```
    #[must_use]
    pub fn as_denial(&self) -> Denial {
        match self {
            Self::UnsupportedVersion(_) | Self::LegacyV1Refused => Denial::UnsupportedVersion,
            Self::MalformedRequest(_) | Self::InvalidKernelId => Denial::Malformed,
            Self::NotAdmissibleAtStage => Denial::NotAdmissibleAtStage,
            Self::RetiredAction => Denial::Unsupported,
            Self::IrreversibleAckMissing => Denial::IrreversibleAckRequired,
            Self::InvalidApprover | Self::CgroupNotAllowed => Denial::PolicyDenied,
            Self::TargetChanged => Denial::TargetChanged,
            Self::LedgerIo(_)
            | Self::InvalidKeyId
            | Self::InvalidKeyMaterial
            | Self::DuplicateKeyId
            | Self::InvalidPolicy => Denial::Unavailable,
            Self::UnknownKeyId
            | Self::BadMac
            | Self::TtlOutOfPolicy
            | Self::NotYetValid
            | Self::Expired
            | Self::TimestampOutOfRange
            | Self::ActionMismatch
            | Self::InvalidHold
            | Self::CgroupMismatch
            | Self::NonceReplayed => Denial::ProofMismatch,
        }
    }
}

/// Formatiert `WardenProtoError` über seine [`std::fmt::Display`]-Meldung.
///
/// # Description
/// `#[derive(harw_macros::HarwError)]` erzeugt kein `Debug` (siehe
/// `harw-macros/src/error.rs`, `expand_harw_error`) — diese Impl schließt die
/// Lücke, indem sie an `Display` delegiert, statt eine zweite, mit `Display`
/// potenziell auseinanderlaufende Formatierung zu pflegen (Contract-Master
/// §H.1-Konvention: eine Formatierung, keine Duplikation).
impl fmt::Debug for WardenProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl WardenProtoError {
    /// Bildet diesen Fehler auf die inhaltsfreie Ablehnungskategorie ab, die
    /// über die Leitung an den Eskalationsleiter zurückgeht.
    ///
    /// # Description
    /// Die einzige Stelle, an der Detail bewusst fallen gelassen wird, bevor
    /// eine Aussage den Prozess verlässt (siehe Moduldoku). Eine
    /// [`Self::ActionEncoding`]-Panne ist keine geschäftslogische Ablehnung
    /// — sie zeigt einen internen Defekt an (z. B. ein nicht kodierbares
    /// Feld) und wird deshalb nicht auf eine `Denial` abgebildet; der
    /// Aufrufer muss sie gesondert behandeln (z. B. als harten Fehler
    /// loggen, nicht als normale Ablehnung an den Aufrufer zurückgeben).
    ///
    /// # Returns
    /// `Some(Denial)` für eine geschäftslogische Ablehnung, `None` für eine
    /// interne Kodierungspanne.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::denial::Denial;
    /// use harw_dod_warden_proto::error::{MismatchAspect, WardenProtoError};
    ///
    /// let err = WardenProtoError::ProofMismatch(MismatchAspect::Finding);
    /// assert_eq!(err.as_denial(), Some(Denial::ProofMismatch));
    /// ```
    #[must_use]
    pub fn as_denial(&self) -> Option<Denial> {
        match self {
            Self::ProofMismatch(_) => Some(Denial::ProofMismatch),
            Self::NotAdmissibleAtStage => Some(Denial::NotAdmissibleAtStage),
            Self::ActionEncoding(_) => None,
            Self::Proof(err) => Some(err.as_denial()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{MismatchAspect, ProofError, WardenProtoError, WardenProtoResult};
    use crate::denial::Denial;
    use crate::test_support::{TestError, TestResult};

    fn sample_json_error() -> TestResult<serde_json::Error> {
        let Err(source) = serde_json::from_str::<serde_json::Value>("not json") else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        Ok(source)
    }

    #[test]
    fn test_action_encoding_display_interpolates_inner_message() -> TestResult {
        let err = WardenProtoError::from(sample_json_error()?);
        let display = err.to_string();
        assert!(
            display.starts_with("failed to encode a warden action for content-addressed binding:")
        );
        Ok(())
    }

    #[test]
    fn test_action_encoding_source_returns_inner_error() -> TestResult {
        let err: WardenProtoError = sample_json_error()?.into();
        assert!(err.source().is_some());
        Ok(())
    }

    #[test]
    fn test_proof_mismatch_display_is_content_free_regardless_of_aspect() {
        let finding = WardenProtoError::ProofMismatch(MismatchAspect::Finding).to_string();
        let action = WardenProtoError::ProofMismatch(MismatchAspect::Action).to_string();
        assert_eq!(finding, action);
        assert_eq!(
            finding,
            "authorization proof does not authorize the requested action"
        );
    }

    #[test]
    fn test_proof_mismatch_source_is_none() {
        let err = WardenProtoError::ProofMismatch(MismatchAspect::Action);
        assert!(err.source().is_none());
    }

    #[test]
    fn test_not_admissible_at_stage_display_and_source() {
        let err = WardenProtoError::NotAdmissibleAtStage;
        assert_eq!(
            err.to_string(),
            "action is not admissible at the proof's escalation stage"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let err = WardenProtoError::NotAdmissibleAtStage;
        assert_eq!(format!("{err:?}"), err.to_string());

        let err = WardenProtoError::ProofMismatch(MismatchAspect::Finding);
        assert_eq!(format!("{err:?}"), err.to_string());
    }

    #[test]
    fn test_as_denial_maps_business_errors_and_drops_internal_ones() -> TestResult {
        assert_eq!(
            WardenProtoError::ProofMismatch(MismatchAspect::Finding).as_denial(),
            Some(Denial::ProofMismatch)
        );
        assert_eq!(
            WardenProtoError::ProofMismatch(MismatchAspect::Action).as_denial(),
            Some(Denial::ProofMismatch)
        );
        assert_eq!(
            WardenProtoError::NotAdmissibleAtStage.as_denial(),
            Some(Denial::NotAdmissibleAtStage)
        );
        assert_eq!(
            WardenProtoError::from(sample_json_error()?).as_denial(),
            None
        );
        assert_eq!(
            WardenProtoError::from(ProofError::UnsupportedVersion(1)).as_denial(),
            Some(Denial::UnsupportedVersion)
        );
        Ok(())
    }

    #[test]
    fn test_proof_error_as_denial_folds_authentication_failures() {
        for err in [
            ProofError::UnknownKeyId,
            ProofError::BadMac,
            ProofError::TtlOutOfPolicy,
            ProofError::NotYetValid,
            ProofError::Expired,
            ProofError::TimestampOutOfRange,
            ProofError::ActionMismatch,
            ProofError::InvalidHold,
            ProofError::CgroupMismatch,
            ProofError::NonceReplayed,
        ] {
            assert_eq!(err.as_denial(), Denial::ProofMismatch);
        }
        assert_eq!(
            ProofError::NotAdmissibleAtStage.as_denial(),
            Denial::NotAdmissibleAtStage
        );
        assert_eq!(
            ProofError::UnsupportedVersion(7).as_denial(),
            Denial::UnsupportedVersion
        );
        let io = ProofError::from(std::io::Error::other("disk full"));
        assert_eq!(io.as_denial(), Denial::Unavailable);
    }

    #[test]
    fn test_proof_error_as_denial_keeps_post_mac_categories_apart() -> TestResult {
        assert_eq!(
            ProofError::LegacyV1Refused.as_denial(),
            Denial::UnsupportedVersion
        );
        assert_eq!(
            ProofError::MalformedRequest(sample_json_error()?).as_denial(),
            Denial::Malformed
        );
        assert_eq!(ProofError::InvalidKernelId.as_denial(), Denial::Malformed);
        assert_eq!(ProofError::RetiredAction.as_denial(), Denial::Unsupported);
        assert_eq!(
            ProofError::IrreversibleAckMissing.as_denial(),
            Denial::IrreversibleAckRequired
        );
        assert_eq!(
            ProofError::InvalidApprover.as_denial(),
            Denial::PolicyDenied
        );
        assert_eq!(
            ProofError::CgroupNotAllowed.as_denial(),
            Denial::PolicyDenied
        );
        assert_eq!(ProofError::TargetChanged.as_denial(), Denial::TargetChanged);
        Ok(())
    }

    #[test]
    fn test_malformed_request_display_hides_decoder_detail_but_keeps_source() -> TestResult {
        let Err(source) = serde_json::from_str::<serde_json::Value>("{\"secret-field\":") else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        let err = ProofError::MalformedRequest(source);
        assert_eq!(err.to_string(), "warden request could not be decoded");
        assert!(!err.to_string().contains("secret-field"));
        assert!(err.source().is_some());
        Ok(())
    }

    #[test]
    fn test_proof_error_display_is_content_free() {
        assert_eq!(
            ProofError::UnsupportedVersion(7).to_string(),
            "unsupported warden proof version"
        );
        let io = ProofError::from(std::io::Error::other("secret-path"));
        assert!(!io.to_string().contains("secret-path"));
        assert!(io.source().is_some());
        assert_eq!(
            format!("{:?}", ProofError::BadMac),
            ProofError::BadMac.to_string()
        );
    }

    #[test]
    fn test_warden_proto_result_alias_carries_warden_proto_error() {
        fn always_fails() -> WardenProtoResult<()> {
            Err(WardenProtoError::NotAdmissibleAtStage)
        }

        assert!(always_fails().is_err());
    }
}
