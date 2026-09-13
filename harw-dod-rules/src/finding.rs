//! `Finding<S>`: der Typestate-Kern dieser Crate — eine Beobachtung wird erst
//! hier zu einem Befund (Knoten AW4-03, Contract-Master §G.1).
//!
//! # Warum `Finding<S>` hier lebt, und nicht in `harw-dod-signals`
//! Der ursprüngliche Arbeitsplan legte `Finding<Raw|RuleChecked|Triaged>`
//! nach `harw-dod-signals`, mit `pub(crate)`-Konstruktoren dort. Das ist
//! unbaubar: das Typestate-Muster funktioniert nur, wenn Typbesitzer und
//! Übergangsbesitzer dieselbe Crate sind — ein `pub(crate)`-Konstruktor ist
//! per Definition nur innerhalb seiner eigenen Crate sichtbar. Läge
//! `Finding<S>` in `harw-dod-signals`, wären [`Finding::raw`] und
//! [`Finding::check`] für die elf Sensor-Crates, die `harw-dod-signals`
//! *einzeln* importieren, unerreichbar — und genau diese Unerreichbarkeit
//! ist der ganze Witz: **Sensoren minten nie einen Befund.** Sie liefern
//! `HostSample` und `SecurityEvent` (siehe `harw_dod_signals::sensor`); ein
//! `Finding` entsteht ausschließlich hier, aus einer [`crate::rule::Rule`]-
//! Auswertung.
//!
//! # Der Besitz des Wertes ist die Berechtigung
//! Dieselbe Frage ließe sich auch mit einem Laufzeit-Check beantworten: ein
//! `bool checked`-Feld, das jede Konsumentin selbst prüfen müsste, bevor sie
//! einen Befund eskaliert. Dieses Projekt hat diese Bewegung an anderer
//! Stelle bereits einmal verpasst — aus einer Zusage wurde dort ein
//! Laufzeit-Check statt einer Unausdrückbarkeit, und die Lücke blieb lange
//! unentdeckt, weil ein grüner Test nichts darüber aussagt, ob der Check an
//! *jeder* Stelle auch wirklich aufgerufen wurde. Der Typestate hier macht
//! denselben Fehler unmöglich, statt ihn nur unwahrscheinlich zu machen:
//! [`Finding::raw`] und [`Finding::check`] sind `pub(crate)`, also kann ein
//! `Finding<RuleChecked>` außerhalb dieser Crate schlicht nicht *existieren*
//! — kein Reviewer muss nachsehen, ob irgendwo ein Check vergessen wurde,
//! weil es keinen Weg gibt, das Feld zu fälschen. `harw-dod-escalate`
//! (Knoten AW5-03) bekommt ein `Finding<Triaged>` nur, indem es ein
//! `Finding<RuleChecked>` **besitzt**, das ausschließlich diese Crate
//! herstellen kann; [`triage`] konsumiert diesen Besitz und gibt ihn als
//! `Finding<Triaged>` zurück.
//!
//! # Reinheitsauflage
//! [`Finding::raw`] führt kein I/O aus, liest keine Systemuhr und ruft
//! keinen Zufallsgenerator auf — jedes Feld kommt entweder vom Aufrufer
//! (typischerweise einer [`crate::rule::Rule`], die `ctx.now` injiziert
//! bekommt) oder ist eine reine Berechnung daraus. Das ist absichtlich: eine
//! `Finding<Raw>`-Konstruktion, die bei gleicher Eingabe unterschiedliche
//! Ausgaben liefert, ließe sich gegen kein festes Fixture mehr prüfen (siehe
//! [`crate::rule::Rule`]-Moduldoku). Die einzige Stelle dieser Crate, die
//! einen Zufallswert erzeugt, ist [`crate::engine::run_rules`] — sie liegt
//! bewusst *hinter* der Regelauswertung, in der reinen Verdrahtung, nicht in
//! einer `Rule::evaluate`-Implementierung.
//!
//! # Die Baseline-Regel
//! [`FindingKind`] unterscheidet, ob ein Befund seine Existenz einer
//! ausgelösten Regel (`RuleTriggered`) oder einer bloßen Abweichung
//! (`Anomaly`) verdankt. Welche Art eine [`crate::baseline::Baseline`]
//! zulässt, entscheidet [`crate::baseline::finding_kind_for_status`] anhand
//! ihres `PalaceStatus`: eine `Established`-Baseline darf `RuleTriggered`
//! auslösen, eine `Provisional`-Baseline nur `Anomaly`. Der Unterschied ist
//! der ganze Zweck des Feldes — eine unbestätigte Beobachtung darf keine
//! Eskalation auslösen, sonst wird die Eskalationsleiter durch Rauschen
//! entwertet (siehe [`crate::rules::baseline_deviation`] für eine Regel, die
//! das durchsetzt).
//!
//! # Nebenläufigkeit
//! `Finding<S>` ist ein reiner Wert ohne innere Veränderlichkeit: alle
//! Felder sind `Send + Sync` (`&'static str`, die Enums aus
//! `harw_dod_signals`, `String`, `jiff::Timestamp`, sowie `S::Identity`/
//! `S::Outcome`, die je Zustand entweder `()` oder ein einfacher, reiner
//! Werttyp sind). `Finding<S>` selbst ist damit `Send + Sync`, beliebig
//! zwischen Threads teilbar.
//!
//! # Fehler
//! Keine. Jede Operation in diesem Modul ist total: [`Finding::raw`],
//! [`Finding::check`] und [`triage`] können nicht fehlschlagen, weil sie
//! reine Umformungen bereits vorhandener, gültiger Werte sind.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::{Verdict, triage};
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::EgressFlowRule;
//! use harw_sandbox::NetworkScope;
//!
//! let scope = NetworkScope::empty();
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &[],
//!     baselines: &[],
//!     network_scope: &scope,
//! };
//! let raw = EgressFlowRule.evaluate(&ctx);
//! assert!(raw.is_empty(), "kein Ereignis, kein Befund");
//! ```

use std::marker::PhantomData;

use harw_dod_signals::{Hardness, Severity};
use harw_types::FindingId;
use jiff::Timestamp;

// Versiegeltes Supertrait: nur Typen aus diesem Modul dürfen `FindingState`
// implementieren. Verhindert, dass eine fremde Crate einen vierten Zustand
// erfindet, für den `Finding<S>` nicht vorgesehen ist (Vorbild:
// `harw_dod_cap::handle::sealed`).
mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Raw {}
    impl Sealed for super::RuleChecked {}
    impl Sealed for super::Triaged {}
}

/// Zustandsmarker: ein frisch von einer Regel geminteter, noch nicht
/// zertifizierter Befund.
///
/// # Description
/// Trägt keine Identität (`S::Identity = ()`) und kein Triage-Ergebnis
/// (`S::Outcome = ()`) — siehe Moduldoku für die Begründung, warum die
/// Identität erst bei [`Finding::check`] entsteht.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::Raw;
///
/// let _marker = Raw;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raw;

/// Zustandsmarker: ein von dieser Crate zertifizierter Befund mit stabiler
/// Identität ([`harw_types::FindingId`]).
///
/// # Description
/// Außerhalb dieser Crate nicht herstellbar: der einzige Weg zu diesem
/// Zustand ist [`Finding::check`], `pub(crate)`. Siehe Moduldoku, Abschnitt
/// „Der Besitz des Wertes ist die Berechtigung“.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::RuleChecked;
///
/// let _marker = RuleChecked;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleChecked;

/// Zustandsmarker: ein triagierter Befund mit [`Verdict`].
///
/// # Description
/// Entsteht ausschließlich über [`triage`], die einen `Finding<RuleChecked>`
/// konsumiert. `harw-dod-escalate` (Knoten AW5-03) bekommt ein
/// `Finding<Triaged>` nur auf diesem Weg.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::Triaged;
///
/// let _marker = Triaged;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Triaged;

/// Internes Bindeglied zwischen Zustands-Marker und den Datenfeldern, die
/// genau dieser Zustand trägt (Vorbild: `harw_dod_cap::handle::HandleState`).
///
/// # Description
/// `Identity` ist `()` in [`Raw`] und [`FindingId`] ab [`RuleChecked`];
/// `Outcome` ist `()` bis [`RuleChecked`] und [`Verdict`] erst in
/// [`Triaged`]. Das hält [`Finding`] frei von `Option`/`unwrap`: die
/// Abwesenheit einer Identität oder eines Triage-Ergebnisses ist eine
/// Typ-Eigenschaft, die der Compiler durchsetzt, kein Laufzeit-Sonderfall.
/// Nicht Teil der `pub`-Fläche dieser Crate im engeren Sinne — deshalb
/// `#[doc(hidden)]` und nicht als Bound-Ziel gedacht; öffentlich nur, weil
/// ein privates Trait als Bound auf einem `pub`-Typ sonst einen
/// Sichtbarkeits-Lint auslöst (`private_bounds`).
#[doc(hidden)]
pub trait FindingState: sealed::Sealed {
    /// Zustandsabhängige Identität dieses Befundes.
    type Identity: std::fmt::Debug + Clone + PartialEq;
    /// Zustandsabhängiges Triage-Ergebnis dieses Befundes.
    type Outcome: std::fmt::Debug + Clone + PartialEq;
}

impl FindingState for Raw {
    type Identity = ();
    type Outcome = ();
}

impl FindingState for RuleChecked {
    type Identity = FindingId;
    type Outcome = ();
}

impl FindingState for Triaged {
    type Identity = FindingId;
    type Outcome = Verdict;
}

/// Womit ein Befund seine Existenz rechtfertigt.
///
/// # Description
/// Die Baseline-Regel (siehe Moduldoku) hängt an dieser Unterscheidung:
/// `RuleTriggered` darf eskaliert werden, `Anomaly` ist zunächst nur eine
/// Beobachtung zur Kenntnisnahme. Geschlossen: eine dritte Art würde die
/// Baseline-Regel unterlaufen, die genau diese zwei Fälle unterscheidet.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::FindingKind;
///
/// assert_ne!(FindingKind::RuleTriggered, FindingKind::Anomaly);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// Eine Regel hat eindeutig ausgelöst — eskalationswürdig.
    RuleTriggered,
    /// Eine bloße Abweichung, z. B. gegen eine `Provisional`-Baseline — zur
    /// Kenntnisnahme, nicht zur Eskalation.
    Anomaly,
}

/// Ergebnis der Triage-Entscheidung über einen geprüften Befund.
///
/// # Description
/// Von [`triage`] auf einen `Finding<RuleChecked>` angewendet. Geschlossen:
/// jede Triage endet in genau einem dieser drei Zustände, nie in einem
/// vierten.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::Verdict;
///
/// assert_ne!(Verdict::Confirmed, Verdict::FalsePositive);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Der Befund ist bestätigt und eskalationswürdig.
    Confirmed,
    /// Der Befund ist ein Fehlalarm; keine Eskalation.
    FalsePositive,
    /// Unklar; braucht eine menschliche Prüfung, bevor eskaliert wird.
    NeedsReview,
}

/// Ein Sicherheitsbefund im Zustand `S`.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung des Typestate-Musters.
/// Die gemeinsamen Felder (`rule_id` .. `observed_at`) sind in jedem Zustand
/// vorhanden und deshalb `pub`; die zustandsabhängigen Felder (Identität,
/// Triage-Ergebnis) sind privat und nur über zustandsspezifische Methoden
/// erreichbar (siehe `impl Finding<RuleChecked>`, `impl Finding<Triaged>`).
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// Siehe Moduldoku für ein vollständiges Beispiel über
/// [`crate::rules::EgressFlowRule`].
pub struct Finding<S: FindingState> {
    /// Die [`crate::rule::Rule::id`] der Regel, die diesen Befund erzeugt hat.
    pub rule_id: &'static str,
    /// Womit dieser Befund seine Existenz rechtfertigt.
    pub kind: FindingKind,
    /// Wie schwer dieser Befund wiegt.
    pub severity: Severity,
    /// Wie hart der Nachweis hinter diesem Befund ist.
    pub hardness: Hardness,
    /// Menschenlesbare Zusammenfassung.
    pub summary: String,
    /// Injizierte Zeit, zu der die auslösende Regel ausgewertet wurde — nie
    /// die Systemuhr der Regel selbst.
    pub observed_at: Timestamp,
    identity: S::Identity,
    outcome: S::Outcome,
    _state: PhantomData<S>,
}

// Manuell statt `#[derive(Debug)]`: eine generisch gebundene Struktur
// (`struct Finding<S: FindingState>`) bekäme von `derive(Debug)` nur die
// Bound `S: Debug`, nicht die hier tatsächlich nötige `S::Identity: Debug` /
// `S::Outcome: Debug` — der generierte Impl würde also nicht kompilieren
// (identisches Problem und identische Lösung wie
// `harw_dod_cap::handle::SensorHandle`, das dieselben Bounds trotz bereits
// vorhandener Supertrait-Bounds auf `HandleState::Scope` explizit wiederholt
// — dieser Code folgt demselben, geprüften Muster).
impl<S: FindingState> std::fmt::Debug for Finding<S>
where
    S::Identity: std::fmt::Debug,
    S::Outcome: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Finding")
            .field("rule_id", &self.rule_id)
            .field("kind", &self.kind)
            .field("severity", &self.severity)
            .field("hardness", &self.hardness)
            .field("summary", &self.summary)
            .field("observed_at", &self.observed_at)
            .field("identity", &self.identity)
            .field("outcome", &self.outcome)
            .finish()
    }
}

// Aus demselben Grund manuell statt `#[derive(Clone)]`.
impl<S: FindingState> Clone for Finding<S>
where
    S::Identity: Clone,
    S::Outcome: Clone,
{
    fn clone(&self) -> Self {
        Self {
            rule_id: self.rule_id,
            kind: self.kind,
            severity: self.severity,
            hardness: self.hardness,
            summary: self.summary.clone(),
            observed_at: self.observed_at,
            identity: self.identity.clone(),
            outcome: self.outcome.clone(),
            _state: PhantomData,
        }
    }
}

// Aus demselben Grund manuell statt `#[derive(PartialEq)]`.
impl<S: FindingState> PartialEq for Finding<S>
where
    S::Identity: PartialEq,
    S::Outcome: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.rule_id == other.rule_id
            && self.kind == other.kind
            && self.severity == other.severity
            && self.hardness == other.hardness
            && self.summary == other.summary
            && self.observed_at == other.observed_at
            && self.identity == other.identity
            && self.outcome == other.outcome
    }
}

impl Finding<Raw> {
    /// Mintet einen rohen Befund. `pub(crate)`: nur Regeln innerhalb dieser
    /// Crate dürfen einen `Finding<Raw>` erzeugen.
    ///
    /// # Description
    /// Reine Funktion: kein Feld entsteht aus I/O, Systemuhr oder Zufall —
    /// siehe Moduldoku „Reinheitsauflage“. `observed_at` muss vom Aufrufer
    /// (typischerweise `ctx.now` in einer [`crate::rule::Rule::evaluate`]-
    /// Implementierung) injiziert werden.
    ///
    /// # Arguments
    /// - `rule_id` (`&'static str`): die [`crate::rule::Rule::id`] der
    ///   erzeugenden Regel.
    /// - `kind` (`FindingKind`): ob dieser Befund eskalationswürdig
    ///   (`RuleTriggered`) oder nur eine Abweichung (`Anomaly`) ist.
    /// - `severity` (`harw_dod_signals::Severity`): wie schwer der Befund
    ///   wiegt.
    /// - `hardness` (`harw_dod_signals::Hardness`): wie hart der Nachweis ist.
    /// - `summary` (`impl Into<String>`): menschenlesbare Zusammenfassung.
    /// - `observed_at` (`jiff::Timestamp`): injizierte Zeit.
    ///
    /// # Returns
    /// Einen `Finding<Raw>` ohne Identität und ohne Triage-Ergebnis.
    ///
    /// # Errors
    /// Keine — totale Konstruktion.
    ///
    /// # Examples
    /// Nicht von außerhalb dieser Crate aufrufbar — siehe den
    /// `compile_fail`-Doctest bei [`Finding::check`].
    pub(crate) fn raw(
        rule_id: &'static str,
        kind: FindingKind,
        severity: Severity,
        hardness: Hardness,
        summary: impl Into<String>,
        observed_at: Timestamp,
    ) -> Self {
        Self {
            rule_id,
            kind,
            severity,
            hardness,
            summary: summary.into(),
            observed_at,
            identity: (),
            outcome: (),
            _state: PhantomData,
        }
    }

    /// Zertifiziert einen rohen Befund mit einer stabilen Identität.
    /// `pub(crate)`: der einzige Weg zu einem `Finding<RuleChecked>`.
    ///
    /// # Description
    /// Konsumiert `self` (Typestate-Übergang): ein `Finding<Raw>` kann nach
    /// `check` nicht mehr als roher Befund verwendet werden, weil er nicht
    /// mehr existiert. Aufgerufen ausschließlich von
    /// [`crate::engine::run_rules`] — siehe dortige Moduldoku, warum die
    /// Identitätsvergabe bewusst außerhalb jeder `Rule::evaluate`-
    /// Implementierung liegt.
    ///
    /// # Arguments
    /// - `id` (`harw_types::FindingId`): die dem Befund zugewiesene, stabile
    ///   Identität.
    ///
    /// # Returns
    /// Einen `Finding<RuleChecked>` mit identischen Feldern plus `id`.
    ///
    /// # Errors
    /// Keine — totale Umformung.
    ///
    /// # Examples
    /// Von außerhalb dieser Crate nicht aufrufbar, da `pub(crate)`:
    /// ```rust,compile_fail
    /// use harw_dod_rules::{Finding, Raw};
    /// use harw_types::FindingId;
    ///
    /// fn attempt(raw: Finding<Raw>) {
    ///     // `check` ist außerhalb von `harw-dod-rules` unsichtbar.
    ///     let _checked = raw.check(FindingId::new());
    /// }
    /// ```
    pub(crate) fn check(self, id: FindingId) -> Finding<RuleChecked> {
        Finding {
            rule_id: self.rule_id,
            kind: self.kind,
            severity: self.severity,
            hardness: self.hardness,
            summary: self.summary,
            observed_at: self.observed_at,
            identity: id,
            outcome: (),
            _state: PhantomData,
        }
    }
}

impl Finding<RuleChecked> {
    /// Die stabile Identität dieses geprüften Befundes.
    ///
    /// # Returns
    /// Die bei [`Finding::check`] vergebene [`harw_types::FindingId`].
    ///
    /// # Examples
    /// Siehe [`triage`] für ein vollständiges Beispiel.
    #[must_use]
    pub fn id(&self) -> &FindingId {
        &self.identity
    }
}

impl Finding<Triaged> {
    /// Die stabile Identität dieses triagierten Befundes.
    ///
    /// # Returns
    /// Dieselbe [`harw_types::FindingId`], die der Befund bereits als
    /// `Finding<RuleChecked>` trug — `triage` erzeugt keine neue Identität.
    #[must_use]
    pub fn id(&self) -> &FindingId {
        &self.identity
    }

    /// Das Ergebnis der Triage-Entscheidung.
    ///
    /// # Returns
    /// Das bei [`triage`] übergebene [`Verdict`].
    #[must_use]
    pub fn verdict(&self) -> &Verdict {
        &self.outcome
    }
}

/// Triagiert einen geprüften Befund und liefert ihn triagiert zurück.
///
/// # Description
/// Der einzige öffentliche Weg zu einem `Finding<Triaged>`. Konsumiert
/// `finding` (Typestate-Übergang: ein `Finding<RuleChecked>` existiert
/// danach nicht mehr) und übernimmt seine Identität unverändert.
/// `harw-dod-escalate` (Knoten AW5-03) bekommt ein `Finding<Triaged>` genau
/// dadurch, dass es zuerst ein `Finding<RuleChecked>` **besitzt** — herstellbar
/// ausschließlich durch [`crate::engine::run_rules`] in dieser Crate — und es
/// dann hier übergibt.
///
/// # Arguments
/// - `finding` (`Finding<RuleChecked>`): der zu triagierende, bereits
///   geprüfte Befund. Wird konsumiert.
/// - `verdict` (`Verdict`): das Ergebnis der Triage-Entscheidung.
///
/// # Returns
/// Einen `Finding<Triaged>` mit identischen Feldern, derselben Identität und
/// dem übergebenen `Verdict`.
///
/// # Errors
/// Keine — reine, totale Umformung.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::rule::{Rule, RuleContext};
/// use harw_dod_rules::rules::EgressFlowRule;
/// use harw_dod_rules::{run_rules, triage, Verdict};
/// use harw_dod_signals::{EventKind, SecurityEvent};
/// use harw_sandbox::NetworkScope;
/// use harw_types::SensorId;
///
/// let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
/// let events = vec![SecurityEvent {
///     sensor: SensorId::from_str("net-0"),
///     observed_at: jiff::Timestamp::UNIX_EPOCH,
///     actor: None,
///     kind: EventKind::EgressFlow {
///         destination: "evil.example.com".to_owned(),
///         port: 443,
///     },
/// }];
/// let ctx = RuleContext {
///     now: jiff::Timestamp::UNIX_EPOCH,
///     samples: &[],
///     events: &events,
///     baselines: &[],
///     network_scope: &scope,
/// };
///
/// let rule: &dyn Rule = &EgressFlowRule;
/// let checked = run_rules(&[rule], &ctx);
/// let finding = checked.into_iter().next().expect("EgressFlowRule löst aus");
/// let triaged = triage(finding, Verdict::Confirmed);
/// assert_eq!(*triaged.verdict(), Verdict::Confirmed);
/// ```
#[must_use]
pub fn triage(finding: Finding<RuleChecked>, verdict: Verdict) -> Finding<Triaged> {
    Finding {
        rule_id: finding.rule_id,
        kind: finding.kind,
        severity: finding.severity,
        hardness: finding.hardness,
        summary: finding.summary,
        observed_at: finding.observed_at,
        identity: finding.identity,
        outcome: verdict,
        _state: PhantomData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_raw() -> Finding<Raw> {
        Finding::raw(
            "test-rule",
            FindingKind::RuleTriggered,
            Severity::High,
            Hardness::Observed,
            "test summary",
            Timestamp::UNIX_EPOCH,
        )
    }

    #[test]
    fn test_raw_carries_given_fields() {
        let finding = sample_raw();
        assert_eq!(finding.rule_id, "test-rule");
        assert_eq!(finding.kind, FindingKind::RuleTriggered);
        assert_eq!(finding.severity, Severity::High);
        assert_eq!(finding.hardness, Hardness::Observed);
        assert_eq!(finding.summary, "test summary");
        assert_eq!(finding.observed_at, Timestamp::UNIX_EPOCH);
    }

    #[test]
    fn test_check_assigns_given_identity_and_preserves_fields() {
        let id = FindingId::try_from_str("finding-1").expect("non-empty id");
        let checked = sample_raw().check(id.clone());
        assert_eq!(checked.id(), &id);
        assert_eq!(checked.rule_id, "test-rule");
        assert_eq!(checked.severity, Severity::High);
    }

    #[test]
    fn test_triage_preserves_identity_and_sets_verdict() {
        let id = FindingId::try_from_str("finding-2").expect("non-empty id");
        let checked = sample_raw().check(id.clone());
        let triaged = triage(checked, Verdict::NeedsReview);
        assert_eq!(triaged.id(), &id);
        assert_eq!(*triaged.verdict(), Verdict::NeedsReview);
        assert_eq!(triaged.summary, "test summary");
    }

    #[test]
    fn test_two_raw_findings_from_identical_inputs_are_equal() {
        // Belegt die Reinheitsauflage auf Werteebene: identische Eingaben
        // ergeben einen identischen `Finding<Raw>` — siehe Moduldoku.
        assert_eq!(sample_raw(), sample_raw());
    }

    #[test]
    fn test_finding_kind_variants_are_distinct() {
        assert_ne!(FindingKind::RuleTriggered, FindingKind::Anomaly);
    }

    #[test]
    fn test_verdict_variants_are_pairwise_distinct() {
        assert_ne!(Verdict::Confirmed, Verdict::FalsePositive);
        assert_ne!(Verdict::Confirmed, Verdict::NeedsReview);
        assert_ne!(Verdict::FalsePositive, Verdict::NeedsReview);
    }
}
