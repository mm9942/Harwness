//! Der Typestate-Kern dieser Crate: [`Action<S>`], [`Proposed`],
//! [`Authorized`], und `authorize` — der einzige Ort, an dem eine
//! [`AuthorizationProof`] entsteht (Invariante S1).
//!
//! # Warum `Action<S>` `ProposedAction` bzw. `WardenActionRequest` wrappt,
//! statt eigene Datenfelder zu erfinden
//! Diese Crate erzeugt keine neue Aktionstaxonomie — die Aktionen
//! (`FreezeCgroup`, `ReleaseCgroup`, `IsolateNetwork`, `KillProcessTree`) und
//! ihr Wire-Umschlag (`WardenActionRequest`) sind bereits in
//! `harw-dod-warden-proto` (AW5-02) vollständig deklariert. `Action<Proposed>`
//! wrappt eine [`ProposedAction`]: ein Vorschlag, der (noch) keinen Beleg
//! trägt. `Action<Authorized>` wrappt eine fertige
//! [`WardenActionRequest`] — genau das, was über den Socket zum Durchsetzer
//! geht. Der Typestate-Übergang dazwischen ist [`Action::authorize`].
//!
//! # Warum `authorize` `pub(crate)` ist, und was das unmöglich macht
//! [`Action::authorize`] ist die einzige Funktion in dieser Crate — und
//! damit im gesamten Workspace, siehe `harw-dod-warden-proto/src/lib.rs`-
//! Moduldoku, Abschnitt „Wer einen Beleg ausstellen darf" — die eine
//! [`AuthorizationProof`] konstruiert. Weil sie `pub(crate)` ist, kann kein
//! Aufrufer außerhalb dieser Crate ein `Action<Authorized>` herstellen: es
//! gibt keinen öffentlichen Konstruktor, keinen `From`-Impl, kein
//! `Deserialize` für `Action<S>` selbst, über den ein Aufrufer den
//! Typestate-Übergang umgehen könnte. Ein `WardenActionRequest`, das
//! `harw-dod-warden` (AW5-04a) über den Socket empfängt, ist ein
//! **deserialisierter Wire-Wert**, kein `Action<Authorized>` — beide tragen
//! dieselben Daten, aber nur die hiesige Konstruktion durchläuft die
//! Prüfungen in `authorize` (Verdict, Zulässigkeitsmatrix, Belegbindung).
//! Das ist dieselbe Bewegung, die `harw_dod_rules::finding` für
//! `Finding<RuleChecked>` vormacht: **der Besitz des Wertes ist die
//! Berechtigung**, nicht ein Laufzeit-Flag, das jede Konsumentin selbst
//! prüfen müsste. Siehe den `compile_fail`-Doctest unten für den Beleg.
//!
//! # Warum der Besitz eines `Finding<RuleChecked>` — vermittelt über
//! `Finding<Triaged>` — die Berechtigung *ist*
//! [`Action::authorize`] verlangt ein `&Finding<Triaged>`. Diesen Typ kann
//! ausschließlich `harw-dod-rules` herstellen: `Finding::raw` und
//! `Finding::check` sind dort `pub(crate)`, und `harw_dod_rules::triage`
//! konsumiert ausschließlich ein bereits zertifiziertes `Finding<RuleChecked>`
//! (siehe `harw-dod-rules/src/finding.rs`-Moduldoku, Abschnitt „Der Besitz
//! des Wertes ist die Berechtigung"). Wer hier ein `Finding<Triaged>`
//! vorweisen kann, hat also zwangsläufig zuerst ein `Finding<RuleChecked>`
//! **besessen** — kein Aufrufer kann sich eines fälschen, um `authorize` zu
//! erreichen. Diese Crate trägt selbst keine Regel, die entscheidet, ob ein
//! Befund echt ist; sie verlässt sich vollständig auf diesen bereits
//! durchgesetzten Besitz.

use std::marker::PhantomData;

use harw_dod_rules::{Finding, Triaged, Verdict};
use harw_dod_warden_proto::{
    AuthorizationProof, EscalationStage, ProposedAction, WardenAction, WardenActionRequest,
};
use harw_types::ApprovalActor;
use jiff::Timestamp;

use crate::error::{EscalateError, EscalateResult};
use crate::ladder::Ladder;

// Versiegeltes Supertrait: nur Typen aus diesem Modul dürfen `ActionState`
// implementieren (Vorbild: `harw_dod_rules::finding::sealed`).
mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Proposed {}
    impl Sealed for super::Authorized {}
}

/// Zustandsmarker: eine vorgeschlagene, noch nicht autorisierte Aktion.
///
/// # Description
/// Jeder darf eine `ProposedAction` vorschlagen ([`Action::propose`]) — das
/// Vorschlagen selbst ist keine privilegierte Operation. Erst
/// [`Action::authorize`] (`pub(crate)`) hebt sie in [`Authorized`].
///
/// # Examples
/// ```rust
/// use harw_dod_escalate::Proposed;
///
/// let _marker = Proposed;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proposed;

/// Zustandsmarker: eine autorisierte Aktion mit gebundenem
/// [`AuthorizationProof`], versandbereit als [`WardenActionRequest`].
///
/// # Description
/// Außerhalb dieser Crate nicht herstellbar — siehe Moduldoku, Abschnitt
/// „Warum `authorize` `pub(crate)` ist".
///
/// # Examples
/// ```rust
/// use harw_dod_escalate::Authorized;
///
/// let _marker = Authorized;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authorized;

/// Internes Bindeglied zwischen Zustands-Marker und dem Datentyp, den
/// [`Action<S>`] in diesem Zustand wrappt (Vorbild:
/// `harw_dod_rules::finding::FindingState`).
///
/// `#[doc(hidden)]`, aus demselben Grund wie dort: ein privates Supertrait
/// als Bound auf einem `pub`-Typ löst sonst den `private_bounds`-Lint aus.
#[doc(hidden)]
pub trait ActionState: sealed::Sealed {
    /// Der Datentyp, den `Action<Self>` trägt.
    type Inner;
}

impl ActionState for Proposed {
    type Inner = ProposedAction;
}

impl ActionState for Authorized {
    type Inner = WardenActionRequest;
}

/// Eine Aktion im Zustand `S` — vorgeschlagen oder autorisiert.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung. `Action<Proposed>` wrappt
/// eine [`ProposedAction`]; `Action<Authorized>` wrappt die fertige, an einen
/// [`AuthorizationProof`] gebundene [`WardenActionRequest`].
///
/// # Errors
/// Keine eigenen Fehler; siehe [`Action::authorize`].
pub struct Action<S: ActionState> {
    inner: S::Inner,
    _state: PhantomData<S>,
}

// Manuell statt `#[derive(Debug)]`/`#[derive(Clone)]`: eine generisch
// gebundene Struktur (`struct Action<S: ActionState>`) bekäme von `derive`
// nur die Bound `S: Debug`/`S: Clone`, nicht die hier tatsächlich nötige
// `S::Inner: Debug`/`S::Inner: Clone` (identisches Problem und identische
// Lösung wie `harw_dod_rules::finding::Finding<S>`).
impl<S: ActionState> std::fmt::Debug for Action<S>
where
    S::Inner: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Action").field("inner", &self.inner).finish()
    }
}

impl<S: ActionState> Clone for Action<S>
where
    S::Inner: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _state: PhantomData,
        }
    }
}

impl Action<Proposed> {
    /// Schlägt eine Aktion vor, ohne sie zu autorisieren.
    ///
    /// # Description
    /// Reine Verpackung — keine Prüfung, keine Systemuhr. Jeder darf
    /// vorschlagen; siehe Moduldoku.
    ///
    /// # Arguments
    /// - `action` (`ProposedAction`): die vorzuschlagende Aktion.
    ///
    /// # Returns
    /// Ein `Action<Proposed>`, das `action` unverändert trägt.
    ///
    /// # Errors
    /// Keine — totale Konstruktion.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_escalate::Action;
    /// use harw_dod_warden_proto::ProposedAction;
    /// use harw_types::CgroupId;
    ///
    /// let proposed = Action::propose(ProposedAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// });
    /// assert_eq!(
    ///     proposed.proposed(),
    ///     &ProposedAction::FreezeCgroup {
    ///         cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    ///     }
    /// );
    /// ```
    #[must_use]
    pub fn propose(action: ProposedAction) -> Self {
        Self {
            inner: action,
            _state: PhantomData,
        }
    }

    /// Die vorgeschlagene Aktion.
    #[must_use]
    pub fn proposed(&self) -> &ProposedAction {
        &self.inner
    }

    /// Autorisiert eine vorgeschlagene Aktion. **Die einzige Stelle in
    /// diesem Workspace, die eine [`AuthorizationProof`] erzeugt (S1).**
    /// `pub(crate)`: siehe Moduldoku, Abschnitt „Warum `authorize`
    /// `pub(crate)` ist".
    ///
    /// # Description
    /// Prüft in dieser Reihenfolge: (1) `finding.verdict()` muss
    /// [`Verdict::Confirmed`] sein — ein Fehlalarm oder ein ungeklärter
    /// Befund autorisiert nichts; (2) die Aktion muss ab `stage` laut
    /// [`crate::ladder::Ladder::admissible`] zulässig sein; (3) der
    /// Inhaltsdigest der Aktion wird gebildet (siehe
    /// [`WardenAction::content_digest`]) und in den neuen
    /// [`AuthorizationProof`] gebunden. Liest keine Systemuhr —
    /// `authorized_at` kommt vom Aufrufer.
    ///
    /// # Arguments
    /// - `finding` (`&Finding<Triaged>`): der auslösende, bereits triagierte
    ///   Befund. Sein bloßer Besitz ist die Berechtigung (siehe Moduldoku).
    /// - `stage` (`EscalationStage`): die vom Aufrufer (üblicherweise über
    ///   [`crate::ladder::Ladder::stage_for`]) ermittelte Stufe.
    /// - `authorized_by` (`ApprovalActor`): wer autorisiert.
    /// - `authorized_at` (`jiff::Timestamp`): wann — injiziert, nie gelesen.
    ///
    /// # Returns
    /// Ein `Action<Authorized>`, das die fertige [`WardenActionRequest`]
    /// (Aktion plus Beleg) trägt.
    ///
    /// # Errors
    /// - [`EscalateError::VerdictNotConfirmed`]: `finding.verdict()` ist
    ///   nicht [`Verdict::Confirmed`].
    /// - [`EscalateError::NotAdmissibleAtStage`]: die Aktion ist ab `stage`
    ///   nicht zulässig.
    /// - [`EscalateError::ActionEncoding`]: die Digestbildung ist
    ///   fehlgeschlagen (siehe [`WardenAction::content_digest`]).
    ///
    /// # Examples
    /// Von außerhalb dieser Crate nicht aufrufbar, da `pub(crate)`:
    /// ```rust,compile_fail
    /// use harw_dod_escalate::Action;
    /// use harw_dod_rules::{Finding, Triaged};
    /// use harw_dod_warden_proto::EscalationStage;
    /// use harw_types::ApprovalActor;
    ///
    /// fn attempt(proposed: Action<harw_dod_escalate::Proposed>, finding: Finding<Triaged>) {
    ///     // `authorize` ist außerhalb von `harw-dod-escalate` unsichtbar.
    ///     let _authorized = proposed.authorize(
    ///         &finding,
    ///         EscalationStage::RuleTriggered,
    ///         ApprovalActor::Operator { id: "operator-1".to_string() },
    ///         jiff::Timestamp::UNIX_EPOCH,
    ///     );
    /// }
    /// ```
    pub(crate) fn authorize(
        self,
        finding: &Finding<Triaged>,
        stage: EscalationStage,
        authorized_by: ApprovalActor,
        authorized_at: Timestamp,
    ) -> EscalateResult<Action<Authorized>> {
        if *finding.verdict() != Verdict::Confirmed {
            return Err(EscalateError::VerdictNotConfirmed);
        }
        let action: WardenAction = self.inner.clone().into();
        if !Ladder::admissible(&action, stage) {
            return Err(EscalateError::NotAdmissibleAtStage);
        }
        let bound_action = action.content_digest()?;
        let proof = AuthorizationProof::new(
            finding.id().clone(),
            stage,
            authorized_by,
            authorized_at,
            bound_action,
        );
        let request = WardenActionRequest::new(self.inner, proof);
        Ok(Action {
            inner: request,
            _state: PhantomData,
        })
    }
}

impl Action<Authorized> {
    /// Die fertige, versandbereite Wire-Nachricht.
    #[must_use]
    pub fn request(&self) -> &WardenActionRequest {
        &self.inner
    }

    /// Konsumiert `self` und liefert die fertige Wire-Nachricht.
    #[must_use]
    pub fn into_request(self) -> WardenActionRequest {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_rules::rule::{Rule, RuleContext};
    use harw_dod_rules::rules::EgressFlowRule;
    use harw_dod_rules::{run_rules, triage};
    use harw_dod_signals::{EventKind, SecurityEvent};
    use harw_sandbox::NetworkScope;
    use harw_types::{CgroupId, SensorId};

    fn confirmed_finding() -> Finding<Triaged> {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("net-0"),
            observed_at: jiff::Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: "evil.example.com".to_owned(),
                port: 443,
            },
        }];
        let ctx = RuleContext {
            now: jiff::Timestamp::UNIX_EPOCH,
            samples: &[],
            events: &events,
            baselines: &[],
            network_scope: &scope,
        };
        let rule: &dyn Rule = &EgressFlowRule;
        let checked = run_rules(&[rule], &ctx);
        let finding = checked.into_iter().next().expect("EgressFlowRule löst aus");
        triage(finding, Verdict::Confirmed)
    }

    fn actor() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "operator-1".to_string(),
        }
    }

    #[test]
    fn test_propose_carries_the_given_action_unchanged() {
        let action = ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        };
        let proposed = Action::propose(action.clone());
        assert_eq!(proposed.proposed(), &action);
    }

    #[test]
    fn test_authorize_rejects_unconfirmed_verdict() {
        let finding = {
            let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
            let events = vec![SecurityEvent {
                sensor: SensorId::from_str("net-0"),
                observed_at: jiff::Timestamp::UNIX_EPOCH,
                actor: None,
                kind: EventKind::EgressFlow {
                    destination: "evil.example.com".to_owned(),
                    port: 443,
                },
            }];
            let ctx = RuleContext {
                now: jiff::Timestamp::UNIX_EPOCH,
                samples: &[],
                events: &events,
                baselines: &[],
                network_scope: &scope,
            };
            let rule: &dyn Rule = &EgressFlowRule;
            let checked = run_rules(&[rule], &ctx);
            let finding = checked.into_iter().next().expect("EgressFlowRule löst aus");
            triage(finding, Verdict::NeedsReview)
        };
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let err = proposed
            .authorize(&finding, EscalationStage::RuleTriggered, actor(), Timestamp::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(err, EscalateError::VerdictNotConfirmed));
    }

    #[test]
    fn test_authorize_rejects_action_not_admissible_at_stage() {
        let finding = confirmed_finding();
        let proposed = Action::propose(ProposedAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let err = proposed
            .authorize(&finding, EscalationStage::RuleTriggered, actor(), Timestamp::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(err, EscalateError::NotAdmissibleAtStage));
    }

    #[test]
    fn test_authorize_succeeds_and_binds_the_exact_action() {
        let finding = confirmed_finding();
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let authorized = proposed
            .authorize(&finding, EscalationStage::RuleTriggered, actor(), Timestamp::UNIX_EPOCH)
            .expect("verdict confirmed and admissible at rule-triggered");

        let request = authorized.request();
        assert!(request.proof.verify(finding.id(), &request.action).is_ok());
    }

    /// Der wichtigste Test dieser Crate: ein Beleg, der an eine andere Aktion
    /// gebunden ist, wird zurückgewiesen.
    #[test]
    fn test_authorized_proof_rejects_a_different_action() {
        let finding = confirmed_finding();
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let authorized = proposed
            .authorize(&finding, EscalationStage::RuleTriggered, actor(), Timestamp::UNIX_EPOCH)
            .expect("authorization succeeds");
        let request = authorized.into_request();

        let different_action = WardenAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-9").unwrap(),
        };
        assert!(request.proof.authorizes(finding.id(), &different_action).is_err());

        let different_kind = WardenAction::IsolateNetwork {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        };
        assert!(request.proof.authorizes(finding.id(), &different_kind).is_err());
    }
}
