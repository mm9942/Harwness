//! `harw-tool-plan` — Plan-Modus wie in Claude Code (Runde 5, Teil F).
//!
//! # Zweck
//! | Baustein | Aufgabe |
//! |---|---|
//! | [`PlanToolProvider`] | `plan.write`, `plan.exit`, `plan.enter`, `ask_user` — nur Wurzel |
//! | [`PlanModeGate`] | sperrt im Plan-Modus sofort alles außerhalb der Plan-Positivliste |
//! | [`PinnedPlanContextProvider`] | liefert den freigegebenen Plan in jeder Anfrage (übersteht die Verdichtung) |
//! | [`PlanSession`] | geteilter Zustand: Sperre, aktueller Slug, angehefteter Plan, Wurzelbindung |
//! | [`prompt`] | Fragekanal zur TUI (`PlanUiRequest`) |
//! | [`confirm`] | Bestätigung eines Plan-Graphen der `plan`-Operation (Teil P) |
//! | [`plan_file`] | `.harw/plans/<slug>.md` lesen, schreiben, auflisten |
//!
//! # Sicherheitskontrakt
//! 1. **`plan.write` schreibt nur unter `.harw/plans/`.** Das Modell nennt
//!    höchstens einen Slug; der Pfad entsteht aus dem von der Montage
//!    gesetzten Plan-Verzeichnis; Symlinks werden abgelehnt.
//! 2. **Nur Wurzel, nur TUI.** Die Montage hängt den Provider nur an die
//!    Wurzel; jeder Aufruf prüft zusätzlich die gebundene Wurzel-Sitzung.
//!    `plan.exit`/`plan.enter`/`ask_user` ohne TUI-Kanal → fail-closed.
//! 3. **Moduswechsel nur nach Bestätigung.** Kein Werkzeug schaltet selbst
//!    um; das tut die TUI nach der Entscheidung der Nutzerin.
//! 4. **Sofortige Sperre.** [`PlanModeGate`] lehnt im Plan-Modus jeden
//!    schreibenden/ausführenden Aufruf ab, auch mitten im Turn.
//!
//! # Nebenläufigkeit
//! Alle öffentlichen Typen sind `Send + Sync` (Fragen: nur `Send`).

#![forbid(unsafe_code)]

pub mod ask_user;
// Runde 5, Teil P: Bestätigung eines Plan-Graphen der `plan`-Operation.
pub mod confirm;
pub mod context;
pub mod gate;
pub mod plan_file;
pub mod prompt;
pub mod provider;
pub mod session;

#[cfg(test)]
mod test_support;

/// Name des Werkzeugs, das den Plan schreibt.
pub const PLAN_WRITE_TOOL: &str = "plan.write";
/// Name des Werkzeugs, das den Plan zur Freigabe vorlegt.
pub const PLAN_EXIT_TOOL: &str = "plan.exit";
/// Name des Werkzeugs, das den Plan-Modus vorschlägt.
pub const PLAN_ENTER_TOOL: &str = "plan.enter";
/// Name des Rückfrage-Werkzeugs.
pub const ASK_USER_TOOL: &str = "ask_user";

/// Die Plan-Werkzeuge, die im Plan-Modus zusätzlich zur Lese-/Recherche-
/// Fläche erlaubt sind (`harw_core::mode` `PLAN_TOOLS` muss sie enthalten).
/// `plan.enter` fehlt bewusst: im Plan-Modus ist es sinnlos.
pub const PLAN_MODE_EXTRA_TOOLS: &[&str] = &[PLAN_WRITE_TOOL, PLAN_EXIT_TOOL, ASK_USER_TOOL];

/// Werkzeuge, deren Ausführung selbst die Bestätigung der Nutzerin ist
/// (eigenes Fenster, keine Wirkung ohne Antwort). Die Standardpolitik fragt
/// für sie nicht zusätzlich nach (`harw_registry_defaults::DefaultApprovalPolicy`).
pub const USER_DIALOG_TOOLS: &[&str] = &[PLAN_EXIT_TOOL, PLAN_ENTER_TOOL, ASK_USER_TOOL];

pub use ask_user::{AskOption, AskQuestion};
pub use confirm::{
    PLAN_CONFIRM_TIMEOUT, PlanConfirmChannel, PlanConfirmDecision, PlanConfirmKind,
    PlanConfirmOutcome, PlanConfirmPrompt,
};
pub use context::{PINNED_PLAN_NAMESPACE, PinnedPlanContextProvider};
pub use gate::{PLAN_MODE_GATE_LABEL, PlanModeGate};
pub use plan_file::{PlanDir, PlanEntry, PlanFileError};
pub use prompt::{
    AskUserAnswer, AskUserPrompt, PlanEnterPrompt, PlanExitDecision, PlanExitPrompt,
    PlanUiReceiver, PlanUiRequest, PlanUiSender, QuestionAnswer, plan_ui_channel,
};
pub use provider::PlanToolProvider;
pub use session::{PinnedPlan, PinnedPlanDoc, PlanModeLock, PlanSession};
