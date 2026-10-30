//! Die Eskalationsleiter: der eine Ort, an dem aus einem Befund eine
//! autorisierte Aktion wird (Knoten **AW5-03**).
//!
//! # Verantwortungsbereich
//! Diese Crate schließt die Kette zwischen Befund und Durchsetzung: sie
//! nimmt ein `Finding<RuleChecked>` (Eigentum ausschließlich über
//! `harw-dod-rules`, AW4-03, erwerbbar) über [`triage`] in ein
//! `Finding<Triaged>` — beide Typen re-exportiert, nicht neu definiert (siehe
//! „Der dritte Zustand" unten). Sie definiert selbst [`Action<Proposed>`] und
//! [`Action<Authorized>`] ([`action`]-Modul) sowie [`Ladder`]
//! ([`ladder`]-Modul), die entscheidet, welche Aktion ab welcher Stufe
//! zulässig ist und welche Stufe ein Befund erreicht hat. Sie verwaltet die
//! durable `FreezeStore`-Persistenz (AW5-05) über [`freeze_ops`]. Sie führt
//! selbst **keine** Durchsetzung aus — das ist Sache von `harw-dod-warden`
//! (AW5-04a), das ein von dieser Crate erzeugtes `WardenActionRequest` über
//! den Socket empfängt und gegen `AuthorizationProof::verify` prüft.
//!
//! # Der dritte Zustand: `Finding<Triaged>` lebt in `harw-dod-rules`, nicht
//! hier
//! `Finding<Raw|RuleChecked|Triaged>` und alle drei Typestate-Übergänge
//! (`Finding::raw`, `Finding::check`, `triage`) sind bereits vollständig in
//! `harw-dod-rules` (AW4-03) realisiert — siehe dessen `finding.rs`-Moduldoku
//! für die Begründung, warum Typbesitzer und Übergangsbesitzer dieselbe
//! Crate sein müssen. Diese Crate definiert `Triaged` **nicht** neu; sie
//! re-exportiert [`Finding`], [`RuleChecked`], [`Triaged`], [`Verdict`] und
//! [`triage`] und konsumiert sie. **Die Berechtigung ist der Besitz des
//! Wertes:** an ein `Finding<Triaged>` kommt nur, wer zuerst ein
//! `Finding<RuleChecked>` **besessen** hat — und das kann ausschließlich
//! `harw-dod-rules` herstellen (`Finding::raw`/`Finding::check` sind dort
//! `pub(crate)`). Der Typ trägt die Regel; kein Reviewer muss sie tragen,
//! kein Aufrufer kann sich eines fälschen, um [`Action::authorize`] zu
//! erreichen.
//!
//! # Invariante S1: Autorisierung entsteht an genau einem Ort
//! [`Action::authorize`] ist `pub(crate)` — **niemand außerhalb dieser Crate
//! kann ein `Action<Authorized>` herstellen.** Es gibt keinen öffentlichen
//! Konstruktor, kein `From`, kein `Deserialize` für `Action<S>` selbst, über
//! den ein Aufrufer den Typestate-Übergang umgehen könnte; ein
//! `WardenActionRequest`, das `harw-dod-warden` über den Socket empfängt, ist
//! ein deserialisierter Wire-Wert, kein `Action<Authorized>` — nur die
//! hiesige Konstruktion durchläuft die Prüfungen (Verdict, Zulässigkeit,
//! Belegbindung). Siehe [`action`]-Moduldoku für den vollständigen Beleg
//! einschließlich eines `compile_fail`-Doctests.
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
//! # Triage-Einreichung und Proof v2 (C-WPROTO, W3)
//! [`submission::TriageSubmission`] ist das eingefrorene Format, in dem ein
//! modellnaher Triage-Prozess nur ein Urteil plus gebundenen Finding-Digest
//! an den Escalator liefert (F-023); der Escalator liest den Record selbst und
//! signiert anschließend eine `harw_dod_warden_proto::SignedAuthorization`
//! (F-001). Die Umstellung von [`action`]/[`freeze_ops`] vom fälschbaren
//! v1-`AuthorizationProof` auf Proof v2 ist Folgearbeit W5 D-ESC.
//!
//! # Autorisierung vor Zustand
//! [`freeze_ops::authorize_freeze`] und [`freeze_ops::authorize_release`]
//! werten zuerst die nebenwirkungsfreien Prüfungen aus (`Ladder::stage_for`,
//! `Action::authorize`); erst danach, noch vor Übergabe der autorisierten
//! Aktion, schreiben sie ihren `FreezeStore`-Datensatz. Eine abgelehnte
//! Autorisierung lässt den Store unverändert — kein Phantom-`.active`, kein
//! `Lifted` für eine noch eingefrorene Cgroup — und wird nur über den
//! inhaltsfreien [`EscalateError`] gemeldet. Siehe [`freeze_ops`]-Moduldoku.
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
//! `harw-types`, `harw-macros`, `harw-dod-rules`, `harw-dod-signals`,
//! `harw-dod-warden-proto`, `harw-session-store`, `jiff`, `serde` (seit
//! C-WPROTO, für `TriageSubmission`) — kein `harw-tools`,
//! kein `harw-core`. Diese Crate liegt auf dem Weg zum Warden; jede
//! vermeidbare zusätzliche Abhängigkeit wäre ein zu meldender Befund.
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
pub mod ladder;
pub mod submission;

pub use action::{Action, ActionState, Authorized, Proposed};
pub use error::{EscalateError, EscalateResult};
pub use freeze_ops::{
    authorize_freeze, authorize_release, authorize_stage_gated, reconcile_expired_freezes,
};
pub use harw_dod_rules::{Finding, RuleChecked, Triaged, Verdict, triage};
pub use ladder::Ladder;
pub use submission::{
    SubmissionError, SubmissionResult, TRIAGE_SUBMISSION_VERSION, TriageSubmission,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
