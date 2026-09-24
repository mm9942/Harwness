//! Geteilter Plan-Zustand einer Sitzung (Runde 5, Teil F).
//!
//! # Beschreibung
//! [`PlanSession`] ist der eine Zustand, den Werkzeuge (`plan.*`, `ask_user`),
//! die sofortige Sperre ([`crate::gate::PlanModeGate`]), der angeheftete Plan
//! ([`crate::context::PinnedPlanContextProvider`]) und die TUI (`/plan …`,
//! Shift+Tab) gemeinsam sehen. Er entsteht einmal je Montage
//! (`harw-runtime`) und wird per `Clone` geteilt (alles hinter `Arc`).
//!
//! - [`PlanModeLock`]: `true`, solange die Stufe `plan` gilt. Die TUI setzt
//!   den Wert **sofort** beim Wechsel (auch mitten im Turn); der Kern
//!   übernimmt `InteractionMode::Plan` erst an der Turn-Grenze.
//! - aktueller Slug: wiederholtes `plan.write` in derselben Sitzung
//!   überschreibt dieselbe Datei; `/plan open <slug>` setzt ihn.
//! - [`PinnedPlan`]: der freigegebene Plan; ein Kontextbeitrag je Anfrage,
//!   deshalb übersteht er jede Verdichtung des Verlaufs.
//! - Wurzelbindung: die Werkzeuge antworten nur der Wurzel-Sitzung, deren ID
//!   die Montage nach dem Bau bindet ([`PlanSession::bind_root`]); ohne
//!   Bindung lehnen sie fail-closed ab.
//!
//! # Nebenläufigkeit
//! `Send + Sync`; Atomics und `Mutex` mit Vergiftungs-Rückfall
//! (`PoisonError::into_inner`) — ein vergifteter Wert ist hier nur Anzeige-
//! bzw. Arbeitszustand, nie Autorität über mehr als die Sperre selbst.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::plan_file::PlanDir;

/// Höchstgröße des angehefteten Plans im Kontext (Bytes).
pub const PINNED_PLAN_MAX_BYTES: usize = 32 * 1024;

/// Die sofort wirkende Plan-Sperre.
#[derive(Debug, Clone, Default)]
pub struct PlanModeLock(Arc<AtomicBool>);

impl PlanModeLock {
    /// Eine neue, offene Sperre.
    #[must_use]
    pub fn new(locked: bool) -> Self {
        Self(Arc::new(AtomicBool::new(locked)))
    }

    /// Setzt die Sperre (`true` = Plan-Modus).
    pub fn set(&self, locked: bool) {
        self.0.store(locked, Ordering::SeqCst);
    }

    /// `true`, solange der Plan-Modus gilt.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Der freigegebene, angeheftete Plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedPlanDoc {
    /// Slug des Plans.
    pub slug: String,
    /// Anzeigepfad (`.harw/plans/<slug>.md`).
    pub display_path: String,
    /// Planinhalt (höchstens [`PINNED_PLAN_MAX_BYTES`], sonst gekürzt).
    pub content: String,
}

/// Geteilte Zelle des angehefteten Plans.
#[derive(Debug, Clone, Default)]
pub struct PinnedPlan(Arc<Mutex<Option<PinnedPlanDoc>>>);

impl PinnedPlan {
    /// Heftet einen Plan an (ersetzt einen vorherigen). Zu große Pläne werden
    /// an einer Zeichengrenze gekürzt und markiert.
    pub fn pin(&self, slug: &str, content: &str) {
        let content = truncate_utf8(content, PINNED_PLAN_MAX_BYTES);
        let doc = PinnedPlanDoc {
            slug: slug.to_owned(),
            display_path: crate::plan_file::display_path(slug),
            content,
        };
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(doc);
    }

    /// Löst den angehefteten Plan.
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Eine Kopie des angehefteten Plans.
    #[must_use]
    pub fn get(&self) -> Option<PinnedPlanDoc> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Kürzt `text` auf höchstens `max` Bytes an einer Zeichengrenze.
pub(crate) fn truncate_utf8(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    const MARK: &str = "\n\n[… gekürzt — vollständiger Plan in der Plan-Datei]";
    let budget = max.saturating_sub(MARK.len());
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARK}", &text[..end])
}

/// Der geteilte Plan-Zustand einer Montage.
#[derive(Debug, Clone)]
pub struct PlanSession {
    dir: PlanDir,
    lock: PlanModeLock,
    current: Arc<Mutex<Option<String>>>,
    pinned: PinnedPlan,
    root: Arc<OnceLock<String>>,
}

impl PlanSession {
    /// Baut den Zustand über dem Plan-Verzeichnis.
    ///
    /// # Arguments
    /// - `dir` ([`PlanDir`]): `<projekt>/.harw/plans` aus der Montage.
    /// - `initially_locked` (`bool`): `true`, wenn die Sitzung im Plan-Modus
    ///   startet (`--mode plan`).
    #[must_use]
    pub fn new(dir: PlanDir, initially_locked: bool) -> Self {
        Self {
            dir,
            lock: PlanModeLock::new(initially_locked),
            current: Arc::new(Mutex::new(None)),
            pinned: PinnedPlan::default(),
            root: Arc::new(OnceLock::new()),
        }
    }

    /// Das Plan-Verzeichnis.
    #[must_use]
    pub fn dir(&self) -> &PlanDir {
        &self.dir
    }

    /// Die Plan-Sperre.
    #[must_use]
    pub fn lock(&self) -> &PlanModeLock {
        &self.lock
    }

    /// Der angeheftete Plan.
    #[must_use]
    pub fn pinned(&self) -> &PinnedPlan {
        &self.pinned
    }

    /// Der aktuelle Slug dieser Sitzung.
    #[must_use]
    pub fn current_slug(&self) -> Option<String> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Setzt den aktuellen Slug (`plan.write`, `/plan open`).
    pub fn set_current_slug(&self, slug: Option<String>) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = slug;
    }

    /// Bindet die Wurzel-Sitzung (einmalig; ein zweiter Aufruf ist wirkungslos).
    ///
    /// # Returns
    /// `true`, wenn diese Bindung gilt (erster Aufruf oder dieselbe ID).
    pub fn bind_root(&self, session_id: &str) -> bool {
        let bound = self.root.get_or_init(|| session_id.to_owned());
        bound == session_id
    }

    /// `true` genau dann, wenn `session_id` die gebundene Wurzel ist.
    /// Ohne Bindung immer `false` (fail-closed).
    #[must_use]
    pub fn is_root(&self, session_id: &str) -> bool {
        self.root.get().is_some_and(|root| root == session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_shared_between_clones() {
        let session = PlanSession::new(PlanDir::new("/p/.harw/plans"), false);
        let clone = session.clone();
        assert!(!clone.lock().is_locked());
        session.lock().set(true);
        assert!(clone.lock().is_locked());
    }

    #[test]
    fn root_binding_is_fail_closed_and_single_assignment() {
        let session = PlanSession::new(PlanDir::new("/p/.harw/plans"), false);
        assert!(!session.is_root("root"), "ohne Bindung nie Wurzel");
        assert!(session.bind_root("root"));
        assert!(!session.bind_root("kind"), "zweite Bindung wirkungslos");
        assert!(session.is_root("root"));
        assert!(!session.is_root("kind"));
    }

    #[test]
    fn pinned_plan_is_truncated_on_a_char_boundary() {
        let pinned = PinnedPlan::default();
        let big = "ä".repeat(PINNED_PLAN_MAX_BYTES);
        pinned.pin("gross", &big);
        let doc = pinned.get();
        assert!(
            doc.as_ref()
                .is_some_and(|doc| doc.content.len() <= PINNED_PLAN_MAX_BYTES)
        );
        assert!(
            doc.as_ref()
                .is_some_and(|doc| doc.content.contains("gekürzt"))
        );
        assert_eq!(
            doc.map(|doc| doc.display_path),
            Some(".harw/plans/gross.md".to_owned())
        );
        pinned.clear();
        assert!(pinned.get().is_none());
    }
}
