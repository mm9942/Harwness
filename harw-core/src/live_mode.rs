//! Live-Interaktionsmodus eines Agentenbaums (Runde 9, E6).
//!
//! # Verantwortung
//! Ein Moduswechsel der Oberfläche (`/mode`, Shift+Tab, Planfreigabe) soll
//! nicht nur die Wurzel erreichen, sondern **jedes laufende Kind** des Baums,
//! auch Hintergrund-Kinder — genau wie der Freigabemodus, der über
//! `ApprovalModeCell::follower` schon live folgt. Vorher bekam ein Kind beim
//! Start den Default-Modus und erfuhr nie von einem späteren Wechsel: ein
//! unter `plan` gestartetes Kind blieb blind, nachdem die Nutzerin längst
//! `work` gewählt hatte.
//!
//! # Schlüsseltypen
//! - [`LiveModeBroadcast`] — die Senderseite: der zuletzt veröffentlichte
//!   Modus plus Generationszähler. Gehört dem Spawner des Baums
//!   (`ManagedAgentSpawner::live_mode`); die Oberfläche veröffentlicht darüber.
//! - [`LiveModeFollower`] — die Empfängerseite je Kind-Sitzung. Die Sitzung
//!   übernimmt einen neuen Modus an ihrer nächsten Runden-Grenze
//!   ([`crate::session::AgentSession::sync_live_mode`], aufgerufen vor jedem
//!   Modellaufruf im Turn-Loop).
//!
//! # Autoritätsmodell
//! Der Modus wird über [`crate::session::AgentSession::set_mode`] angewandt,
//! also immer **von der Basis** der Kind-Sitzung aus (Agent-IR ∩
//! Eltern-Schnitt, Basis-Sandbox). Ein Wechsel kann deshalb nie mehr
//! freigeben, als das Profil des Kindes je hatte — `plan → work` stellt nur
//! die eigenen Schreib-/Delegationswerkzeuge des Kindes wieder her.
//!
//! # Nebenläufigkeit
//! `Send + Sync`: ein `Arc<RwLock<_>>`; ein vergifteter Lock wird über
//! `into_inner` aufgelöst, nie weitergereicht.

use std::sync::{Arc, RwLock};

use crate::mode::InteractionMode;

/// Innerer Zustand eines [`LiveModeBroadcast`].
#[derive(Debug, Clone, Copy, Default)]
struct LiveModeState {
    /// Zuletzt veröffentlichter Modus; `None`, solange nie einer
    /// veröffentlicht wurde (Kinder behalten dann ihren eigenen).
    mode: Option<InteractionMode>,
    /// Wird bei jeder Veröffentlichung erhöht.
    generation: u64,
}

/// Senderseite des Live-Modus eines Agentenbaums.
///
/// # Beschreibung
/// Klone teilen denselben Zustand. [`Self::publish`] wirkt nicht sofort auf
/// eine Sitzung, sondern erst, wenn deren [`LiveModeFollower`] an der
/// nächsten Runden-Grenze abgefragt wird.
///
/// # Beispiele
/// ```rust
/// use harw_core::InteractionMode;
/// use harw_core::live_mode::LiveModeBroadcast;
///
/// let live = LiveModeBroadcast::default();
/// let mut follower = live.follower();
/// assert_eq!(follower.poll(), None);
/// live.publish(InteractionMode::Work);
/// assert_eq!(follower.poll(), Some(InteractionMode::Work));
/// assert_eq!(follower.poll(), None);
/// ```
#[derive(Debug, Clone, Default)]
pub struct LiveModeBroadcast(Arc<RwLock<LiveModeState>>);

impl LiveModeBroadcast {
    /// Veröffentlicht `mode` als aktuellen Modus des Baums.
    ///
    /// # Arguments
    /// - `mode` (`InteractionMode`): der neue Modus.
    pub fn publish(&self, mode: InteractionMode) {
        let mut guard = match self.0.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.mode = Some(mode);
        guard.generation = guard.generation.saturating_add(1);
    }

    /// Der zuletzt veröffentlichte Modus (`None`: nie veröffentlicht).
    #[must_use]
    pub fn current(&self) -> Option<InteractionMode> {
        self.state().mode
    }

    /// Erzeugt einen Empfänger, der ab dem aktuellen Stand nur **spätere**
    /// Veröffentlichungen meldet. Den aktuellen Modus liest der Aufrufer bei
    /// Bedarf über [`Self::current`] (Start eines neuen Kindes).
    #[must_use]
    pub fn follower(&self) -> LiveModeFollower {
        LiveModeFollower {
            source: self.clone(),
            seen: self.state().generation,
        }
    }

    fn state(&self) -> LiveModeState {
        match self.0.read() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Empfängerseite des Live-Modus für genau eine Sitzung.
#[derive(Debug, Clone)]
pub struct LiveModeFollower {
    source: LiveModeBroadcast,
    /// Die zuletzt gesehene Generation.
    seen: u64,
}

impl LiveModeFollower {
    /// Meldet den Modus einer seit dem letzten Aufruf neuen Veröffentlichung.
    ///
    /// # Rückgabe
    /// `Some(mode)` genau einmal je neuer Generation; sonst `None`. Mehrere
    /// Veröffentlichungen zwischen zwei Aufrufen fallen zur letzten zusammen.
    pub fn poll(&mut self) -> Option<InteractionMode> {
        let state = self.source.state();
        if state.generation == self.seen {
            return None;
        }
        self.seen = state.generation;
        state.mode
    }

    /// Der aktuell veröffentlichte Modus, ohne den Stand zu verändern.
    #[must_use]
    pub fn current(&self) -> Option<InteractionMode> {
        self.source.current()
    }
}

/// Die Systemnotiz, die eine Kind-Sitzung bei einem Live-Wechsel bekommt.
///
/// # Beispiele
/// ```rust
/// use harw_core::InteractionMode;
/// use harw_core::live_mode::mode_change_note;
///
/// assert_eq!(
///     mode_change_note(InteractionMode::Plan, InteractionMode::Work),
///     "[harw] Modus geändert: plan → work"
/// );
/// ```
#[must_use]
pub fn mode_change_note(from: InteractionMode, to: InteractionMode) -> String {
    format!("[harw] Modus geändert: {} → {}", from.as_str(), to.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_follower_sees_only_later_publications_once() {
        let live = LiveModeBroadcast::default();
        live.publish(InteractionMode::Plan);
        let mut follower = live.follower();
        assert_eq!(follower.poll(), None, "Stand beim Start ist kein Wechsel");
        assert_eq!(follower.current(), Some(InteractionMode::Plan));

        live.publish(InteractionMode::Explore);
        live.publish(InteractionMode::Work);
        assert_eq!(follower.poll(), Some(InteractionMode::Work));
        assert_eq!(follower.poll(), None);
    }

    #[test]
    fn clones_share_the_broadcast() {
        let live = LiveModeBroadcast::default();
        let mut follower = live.follower();
        live.clone().publish(InteractionMode::Plan);
        assert_eq!(follower.poll(), Some(InteractionMode::Plan));
        assert_eq!(live.current(), Some(InteractionMode::Plan));
    }
}
