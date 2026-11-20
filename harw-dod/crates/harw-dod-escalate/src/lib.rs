//! Die Eskalationsleiter: der eine Ort, an dem aus einem Befund eine
//! autorisierte Aktion wird (Knoten **AW5-03**).
//!
//! # Verantwortungsbereich
//! Diese Crate schließt die Kette zwischen Befund und Durchsetzung: sie
//! nimmt ein `Finding<RuleChecked>` (Eigentum ausschließlich über
//! `harw-dod-rules`, AW4-03, erwerbbar) über [`triage_record`] in ein
//! `Finding<Triaged>` — beide Typen re-exportiert, nicht neu definiert (siehe
//! „Der dritte Zustand" unten). Sie definiert selbst [`Action<Proposed>`] und
//! [`Action<Authorized>`] ([`action`]-Modul) und nutzt [`Ladder`]
//! (seit PL-100 WP 3.0 in `harw-dod-ladder`, hier als [`ladder`]
//! re-exportiert), die entscheidet, welche Aktion ab welcher Stufe
//! zulässig ist und welche Stufe ein Befund erreicht hat. Sie verwaltet die
//! durable `FreezeStore`-Persistenz (AW5-05) über [`freeze_ops`]. Sie führt
//! selbst **keine** Durchsetzung aus und signiert **nichts**: ihr Ergebnis
//! ist der unsignierte [`AuthorizedAction`]. Den signierten v2-Auftrag
//! (`harw_dod_warden_proto::SignedAuthorization`) stellt `harw-escalator`
//! (PL-100 WP 3.2) daraus aus; `harw-dod-warden` (AW5-04a) nimmt nur diesen
//! an und prüft ihn mit `SignedAuthorization::verify`.
//!
//! # Der dritte Zustand: `Finding<Triaged>` lebt in `harw-dod-rules`, nicht
//! hier
//! `Finding<Raw|RuleChecked|Triaged>` und alle drei Typestate-Übergänge
//! (`Finding::raw`, `Finding::check`, `triage_record`) sind bereits vollständig in
//! `harw-dod-rules` (AW4-03) realisiert — siehe dessen `finding.rs`-Moduldoku
//! für die Begründung, warum Typbesitzer und Übergangsbesitzer dieselbe
//! Crate sein müssen. Diese Crate definiert `Triaged` **nicht** neu; sie
//! re-exportiert [`Finding`], [`RuleChecked`], [`Triaged`], [`Verdict`] und
//! [`triage_record`] und konsumiert sie. **Die Berechtigung ist der Besitz des
//! Wertes:** an ein `Finding<Triaged>` kommt nur, wer zuerst ein
//! `Finding<RuleChecked>` **besessen** hat — und das kann ausschließlich
//! `harw-dod-rules` herstellen (`Finding::raw`/`Finding::check` sind dort
//! `pub(crate)`). Der Typ trägt die Regel; kein Reviewer muss sie tragen,
//! kein Aufrufer kann sich eines fälschen, um [`Action::authorize`] zu
//! erreichen.
//!
//! # Invariante S1: Autorisierung entsteht an genau einem Ort
//! [`Action::authorize`] ist `pub(crate)` — **niemand außerhalb dieser Crate
//! kann ein `Action<Authorized>` oder einen [`AuthorizedAction`]
//! herstellen.** Es gibt keinen öffentlichen Konstruktor, kein `From`, kein
//! `Deserialize` für `Action<S>` oder [`AuthorizedAction`], über den ein
//! Aufrufer den Typestate-Übergang umgehen könnte — nur die hiesige
//! Konstruktion durchläuft die Prüfungen (Verdict, stillgelegte Aktion,
//! Zulässigkeit). Siehe [`action`]-Moduldoku für den vollständigen Beleg
//! einschließlich der `compile_fail`-Doctests.
//!
//! # Die Aufstiegsregel in einem Absatz
//! Ein Befund erreicht `EscalationStage::RuleTriggered` nur, wenn er eine
//! ausgelöste Regel ist (`FindingKind::RuleTriggered`, keine `Anomaly`) und
//! als echt bestätigt wurde (`Verdict::Confirmed`); er steigt auf
//! `EscalationStage::Escalated` nur, wenn er zusätzlich gleichzeitig das
//! höchste Gewicht (`Severity::Critical`) und den härtesten Nachweisgrad
//! (`Hardness::Observed`) trägt. Keine Systemuhr, kein Zufall, keine
//! Historie — reine Funktion der Befundfelder. Siehe [`ladder`]-Moduldoku
//! für die vollständige Begründung.
//!
//! `Verdict::Confirmed` stammt nie aus einem Modell: [`triage_record`]
//! deckelt ein Modell-`Confirmed` auf `NeedsReview` (PL-100 WP0.11). Den Weg
//! eines Menschen (`OperatorDecision`, an den Record-Digest gebunden, über
//! `triage_record_operator`) gibt es in `harw-dod-rules` vorerst nur in
//! Tests; er kehrt mit `harw-escalator` (PL-100 WP 3.2) hinter dem Feature
//! `operator-decision` zurück, das diese Crate nie einschaltet. Die
//! ungebundenen Testhilfen von `harw-dod-rules` (`triage`,
//! `triaged_finding_for_test`, Feature `test-support`) nehmen zwar jedes
//! Urteil an, dürfen aber nur aus Dev-Abhängigkeiten angefordert werden
//! (xtask-Gate `operator-decision`, Regel (d)); diese Crate tut das nur unter
//! `[dev-dependencies]`. Bis WP 3.2 erreicht deshalb in einem produktiven
//! Build (ohne Dev-Ziele) kein Befund eine Stufe der Leiter.
//!
//! # Aufteilung (PL-100 WP 3.0, Leitprinzip I8)
//! [`Ladder`] liegt seit WP 3.0 in `harw-dod-ladder`, [`TriageSubmission`] in
//! `harw-dod-submit-proto`. Beide hängen nicht am Signierteil
//! (`harw-dod-warden-proto`), sodass modellnahe Prozesse sie linken können,
//! ohne diese Crate zu ziehen. Diese Crate re-exportiert beide unter den
//! bisherigen Pfaden ([`ladder`], [`submission`] sowie die Typen an der
//! Wurzel); [`LadderError`] faltet sie per `From` in
//! [`EscalateError::NotEscalatable`].
//!
//! # Triage-Einreichung und Proof v2 (C-WPROTO, W3; PL-100 WP 3.0)
//! [`submission::TriageSubmission`] ist das eingefrorene Format, in dem ein
//! modellnaher Triage-Prozess nur ein Urteil plus gebundenen Finding-Digest
//! an den Escalator liefert (F-023); der Escalator liest den Record selbst und
//! signiert anschließend eine `harw_dod_warden_proto::SignedAuthorization`
//! (F-001). Seit PL-100 WP 3.0 baut diese Crate keinen fälschbaren
//! v1-`AuthorizationProof` und keine `WardenActionRequest` mehr und hängt
//! nicht am Signierteil `harw-dod-warden-proto`; [`action`]/[`freeze_ops`]
//! liefern nur den unsignierten [`AuthorizedAction`]. Die stillgelegte
//! Aktion `ReleaseCgroup` lehnt [`Action::authorize`] ab
//! ([`EscalateError::RetiredAction`]); `freeze_ops::authorize_release` ist
//! durch [`freeze_ops::authorize_thaw`] ersetzt.
//!
//! # Autorisierung vor Zustand
//! [`freeze_ops::authorize_freeze`] und [`freeze_ops::authorize_thaw`]
//! werten zuerst die nebenwirkungsfreien Prüfungen aus (`Ladder::stage_for`,
//! `Action::authorize`); erst danach, noch vor Übergabe der autorisierten
//! Aktion, schreiben sie ihren `FreezeStore`-Datensatz. Eine abgelehnte
//! Autorisierung lässt den Store unverändert — kein Phantom-`.active`, kein
//! `Lifted` für eine noch eingefrorene Cgroup — und wird nur über den
//! inhaltsfreien [`EscalateError`] gemeldet. [`freeze_ops::authorize_stage_gated`]
//! schreibt keinen Datensatz und lehnt `FreezeCgroup`/`ThawCgroup` deshalb ab
//! ([`EscalateError::NotAStageGatedAction`]): keine Freigabe ohne Datensatz.
//! Siehe [`freeze_ops`]-Moduldoku.
//!
//! # Rekonziliationsdisziplin beim Start
//! [`freeze_ops::reconcile_expired_freezes`] muss vom Aufrufer einmal, vor
//! jeder anderen Freeze-Operation, beim Start aufgerufen werden — sie wird
//! nicht intern selbsttätig ausgelöst. Siehe [`freeze_ops`]-Moduldoku.
//!
//! # Inhaltsfreie Ablehnungen
//! [`EscalateError`] meldet nie mehr als eine feste, nicht interpolierte
//! Kategorie — diese Crate liegt auf dem Weg zum Warden. Siehe
//! `error`-Moduldoku.
//!
//! # Keine Systemuhr
//! Jede Zeit (`authorized_at`, `frozen_at`, `resolved_at`, `now`) wird als
//! Parameter entgegengenommen; keine Funktion dieser Crate liest
//! `Timestamp::now()`.
//!
//! # Abhängigkeiten
//! `harw-types`, `harw-macros`, `harw-authority`, `harw-dod-rules`,
//! `harw-dod-ladder`, `harw-dod-submit-proto`, `harw-dod-warden-types`,
//! `harw-session-store`, `jiff` — kein `harw-tools`, kein `harw-core` und
//! seit PL-100 WP 3.0 kein Signierteil (`harw-dod-warden-proto`).
//! `serde` und `harw-dod-signals` brauchte nur der Code, der nach
//! `harw-dod-submit-proto` bzw. `harw-dod-ladder` gewandert ist (PL-100
//! WP 3.0); `harw-dod-signals` bleibt Dev-Abhängigkeit der Tests. Diese Crate
//! liegt auf dem Weg zum Warden; jede vermeidbare zusätzliche Abhängigkeit
//! wäre ein zu meldender Befund.
//!
//! # Nebenläufigkeit
//! Alle Typen dieser Crate sind reine Werte ohne innere Veränderlichkeit
//! (abgesehen von [`harw_session_store::FreezeStore`]s eigenem, dort
//! dokumentiertem Locking). `Send + Sync`, sicher aus jedem Thread nutzbar.
//!
//! # Fehler
//! [`EscalateError`] — siehe `error`-Moduldoku.

#![forbid(unsafe_code)]

pub mod action;
pub mod error;
pub mod freeze_ops;

/// Die Eskalationsleiter, seit PL-100 WP 3.0 die Crate `harw-dod-ladder`;
/// unter dem bisherigen Modulpfad `harw_dod_escalate::ladder` re-exportiert.
pub use harw_dod_ladder as ladder;
/// Die Triage-Einreichung, seit PL-100 WP 3.0 die Crate
/// `harw-dod-submit-proto`; unter dem bisherigen Modulpfad
/// `harw_dod_escalate::submission` re-exportiert.
pub use harw_dod_submit_proto as submission;

pub use action::{Action, ActionState, Authorized, AuthorizedAction, Proposed};
pub use error::{EscalateError, EscalateResult};
pub use freeze_ops::{
    authorize_freeze, authorize_stage_gated, authorize_thaw, reconcile_expired_freezes,
};
pub use harw_dod_ladder::{Ladder, LadderError, LadderResult};
pub use harw_dod_rules::{Finding, RuleChecked, Triaged, Verdict, triage_record};
pub use harw_dod_submit_proto::{
    SubmissionError, SubmissionResult, TRIAGE_SUBMISSION_VERSION, TriageSubmission,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
