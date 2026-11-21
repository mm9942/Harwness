//! Der Autorisierungsbeleg v1: [`AuthorizationProof`] — **Altlast**.
//!
//! # v1 ist nicht mehr gültig (C-WPROTO, W3; PL-100 WP 3.1)
//! Dieser Beleg hat weder MAC noch Nonce noch Ablauf und ist von jedem
//! Absender fälschbar (Befund F-001); die unten stehende Aussage, er sei eine
//! „Schicht, die selbst ein Absender, der den Socket benutzen darf, nicht
//! umgehen kann“, trifft nicht zu. Nachfolger: [`crate::signed::SignedAuthorization`].
//! Der Typ ist `#[deprecated]`; seit PL-100 WP 3.0 (Schritt 2) baut
//! `harw-dod-escalate` ihn nicht mehr, und er bleibt nur noch für die Tests der
//! v1-Ablehnung, bis er entfernt wird. Dasselbe gilt für
//! [`ActionContentDigest`], das Erweiterungs-Trait,
//! das seit PL-100 WP 3.0 den früheren inhärenten v1-Digest
//! `WardenAction::content_digest` trägt (`WardenAction` liegt seitdem in
//! `harw-dod-warden-types`, an das der Signierteil keine inhärente Methode
//! mehr hängen kann). Der Warden lehnt einen v1-Auftrag ausdrücklich ab
//! ([`crate::response::WardenRequest::decode`],
//! [`crate::error::ProofError::LegacyV1Refused`]).
//!
//! # Verantwortungsbereich
//! Der Beleg, dass eine Aktion autorisiert wurde — so beschaffen, dass der
//! Durchsetzer ihn **nachprüfen** kann, ohne der Gegenseite zu glauben
//! (siehe `lib.rs`-Moduldoku, Abschnitt „Wer einen Beleg ausstellen darf",
//! für die Architektur, in die dieser Typ eingebettet ist). „Nachprüfen"
//! heißt hier konkret zweierlei, beides über
//! [`AuthorizationProof::verify`] erreichbar: **Bindung** — passt der Beleg
//! überhaupt zu diesem Befund und dieser Aktion, wörtlich, nicht nur dem Typ
//! nach ([`AuthorizationProof::authorizes`])? — und **Zulässigkeit** — ist
//! die Aktion ab der belegten Stufe überhaupt erlaubt
//! ([`crate::action::WardenAction::is_admissible_from`])? Beide Prüfungen
//! sind rein strukturell; sie ersetzen nicht die eigentliche
//! Vertrauensgrenze des Systems (ein `SOCK_SEQPACKET`-Unix-Socket mit
//! `SO_PEERCRED`, AW5-04b, Entscheidung Nr. 3 in `docs/design/build-history.md`) — sie
//! sind die zusätzliche Schicht, die selbst ein Absender, der den Socket
//! benutzen darf, nicht umgehen kann, indem er einfach andere Nutzdaten in
//! dieselbe Beleg-Hülle packt.
//!
//! # Warum jedes Feld da ist
//! - `finding` (`harw_types::FindingId`): welcher Befund diese Autorisierung
//!   ausgelöst hat. Ohne ihn ließe sich ein für Befund A ausgestellter Beleg
//!   für Befund B wiederverwenden.
//! - `stage` (`EscalationStage`): auf welcher Stufe der Leiter autorisiert
//!   wurde — die Grundlage für [`crate::action::WardenAction::is_admissible_from`].
//! - `authorized_by` (`harw_types::ApprovalActor`): wer autorisiert hat —
//!   derselbe Typ, den `harw-types` bereits für Freigabeentscheidungen an
//!   anderer Stelle im Workspace verwendet (Operator oder Kanal-Peer), statt
//!   eine zweite, hier neu erfundene Identitätsdarstellung einzuführen.
//! - `authorized_at` (`jiff::Timestamp`): wann. Wird als Parameter
//!   entgegengenommen, nie über `Timestamp::now()` gelesen — diese Crate
//!   liest keine Systemuhr (Brief, Abschnitt „Die Regeln, die dieses
//!   Protokoll tragen"); wer autorisiert (`harw-dod-escalate`, AW5-03),
//!   liefert den Zeitpunkt.
//! - `bound_action` (`harw_types::ContentDigest`): wogegen der Beleg
//!   gebunden ist — der Inhaltsdigest der exakten Aktion (siehe
//!   [`ActionContentDigest::content_digest`]). Ein Beleg, der nur
//!   an eine Aktions*art* gebunden wäre (z. B. „irgendein `FreezeCgroup`"),
//!   passte auf jede `FreezeCgroup`-Aktion, gleich welche cgroup sie nennt —
//!   genau das schließt der Brief aus („ein Proof, der auf eine beliebige
//!   Aktion passt, ist keiner"). Der Digest bindet an die exakten Feldwerte.
//!
//! # Wer diesen Typ konstruieren darf
//! **Niemand außerhalb dieser Crate über einen öffentlichen Weg, der eine
//! Autorisierungsentscheidung träfe.** [`AuthorizationProof::new`] ist die
//! einzige Konstruktionsfunktion, die diese Crate anbietet, und sie trifft
//! selbst **keine** Entscheidung: sie verlangt alle fünf Felder als
//! Pflichtparameter, hat keinen `Default`, keinen Builder mit optionalen
//! Feldern, und liest keine Systemuhr. Sie ist reine Datenzusammensetzung
//! aus bereits getroffenen Fakten — nicht die Stelle, an der eine
//! Eskalationsleiter befragt oder eine Autorisierungsregel ausgewertet wird.
//! **Diese Entscheidung trifft ausschließlich `harw_dod_escalate::authorize`
//! (AW5-03, dort `pub(crate)`).** Rust hat keine Sichtbarkeit „nur für
//! Crate X" über Crate-Grenzen hinweg; diese Grenze ist deshalb eine
//! dokumentierte Konvention, keine vom Compiler erzwungene. Jeder andere
//! Aufrufer — insbesondere `harw-dod-warden` (AW5-04a) — erhält
//! `AuthorizationProof`-Werte ausschließlich durch Deserialisieren
//! empfangener Wire-Bytes (über [`crate::response::WardenActionRequest`])
//! und liest sie nur über die Zugriffsmethoden dieses Moduls.

// Dieses Modul ist vollständig v1-Altlast und nennt deshalb die eigenen
// stillgelegten Typen; die Warnung gilt weiter für jeden Aufrufer außerhalb.
#![allow(deprecated)]

use harw_types::{ApprovalActor, ContentDigest, FindingId};
use serde::{Deserialize, Serialize};

use crate::action::WardenAction;
use crate::error::{MismatchAspect, WardenProtoError};
use crate::stage::EscalationStage;

/// Der Beleg, dass eine Aktion autorisiert wurde.
///
/// # Description
/// Siehe Moduldoku für die Begründung jedes Feldes und dafür, wer diesen Typ
/// konstruieren darf. Alle Felder sind privat; Lesezugriff geht ausschließlich
/// über die gleichnamigen Methoden.
///
/// # Wire-Format
/// `deny_unknown_fields` (K19) — dieser Typ nimmt Daten entgegen, die über
/// die Leitung vom Eskalationsleiter kommen.
#[deprecated(
    note = "forgeable v1 proof (F-001), refused by the warden; sign a SignedAuthorization (proof v2) instead"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationProof {
    finding: FindingId,
    stage: EscalationStage,
    authorized_by: ApprovalActor,
    authorized_at: jiff::Timestamp,
    bound_action: ContentDigest,
}

impl AuthorizationProof {
    /// Baut den Datensatz für eine bereits getroffene Autorisierungsentscheidung.
    ///
    /// # Description
    /// **v1-Altlast:** war nur für `harw_dod_escalate::authorize` (AW5-03)
    /// bestimmt, das ihn seit PL-100 WP 3.0 nicht mehr aufruft — siehe
    /// Moduldoku, Abschnitt „Wer diesen Typ konstruieren darf". Trifft selbst
    /// keine Entscheidung: jeder Parameter muss vom Aufrufer bereits ermittelt
    /// worden sein (welcher Befund, welche Stufe laut `Ladder::admissible`,
    /// wer laut Identitätsprüfung, wann laut injizierter Zeit, wogegen laut
    /// [`ActionContentDigest::content_digest`]).
    ///
    /// # Arguments
    /// - `finding` (`FindingId`): der auslösende Befund.
    /// - `stage` (`EscalationStage`): die Stufe, auf der autorisiert wurde.
    /// - `authorized_by` (`ApprovalActor`): wer autorisiert hat.
    /// - `authorized_at` (`jiff::Timestamp`): wann — vom Aufrufer geliefert,
    ///   nie von dieser Funktion selbst gelesen.
    /// - `bound_action` (`ContentDigest`): der Inhaltsdigest der exakten
    ///   autorisierten Aktion (siehe [`ActionContentDigest::content_digest`]).
    ///
    /// # Returns
    /// Den zusammengesetzten Beleg.
    ///
    /// # Examples
    /// ```rust
    /// #![allow(deprecated)]
    /// use harw_dod_warden_proto::{
    ///     ActionContentDigest, AuthorizationProof, EscalationStage, WardenAction,
    /// };
    /// use harw_types::{ApprovalActor, CgroupId, FindingId};
    ///
    /// let (Ok(cgroup), Ok(finding)) = (
    ///     CgroupId::try_from_str("cgroup-1"),
    ///     FindingId::try_from_str("finding-1"),
    /// ) else {
    ///     return;
    /// };
    /// let action = WardenAction::FreezeCgroup { cgroup };
    /// let Ok(bound_action) = action.content_digest() else { return };
    /// let proof = AuthorizationProof::new(
    ///     finding,
    ///     EscalationStage::RuleTriggered,
    ///     ApprovalActor::Operator { id: "operator-1".to_string() },
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     bound_action,
    /// );
    /// assert_eq!(proof.stage(), EscalationStage::RuleTriggered);
    /// ```
    #[must_use]
    pub fn new(
        finding: FindingId,
        stage: EscalationStage,
        authorized_by: ApprovalActor,
        authorized_at: jiff::Timestamp,
        bound_action: ContentDigest,
    ) -> Self {
        Self {
            finding,
            stage,
            authorized_by,
            authorized_at,
            bound_action,
        }
    }

    /// Der auslösende Befund.
    #[must_use]
    pub fn finding(&self) -> &FindingId {
        &self.finding
    }

    /// Die Stufe, auf der autorisiert wurde.
    #[must_use]
    pub fn stage(&self) -> EscalationStage {
        self.stage
    }

    /// Wer autorisiert hat.
    #[must_use]
    pub fn authorized_by(&self) -> &ApprovalActor {
        &self.authorized_by
    }

    /// Wann autorisiert wurde.
    #[must_use]
    pub fn authorized_at(&self) -> jiff::Timestamp {
        self.authorized_at
    }

    /// Der Inhaltsdigest der autorisierten Aktion.
    #[must_use]
    pub fn bound_action(&self) -> ContentDigest {
        self.bound_action
    }

    /// Prüft, ob dieser Beleg an genau `finding` und `action` gebunden ist.
    ///
    /// # Description
    /// Rein strukturelle Bindungsprüfung (siehe Moduldoku, Abschnitt „Warum
    /// jedes Feld da ist", zu `bound_action`). Prüft **nicht** die
    /// Zulässigkeit ab der belegten Stufe — dafür [`Self::verify`], das
    /// beide Prüfungen kombiniert.
    ///
    /// # Arguments
    /// - `finding` (`&FindingId`): der Befund, gegen den geprüft wird.
    /// - `action` (`&WardenAction`): die Aktion, gegen die geprüft wird.
    ///
    /// # Returns
    /// `Ok(())`, wenn Befund und Aktion zum Beleg passen.
    ///
    /// # Errors
    /// - [`WardenProtoError::ProofMismatch`] mit [`MismatchAspect::Finding`]:
    ///   `finding` stimmt nicht mit [`Self::finding`] überein.
    /// - [`WardenProtoError::ProofMismatch`] mit [`MismatchAspect::Action`]:
    ///   der Inhaltsdigest von `action` stimmt nicht mit
    ///   [`Self::bound_action`] überein.
    /// - [`WardenProtoError::ActionEncoding`] — siehe
    ///   [`ActionContentDigest::content_digest`].
    ///
    /// # Examples
    /// ```rust
    /// #![allow(deprecated)]
    /// use harw_dod_warden_proto::{
    ///     ActionContentDigest, AuthorizationProof, EscalationStage, WardenAction,
    /// };
    /// use harw_types::{ApprovalActor, CgroupId, FindingId};
    ///
    /// let (Ok(cgroup), Ok(other_cgroup), Ok(finding)) = (
    ///     CgroupId::try_from_str("cgroup-1"),
    ///     CgroupId::try_from_str("cgroup-2"),
    ///     FindingId::try_from_str("finding-1"),
    /// ) else {
    ///     return;
    /// };
    /// let action = WardenAction::FreezeCgroup { cgroup };
    /// let Ok(bound_action) = action.content_digest() else { return };
    /// let proof = AuthorizationProof::new(
    ///     finding.clone(),
    ///     EscalationStage::RuleTriggered,
    ///     ApprovalActor::Operator { id: "operator-1".to_string() },
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     bound_action,
    /// );
    /// assert!(proof.authorizes(&finding, &action).is_ok());
    ///
    /// let other_cgroup_action = WardenAction::FreezeCgroup { cgroup: other_cgroup };
    /// assert!(proof.authorizes(&finding, &other_cgroup_action).is_err());
    /// ```
    pub fn authorizes(
        &self,
        finding: &FindingId,
        action: &WardenAction,
    ) -> Result<(), WardenProtoError> {
        if &self.finding != finding {
            return Err(WardenProtoError::ProofMismatch(MismatchAspect::Finding));
        }
        let actual_digest = action.content_digest()?;
        if self.bound_action != actual_digest {
            return Err(WardenProtoError::ProofMismatch(MismatchAspect::Action));
        }
        Ok(())
    }

    /// Vollständige Nachprüfung: Bindung **und** Zulässigkeit ab der
    /// belegten Stufe.
    ///
    /// # Description
    /// Kombiniert [`Self::authorizes`] mit
    /// [`WardenAction::is_admissible_from`]. Dies ist die Methode, die
    /// `harw-dod-warden` (AW5-04a) vor jeder Durchsetzung aufrufen sollte —
    /// „Proof-Nachprüfung" laut dessen Gerüstdoku.
    ///
    /// # Arguments
    /// - `finding` (`&FindingId`): der Befund, gegen den geprüft wird.
    /// - `action` (`&WardenAction`): die Aktion, gegen die geprüft wird.
    ///
    /// # Returns
    /// `Ok(())`, wenn die Aktion sowohl an diesen Beleg gebunden als auch ab
    /// dessen Stufe zulässig ist.
    ///
    /// # Errors
    /// - Alles, was [`Self::authorizes`] zurückgeben kann.
    /// - [`WardenProtoError::NotAdmissibleAtStage`]: die Bindung passt, aber
    ///   die Aktion ist ab [`Self::stage`] nicht zulässig.
    ///
    /// # Examples
    /// ```rust
    /// #![allow(deprecated)]
    /// use harw_dod_warden_proto::{
    ///     ActionContentDigest, AuthorizationProof, EscalationStage, WardenAction,
    /// };
    /// use harw_types::{ApprovalActor, CgroupId, FindingId};
    ///
    /// let (Ok(cgroup), Ok(finding)) = (
    ///     CgroupId::try_from_str("cgroup-1"),
    ///     FindingId::try_from_str("finding-1"),
    /// ) else {
    ///     return;
    /// };
    /// let action = WardenAction::KillProcessTree { cgroup };
    /// let Ok(bound_action) = action.content_digest() else { return };
    /// let proof = AuthorizationProof::new(
    ///     finding.clone(),
    ///     EscalationStage::RuleTriggered,
    ///     ApprovalActor::Operator { id: "operator-1".to_string() },
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     bound_action,
    /// );
    /// // Bindung passt, aber `KillProcessTree` ist ab `RuleTriggered` nicht zulässig.
    /// assert!(proof.verify(&finding, &action).is_err());
    /// ```
    pub fn verify(
        &self,
        finding: &FindingId,
        action: &WardenAction,
    ) -> Result<(), WardenProtoError> {
        self.authorizes(finding, action)?;
        if !action.is_admissible_from(self.stage) {
            return Err(WardenProtoError::NotAdmissibleAtStage);
        }
        Ok(())
    }
}

/// Der v1-Inhaltsdigest einer [`WardenAction`] — die Grundlage für
/// [`AuthorizationProof::bound_action`] (**v1-Altlast**).
///
/// # Description
/// Bis PL-100 WP 3.0 die inhärente Methode `WardenAction::content_digest`.
/// `WardenAction` liegt seitdem im signierfreien `harw-dod-warden-types`;
/// dieses Erweiterungs-Trait trägt dieselbe Methode mit derselben Signatur
/// und denselben Bytes weiter, bis der v1-Pfad mit seinen letzten Aufrufern
/// entfällt. Aufrufer brauchen zusätzlich `use
/// harw_dod_warden_proto::ActionContentDigest;`.
///
/// Kodiert die Aktion als JSON (Feldreihenfolge folgt der Deklaration, siehe
/// `serde_json`s Verhalten für von `#[derive]` erzeugte `Serialize`-Impls)
/// und hasht das Ergebnis mit [`ContentDigest::of`]. Zwei strukturell
/// identische Aktionen ergeben denselben Digest; jede Abweichung — auch nur
/// in einem `CgroupId`-Feld — einen anderen. Proof v2 nutzt stattdessen
/// [`WardenAction::binding_digest`] (serde-unabhängig, unfehlbar).
///
/// # Examples
/// ```rust
/// #![allow(deprecated)]
/// use harw_dod_warden_proto::{ActionContentDigest, WardenAction};
/// use harw_types::CgroupId;
///
/// let Ok(cgroup) = CgroupId::try_from_str("cgroup-1") else { return };
/// let action = WardenAction::FreezeCgroup { cgroup };
/// assert!(action.content_digest().is_ok());
/// ```
#[deprecated(
    note = "v1 binding for the forgeable AuthorizationProof (F-001); proof v2 binds WardenAction::binding_digest inside SignedAuthorization"
)]
pub trait ActionContentDigest {
    /// Computes the v1 JSON content digest of this action.
    ///
    /// # Errors
    /// - [`WardenProtoError::ActionEncoding`]: die JSON-Kodierung ist
    ///   fehlgeschlagen. Für die Feldtypen von `WardenAction` (ausschließlich
    ///   `harw-types`-Kennungen) praktisch unerreichbar — dieselbe Begründung
    ///   wie bei `harw_dod_signals::SignalsError::DigestEncoding`.
    #[deprecated(
        note = "v1 binding for the forgeable AuthorizationProof (F-001); proof v2 binds WardenAction::binding_digest inside SignedAuthorization"
    )]
    fn content_digest(&self) -> Result<ContentDigest, WardenProtoError>;
}

impl ActionContentDigest for WardenAction {
    fn content_digest(&self) -> Result<ContentDigest, WardenProtoError> {
        let bytes = serde_json::to_vec(self)?;
        Ok(ContentDigest::of(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionContentDigest, AuthorizationProof};
    use crate::action::WardenAction;
    use crate::error::{MismatchAspect, WardenProtoError};
    use crate::stage::EscalationStage;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{ApprovalActor, CgroupId, FindingId};

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn finding(id: &str) -> TestResult<FindingId> {
        FindingId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn actor() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "operator-1".to_string(),
        }
    }

    fn sample_proof(
        finding_id: FindingId,
        action: &WardenAction,
        stage: EscalationStage,
    ) -> TestResult<AuthorizationProof> {
        Ok(AuthorizationProof::new(
            finding_id,
            stage,
            actor(),
            jiff::Timestamp::UNIX_EPOCH,
            action.content_digest().map_err(ctx("action encodes"))?,
        ))
    }

    // -- Rundlauf / deny_unknown_fields --------------------------------------

    #[test]
    fn test_serde_roundtrip() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let proof = sample_proof(finding("finding-1")?, &action, EscalationStage::Escalated)?;

        let json = serde_json::to_string(&proof).map_err(ctx("serializes"))?;
        let round_tripped: AuthorizationProof =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, proof);
        Ok(())
    }

    #[test]
    fn test_rejects_unknown_field() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let proof = sample_proof(
            finding("finding-1")?,
            &action,
            EscalationStage::RuleTriggered,
        )?;
        let mut value = serde_json::to_value(&proof).map_err(ctx("serializes"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Missing("proof serializes to a JSON object"))?
            .insert("extra".to_string(), serde_json::Value::Bool(true));
        let result: Result<AuthorizationProof, _> = serde_json::from_value(value);
        assert!(result.is_err());
        Ok(())
    }

    // -- Accessoren -------------------------------------------------------------

    #[test]
    fn test_accessors_return_constructed_values() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = sample_proof(f.clone(), &action, EscalationStage::Escalated)?;

        assert_eq!(proof.finding(), &f);
        assert_eq!(proof.stage(), EscalationStage::Escalated);
        assert_eq!(proof.authorized_by(), &actor());
        assert_eq!(proof.authorized_at(), jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(
            proof.bound_action(),
            action.content_digest().map_err(ctx("action encodes"))?
        );
        Ok(())
    }

    // -- authorizes: Bindung ---------------------------------------------------

    #[test]
    fn test_authorizes_accepts_matching_finding_and_action() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = sample_proof(f.clone(), &action, EscalationStage::RuleTriggered)?;

        assert!(proof.authorizes(&f, &action).is_ok());
        Ok(())
    }

    #[test]
    fn test_authorizes_rejects_different_finding() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let proof = sample_proof(
            finding("finding-1")?,
            &action,
            EscalationStage::RuleTriggered,
        )?;

        let other_finding = finding("finding-2")?;
        let Err(err) = proof.authorizes(&other_finding, &action) else {
            return Err(TestError::Unexpected(
                "authorizes must reject a different finding".into(),
            ));
        };
        assert!(matches!(
            err,
            WardenProtoError::ProofMismatch(MismatchAspect::Finding)
        ));
        Ok(())
    }

    #[test]
    fn test_authorizes_rejects_different_action() -> TestResult {
        let bound_action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = sample_proof(f.clone(), &bound_action, EscalationStage::RuleTriggered)?;

        let different_action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-2")?,
        };
        let Err(err) = proof.authorizes(&f, &different_action) else {
            return Err(TestError::Unexpected(
                "authorizes must reject a different action".into(),
            ));
        };
        assert!(matches!(
            err,
            WardenProtoError::ProofMismatch(MismatchAspect::Action)
        ));
        Ok(())
    }

    #[test]
    fn test_authorizes_rejects_different_action_kind_on_same_cgroup() -> TestResult {
        let bound_action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = sample_proof(f.clone(), &bound_action, EscalationStage::RuleTriggered)?;

        let different_kind = WardenAction::KillProcessTree {
            cgroup: cgroup("cgroup-1")?,
        };
        let Err(err) = proof.authorizes(&f, &different_kind) else {
            return Err(TestError::Unexpected(
                "authorizes must reject a different action kind".into(),
            ));
        };
        assert!(matches!(
            err,
            WardenProtoError::ProofMismatch(MismatchAspect::Action)
        ));
        Ok(())
    }

    // -- ActionContentDigest (v1, bis PL-100 WP 3.0 inhärent an WardenAction) --

    #[test]
    fn test_content_digest_is_deterministic_for_identical_actions() -> TestResult {
        let a = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let b = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        assert_eq!(
            a.content_digest().map_err(ctx("content_digest"))?,
            b.content_digest().map_err(ctx("content_digest"))?
        );
        Ok(())
    }

    #[test]
    fn test_content_digest_differs_for_different_cgroup() -> TestResult {
        let a = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let b = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-2")?,
        };
        assert_ne!(
            a.content_digest().map_err(ctx("content_digest"))?,
            b.content_digest().map_err(ctx("content_digest"))?
        );
        Ok(())
    }

    #[test]
    fn test_content_digest_differs_for_different_action_kind_with_same_cgroup() -> TestResult {
        let freeze = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let release = WardenAction::ReleaseCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        assert_ne!(
            freeze.content_digest().map_err(ctx("content_digest"))?,
            release.content_digest().map_err(ctx("content_digest"))?
        );
        Ok(())
    }

    // Die Bytes des v1-Digests sind durch die Aufteilung unverändert: JSON der
    // Aktion, gehasht mit `ContentDigest::of`.
    #[test]
    fn test_content_digest_hashes_the_json_encoding() -> TestResult {
        let action = WardenAction::KillProcessTree {
            cgroup: cgroup("cgroup-1")?,
        };
        let expected =
            harw_types::ContentDigest::of(br#"{"kind":"kill-process-tree","cgroup":"cgroup-1"}"#);
        assert_eq!(
            action.content_digest().map_err(ctx("content_digest"))?,
            expected
        );
        Ok(())
    }

    // -- verify: Bindung + Zulässigkeit -----------------------------------------

    #[test]
    fn test_verify_succeeds_when_bound_and_admissible() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = sample_proof(f.clone(), &action, EscalationStage::RuleTriggered)?;

        assert!(proof.verify(&f, &action).is_ok());
        Ok(())
    }

    #[test]
    fn test_verify_fails_when_bound_but_not_admissible_at_stage() -> TestResult {
        let action = WardenAction::KillProcessTree {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = sample_proof(f.clone(), &action, EscalationStage::RuleTriggered)?;

        let Err(err) = proof.verify(&f, &action) else {
            return Err(TestError::Unexpected(
                "verify must fail when not admissible at this stage".into(),
            ));
        };
        assert!(matches!(err, WardenProtoError::NotAdmissibleAtStage));
        Ok(())
    }

    #[test]
    fn test_verify_fails_when_admissible_but_not_bound() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = sample_proof(f.clone(), &action, EscalationStage::Escalated)?;

        let unbound_action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-9")?,
        };
        let Err(err) = proof.verify(&f, &unbound_action) else {
            return Err(TestError::Unexpected(
                "verify must fail when the action is not bound".into(),
            ));
        };
        assert!(matches!(
            err,
            WardenProtoError::ProofMismatch(MismatchAspect::Action)
        ));
        Ok(())
    }
}
