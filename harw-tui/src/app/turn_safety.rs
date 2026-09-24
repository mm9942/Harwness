//! Schutz laufender Turns und Kinder vor versehentlichem Abbruch
//! (Runde 5, Teil O).
//!
//! # Beschreibung
//! Befund aus dem Praxis-Transkript: Esc im Busy-Pfad
//! (`handle_busy_event`) brach den UIA-Turn **sofort** ab — und über den
//! registrierten Eltern-Token (`register_parent_cancel_token`) jedes
//! laufende Kind samt Nachkommen. Ein 20-Minuten-Orchestrator endete so als
//! „failed“, nur weil die Nutzerin beim Tippen einer neuen Nachricht Esc
//! drückte. Dieses Modul hält zwei Dinge:
//!
//! 1. **Doppel-Esc bei laufenden Kindern** ([`esc_should_interrupt`]):
//!    Laufen Kinder, die der Abbruch mitreißen würde, scharft das erste Esc
//!    nur eine Nachfrage („Esc bricht den Turn und N laufende Agenten ab –
//!    nochmal Esc zum Bestätigen“); erst ein zweites Esc innerhalb von
//!    [`ESC_CONFIRM_WINDOW`] bricht ab. Ohne laufende Kinder bleibt Esc der
//!    sofortige Abbruch (Runde 4). Hintergrund-Agenten zählen nicht mit: sie
//!    haben einen eigenen Cancel-Token und überleben den Turn.
//! 2. **Ehrliche Abbruch-Beschriftung** ([`aborted_label`]): offene
//!    Werkzeugaufrufe eines abgebrochenen Turns nennen, ob die Nutzerin
//!    abgebrochen hat oder eine Turn-Grenze (Budget) erreicht wurde.
//!
//! Neue Nachrichten während der Arbeit (Enter) brechen nie ab — sie gehen
//! über `queue_busy_key` in `pending_turns` und werden als nächster Turn
//! gesendet; Tests unten halten das fest.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use harw_core::cancel::CancelReason;
use harw_core::child_controller::ChildStatus;
use harw_types::SessionId;

use super::{ChatApp, Role};

/// Wie lange das erste Esc auf seine Bestätigung wartet.
pub(crate) const ESC_CONFIRM_WINDOW: Duration = Duration::from_secs(3);

/// Zustand der Esc-Nachfrage (Feld [`ChatApp::esc_confirm`]).
#[derive(Debug, Default)]
pub(crate) struct EscConfirm {
    /// Zeitpunkt des ersten, noch unbestätigten Esc.
    armed_at: Option<Instant>,
    /// Abbruchgrund des zuletzt beendeten Turns — für `TurnAborted`-Ereignisse,
    /// die erst nach dem Turn-Ende verarbeitet werden (dann ist
    /// `ChatApp::active_cancel` schon leer).
    last_cancel: Option<CancelReason>,
    /// Nur Tests: feste Zahl laufender Kinder statt der Spawner-Abfrage.
    #[cfg(test)]
    pub(crate) children_override: Option<usize>,
}

impl EscConfirm {
    /// Ob gerade eine Nachfrage offen ist.
    #[must_use]
    pub(crate) fn is_armed(&self) -> bool {
        self.armed_at
            .is_some_and(|at| at.elapsed() <= ESC_CONFIRM_WINDOW)
    }

    /// Verwirft eine offene Nachfrage (Turn-Ende, neuer Turn).
    pub(crate) fn reset(&mut self) {
        self.armed_at = None;
    }
}

/// Turn-Beginn: Nachfrage und gemerkter Abbruchgrund gelten nur je Turn.
pub(crate) fn begin_turn(app: &mut ChatApp) {
    app.esc_confirm.reset();
    app.esc_confirm.last_cancel = None;
}

/// Turn-Ende (vor `active_cancel = None`): merkt den Abbruchgrund.
pub(crate) fn end_turn(app: &mut ChatApp) {
    app.esc_confirm.reset();
    app.esc_confirm.last_cancel = app.active_cancel.as_ref().and_then(|token| token.reason());
}

/// Die Nachfrage-Zeile für `children` laufende Agenten.
#[must_use]
pub(crate) fn esc_confirm_hint(children: usize) -> String {
    let agents = if children == 1 {
        "1 laufenden Agenten".to_owned()
    } else {
        format!("{children} laufende Agenten")
    };
    format!("Esc bricht den Turn und {agents} ab – nochmal Esc zum Bestätigen")
}

/// Entscheidet, ob ein Esc im Busy-Pfad den Turn jetzt abbricht.
///
/// # Rückgabe
/// `true`: abbrechen (keine Kinder betroffen oder bestätigtes zweites Esc).
/// `false`: nur die Nachfrage wurde gescharft (Systemzeile), der Turn läuft
/// weiter.
pub(crate) fn esc_should_interrupt(app: &mut ChatApp) -> bool {
    let children = cancellable_children(app);
    if children == 0 {
        app.esc_confirm.reset();
        return true;
    }
    if app.esc_confirm.is_armed() {
        app.esc_confirm.reset();
        tracing::info!(children, "tui.turn.esc_confirmed");
        return true;
    }
    app.esc_confirm.armed_at = Some(Instant::now());
    tracing::info!(children, "tui.turn.esc_armed");
    app.push_line(Role::System, esc_confirm_hint(children));
    false
}

/// Zahl der laufenden Kinder, die ein Turn-Abbruch mitreißen würde.
///
/// # Beschreibung
/// Alle admittierten, noch nicht beendeten Nachkommen der Wurzel, außer
/// abgekoppelten Hintergrund-Kindern und deren Nachkommen (eigener Token).
fn cancellable_children(app: &ChatApp) -> usize {
    #[cfg(test)]
    if let Some(count) = app.esc_confirm.children_override {
        return count;
    }
    let Some(spawner) = app.managed_spawner() else {
        return 0;
    };
    let root = app.session_id();
    let detached: HashSet<String> = spawner
        .background_children()
        .running_for(root)
        .into_iter()
        .map(|run| run.child.as_str().to_owned())
        .collect();
    let nodes: Vec<(SessionId, SessionId, ChildStatus)> = spawner
        .list_descendants_for(root)
        .into_iter()
        .map(|record| (record.child, record.parent, record.status))
        .collect();
    count_cancellable(&nodes, &detached)
}

/// Reine Zählung zu [`cancellable_children`].
///
/// # Argumente
/// - `nodes`: `(kind, elternteil, status)` in Eltern-vor-Kind-Reihenfolge
///   (wie `list_descendants_for`).
/// - `detached`: IDs abgekoppelter Hintergrund-Kinder.
#[must_use]
pub(crate) fn count_cancellable(
    nodes: &[(SessionId, SessionId, ChildStatus)],
    detached: &HashSet<String>,
) -> usize {
    let mut excluded: HashSet<String> = HashSet::new();
    let mut count = 0_usize;
    for (child, parent, status) in nodes {
        if detached.contains(child.as_str()) || excluded.contains(parent.as_str()) {
            excluded.insert(child.as_str().to_owned());
            continue;
        }
        if matches!(
            status,
            ChildStatus::Admitted | ChildStatus::Running | ChildStatus::Paused
        ) {
            count = count.saturating_add(1);
        }
    }
    count
}

/// Beschriftung offener Werkzeugaufrufe eines abgebrochenen Turns.
///
/// # Beschreibung
/// Der Grund steht im Cancel-Token des Turns: gesetzt heißt ausdrücklicher
/// Abbruch; ungesetzt heißt, der Kern hat den Turn an einem Prüfpunkt
/// wegen einer Grenze (Runden, Werkzeugaufrufe, Tokens, Wanduhr) beendet.
#[must_use]
pub(crate) fn aborted_label(app: &ChatApp) -> String {
    let reason = app
        .active_cancel
        .as_ref()
        .and_then(|token| token.reason())
        .or(app.esc_confirm.last_cancel);
    aborted_label_for(reason)
}

/// Reine Form von [`aborted_label`].
#[must_use]
pub(crate) fn aborted_label_for(reason: Option<CancelReason>) -> String {
    match reason {
        Some(CancelReason::User) => "unvollständig (abgebrochen)".to_owned(),
        Some(CancelReason::Parent) => "unvollständig (abgebrochen: Elternteil)".to_owned(),
        Some(CancelReason::Budget) | None => {
            "unvollständig (abgebrochen: Turn-Grenze erreicht)".to_owned()
        }
        Some(CancelReason::LeaseLost) => "unvollständig (abgebrochen: Lease verloren)".to_owned(),
        Some(CancelReason::Shutdown) => "unvollständig (abgebrochen: Beenden)".to_owned(),
    }
}

#[cfg(test)]
mod tests;
