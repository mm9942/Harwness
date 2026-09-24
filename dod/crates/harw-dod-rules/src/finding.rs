//! `Finding<S>`: der Typestate-Kern dieser Crate — eine Beobachtung wird erst
//! hier zu einem Befund (Knoten AW4-03, Contract-Master §G.1; gehärtet in
//! W3/C-FIND, Befunde F-023, F-074, G-037).
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
//! [`Finding::raw`] und [`Finding::check`] sind `pub(crate)`, also kann ein
//! `Finding<RuleChecked>` außerhalb dieser Crate nicht *entstehen*. Seit
//! C-FIND (F-023) sind zusätzlich **alle Felder** crate-privat
//! (`pub(crate)`): außerhalb dieser Crate ist ein Befund nur noch über
//! Lesemethoden ([`Finding::rule_id`], [`Finding::kind`],
//! [`Finding::severity`], [`Finding::hardness`], [`Finding::summary`],
//! [`Finding::observed_at`]) zugänglich. Vorher konnte jede Konsumentin
//! `severity`/`hardness`/`kind` eines bereits geprüften Befundes nachträglich
//! überschreiben — die Prüfung sagte dann nichts mehr über den Wert aus, der
//! tatsächlich eskaliert wurde.
//!
//! # Der Weg über die Prozessgrenze: `FindingRecord` und `triage_record`
//! Ein `Finding<RuleChecked>` lebt im Sammelprozess (`harw-sentinel`); die
//! Triage läuft in einem anderen, modellnahen Prozess, und die Eskalation in
//! einem dritten (Escalator). Ein Typestate überquert keine Prozessgrenze.
//! Deshalb gibt es genau einen serialisierbaren Zwischenstand:
//!
//! 1. [`Finding::record`] friert einen `Finding<RuleChecked>` zusammen mit
//!    seinem Beleg ([`harw_dod_signals::SecurityEvidence`]) zu einem
//!    [`FindingRecord`] ein. Der Record trägt einen Inhaltsdigest
//!    ([`FindingRecord::digest`], BLAKE3 über kanonische, längenpräfixierte
//!    Bytes mit Domain-Trenner `harw:finding-record:v1\0`).
//! 2. Der Triage-Agent liest den Record und liefert ein
//!    [`harw_dod_signals::SecurityVerdict`], dessen `bound_evidence` genau
//!    diesen Digest nennt.
//! 3. Der Escalator liest den Record **selbst** (aus dem Spool, nicht aus der
//!    Antwort des Agenten) und ruft [`triage_record`] auf. Diese Funktion
//!    rechnet Beleg- und Record-Digest **selbst neu** und prüft erst dann
//!    [`harw_dod_signals::SecurityVerdict::binds`] gegen den selbst
//!    berechneten Wert — nie gegen einen Wert, der aus dem Verdikt oder aus
//!    einem ungeprüften Feld stammt (K43-Falle, siehe
//!    `harw_dod_signals::verdict`-Moduldoku; G-037).
//!
//! **Grenze dieser Garantie:** Der Digest ist kein MAC. Wer in den Spool
//! schreiben darf, kann einen in sich stimmigen Record erzeugen. Die
//! Vertrauenswürdigkeit eines Records ist daher genau die seiner Quelle
//! (Spool-Verzeichnis, Dateirechte, Peer-Prüfung); was diese Funktion
//! garantiert, ist, dass ein Verdikt nur zu **exakt dem Inhalt** passt, den
//! der Agent gesehen hat, und dass ein Record, dessen Felder nach dem
//! Einfrieren verändert wurden, abgelehnt wird.
//!
//! **Offene Restlücke (F-023):** [`triage`] mit frei wählbarem [`Verdict`]
//! ist weiterhin `pub`, weil `src/lib.rs` sie re-exportiert und diese Datei
//! außerhalb der Zuständigkeit von C-FIND liegt. Neue Aufrufer müssen
//! [`triage_record`] verwenden; `triage` wird entfernt, sobald die Aufrufer
//! (siehe Ledger `docs/remediation/ledger/W3/C-FIND.md`) migriert sind.
//!
//! # Reinheitsauflage
//! [`Finding::raw`] führt kein I/O aus, liest keine Systemuhr und ruft
//! keinen Zufallsgenerator auf — jedes Feld kommt entweder vom Aufrufer
//! (typischerweise einer [`crate::rule::Rule`], die `ctx.now` injiziert
//! bekommt) oder ist eine reine Berechnung daraus. Dasselbe gilt für
//! [`Finding::record`] und [`triage_record`]: beide sind reine Funktionen
//! ihrer Eingaben. Zufallswerte (die [`FindingId`] eines geprüften Befundes)
//! erzeugen nur die Prägestellen [`crate::engine::run_rules_checked`] und
//! [`crate::advisory::correlate_advisories`].
//!
//! # Die Baseline-Regel
//! [`FindingKind`] unterscheidet, ob ein Befund seine Existenz einer
//! ausgelösten Regel (`RuleTriggered`) oder einer bloßen Abweichung
//! (`Anomaly`) verdankt. Welche Art eine [`crate::baseline::Baseline`]
//! zulässt, entscheidet [`crate::baseline::finding_kind_for_status`] anhand
//! ihres `PalaceStatus`: eine `Established`-Baseline darf `RuleTriggered`
//! auslösen, eine `Provisional`-Baseline nur `Anomaly`.
//!
//! # Nebenläufigkeit
//! `Finding<S>` und [`FindingRecord`] sind reine Werte ohne innere
//! Veränderlichkeit und `Send + Sync`. Alle Funktionen dieses Moduls sind
//! zustandslos und aus beliebigen Threads aufrufbar.
//!
//! # Fehler
//! - [`RecordError`]: ein Record ist inkonsistent (unbekannte Version oder
//!   Regel, Beleg- oder Record-Digest passt nicht). Entsteht beim
//!   Deserialisieren eines [`FindingRecord`] und in [`triage_record`].
//! - [`TriageError`]: [`triage_record`] lehnt Record oder Verdikt ab.
//!
//! Beide Fehlertypen sind **inhaltsfrei**: keine Meldung interpoliert einen
//! Wert aus dem Record oder dem Verdikt (angreiferbeeinflusste Sensorfelder).
//! [`Finding::raw`], [`Finding::check`], [`Finding::record`] und [`triage`]
//! sind total.
//!
//! # Examples
//! ```rust
//! use harw_code_graph::lockfile::LockedPackage;
//! use harw_dod_rules::advisory::{Advisory, correlate_advisories};
//! use harw_dod_rules::finding::triage_record;
//! use harw_dod_rules::Verdict;
//! use harw_dod_signals::{SecurityEvidence, SecurityVerdict, Severity, VerdictClassification};
//! use semver::VersionReq;
//!
//! let locked = vec![LockedPackage {
//!     name: "example-crate".to_owned(),
//!     version: "1.9.0".to_owned(),
//!     source: None,
//!     checksum: None,
//! }];
//! let advisories = vec![Advisory {
//!     id: "RUSTSEC-2024-0001".to_owned(),
//!     crate_name: "example-crate".to_owned(),
//!     vulnerable_ranges: vec![VersionReq::parse("<1.10.0").expect("gültiger Bereich")],
//!     severity: Severity::High,
//!     summary: "Beispiel-Advisory".to_owned(),
//! }];
//! // `run_rules` liefert nur `Finding<Raw>`; öffentliche Prägestellen für
//! // `Finding<RuleChecked>` sind `run_rules_checked` und `correlate_advisories`.
//! let finding = correlate_advisories(&advisories, &locked, jiff::Timestamp::UNIX_EPOCH)
//!     .into_iter()
//!     .next()
//!     .expect("Advisory trifft");
//!
//! let evidence = SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
//!     .expect("Beleg kodierbar");
//! let record = finding.record(evidence);
//!
//! // Der Triage-Agent bindet sein Urteil an den Digest des Records.
//! let verdict = SecurityVerdict::new(
//!     record.finding_id().clone(),
//!     record.digest(),
//!     VerdictClassification::Confirmed,
//!     Severity::High,
//!     "Ziel außerhalb des erlaubten Bereichs".to_owned(),
//!     None,
//!     "security-triage-1".to_owned(),
//!     jiff::Timestamp::UNIX_EPOCH,
//! );
//! let triaged = triage_record(&record, &verdict).expect("Verdikt gebunden");
//! assert_eq!(*triaged.verdict(), Verdict::Confirmed);
//! assert_eq!(triaged.record_digest(), Some(record.digest()));
//! ```

use std::fmt;
use std::marker::PhantomData;

use harw_dod_signals::{
    SecurityEvidence, SecurityVerdict, Severity, SignalsError, VerdictClassification,
    validate_verdict,
};
// `pub use`, nicht `use`: die konkreten Regeln in `crate::rules` importieren
// `Hardness` über `crate::finding::Hardness` statt über eine zweite, direkte
// `harw-dod-signals`-Abhängigkeit derselben Bezeichnung.
pub use harw_dod_signals::Hardness;
use harw_types::{ContentDigest, FindingId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::rule::Rule;

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
/// (`S::Outcome = ()`) — die Identität entsteht erst bei [`Finding::check`].
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
/// Entsteht über [`triage_record`] (gebunden an einen selbst geprüften
/// [`FindingRecord`]) oder — auslaufend, siehe Moduldoku — über [`triage`].
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
/// `Outcome` ist `()` bis [`RuleChecked`] und `(Verdict, Option<ContentDigest>)`
/// in [`Triaged`] — das Urteil plus, falls über [`triage_record`] entstanden,
/// der selbst geprüfte Record-Digest, an den das Urteil gebunden war.
/// Öffentlich nur, weil ein privates Trait als Bound auf einem `pub`-Typ
/// einen Sichtbarkeits-Lint auslöst (`private_bounds`).
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
    type Outcome = (Verdict, Option<ContentDigest>);
}

/// Womit ein Befund seine Existenz rechtfertigt.
///
/// # Description
/// Die Baseline-Regel (siehe Moduldoku) hängt an dieser Unterscheidung:
/// `RuleTriggered` darf eskaliert werden, `Anomaly` ist zunächst nur eine
/// Beobachtung zur Kenntnisnahme. Geschlossen. Wire-Form kebab-case
/// (`"rule-triggered"`, `"anomaly"`), Teil des [`FindingRecord`]-Formats.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
/// Geschlossen: jede Triage endet in genau einem dieser drei Zustände.
/// [`triage_record`] bildet [`harw_dod_signals::VerdictClassification`] ab:
/// `Confirmed → Confirmed`, `Benign → FalsePositive`,
/// `Suspicious → NeedsReview`.
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

impl Verdict {
    /// Bildet die Einstufung eines Sicherheitsagenten auf ein [`Verdict`] ab.
    ///
    /// # Arguments
    /// - `classification` (`VerdictClassification`): die Einstufung aus dem
    ///   geprüften [`harw_dod_signals::SecurityVerdict`].
    ///
    /// # Returns
    /// `Confirmed → Confirmed`, `Benign → FalsePositive`,
    /// `Suspicious → NeedsReview`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_rules::Verdict;
    /// use harw_dod_signals::VerdictClassification;
    ///
    /// assert_eq!(
    ///     Verdict::from_classification(VerdictClassification::Benign),
    ///     Verdict::FalsePositive
    /// );
    /// ```
    #[must_use]
    pub fn from_classification(classification: VerdictClassification) -> Self {
        match classification {
            VerdictClassification::Confirmed => Self::Confirmed,
            VerdictClassification::Benign => Self::FalsePositive,
            VerdictClassification::Suspicious => Self::NeedsReview,
        }
    }
}

/// Ein Sicherheitsbefund im Zustand `S`.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung des Typestate-Musters.
/// Alle Felder sind crate-privat (F-023); außerhalb dieser Crate nur über die
/// Lesemethoden erreichbar. Zustandsabhängige Daten (Identität,
/// Triage-Ergebnis) nur über zustandsspezifische Methoden.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// Siehe Moduldoku für ein vollständiges Beispiel über
/// [`crate::rules::EgressFlowRule`].
///
/// Felder sind außerhalb dieser Crate nicht schreibbar:
/// ```rust,compile_fail
/// use harw_dod_rules::{Finding, RuleChecked};
/// use harw_dod_signals::Severity;
///
/// fn attempt(mut finding: Finding<RuleChecked>) {
///     finding.severity = Severity::Critical;
/// }
/// ```
pub struct Finding<S: FindingState> {
    // Die `Rule::id` der erzeugenden Regel.
    pub(crate) rule_id: &'static str,
    // Womit dieser Befund seine Existenz rechtfertigt.
    pub(crate) kind: FindingKind,
    // Wie schwer dieser Befund wiegt.
    pub(crate) severity: Severity,
    // Wie hart der Nachweis hinter diesem Befund ist.
    pub(crate) hardness: Hardness,
    // Menschenlesbare Zusammenfassung (kann angreiferbeeinflusste Teile tragen).
    pub(crate) summary: String,
    // Injizierte Zeit der Regelauswertung — nie die Systemuhr der Regel.
    pub(crate) observed_at: Timestamp,
    identity: S::Identity,
    outcome: S::Outcome,
    _state: PhantomData<S>,
}

// Manuell statt `#[derive(Debug)]`: `derive` bekäme nur die Bound `S: Debug`,
// nicht die nötige `S::Identity: Debug` / `S::Outcome: Debug` (identisches
// Muster wie `harw_dod_cap::handle::SensorHandle`).
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

/// Lesemethoden, die in jedem Zustand gelten.
///
/// # Description
/// Ersetzen die früheren `pub`-Felder (F-023): lesen ja, schreiben nein.
///
/// # Concurrency
/// Reine Lesezugriffe auf einen unveränderlichen Wert.
impl<S: FindingState> Finding<S> {
    /// Die [`crate::rule::Rule::id`] der Regel, die diesen Befund erzeugt hat.
    ///
    /// # Returns
    /// Die statische Regel-Kennung.
    #[must_use]
    pub fn rule_id(&self) -> &'static str {
        self.rule_id
    }

    /// Womit dieser Befund seine Existenz rechtfertigt.
    ///
    /// # Returns
    /// Die [`FindingKind`] dieses Befundes.
    #[must_use]
    pub fn kind(&self) -> FindingKind {
        self.kind
    }

    /// Wie schwer dieser Befund wiegt.
    ///
    /// # Returns
    /// Die [`harw_dod_signals::Severity`] dieses Befundes.
    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// Wie hart der Nachweis hinter diesem Befund ist.
    ///
    /// # Returns
    /// Die [`harw_dod_signals::Hardness`] dieses Befundes.
    #[must_use]
    pub fn hardness(&self) -> Hardness {
        self.hardness
    }

    /// Menschenlesbare Zusammenfassung.
    ///
    /// # Description
    /// Kann angreiferbeeinflusste Teile tragen (Hostnamen, Pfade,
    /// Advisory-Texte) — nie ungezäunt in einen Modell-Prompt geben.
    ///
    /// # Returns
    /// Die Zusammenfassung als `&str`.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Injizierte Zeit, zu der die auslösende Regel ausgewertet wurde.
    ///
    /// # Returns
    /// Den `jiff::Timestamp` der Regelauswertung.
    #[must_use]
    pub fn observed_at(&self) -> Timestamp {
        self.observed_at
    }
}

impl Finding<Raw> {
    /// Mintet einen rohen Befund. `pub(crate)`: nur Regeln innerhalb dieser
    /// Crate dürfen einen `Finding<Raw>` erzeugen.
    ///
    /// # Description
    /// Reine Funktion: kein Feld entsteht aus I/O, Systemuhr oder Zufall —
    /// siehe Moduldoku „Reinheitsauflage“.
    ///
    /// # Arguments
    /// - `rule_id` (`&'static str`): die [`crate::rule::Rule::id`] der
    ///   erzeugenden Regel.
    /// - `kind` (`FindingKind`): eskalationswürdig oder nur Abweichung.
    /// - `severity` (`harw_dod_signals::Severity`): wie schwer der Befund wiegt.
    /// - `hardness` (`harw_dod_signals::Hardness`): wie hart der Nachweis ist.
    /// - `summary` (`impl Into<String>`): menschenlesbare Zusammenfassung.
    /// - `observed_at` (`jiff::Timestamp`): injizierte Zeit.
    ///
    /// # Returns
    /// Einen `Finding<Raw>` ohne Identität und ohne Triage-Ergebnis.
    ///
    /// # Errors
    /// Keine — totale Konstruktion.
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
    /// Konsumiert `self` (Typestate-Übergang). Aufgerufen von den beiden
    /// öffentlichen Prägestellen [`crate::engine::run_rules_checked`] und
    /// [`crate::advisory::correlate_advisories`] (sowie der Testhilfe
    /// `triaged_finding_for_test`). Außerhalb dieser Crate entsteht ein
    /// `Finding<RuleChecked>` ausschließlich über diese Prägestellen.
    ///
    /// # Arguments
    /// - `id` (`harw_types::FindingId`): die dem Befund zugewiesene Identität.
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
    /// Siehe Moduldoku für ein vollständiges Beispiel.
    #[must_use]
    pub fn id(&self) -> &FindingId {
        &self.identity
    }

    /// Friert diesen geprüften Befund mit seinem Beleg zu einem
    /// serialisierbaren [`FindingRecord`] ein.
    ///
    /// # Description
    /// Übernimmt alle Felder und die Identität, bettet `evidence` vollständig
    /// ein und berechnet den Record-Digest über die kanonischen Bytes (siehe
    /// [`FindingRecord::digest`]). Der Digest deckt den Beleg über dessen
    /// `digest`-Feld ab; ob dieses Feld zum Inhalt passt, prüft nicht diese
    /// totale Funktion, sondern jede Deserialisierung und [`triage_record`]
    /// (ein `SecurityEvidence` hat öffentliche Felder und kann in-process
    /// verändert worden sein).
    ///
    /// # Arguments
    /// - `evidence` (`SecurityEvidence`): der eingefrorene Beleg, typischerweise
    ///   aus `harw_dod_sentinel::Sentinel::freeze`. Wird übernommen.
    ///
    /// # Returns
    /// Den eingefrorenen [`FindingRecord`] im Format [`FindingRecord::VERSION`].
    ///
    /// # Errors
    /// Keine — totale Umformung.
    ///
    /// # Concurrency
    /// Reine Funktion, kein geteilter Zustand.
    ///
    /// # Examples
    /// Siehe Moduldoku.
    #[must_use]
    pub fn record(&self, evidence: SecurityEvidence) -> FindingRecord {
        let mut record = FindingRecord {
            version: FindingRecord::VERSION,
            finding_id: self.identity.clone(),
            rule_id: self.rule_id.to_owned(),
            kind: self.kind,
            severity: self.severity,
            hardness: self.hardness,
            summary: self.summary.clone(),
            observed_at: self.observed_at,
            evidence,
            // Platzhalter; wird unmittelbar darunter über die kanonischen
            // Bytes ersetzt, bevor der Wert diese Funktion verlässt.
            digest: ContentDigest::of(&[]),
        };
        record.digest = record.compute_digest();
        record
    }
}

impl Finding<Triaged> {
    /// Die stabile Identität dieses triagierten Befundes.
    ///
    /// # Returns
    /// Dieselbe [`harw_types::FindingId`], die der Befund als
    /// `Finding<RuleChecked>` bzw. im [`FindingRecord`] trug.
    #[must_use]
    pub fn id(&self) -> &FindingId {
        &self.identity
    }

    /// Das Ergebnis der Triage-Entscheidung.
    ///
    /// # Returns
    /// Das [`Verdict`] dieses Befundes.
    #[must_use]
    pub fn verdict(&self) -> &Verdict {
        &self.outcome.0
    }

    /// Der selbst geprüfte Record-Digest, an den das Urteil gebunden war.
    ///
    /// # Description
    /// `Some`, wenn der Befund über [`triage_record`] entstand; `None` bei
    /// der auslaufenden [`triage`]. Eine Eskalationsstufe, die Autorität
    /// braucht, sollte `None` ablehnen und den Digest in ihr Audit
    /// übernehmen.
    ///
    /// # Returns
    /// `Option<ContentDigest>`.
    #[must_use]
    pub fn record_digest(&self) -> Option<ContentDigest> {
        self.outcome.1
    }
}

/// Liefert die statische Kennung einer dieser Crate bekannten Regel.
///
/// # Description
/// Ein [`FindingRecord`] trägt die Regel-Kennung als `String`; ein
/// `Finding<S>` braucht `&'static str`. Statt einen String zu leaken, wird
/// die Kennung gegen die Regeln dieser Crate aufgelöst — ein Record mit
/// unbekannter Regel wird abgelehnt. **Jede neue Regel muss hier ergänzt
/// werden** (der Test `test_known_rule_id_covers_every_rule` hält die Liste
/// ehrlich).
fn known_rule_id(candidate: &str) -> Option<&'static str> {
    [
        crate::rules::EgressFlowRule.id(),
        crate::rules::StructureDriftRule.id(),
        crate::rules::BaselineDeviationRule.id(),
        crate::advisory::RULE_ID,
    ]
    .into_iter()
    .find(|known| *known == candidate)
}

/// Kanonische Kennung einer [`FindingKind`] in den Digest-Bytes.
fn kind_tag(kind: FindingKind) -> &'static [u8] {
    match kind {
        FindingKind::RuleTriggered => b"rule-triggered",
        FindingKind::Anomaly => b"anomaly",
    }
}

/// Kanonische Kennung einer [`Severity`] in den Digest-Bytes.
fn severity_tag(severity: Severity) -> &'static [u8] {
    match severity {
        Severity::Info => b"info",
        Severity::Low => b"low",
        Severity::Medium => b"medium",
        Severity::High => b"high",
        Severity::Critical => b"critical",
    }
}

/// Kanonische Kennung einer [`Hardness`] in den Digest-Bytes.
fn hardness_tag(hardness: Hardness) -> &'static [u8] {
    match hardness {
        Hardness::Observed => b"observed",
        Hardness::Correlated => b"correlated",
        Hardness::Inferred => b"inferred",
    }
}

/// Hängt ein längenpräfixiertes Feld (u64 little-endian + Bytes) an.
fn push_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// Ein eingefrorener, serialisierbarer Befund samt Beleg — der einzige Weg
/// eines Befundes über eine Prozessgrenze.
///
/// # Description
/// Entsteht ausschließlich über [`Finding::record`] oder durch
/// Deserialisierung. Die Deserialisierung **prüft** den Record vollständig
/// (Version, bekannte Regel, Beleg-Digest neu berechnet, Record-Digest neu
/// berechnet) und schlägt bei jeder Abweichung fehl — ein gelesener
/// `FindingRecord` ist in sich stimmig. Alle Felder sind privat.
///
/// # Wire-Format (Version 1)
/// JSON-Objekt mit den Feldern `version`, `finding_id`, `rule_id`, `kind`,
/// `severity`, `hardness`, `summary`, `observed_at`, `evidence`, `digest`;
/// unbekannte Felder werden abgelehnt.
///
/// # Digest
/// BLAKE3 ([`harw_types::ContentDigest::of`]) über:
/// `"harw:finding-record:v1\0"` ‖ `version` (u32 LE) ‖ je Feld
/// `len` (u64 LE) ‖ Bytes für `finding_id`, `rule_id`, `kind`, `severity`,
/// `hardness`, `summary` (UTF-8, feste Kennungen für die Enums) ‖
/// `observed_at` (Nanosekunden, i128 LE) ‖ `evidence.digest` (32 B) ‖
/// `evidence.captured_at` (Nanosekunden, i128 LE). Der Beleginhalt
/// (`samples`/`events`) ist über `evidence.digest` abgedeckt, der bei jeder
/// Prüfung neu berechnet wird.
///
/// # Bekannte Grenze
/// `evidence.samples[].value` ist `f64`. Überlebt ein Wert die
/// JSON-Rundreise nicht bitgenau (NaN/±∞ sind in JSON nicht darstellbar;
/// ohne `serde_json/float_roundtrip` sind einzelne Werte um 1 ULP
/// verschoben), scheitert die Prüfung — fail-closed, nie fail-open.
///
/// # Concurrency
/// Reiner Wert, `Send + Sync`.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FindingRecordWire")]
pub struct FindingRecord {
    version: u32,
    finding_id: FindingId,
    rule_id: String,
    kind: FindingKind,
    severity: Severity,
    hardness: Hardness,
    summary: String,
    observed_at: Timestamp,
    evidence: SecurityEvidence,
    digest: ContentDigest,
}

// Ungeprüfte Wire-Form; wird nur über `TryFrom` (vollständige Prüfung) zu
// einem `FindingRecord`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindingRecordWire {
    version: u32,
    finding_id: FindingId,
    rule_id: String,
    kind: FindingKind,
    severity: Severity,
    hardness: Hardness,
    summary: String,
    observed_at: Timestamp,
    evidence: SecurityEvidence,
    digest: ContentDigest,
}

impl TryFrom<FindingRecordWire> for FindingRecord {
    type Error = RecordError;

    fn try_from(wire: FindingRecordWire) -> Result<Self, Self::Error> {
        verify_record(Self {
            version: wire.version,
            finding_id: wire.finding_id,
            rule_id: wire.rule_id,
            kind: wire.kind,
            severity: wire.severity,
            hardness: wire.hardness,
            summary: wire.summary,
            observed_at: wire.observed_at,
            evidence: wire.evidence,
            digest: wire.digest,
        })
    }
}

impl FindingRecord {
    /// Aktuelle Formatversion; Teil der Digest-Bytes.
    pub const VERSION: u32 = 1;

    /// Domain-Trenner der Digest-Bytes.
    const DOMAIN: &'static [u8] = b"harw:finding-record:v1\0";

    /// Formatversion dieses Records.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Identität des eingefrorenen Befundes.
    #[must_use]
    pub fn finding_id(&self) -> &FindingId {
        &self.finding_id
    }

    /// Regel-Kennung des eingefrorenen Befundes.
    #[must_use]
    pub fn rule_id(&self) -> &str {
        &self.rule_id
    }

    /// Art des eingefrorenen Befundes.
    #[must_use]
    pub fn kind(&self) -> FindingKind {
        self.kind
    }

    /// Schwere des eingefrorenen Befundes.
    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// Härte des Nachweises.
    #[must_use]
    pub fn hardness(&self) -> Hardness {
        self.hardness
    }

    /// Zusammenfassung (angreiferbeeinflusst möglich, siehe [`Finding::summary`]).
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Injizierte Zeit der Regelauswertung.
    #[must_use]
    pub fn observed_at(&self) -> Timestamp {
        self.observed_at
    }

    /// Der eingebettete Beleg.
    #[must_use]
    pub fn evidence(&self) -> &SecurityEvidence {
        &self.evidence
    }

    /// Der Inhaltsdigest dieses Records (siehe Typdoku, Abschnitt „Digest“).
    ///
    /// # Description
    /// Genau dieser Wert gehört in `SecurityVerdict::bound_evidence`. Für
    /// eine Autorisierungsentscheidung nicht diesen gespeicherten Wert
    /// verwenden, sondern [`triage_record`] aufrufen — die rechnet neu.
    ///
    /// # Returns
    /// Den `ContentDigest` dieses Records.
    #[must_use]
    pub fn digest(&self) -> ContentDigest {
        self.digest
    }

    /// Berechnet den Digest über die kanonischen Bytes (ohne das
    /// gespeicherte `digest`-Feld).
    fn compute_digest(&self) -> ContentDigest {
        let mut buf = Vec::with_capacity(256 + self.summary.len());
        buf.extend_from_slice(Self::DOMAIN);
        buf.extend_from_slice(&self.version.to_le_bytes());
        push_field(&mut buf, self.finding_id.as_str().as_bytes());
        push_field(&mut buf, self.rule_id.as_bytes());
        push_field(&mut buf, kind_tag(self.kind));
        push_field(&mut buf, severity_tag(self.severity));
        push_field(&mut buf, hardness_tag(self.hardness));
        push_field(&mut buf, self.summary.as_bytes());
        buf.extend_from_slice(&self.observed_at.as_nanosecond().to_le_bytes());
        buf.extend_from_slice(self.evidence.digest.as_bytes());
        buf.extend_from_slice(&self.evidence.captured_at.as_nanosecond().to_le_bytes());
        ContentDigest::of(&buf)
    }
}

/// Prüft einen Record vollständig und gibt ihn mit neu berechnetem Beleg
/// zurück.
fn verify_record(record: FindingRecord) -> Result<FindingRecord, RecordError> {
    let FindingRecord {
        version,
        finding_id,
        rule_id,
        kind,
        severity,
        hardness,
        summary,
        observed_at,
        evidence,
        digest,
    } = record;
    if version != FindingRecord::VERSION {
        return Err(RecordError::UnsupportedVersion);
    }
    if known_rule_id(&rule_id).is_none() {
        return Err(RecordError::UnknownRule);
    }
    let SecurityEvidence {
        digest: claimed_evidence_digest,
        captured_at,
        samples,
        events,
    } = evidence;
    let recomputed = SecurityEvidence::capture(samples, events, captured_at)
        .map_err(RecordError::EvidenceEncoding)?;
    if recomputed.digest != claimed_evidence_digest {
        return Err(RecordError::EvidenceDigestMismatch);
    }
    let rebuilt = FindingRecord {
        version,
        finding_id,
        rule_id,
        kind,
        severity,
        hardness,
        summary,
        observed_at,
        evidence: recomputed,
        digest,
    };
    if rebuilt.compute_digest() != digest {
        return Err(RecordError::RecordDigestMismatch);
    }
    Ok(rebuilt)
}

/// Ein [`FindingRecord`] ist inkonsistent.
///
/// # Description
/// Inhaltsfrei: keine Variante trägt einen Wert aus dem Record.
///
/// # Errors
/// Entsteht beim Deserialisieren eines [`FindingRecord`] und in
/// [`triage_record`].
#[derive(Debug)]
pub enum RecordError {
    /// Die Formatversion ist nicht [`FindingRecord::VERSION`].
    UnsupportedVersion,
    /// Die Regel-Kennung gehört zu keiner Regel dieser Crate.
    UnknownRule,
    /// Der Beleg ließ sich zur Digest-Neuberechnung nicht kodieren.
    EvidenceEncoding(SignalsError),
    /// Der Beleg-Digest passt nicht zu `samples`/`events`.
    EvidenceDigestMismatch,
    /// Der Record-Digest passt nicht zu den kanonischen Bytes.
    RecordDigestMismatch,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => {
                f.write_str("finding record has an unsupported format version")
            }
            Self::UnknownRule => f.write_str("finding record names an unknown rule"),
            Self::EvidenceEncoding(_) => {
                f.write_str("finding record evidence could not be encoded for digest verification")
            }
            Self::EvidenceDigestMismatch => {
                f.write_str("finding record evidence digest does not match its contents")
            }
            Self::RecordDigestMismatch => {
                f.write_str("finding record digest does not match its contents")
            }
        }
    }
}

impl std::error::Error for RecordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EvidenceEncoding(inner) => Some(inner),
            Self::UnsupportedVersion
            | Self::UnknownRule
            | Self::EvidenceDigestMismatch
            | Self::RecordDigestMismatch => None,
        }
    }
}

/// [`triage_record`] lehnt Record oder Verdikt ab.
///
/// # Description
/// Inhaltsfrei wie [`RecordError`].
///
/// # Errors
/// Nur von [`triage_record`] erzeugt.
#[derive(Debug)]
pub enum TriageError {
    /// Der Record ist inkonsistent (siehe [`RecordError`]).
    Record(RecordError),
    /// Das Verdikt nennt eine andere Vertragsfassung als
    /// [`harw_dod_signals::SecurityVerdict::CONTRACT_ID`].
    VerdictContract,
    /// Das Verdikt verletzt die Pflichtfeldregeln
    /// ([`harw_dod_signals::validate_verdict`]).
    VerdictInvalid(SignalsError),
    /// Das Verdikt ist nicht an Befund und selbst berechneten Record-Digest
    /// gebunden ([`harw_dod_signals::SecurityVerdict::binds`]).
    VerdictUnbound(SignalsError),
}

impl fmt::Display for TriageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record(_) => f.write_str("finding record rejected during triage"),
            Self::VerdictContract => f.write_str("security verdict names an unsupported contract"),
            Self::VerdictInvalid(_) => {
                f.write_str("security verdict violates the contract field rules")
            }
            Self::VerdictUnbound(_) => {
                f.write_str("security verdict is not bound to this finding record")
            }
        }
    }
}

impl std::error::Error for TriageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Record(inner) => Some(inner),
            Self::VerdictInvalid(inner) | Self::VerdictUnbound(inner) => Some(inner),
            Self::VerdictContract => None,
        }
    }
}

impl From<RecordError> for TriageError {
    fn from(inner: RecordError) -> Self {
        Self::Record(inner)
    }
}

/// Triagiert einen eingefrorenen Befund anhand eines gebundenen Verdikts.
///
/// # Description
/// Der vorgesehene Weg zu einem `Finding<Triaged>` (Warden-Proof v2:
/// „`triage_record` bindet an selbst gelesenen Digest“). Ablauf:
/// 1. Record vollständig neu prüfen (Version, bekannte Regel, Beleg-Digest
///    und Record-Digest **neu berechnet**) — auch wenn der Record
///    in-process über [`Finding::record`] entstand.
/// 2. Vertragsfassung und Pflichtfelder des Verdikts prüfen.
/// 3. `verdict.binds(record.finding_id(), <selbst berechneter Digest>)`.
/// 4. `Finding<Triaged>` mit den Feldern **des Records** bauen; Schwere und
///    Härte aus dem Verdikt werden nicht übernommen (ein Agent kann einen
///    Befund nicht hochstufen). Das Urteil folgt
///    [`Verdict::from_classification`].
///
/// # Arguments
/// - `record` (`&FindingRecord`): der Record, den der Aufrufer **selbst**
///   gelesen hat (z. B. aus dem Spool) — nie eine Kopie aus der Antwort des
///   Agenten.
/// - `verdict` (`&SecurityVerdict`): das Urteil des Triage-Agenten.
///
/// # Returns
/// `Ok(Finding<Triaged>)` mit Identität des Records, abgebildetem Urteil und
/// `record_digest() == Some(record.digest())`.
///
/// # Errors
/// - [`TriageError::Record`]: der Record ist inkonsistent.
/// - [`TriageError::VerdictContract`]: falsche Vertragsfassung.
/// - [`TriageError::VerdictInvalid`]: leere Pflichtfelder im Verdikt.
/// - [`TriageError::VerdictUnbound`]: Befund-ID oder Digest passen nicht.
///
/// # Concurrency
/// Reine Funktion; klont den Beleg für die Neuberechnung.
///
/// # Examples
/// Siehe Moduldoku.
pub fn triage_record(
    record: &FindingRecord,
    verdict: &SecurityVerdict,
) -> Result<Finding<Triaged>, TriageError> {
    let verified = verify_record(record.clone())?;
    let rule_id = known_rule_id(&verified.rule_id).ok_or(RecordError::UnknownRule)?;
    if verdict.contract() != SecurityVerdict::CONTRACT_ID {
        return Err(TriageError::VerdictContract);
    }
    validate_verdict(verdict).map_err(TriageError::VerdictInvalid)?;
    let self_computed = verified.compute_digest();
    verdict
        .binds(&verified.finding_id, self_computed)
        .map_err(TriageError::VerdictUnbound)?;
    Ok(Finding {
        rule_id,
        kind: verified.kind,
        severity: verified.severity,
        hardness: verified.hardness,
        summary: verified.summary,
        observed_at: verified.observed_at,
        identity: verified.finding_id,
        outcome: (
            Verdict::from_classification(verdict.classification()),
            Some(self_computed),
        ),
        _state: PhantomData,
    })
}

/// Triagiert einen geprüften Befund mit frei gewähltem Urteil — **auslaufend**.
///
/// # Description
/// Nicht an einen Beleg gebunden; das Ergebnis trägt
/// `record_digest() == None`. Bleibt nur `pub`, weil `src/lib.rs` (nicht in
/// C-FIND-Zuständigkeit) sie re-exportiert und Aufrufer in
/// `harw-dod-escalate`, `harw-plan-bridge` und `harw-dod` sie noch nutzen
/// (siehe Moduldoku, „Offene Restlücke“). Neuer Code verwendet
/// [`triage_record`].
///
/// # Arguments
/// - `finding` (`Finding<RuleChecked>`): der geprüfte Befund. Wird konsumiert.
/// - `verdict` (`Verdict`): das Ergebnis der Triage-Entscheidung.
///
/// # Returns
/// Einen `Finding<Triaged>` mit identischen Feldern, derselben Identität und
/// dem übergebenen `Verdict`, ohne Record-Digest.
///
/// # Errors
/// Keine — reine, totale Umformung.
///
/// # Examples
/// ```rust
/// use harw_code_graph::lockfile::LockedPackage;
/// use harw_dod_rules::advisory::{Advisory, correlate_advisories};
/// use harw_dod_rules::{triage, Verdict};
/// use harw_dod_signals::Severity;
/// use semver::VersionReq;
///
/// let locked = vec![LockedPackage {
///     name: "example-crate".to_owned(),
///     version: "1.9.0".to_owned(),
///     source: None,
///     checksum: None,
/// }];
/// let advisories = vec![Advisory {
///     id: "RUSTSEC-2024-0001".to_owned(),
///     crate_name: "example-crate".to_owned(),
///     vulnerable_ranges: vec![VersionReq::parse("<1.10.0").expect("gültiger Bereich")],
///     severity: Severity::High,
///     summary: "Beispiel-Advisory".to_owned(),
/// }];
/// // `run_rules` liefert nur `Finding<Raw>`; öffentliche Prägestellen für
/// // `Finding<RuleChecked>` sind `run_rules_checked` und `correlate_advisories`.
/// let finding = correlate_advisories(&advisories, &locked, jiff::Timestamp::UNIX_EPOCH)
///     .into_iter()
///     .next()
///     .expect("Advisory trifft");
/// let triaged = triage(finding, Verdict::Confirmed);
/// assert_eq!(*triaged.verdict(), Verdict::Confirmed);
/// assert_eq!(triaged.record_digest(), None);
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
        outcome: (verdict, None),
        _state: PhantomData,
    }
}

/// Nur für Tests abhängiger Crates: baut einen `Finding<Triaged>` mit
/// beliebigen Feldwerten, hinter dem Feature `test-support` verriegelt.
///
/// # Description
/// Entspricht genau dem intern verwendeten Pfad `Finding::raw(..).check(id)`
/// gefolgt von [`triage`] — nur öffentlich gemacht, damit Testfixtures
/// außerhalb dieser Crate (z. B. `harw-dod-escalate`s `Ladder`-Tests) eine
/// `Severity`/`Hardness`-Kombination erzeugen können, die derzeit keine
/// echte Regel liefert (z. B. `Severity::Critical`). Ändert weder das
/// Datenmodell noch produktives Verhalten: `test-support` steht ausschließlich
/// in `[dev-dependencies]`-Positionen und ist wegen `resolver = "2"` nie im
/// Build eines produktiven Konsumenten aktiv. Die in der Moduldoku
/// beschriebene Invariante („Der Besitz des Wertes ist die Berechtigung")
/// bleibt für jeden produktiven Pfad unangetastet — `Finding::raw` und
/// `Finding::check` bleiben `pub(crate)`.
///
/// # Arguments
/// - `rule_id` (`&'static str`): die vorzutäuschende Regel-Kennung.
/// - `kind` (`FindingKind`): eskalationswürdig oder nur Abweichung.
/// - `severity` (`harw_dod_signals::Severity`): die zu erzwingende Schwere.
/// - `hardness` (`harw_dod_signals::Hardness`): die zu erzwingende Beleghärte.
/// - `summary` (`impl Into<String>`): menschenlesbare Zusammenfassung.
/// - `observed_at` (`jiff::Timestamp`): injizierte Zeit.
/// - `id` (`harw_types::FindingId`): die zu vergebende Identität.
/// - `verdict` (`Verdict`): das zu vergebende Triage-Urteil.
///
/// # Returns
/// Einen `Finding<Triaged>` mit exakt den übergebenen Feldwerten.
///
/// # Errors
/// Keine — totale Konstruktion.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
// Acht Parameter sind hier Absicht: die Testhilfe erzwingt jede Eigenschaft
// eines Befundes einzeln, gerade damit ein Test eine Kombination erzeugen kann,
// die keine echte Regel liefert. Ein Bündel-Struct würde die Aufrufe nur
// umschreiben, ohne die Zahl der Entscheidungen zu senken.
#[allow(clippy::too_many_arguments)]
pub fn triaged_finding_for_test(
    rule_id: &'static str,
    kind: FindingKind,
    severity: Severity,
    hardness: Hardness,
    summary: impl Into<String>,
    observed_at: Timestamp,
    id: FindingId,
    verdict: Verdict,
) -> Finding<Triaged> {
    triage(
        Finding::raw(rule_id, kind, severity, hardness, summary, observed_at).check(id),
        verdict,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent};
    use harw_types::SensorId;

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

    // Ein geprüfter Befund einer bekannten Regel (Records verlangen eine).
    fn checked(id: &str) -> TestResult<Finding<RuleChecked>> {
        Ok(Finding::raw(
            "structure-drift",
            FindingKind::RuleTriggered,
            Severity::High,
            Hardness::Observed,
            "unexpected setuid binary",
            Timestamp::UNIX_EPOCH,
        )
        .check(FindingId::try_from_str(id).map_err(ctx("test id is non-empty"))?))
    }

    fn evidence() -> TestResult<SecurityEvidence> {
        let event = SecurityEvent {
            sensor: SensorId::from_str("workspace-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::StructureDrift {
                severity: DriftSeverity::High,
                detail: "unexpected setuid binary in workspace".to_owned(),
            },
        };
        SecurityEvidence::capture(vec![], vec![event], Timestamp::UNIX_EPOCH)
            .map_err(ctx("test evidence encodes"))
    }

    fn verdict_for(
        finding: &FindingId,
        digest: ContentDigest,
        classification: VerdictClassification,
    ) -> SecurityVerdict {
        SecurityVerdict::new(
            finding.clone(),
            digest,
            classification,
            Severity::Critical,
            "test rationale".to_owned(),
            None,
            "security-triage-test".to_owned(),
            Timestamp::UNIX_EPOCH,
        )
    }

    #[test]
    fn test_raw_carries_given_fields() {
        let finding = sample_raw();
        assert_eq!(finding.rule_id(), "test-rule");
        assert_eq!(finding.kind(), FindingKind::RuleTriggered);
        assert_eq!(finding.severity(), Severity::High);
        assert_eq!(finding.hardness(), Hardness::Observed);
        assert_eq!(finding.summary(), "test summary");
        assert_eq!(finding.observed_at(), Timestamp::UNIX_EPOCH);
    }

    #[test]
    fn test_check_assigns_given_identity_and_preserves_fields() -> TestResult {
        let id = FindingId::try_from_str("finding-1").map_err(ctx("non-empty id"))?;
        let checked = sample_raw().check(id.clone());
        assert_eq!(checked.id(), &id);
        assert_eq!(checked.rule_id(), "test-rule");
        assert_eq!(checked.severity(), Severity::High);
        Ok(())
    }

    #[test]
    fn test_triage_preserves_identity_and_sets_verdict_without_digest() -> TestResult {
        let id = FindingId::try_from_str("finding-2").map_err(ctx("non-empty id"))?;
        let checked = sample_raw().check(id.clone());
        let triaged = triage(checked, Verdict::NeedsReview);
        assert_eq!(triaged.id(), &id);
        assert_eq!(*triaged.verdict(), Verdict::NeedsReview);
        assert_eq!(triaged.summary(), "test summary");
        assert_eq!(triaged.record_digest(), None);
        Ok(())
    }

    #[test]
    fn test_two_raw_findings_from_identical_inputs_are_equal() {
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

    #[test]
    fn test_from_classification_maps_every_variant() {
        assert_eq!(
            Verdict::from_classification(VerdictClassification::Confirmed),
            Verdict::Confirmed
        );
        assert_eq!(
            Verdict::from_classification(VerdictClassification::Benign),
            Verdict::FalsePositive
        );
        assert_eq!(
            Verdict::from_classification(VerdictClassification::Suspicious),
            Verdict::NeedsReview
        );
    }

    #[test]
    fn test_known_rule_id_covers_every_rule() {
        for id in [
            "egress-flow",
            "structure-drift",
            "baseline-deviation",
            "advisory-correlate",
        ] {
            assert_eq!(known_rule_id(id), Some(id));
        }
        assert_eq!(known_rule_id("test-rule"), None);
    }

    #[test]
    fn test_record_copies_fields_and_is_deterministic() -> TestResult {
        let finding = checked("finding-r1")?;
        let a = finding.record(evidence()?);
        let b = finding.record(evidence()?);
        assert_eq!(a.version(), FindingRecord::VERSION);
        assert_eq!(a.finding_id().as_str(), "finding-r1");
        assert_eq!(a.rule_id(), "structure-drift");
        assert_eq!(a.kind(), FindingKind::RuleTriggered);
        assert_eq!(a.severity(), Severity::High);
        assert_eq!(a.hardness(), Hardness::Observed);
        assert_eq!(a.summary(), "unexpected setuid binary");
        assert_eq!(a.evidence().events.len(), 1);
        assert_eq!(a.digest(), b.digest());
        Ok(())
    }

    #[test]
    fn test_record_digest_differs_for_different_identity() -> TestResult {
        let a = checked("finding-a")?.record(evidence()?);
        let b = checked("finding-b")?.record(evidence()?);
        assert_ne!(a.digest(), b.digest());
        Ok(())
    }

    #[test]
    fn test_triage_record_with_matching_digest_yields_triaged() -> TestResult {
        let record = checked("finding-t1")?.record(evidence()?);
        let verdict = verdict_for(
            record.finding_id(),
            record.digest(),
            VerdictClassification::Confirmed,
        );
        let triaged = triage_record(&record, &verdict).map_err(ctx("bound verdict accepted"))?;
        assert_eq!(triaged.id().as_str(), "finding-t1");
        assert_eq!(*triaged.verdict(), Verdict::Confirmed);
        assert_eq!(triaged.record_digest(), Some(record.digest()));
        // Schwere stammt aus dem Record, nicht aus dem Verdikt (Critical).
        assert_eq!(triaged.severity(), Severity::High);
        assert_eq!(triaged.rule_id(), "structure-drift");
        Ok(())
    }

    #[test]
    fn test_triage_record_with_wrong_digest_is_rejected() -> TestResult {
        let record = checked("finding-t2")?.record(evidence()?);
        let wrong = ContentDigest::of(b"some other record");
        let verdict = verdict_for(record.finding_id(), wrong, VerdictClassification::Benign);
        let Err(err) = triage_record(&record, &verdict) else {
            return Err(TestError::Unexpected(
                "unbound verdict was accepted".to_owned(),
            ));
        };
        assert!(matches!(err, TriageError::VerdictUnbound(_)));
        Ok(())
    }

    #[test]
    fn test_triage_record_with_wrong_finding_id_is_rejected() -> TestResult {
        let record = checked("finding-t3")?.record(evidence()?);
        let other = FindingId::try_from_str("finding-other").map_err(ctx("non-empty id"))?;
        let verdict = verdict_for(&other, record.digest(), VerdictClassification::Confirmed);
        let Err(err) = triage_record(&record, &verdict) else {
            return Err(TestError::Unexpected(
                "foreign finding was accepted".to_owned(),
            ));
        };
        assert!(matches!(err, TriageError::VerdictUnbound(_)));
        Ok(())
    }

    #[test]
    fn test_triage_record_with_tampered_evidence_is_rejected() -> TestResult {
        let mut forged = evidence()?;
        // Beleg-Digest passt nicht mehr zum Inhalt (öffentliche Felder).
        forged.digest = ContentDigest::of(b"forged");
        let record = checked("finding-t4")?.record(forged);
        let verdict = verdict_for(
            record.finding_id(),
            record.digest(),
            VerdictClassification::Confirmed,
        );
        let Err(err) = triage_record(&record, &verdict) else {
            return Err(TestError::Unexpected(
                "tampered evidence was accepted".to_owned(),
            ));
        };
        assert!(matches!(
            err,
            TriageError::Record(RecordError::EvidenceDigestMismatch)
        ));
        Ok(())
    }

    #[test]
    fn test_triage_record_with_empty_rationale_is_rejected() -> TestResult {
        let record = checked("finding-t5")?.record(evidence()?);
        let verdict = SecurityVerdict::new(
            record.finding_id().clone(),
            record.digest(),
            VerdictClassification::Confirmed,
            Severity::High,
            "   ".to_owned(),
            None,
            "security-triage-test".to_owned(),
            Timestamp::UNIX_EPOCH,
        );
        let Err(err) = triage_record(&record, &verdict) else {
            return Err(TestError::Unexpected(
                "invalid verdict was accepted".to_owned(),
            ));
        };
        assert!(matches!(err, TriageError::VerdictInvalid(_)));
        Ok(())
    }

    #[test]
    fn test_record_json_roundtrip_preserves_record_and_binding() -> TestResult {
        let record = checked("finding-j1")?.record(evidence()?);
        let json = serde_json::to_vec(&record).map_err(ctx("record serializes"))?;
        let back: FindingRecord =
            serde_json::from_slice(&json).map_err(ctx("record deserializes"))?;
        assert_eq!(back, record);
        let verdict = verdict_for(
            back.finding_id(),
            back.digest(),
            VerdictClassification::Suspicious,
        );
        let triaged = triage_record(&back, &verdict).map_err(ctx("roundtripped record triages"))?;
        assert_eq!(*triaged.verdict(), Verdict::NeedsReview);
        Ok(())
    }

    #[test]
    fn test_record_deserialize_rejects_tampered_summary() -> TestResult {
        let record = checked("finding-j2")?.record(evidence()?);
        let mut value = serde_json::to_value(&record).map_err(ctx("record serializes"))?;
        value["summary"] = serde_json::Value::String("harmless".to_owned());
        let result = serde_json::from_value::<FindingRecord>(value);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_record_deserialize_rejects_tampered_severity() -> TestResult {
        let record = checked("finding-j3")?.record(evidence()?);
        let mut value = serde_json::to_value(&record).map_err(ctx("record serializes"))?;
        value["severity"] = serde_json::Value::String("info".to_owned());
        assert!(serde_json::from_value::<FindingRecord>(value).is_err());
        Ok(())
    }

    #[test]
    fn test_record_deserialize_rejects_unknown_rule() -> TestResult {
        let record = checked("finding-j4")?.record(evidence()?);
        let mut value = serde_json::to_value(&record).map_err(ctx("record serializes"))?;
        value["rule_id"] = serde_json::Value::String("made-up-rule".to_owned());
        assert!(serde_json::from_value::<FindingRecord>(value).is_err());
        Ok(())
    }

    #[test]
    fn test_record_deserialize_rejects_unknown_field() -> TestResult {
        let record = checked("finding-j5")?.record(evidence()?);
        let mut value = serde_json::to_value(&record).map_err(ctx("record serializes"))?;
        value["extra"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<FindingRecord>(value).is_err());
        Ok(())
    }

    #[test]
    fn test_verify_record_rejects_unsupported_version() -> TestResult {
        let mut record = checked("finding-v1")?.record(evidence()?);
        record.version = 2;
        assert!(matches!(
            verify_record(record),
            Err(RecordError::UnsupportedVersion)
        ));
        Ok(())
    }

    #[test]
    fn test_record_error_display_is_content_free() {
        let err = TriageError::Record(RecordError::RecordDigestMismatch);
        assert_eq!(err.to_string(), "finding record rejected during triage");
        assert_eq!(
            RecordError::UnknownRule.to_string(),
            "finding record names an unknown rule"
        );
    }
}
