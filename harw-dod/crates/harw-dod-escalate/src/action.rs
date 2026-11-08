//! Der Typestate-Kern dieser Crate: [`Action<S>`], [`Proposed`],
//! [`Authorized`], [`AuthorizedAction`] und `authorize` — der einzige Ort, an
//! dem eine Aktion als autorisiert gilt (Invariante S1).
//!
//! # Warum `Action<S>` `ProposedAction` bzw. [`AuthorizedAction`] wrappt,
//! statt eigene Datenfelder zu erfinden
//! Diese Crate erzeugt keine neue Aktionstaxonomie — die Aktionen
//! (`FreezeCgroup`, `ThawCgroup`, `IsolateNetwork`, `LiftIsolation`,
//! `KillProcessTree`) sind in `harw-dod-warden-types` (PL-100 WP 3.0)
//! vollständig deklariert. `Action<Proposed>` wrappt eine [`ProposedAction`]:
//! ein Vorschlag, der (noch) keine Freigabe trägt. `Action<Authorized>` wrappt
//! einen [`AuthorizedAction`]: den **unsignierten**, typisierten Inhalt einer
//! Freigabe — Befund, Stufe, Freigebender, Zeitpunkt und die exakte
//! [`WardenAction`]. Der Typestate-Übergang dazwischen ist
//! [`Action::authorize`].
//!
//! # Diese Crate signiert nichts (PL-100 WP 3.0, Leitprinzip I8)
//! Bis WP 3.0 baute `authorize` einen v1-`AuthorizationProof` und eine
//! `WardenActionRequest`-Hülle. Beide waren fälschbar (F-001) und werden von
//! keinem Warden mehr angenommen. Seitdem hängt diese Crate nicht mehr am
//! Signierteil (`harw-dod-warden-proto`), sondern nur am signierfreien
//! Vokabular (`harw-dod-warden-types`). [`AuthorizedAction`] hat bewusst
//! **keine Wire-Form** (kein `Serialize`/`Deserialize`), keinen MAC und keine
//! Nonce: er ist die geprüfte Eingabe, aus der `harw-escalator` (WP 3.2)
//! zusammen mit dem verifizierten Ziel-Tripel (`CgroupTarget`), Hold,
//! `irreversible_ack`, Nonce und Zeitfenster den signierten v2-Auftrag
//! (`harw_dod_warden_proto::SignedAuthorization`) ausstellt. Der Warden
//! akzeptiert nur diesen signierten Auftrag; ein `Action<Authorized>` allein
//! verleiht außerhalb dieses Prozesses keine Berechtigung.
//!
//! # Warum `authorize` `pub(crate)` ist, und was das unmöglich macht
//! [`Action::authorize`] ist die einzige Funktion in dieser Crate — und
//! damit im gesamten Workspace —, die einen [`AuthorizedAction`] konstruiert.
//! Weil sie `pub(crate)` ist und [`AuthorizedAction`] nur private Felder hat,
//! kann kein Aufrufer außerhalb dieser Crate ein `Action<Authorized>` oder
//! einen [`AuthorizedAction`] herstellen: es gibt keinen öffentlichen
//! Konstruktor, keinen `From`-Impl und kein `Deserialize`, über den ein
//! Aufrufer den Typestate-Übergang umgehen könnte. Nur die hiesige
//! Konstruktion durchläuft die Prüfungen in `authorize` (Verdict,
//! stillgelegte Aktion, Zulässigkeitsmatrix). Das ist dieselbe Bewegung, die
//! `harw_dod_rules::finding` für `Finding<RuleChecked>` vormacht: **der Besitz
//! des Wertes ist die Berechtigung**, nicht ein Laufzeit-Flag, das jede
//! Konsumentin selbst prüfen müsste. Siehe die `compile_fail`-Doctests unten
//! für den Beleg.
//!
//! # Warum der Besitz eines `Finding<RuleChecked>` — vermittelt über
//! `Finding<Triaged>` — die Berechtigung *ist*
//! [`Action::authorize`] verlangt ein `&Finding<Triaged>`. Diesen Typ kann
//! ausschließlich `harw-dod-rules` herstellen: `Finding::raw` und
//! `Finding::check` sind dort `pub(crate)`, und `harw_dod_rules::triage_record`
//! baut es ausschließlich aus einem selbst geprüften `FindingRecord`
//! (siehe `harw-dod-rules/src/finding.rs`-Moduldoku, Abschnitt „Der Besitz
//! des Wertes ist die Berechtigung"). Wer hier ein `Finding<Triaged>`
//! vorweisen kann, hat also zwangsläufig zuerst ein `Finding<RuleChecked>`
//! **besessen** — kein Aufrufer kann sich eines fälschen, um `authorize` zu
//! erreichen. Diese Crate trägt selbst keine Regel, die entscheidet, ob ein
//! Befund echt ist; sie verlässt sich vollständig auf diesen bereits
//! durchgesetzten Besitz.
//!
//! # Nebenläufigkeit
//! Reine Werttypen ohne innere Veränderlichkeit; `Send + Sync`. Keine
//! Systemuhr: `authorized_at` kommt immer vom Aufrufer.
//!
//! # Fehler
//! [`EscalateError::VerdictNotConfirmed`], [`EscalateError::RetiredAction`],
//! [`EscalateError::NotAdmissibleAtStage`] — siehe [`Action::authorize`].
//!
//! # Examples
//! ```rust
//! use harw_dod_escalate::Action;
//! use harw_dod_warden_types::ProposedAction;
//! use harw_types::CgroupId;
//!
//! let Ok(cgroup) = CgroupId::try_from_str("harw.slice/job-1") else { return };
//! let proposed = Action::propose(ProposedAction::FreezeCgroup { cgroup });
//! assert!(matches!(proposed.proposed(), ProposedAction::FreezeCgroup { .. }));
//! ```

use std::marker::PhantomData;

use harw_dod_rules::{Finding, Triaged, Verdict};
use harw_dod_warden_types::{EscalationStage, ProposedAction, WardenAction};
use harw_types::{ApprovalActor, FindingId};
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

/// Zustandsmarker: eine autorisierte Aktion, deren unsignierter Inhalt als
/// [`AuthorizedAction`] vorliegt.
///
/// # Description
/// Außerhalb dieser Crate nicht herstellbar — siehe Moduldoku, Abschnitt
/// „Warum `authorize` `pub(crate)` ist". Kein versandbereiter Auftrag: den
/// signierten v2-Auftrag stellt erst `harw-escalator` aus (Moduldoku,
/// Abschnitt „Diese Crate signiert nichts").
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
    type Inner = AuthorizedAction;
}

/// Der unsignierte, typisierte Inhalt einer autorisierten Aktion.
///
/// # Description
/// Hält genau die Fakten, die [`Action::authorize`] geprüft hat: welcher
/// Befund (`finding_id`), auf welcher Stufe der Leiter (`stage`), wer
/// freigegeben hat (`approver`), wann (`authorized_at`) und welche exakte
/// Aktion (`action`, eine Variante plus Ziel-cgroup). Die Felder sind privat
/// und es gibt keinen öffentlichen Konstruktor: nur `authorize` stellt
/// diesen Wert her (siehe Moduldoku, Abschnitt „Warum `authorize`
/// `pub(crate)` ist").
///
/// Bewusst **keine Wire-Form**, kein MAC, keine Nonce und kein Digest-Feld:
/// dieser Wert verlässt den Prozess nie direkt. `harw-escalator` (PL-100
/// WP 3.2) übernimmt ihn in den signierten v2-Auftrag
/// (`harw_dod_warden_proto::AuthorizationBody` →
/// `SignedAuthorization::sign`), ergänzt um das selbst geprüfte Ziel-Tripel,
/// Hold, `irreversible_ack`, Nonce und Zeitfenster. Die Bindung an die
/// exakte Aktion leistet dort [`WardenAction::binding_digest`].
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
///
/// # Examples
/// Außerhalb dieser Crate nicht konstruierbar (private Felder):
/// ```rust,compile_fail
/// use harw_dod_escalate::AuthorizedAction;
/// use harw_dod_warden_types::{EscalationStage, WardenAction};
/// use harw_types::{ApprovalActor, CgroupId, FindingId};
///
/// fn forge(finding_id: FindingId, cgroup: CgroupId) -> AuthorizedAction {
///     AuthorizedAction {
///         finding_id,
///         stage: EscalationStage::Escalated,
///         approver: ApprovalActor::Operator { id: "operator-1".to_owned() },
///         authorized_at: jiff::Timestamp::UNIX_EPOCH,
///         action: WardenAction::KillProcessTree { cgroup },
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedAction {
    // Der auslösende, triagierte Befund.
    finding_id: FindingId,
    // Die von der Leiter ermittelte Stufe, ab der `action` zulässig ist.
    stage: EscalationStage,
    // Wer freigegeben hat.
    approver: ApprovalActor,
    // Wann freigegeben wurde — injiziert, nie von dieser Crate gelesen.
    authorized_at: Timestamp,
    // Die exakte, nicht stillgelegte Aktion.
    action: WardenAction,
}

impl AuthorizedAction {
    /// Der Befund, für den freigegeben wurde.
    ///
    /// # Returns
    /// Die `FindingId` des `Finding<Triaged>`, das [`Action::authorize`]
    /// geprüft hat.
    #[must_use]
    pub fn finding_id(&self) -> &FindingId {
        &self.finding_id
    }

    /// Die Stufe der Leiter, auf der freigegeben wurde.
    ///
    /// # Returns
    /// Die an [`Action::authorize`] übergebene, dort gegen die
    /// Zulässigkeitsmatrix geprüfte [`EscalationStage`].
    #[must_use]
    pub fn stage(&self) -> EscalationStage {
        self.stage
    }

    /// Wer freigegeben hat.
    ///
    /// # Returns
    /// Den [`ApprovalActor`], den der Aufrufer von [`Action::authorize`]
    /// genannt hat. Ob er eine v2-Freigabe tragen darf (lokaler Operator, nie
    /// ein Kanal-Peer), prüft der Warden beim signierten Auftrag.
    #[must_use]
    pub fn approver(&self) -> &ApprovalActor {
        &self.approver
    }

    /// Wann freigegeben wurde.
    ///
    /// # Returns
    /// Den vom Aufrufer injizierten Zeitpunkt; diese Crate liest keine
    /// Systemuhr.
    #[must_use]
    pub fn authorized_at(&self) -> Timestamp {
        self.authorized_at
    }

    /// Die exakte, freigegebene Aktion.
    ///
    /// # Returns
    /// Die aus dem Vorschlag übernommene [`WardenAction`] — nie eine
    /// stillgelegte Variante ([`WardenAction::is_retired`]) und immer ab
    /// [`Self::stage`] zulässig.
    #[must_use]
    pub fn action(&self) -> &WardenAction {
        &self.action
    }
}

/// Eine Aktion im Zustand `S` — vorgeschlagen oder autorisiert.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung. `Action<Proposed>` wrappt
/// eine [`ProposedAction`]; `Action<Authorized>` wrappt den unsignierten
/// [`AuthorizedAction`].
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
        f.debug_struct("Action")
            .field("inner", &self.inner)
            .finish()
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
    /// use harw_dod_warden_types::ProposedAction;
    /// use harw_types::CgroupId;
    ///
    /// let Ok(cgroup) = CgroupId::try_from_str("cgroup-1") else { return };
    /// let proposed = Action::propose(ProposedAction::FreezeCgroup {
    ///     cgroup: cgroup.clone(),
    /// });
    /// assert_eq!(
    ///     proposed.proposed(),
    ///     &ProposedAction::FreezeCgroup { cgroup }
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
    /// diesem Workspace, die einen [`AuthorizedAction`] erzeugt (S1).**
    /// `pub(crate)`: siehe Moduldoku, Abschnitt „Warum `authorize`
    /// `pub(crate)` ist".
    ///
    /// # Description
    /// Prüft in dieser Reihenfolge: (1) `finding.verdict()` muss
    /// [`Verdict::Confirmed`] sein — ein Fehlalarm oder ein ungeklärter
    /// Befund autorisiert nichts; (2) die Aktion darf nicht stillgelegt sein
    /// ([`WardenAction::is_retired`], heute `ReleaseCgroup`) — der Warden
    /// lehnt sie im v2-Auftrag ohnehin ab, hier fällt die Ablehnung schon vor
    /// jeder Freigabe; (3) die Aktion muss ab `stage` laut
    /// [`crate::ladder::Ladder::admissible`] zulässig sein. Erst danach
    /// entsteht der unsignierte [`AuthorizedAction`]. Signiert nichts und
    /// liest keine Systemuhr — `authorized_at` kommt vom Aufrufer.
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
    /// Ein `Action<Authorized>`, das den unsignierten [`AuthorizedAction`]
    /// trägt.
    ///
    /// # Errors
    /// - [`EscalateError::VerdictNotConfirmed`]: `finding.verdict()` ist
    ///   nicht [`Verdict::Confirmed`].
    /// - [`EscalateError::RetiredAction`]: die Aktion ist stillgelegt.
    /// - [`EscalateError::NotAdmissibleAtStage`]: die Aktion ist ab `stage`
    ///   nicht zulässig.
    ///
    /// # Examples
    /// Von außerhalb dieser Crate nicht aufrufbar, da `pub(crate)`:
    /// ```rust,compile_fail
    /// use harw_dod_escalate::Action;
    /// use harw_dod_rules::{Finding, Triaged};
    /// use harw_dod_warden_types::EscalationStage;
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
        let action = WardenAction::from(self.inner);
        if action.is_retired() {
            return Err(EscalateError::RetiredAction);
        }
        if !Ladder::admissible(&action, stage) {
            return Err(EscalateError::NotAdmissibleAtStage);
        }
        Ok(Action {
            inner: AuthorizedAction {
                finding_id: finding.id().clone(),
                stage,
                approver: authorized_by,
                authorized_at,
                action,
            },
            _state: PhantomData,
        })
    }
}

impl Action<Authorized> {
    /// Der unsignierte Inhalt der Freigabe.
    ///
    /// # Returns
    /// Eine Referenz auf den [`AuthorizedAction`], den
    /// [`Action::authorize`] erzeugt hat.
    #[must_use]
    pub fn body(&self) -> &AuthorizedAction {
        &self.inner
    }

    /// Konsumiert `self` und liefert den unsignierten Inhalt der Freigabe.
    ///
    /// # Returns
    /// Den [`AuthorizedAction`], etwa zur Übergabe an den Signierer
    /// (`harw-escalator`, PL-100 WP 3.2).
    #[must_use]
    pub fn into_body(self) -> AuthorizedAction {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::NetworkScope;
    use harw_dod_rules::rule::{Rule, RuleContext};
    use harw_dod_rules::rules::EgressFlowRule;
    use harw_dod_rules::{run_rules, triaged_finding_for_test};
    use harw_dod_signals::{EventKind, SecurityEvent};
    // `FindingId` kommt über `use super::*` aus dem Modulkopf.
    use harw_types::{CgroupId, SensorId};

    // Die Testfaelle unten binden `RuleContext`-Werte an den lokalen Namen
    // `ctx` (Vorbild fuer echten Aufrufcode) -- der Test-Helfer `ctx()` wird
    // dort deshalb voll qualifiziert aufgerufen, um die Verschattung zu
    // vermeiden (siehe Worker-Zusatz).

    /// Wertet `EgressFlowRule` ueber die oeffentliche Regel-Pipeline aus und
    /// triagiert den ausgeloesten Befund mit `verdict`.
    ///
    /// `run_rules` liefert seit der Umstellung `Finding<Raw>`; der Uebergang
    /// `Raw -> RuleChecked` ist in `harw-dod-rules` `pub(crate)`. Der Test
    /// traegt die Felder des echten Regelbefundes deshalb ueber die dafuer
    /// vorgesehene Testhilfe `triaged_finding_for_test` (Feature
    /// `test-support`) in einen `Finding<Triaged>` -- derselbe interne Pfad
    /// `Finding::raw(..).check(id)` plus `triage`.
    fn triaged_from_rule(verdict: Verdict) -> TestResult<Finding<Triaged>> {
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
            listener_history: None,
            rule_config: None,
        };
        let raw = run_rules(&ctx)
            .into_iter()
            .find(|finding| finding.rule_id() == EgressFlowRule.id())
            .ok_or(TestError::Missing("EgressFlowRule löst aus"))?;
        Ok(triaged_finding_for_test(
            raw.rule_id(),
            raw.kind(),
            raw.severity(),
            raw.hardness(),
            raw.summary(),
            raw.observed_at(),
            FindingId::new(),
            verdict,
        ))
    }

    fn confirmed_finding() -> TestResult<Finding<Triaged>> {
        triaged_from_rule(Verdict::Confirmed)
    }

    fn actor() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "operator-1".to_string(),
        }
    }

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(crate::test_support::ctx("CgroupId parsen"))
    }

    #[test]
    fn test_propose_carries_the_given_action_unchanged() -> TestResult {
        let action = ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("CgroupId parsen"))?,
        };
        let proposed = Action::propose(action.clone());
        assert_eq!(proposed.proposed(), &action);
        Ok(())
    }

    #[test]
    fn test_authorize_rejects_unconfirmed_verdict() -> TestResult {
        let finding = triaged_from_rule(Verdict::NeedsReview)?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1")
                .map_err(crate::test_support::ctx("CgroupId parsen"))?,
        });

        let result = proposed.authorize(
            &finding,
            EscalationStage::RuleTriggered,
            actor(),
            Timestamp::UNIX_EPOCH,
        );
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::VerdictNotConfirmed));
        Ok(())
    }

    #[test]
    fn test_authorize_rejects_action_not_admissible_at_stage() -> TestResult {
        let finding = confirmed_finding()?;
        let proposed = Action::propose(ProposedAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("CgroupId parsen"))?,
        });

        let result = proposed.authorize(
            &finding,
            EscalationStage::RuleTriggered,
            actor(),
            Timestamp::UNIX_EPOCH,
        );
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAdmissibleAtStage));
        Ok(())
    }

    /// `LiftIsolation` steht wie `IsolateNetwork` erst ab `Escalated` offen
    /// (PL-100 WP 3.1); `ThawCgroup` schon ab `RuleTriggered`.
    #[test]
    fn test_authorize_follows_the_thaw_and_lift_rows_of_the_matrix() -> TestResult {
        let finding = confirmed_finding()?;

        let lift = Action::propose(ProposedAction::LiftIsolation {
            cgroup: cgroup("cgroup-1")?,
        });
        let Err(err) = lift.authorize(
            &finding,
            EscalationStage::RuleTriggered,
            actor(),
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAdmissibleAtStage));

        let thaw = Action::propose(ProposedAction::ThawCgroup {
            cgroup: cgroup("cgroup-1")?,
        });
        let authorized = thaw
            .authorize(
                &finding,
                EscalationStage::RuleTriggered,
                actor(),
                Timestamp::UNIX_EPOCH,
            )
            .map_err(ctx("thaw is admissible from rule-triggered"))?;
        assert_eq!(
            authorized.body().action(),
            &WardenAction::ThawCgroup {
                cgroup: cgroup("cgroup-1")?
            }
        );
        Ok(())
    }

    /// Ablehnungstest für die stillgelegte v1-Variante: `ReleaseCgroup` wird
    /// nie autorisiert, auch nicht bei bestätigtem Befund auf der höchsten
    /// Stufe.
    #[test]
    fn test_authorize_refuses_the_retired_release_action() -> TestResult {
        let finding = confirmed_finding()?;
        // `ReleaseCgroup` ist seit PL-100 WP 3.1 `#[deprecated]`; dieser Test
        // nennt die Variante nur, um ihre Ablehnung zu prüfen.
        #[allow(deprecated)]
        let retired = ProposedAction::ReleaseCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let proposed = Action::propose(retired);

        let Err(err) = proposed.authorize(
            &finding,
            EscalationStage::Escalated,
            actor(),
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::RetiredAction));
        Ok(())
    }

    #[test]
    fn test_authorize_succeeds_and_binds_the_exact_action() -> TestResult {
        let finding = confirmed_finding()?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("CgroupId parsen"))?,
        });
        let authorized_at = Timestamp::UNIX_EPOCH
            .checked_add(jiff::SignedDuration::from_secs(42))
            .map_err(ctx("checked_add"))?;

        let authorized = proposed
            .authorize(
                &finding,
                EscalationStage::RuleTriggered,
                actor(),
                authorized_at,
            )
            .map_err(ctx("verdict confirmed and admissible at rule-triggered"))?;

        let body = authorized.body();
        assert_eq!(body.finding_id(), finding.id());
        assert_eq!(body.stage(), EscalationStage::RuleTriggered);
        assert_eq!(body.approver(), &actor());
        assert_eq!(body.authorized_at(), authorized_at);
        assert_eq!(
            body.action(),
            &WardenAction::FreezeCgroup {
                cgroup: cgroup("cgroup-1")?
            }
        );
        Ok(())
    }

    /// Der wichtigste Test dieser Crate: der freigegebene Inhalt ist an die
    /// exakte Aktion gebunden — eine andere cgroup oder eine andere Variante
    /// ergibt einen anderen Bindungsdigest, den der Signierer in den v2-MAC
    /// übernimmt.
    #[test]
    fn test_authorized_body_binds_only_the_proposed_action() -> TestResult {
        let finding = confirmed_finding()?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("CgroupId parsen"))?,
        });

        let authorized = proposed
            .authorize(
                &finding,
                EscalationStage::RuleTriggered,
                actor(),
                Timestamp::UNIX_EPOCH,
            )
            .map_err(ctx("authorization succeeds"))?;
        let body = authorized.into_body();

        let different_action = WardenAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-9").map_err(ctx("CgroupId parsen"))?,
        };
        assert_ne!(body.action(), &different_action);
        assert_ne!(
            body.action().binding_digest(),
            different_action.binding_digest()
        );

        let different_kind = WardenAction::IsolateNetwork {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("CgroupId parsen"))?,
        };
        assert_ne!(body.action(), &different_kind);
        assert_ne!(
            body.action().binding_digest(),
            different_kind.binding_digest()
        );
        Ok(())
    }

    /// Ob eine Nicht-Kommentarzeile von `manifest` den Paketnamen
    /// `forbidden` nennt (auch in Unterstrich-Schreibweise). Bewusst grob:
    /// jede Schreibweise einer Abhängigkeit (Schlüssel, Tabellenform
    /// `[dependencies.<name>]`, gequoteter Schlüssel, `[target.'…']`-Tabelle,
    /// `package = …` mit beliebigen Leerzeichen und Anführungszeichen) trägt
    /// den Namen wörtlich in einer solchen Zeile. Ein Fehlalarm (etwa ein
    /// Name in einem Zeilenend-Kommentar) ist billiger als ein Durchschlupf.
    fn manifest_mentions(manifest: &str, forbidden: &str) -> bool {
        let underscored = forbidden.replace('-', "_");
        manifest
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with('#'))
            .any(|line| line.contains(forbidden) || line.contains(underscored.as_str()))
    }

    /// Der Detektor `manifest_mentions` erkennt jede Schreibweise einer
    /// Abhängigkeit und überspringt reine Kommentarzeilen.
    #[test]
    fn test_manifest_mentions_detects_every_dependency_spelling() {
        let name = "harw-dod-warden-proto";
        for spelling in [
            "harw-dod-warden-proto = { path = \"../x\" }",
            "[dependencies.harw-dod-warden-proto]",
            "\"harw-dod-warden-proto\" = { path = \"../x\" }",
            "[target.'cfg(unix)'.dependencies.harw-dod-warden-proto]",
            "[target.'cfg(unix)'.build-dependencies]\nsigning = { package = \"harw-dod-warden-proto\" }",
            "signing = { package='harw-dod-warden-proto', path = \"../x\" }",
            "signing = {package=\"harw-dod-warden-proto\"}",
            "    harw_dod_warden_proto = { path = \"../x\" }",
        ] {
            let manifest = format!("[package]\nname = \"x\"\n\n[dependencies]\n{spelling}\n");
            assert!(manifest_mentions(&manifest, name), "{spelling}");
        }
        assert!(!manifest_mentions(
            "# nie harw-dod-warden-proto\n  # auch nicht harw_dod_warden_proto\n",
            name
        ));
    }

    /// PL-100 WP 3.0 (Leitprinzip I8): diese Crate signiert nichts und nennt
    /// deshalb den Signierteil `harw-dod-warden-proto` (und den künftigen
    /// `harw-escalator`) in keiner Zeile ihres Manifests außer Kommentaren —
    /// weder als Schlüssel noch umbenannt (`package = …`), in Tabellenform,
    /// unter `[target.'…']` oder als Dev-Abhängigkeit. Die transitive Hülle
    /// prüft das xtask-Gate `arch`.
    #[test]
    fn test_manifest_names_no_signing_crate() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in ["harw-dod-warden-proto", "harw-escalator"] {
            assert!(
                !manifest_mentions(manifest, forbidden),
                "Cargo.toml nennt {forbidden}"
            );
        }
    }
}
