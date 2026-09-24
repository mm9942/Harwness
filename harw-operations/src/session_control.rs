//! Session-scoped mutation controller for state-changing operations.
//!
//! # Verantwortungsbereich
//! Definiert [`SessionController`] — den Provider-neutralen Trait, über den
//! zustandsändernde Slash-Commands (`/effort`, `/model switch`, `/provider switch`)
//! dauerhaften Session-Zustand mutieren, den ein nachfolgender `ModelRequest`
//! ausliest.
//!
//! Ohne diesen Controller können Ops den Zustand nur anzeigen oder ablehnen —
//! der klassische „TUI zeigt effort=high, ModelRequest sendet dennoch None"-Bug.
//!
//! # Schlüsseltypen
//! - [`SessionController`] — Trait für Session-Mutationen (Interior Mutability, `Send + Sync`)
//! - [`SessionControlSnapshot`] — Momentaufnahme des aktuellen Zustands
//! - [`SessionControlError`] — Fehlervarianten bei ungültigen Mutationen
//! - [`SharedSessionController`] — `Arc<dyn SessionController>` für ServiceMap
//! - [`NullSessionController`] — Drop-in-Dummy für Tests und CLI-Echo-Pfade
//!
//! # Nebenläufigkeit
//! `SessionController` erfordert `Send + Sync`. Implementierungen MÜSSEN
//! Interior Mutability verwenden (z. B. `Mutex<T>`), da der Trait `&self`
//! exponiert, um nahtlos in `Arc<dyn SessionController>` zu passen.
//!
//! # Fehlertypen
//! [`SessionControlError`] — drei Varianten: `UnknownProvider`, `UnknownModel`,
//! `Disconnected`.
//!
//! # Hinweis zur Implementierung
//! Die konkrete Implementierung mit echtem Effekt wird von der TUI geliefert
//! (via `Arc<Mutex<PendingSessionMutations>>`) und in die `ServiceMap` des
//! [`crate::context::OpContext`] injiziert. Spec: harw-operations Design §session_control.
//!
//! # Beispiel
//! ```rust
//! use harw_operations::session_control::{NullSessionController, SessionController};
//! use harw_types::ReasoningEffort;
//!
//! let ctrl = NullSessionController::new();
//! ctrl.set_reasoning_effort(Some(ReasoningEffort::High)).unwrap();
//! let snap = ctrl.snapshot();
//! assert_eq!(snap.reasoning_effort, Some(ReasoningEffort::High));
//! ```

use harw_types::ReasoningEffort;
use std::sync::Arc;

// ── UiaSelection ─────────────────────────────────────────────────────────────

/// Atomare Provider-/Modell-Auswahl für die UIA-Wurzelsitzung.
///
/// Die Auswahl ist bewusst ein eigener Wert und nicht eine Umdeutung von
/// [`SessionControlSnapshot::active_provider`] bzw.
/// [`SessionControlSnapshot::active_model`]. Diese generischen Felder bleiben
/// für Defaults, Worker und bereits bestehende Aufrufer erhalten; ein UIA-Pin
/// kann daneben unabhängig davon leben.
///
/// `None` in einem Feld bedeutet: Es gibt für diese Achse keinen expliziten
/// UIA-Wert. Die TUI kann beim Aufbau den jeweiligen Config-Default einsetzen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiaSelection {
    /// Provider-ID der UIA, falls explizit gewählt.
    pub provider: Option<String>,
    /// Modell-ID der UIA, falls explizit gewählt.
    pub model: Option<String>,
}

impl UiaSelection {
    /// Erzeugt eine UIA-Auswahl aus einem Provider-/Modell-Paar.
    #[must_use]
    pub fn new(provider: Option<String>, model: Option<String>) -> Self {
        Self { provider, model }
    }

    /// Erzeugt eine leere UIA-Auswahl.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Erzeugt die effektive UIA-Auswahl: UIA-spezifische Config-Werte haben
    /// Vorrang, die generischen Defaults dienen als Fallback.
    #[must_use]
    pub fn from_config(
        uia_provider: Option<&str>,
        uia_model: Option<&str>,
        default_provider: Option<&str>,
        default_model: Option<&str>,
    ) -> Self {
        Self {
            provider: uia_provider.or(default_provider).map(str::to_owned),
            model: uia_model.or(default_model).map(str::to_owned),
        }
    }

    /// Liefert die Provider-ID ohne Ownership-Transfer.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Liefert die Modell-ID ohne Ownership-Transfer.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Ob weder Provider noch Modell gesetzt ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none()
    }
}

// ── SessionControlSnapshot ────────────────────────────────────────────────────

/// Momentaufnahme des aktuell aktiven Session-Control-Zustands.
///
/// # Beschreibung
/// Lesepfad-Typ ohne Locks — erzeugt von [`SessionController::snapshot`].
/// Alle Felder sind `Option`, weil keine explizite Vorgabe gesetzt sein muss
/// (der Session-Zustand erbt dann Provider-Defaults).
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync` — sicher über Thread-Grenzen zu übertragen.
///
/// # Spec
/// harw-operations Design §session_control — Snapshot-Semantik.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionControlSnapshot {
    /// Aktuell gewünschter Reasoning-Effort-Level; `None` = Provider-Default.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// ID des aktiv ausgewählten Modells; `None` = Provider-Default.
    pub active_model: Option<String>,
    /// ID des aktiv ausgewählten Providers; `None` = Harness-Default.
    pub active_provider: Option<String>,
    /// UIA-spezifische Provider-/Modell-Auswahl. Sie wird nur auf eine
    /// elternlose UIA-Root-Session angewandt, nicht auf Child-/Worker-Sessions.
    pub uia_selection: UiaSelection,
    /// Kanonischer Name des aktiven Interaktionsmodus (`"chat"`, `"plan"`,
    /// `"explore"`, `"work"`); `None` = die Oberfläche führt keinen Modus.
    ///
    /// Bewusst `String` und nicht `harw_core::InteractionMode`: `harw-operations`
    /// liegt **unter** `harw-core` in der Schichtung und darf nicht auf die
    /// Laufzeit zeigen. Der Konsument typisiert mit `InteractionMode::parse`.
    pub interaction_mode: Option<String>,
}

impl SessionControlSnapshot {
    /// Erstellt eine leere Momentaufnahme — alle Felder `None`.
    ///
    /// # Rückgabe
    /// [`SessionControlSnapshot`] ohne explizit gesetzte Werte.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::session_control::SessionControlSnapshot;
    /// let snap = SessionControlSnapshot::empty();
    /// assert!(snap.reasoning_effort.is_none());
    /// assert!(snap.active_model.is_none());
    /// assert!(snap.active_provider.is_none());
    /// assert!(snap.interaction_mode.is_none());
    /// ```
    #[must_use]
    pub fn empty() -> Self {
        Self {
            reasoning_effort: None,
            active_model: None,
            active_provider: None,
            uia_selection: UiaSelection::empty(),
            interaction_mode: None,
        }
    }
}

// ── ContextUsageSnapshot ──────────────────────────────────────────────────────

/// Letzte angewandte (Auto-)Kompaktierung einer Session (Welle 3).
///
/// # Beschreibung
/// Reine Lese-Kopie der Felder des Protokoll-Ereignisses
/// `TurnEvent::CompactionApplied`. Bewusst ohne Abhängigkeit auf
/// `harw-protocol`: `harw-operations` liegt unter Protokoll und Laufzeit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LastCompaction {
    /// Auslöser der Kompaktierung (z. B. `"auto"`, `"manual"`, `"emergency"`).
    pub reason: String,
    /// Geschätzte Verlaufs-Tokens vor der Kompaktierung, falls bekannt.
    pub tokens_before: Option<u64>,
    /// Geschätzte Verlaufs-Tokens nach der Kompaktierung, falls bekannt.
    pub tokens_after: Option<u64>,
    /// Ob eine Zusammenfassung erzeugt wurde.
    pub summarized: bool,
    /// Anzahl ausgelassener Werkzeugergebnisse.
    pub elided_results: u32,
}

/// Momentaufnahme der Kontextfenster-Auslastung einer Session (Welle 3).
///
/// # Beschreibung
/// Spiegelt die zuletzt beobachteten Werte der Ereignisse
/// `TurnEvent::ContextUpdated` und `TurnEvent::CompactionApplied`, wie sie
/// die Oberfläche (TUI/Runtime) mitliest. Wird von `/status` und `/usage`
/// gelesen. `None`-Felder bedeuten: der Wert ist (noch) nicht bekannt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextUsageSnapshot {
    /// Zuletzt gemeldete belegte Tokens des Kontextfensters.
    pub used_tokens: u64,
    /// Größe des Kontextfensters in Tokens.
    pub window_tokens: u64,
    /// Schätzung der Tokens der nächsten Anfrage, falls bekannt.
    pub estimated_next_tokens: Option<u64>,
    /// Schwelle, ab der die nächste Kompaktierung ausgelöst wird.
    pub threshold_tokens: Option<u64>,
    /// Für die Modellausgabe reservierte Tokens.
    pub reserve_tokens: Option<u64>,
    /// Letzte angewandte Kompaktierung; `None` = in dieser Session noch keine.
    pub last_compaction: Option<LastCompaction>,
}

// ── SessionControlError ───────────────────────────────────────────────────────

/// Fehlervarianten eines [`SessionController`]-Aufrufs.
///
/// # Varianten
/// - [`Self::UnknownProvider`]: Provider-ID nicht im Modell-Katalog registriert.
/// - [`Self::UnknownModel`]: Modell-ID nicht bekannt oder mit aktivem Provider
///   inkompatibel.
/// - [`Self::Disconnected`]: Mutations-Kanal des Controllers geschlossen
///   (TUI-Shutdown).
/// - [`Self::ModeUnsupported`]: Die Oberfläche führt keinen Interaktionsmodus.
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync`.
///
/// # Spec
/// harw-operations Design §session_control — Fehlermodell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionControlError {
    /// Die Provider-ID ist nicht im Modell-Katalog registriert.
    UnknownProvider(String),
    /// Die Modell-ID ist nicht registriert oder mit dem aktiven Provider inkompatibel.
    UnknownModel(String),
    /// Der Mutations-Kanal des Controllers ist geschlossen (TUI-Shutdown).
    Disconnected,
    /// Die Oberfläche setzt keinen Interaktionsmodus um.
    ///
    /// Trägt den angeforderten Modusnamen. Wird von der Default-Implementierung
    /// von [`SessionController::request_mode`] erzeugt — siehe dort, warum der
    /// Default fehlschlägt statt Erfolg zu melden.
    ModeUnsupported(String),
}

impl std::fmt::Display for SessionControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider(id) => write!(f, "unknown provider: {id}"),
            Self::UnknownModel(id) => write!(f, "unknown model: {id}"),
            Self::Disconnected => write!(f, "session controller is disconnected"),
            Self::ModeUnsupported(mode) => write!(
                f,
                "this surface does not implement interaction modes (requested: {mode})"
            ),
        }
    }
}

impl std::error::Error for SessionControlError {}

// ── SessionController ─────────────────────────────────────────────────────────

/// Provider-neutraler Session-Mutations-Trait für zustandsändernde Ops.
///
/// # Beschreibung
/// Ermöglicht `/effort`, `/model switch` und `/provider switch` — Operationen,
/// die dauerhaften Session-Zustand ändern müssen, den ein nachfolgender
/// `ModelRequest` liest. Implementierungen MÜSSEN Interior Mutability verwenden
/// (`Mutex<T>`, `RwLock<T>`, `AtomicT`, Channels …), weil der Trait ausschließlich
/// `&self` exponiert.
///
/// # Nebenläufigkeit
/// `Send + Sync` — sicher als `Arc<dyn SessionController>` in der
/// [`crate::context::ServiceMap`] registrierbar.
///
/// # Fehlertypen
/// Alle Setter geben [`SessionControlError`] zurück.
///
/// # Spec
/// harw-operations Design §session_control — Trait-Kontrakt.
///
/// # Beispiel
/// ```rust
/// use harw_operations::session_control::{NullSessionController, SessionController};
/// use harw_types::ReasoningEffort;
///
/// let ctrl = NullSessionController::new();
/// ctrl.set_reasoning_effort(Some(ReasoningEffort::Medium)).unwrap();
/// assert_eq!(ctrl.snapshot().reasoning_effort, Some(ReasoningEffort::Medium));
/// ```
pub trait SessionController: Send + Sync {
    /// Setzt den gewünschten Reasoning-Effort-Level.
    ///
    /// # Argumente
    /// - `effort` (`Option<ReasoningEffort>`): Gewünschter Level; `None` setzt
    ///   auf Provider-Default zurück.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Mutations-Kanal geschlossen.
    fn set_reasoning_effort(
        &self,
        effort: Option<ReasoningEffort>,
    ) -> Result<(), SessionControlError>;

    /// Wechselt das aktive Modell auf die angegebene Modell-ID.
    ///
    /// # Argumente
    /// - `model_id` (`String`): Registrierte Modell-ID; der Controller
    ///   prüft Verfügbarkeit gegen den aktiven Provider.
    ///
    /// # Fehler
    /// - [`SessionControlError::UnknownModel`]: ID nicht bekannt.
    /// - [`SessionControlError::Disconnected`]: Mutations-Kanal geschlossen.
    fn set_active_model(&self, model_id: String) -> Result<(), SessionControlError>;

    /// Wechselt den aktiven Provider auf die angegebene Provider-ID.
    ///
    /// # Argumente
    /// - `provider_id` (`String`): Im Modell-Katalog registrierte Provider-ID.
    ///
    /// # Fehler
    /// - [`SessionControlError::UnknownProvider`]: ID nicht im Katalog.
    /// - [`SessionControlError::Disconnected`]: Mutations-Kanal geschlossen.
    fn set_active_provider(&self, provider_id: String) -> Result<(), SessionControlError>;

    /// Setzt Provider und Modell der UIA-Auswahl als zusammengehörigen Wert.
    ///
    /// Diese Mutation ist der bevorzugte Pfad für Config-Initialisierung und
    /// atomare UIA-Pin-Wechsel. Sie verändert die generischen
    /// `active_*`-Felder nicht.
    fn set_uia_selection(&self, selection: UiaSelection) -> Result<(), SessionControlError> {
        let _ = selection;
        Err(SessionControlError::Disconnected)
    }

    /// Setzt nur den Provider der UIA-Auswahl und erhält deren Modell.
    fn set_uia_provider(&self, provider_id: String) -> Result<(), SessionControlError> {
        let _ = provider_id;
        Err(SessionControlError::Disconnected)
    }

    /// Setzt nur das Modell der UIA-Auswahl und erhält deren Provider.
    fn set_uia_model(&self, model_id: String) -> Result<(), SessionControlError> {
        let _ = model_id;
        Err(SessionControlError::Disconnected)
    }

    /// Liefert die UIA-Auswahl als lockfreie Wertkopie.
    fn uia_selection(&self) -> UiaSelection {
        UiaSelection::empty()
    }

    /// Benannte Snapshot-Variante des UIA-Getters für Ops-/Runtime-Lesepfade.
    fn snapshot_uia_selection(&self) -> UiaSelection {
        self.uia_selection()
    }

    /// Fordert einen Wechsel des Interaktionsmodus an (`/mode chat|plan|explore|work|shell`).
    ///
    /// # Beschreibung
    /// Der Modus steuert Tool-Profil, Sandbox-Obergrenze und Prompt-Sektion einer
    /// Session. Wie bei `/model` und `/effort` *signalisiert* die Operation nur —
    /// angewandt wird der Wechsel von der Oberfläche an der Turn-Grenze, weil ein
    /// laufender Turn seine Tool-Menge nicht unter sich wechseln darf.
    ///
    /// Der Modusname wird als `&str` übergeben, nicht als `harw_core::InteractionMode`:
    /// `harw-operations` liegt unter `harw-core` und darf nicht auf die Laufzeit
    /// zeigen. Die Oberfläche typisiert mit `InteractionMode::parse` und lehnt
    /// unbekannte Namen ab.
    ///
    /// # Argumente
    /// - `mode` (`&str`): Kanonischer Modusname — `"chat"`, `"plan"`, `"explore"`
    ///   oder `"work"`.
    ///
    /// # Fehler
    /// - [`SessionControlError::ModeUnsupported`]: Diese Oberfläche führt keinen
    ///   Modus (Default-Implementierung) oder kennt den Namen nicht.
    /// - [`SessionControlError::Disconnected`]: Mutations-Kanal geschlossen.
    ///
    /// # Standardverhalten
    /// Die Default-Implementierung schlägt **fehl**. Das ist Absicht: ein
    /// `Ok(())`-Default würde jeder Oberfläche, die den Modus nicht umsetzt,
    /// stillschweigend „Modus gewechselt" bescheinigen — der Nutzer glaubte dann,
    /// im Explore-Modus zu sein, während die Session weiter Schreibrechte hätte.
    /// Fail-closed macht die Lücke sichtbar, statt sie zu verbergen.
    fn request_mode(&self, mode: &str) -> Result<(), SessionControlError> {
        Err(SessionControlError::ModeUnsupported(mode.to_owned()))
    }

    /// Gibt eine Momentaufnahme des aktuellen Session-Zustands zurück.
    ///
    /// # Rückgabe
    /// [`SessionControlSnapshot`] — Kopie des internen Zustands ohne Locks
    /// auf Aufrufseite.
    ///
    /// # Nebenläufigkeit
    /// Implementierungen müssen intern locken, falls nötig. Die zurückgegebene
    /// Momentaufnahme ist lock-frei und `Send + Sync`.
    fn snapshot(&self) -> SessionControlSnapshot;

    /// Liefert die zuletzt beobachtete Kontextfenster-Auslastung (Welle 3).
    ///
    /// # Beschreibung
    /// Rein lesender Zugriff für `/status` und `/usage`. Oberflächen, die
    /// `ContextUpdated`-/`CompactionApplied`-Ereignisse mitlesen, überschreiben
    /// diese Methode.
    ///
    /// # Standardverhalten
    /// `None` — die Oberfläche führt keine Kontext-Momentaufnahme; die
    /// Aufrufer lassen die Kontextzeile dann stillschweigend weg.
    fn context_usage(&self) -> Option<ContextUsageSnapshot> {
        None
    }
}

// ── SharedSessionController ───────────────────────────────────────────────────

/// Typ-Alias für die ServiceMap-Registration eines [`SessionController`].
///
/// # Beschreibung
/// Bequemer Alias, der [`SessionController`]-Implementierungen nahtlos in die
/// [`crate::context::ServiceMap`] einzufügen erlaubt:
/// ```rust,no_run
/// use harw_operations::session_control::{SharedSessionController, NullSessionController};
/// use harw_operations::context::ServiceMap;
/// use std::sync::Arc;
///
/// let mut services = ServiceMap::new();
/// let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
/// services.insert(ctrl);
/// ```
pub type SharedSessionController = Arc<dyn SessionController>;

// ── NullSessionController ─────────────────────────────────────────────────────

/// Drop-in-Dummy-Controller für Tests und CLI-Echo-Pfade ohne echten Session-Effekt.
///
/// # Beschreibung
/// Alle Setter gelingen und zeichnen die Werte lokal in einem `Mutex<SessionControlSnapshot>`
/// auf. `snapshot()` gibt die aufgezeichneten Werte zurück. Kein Netzwerk-,
/// Kanal- oder TUI-Bezug.
///
/// # Nebenläufigkeit
/// `Send + Sync` — intern via `std::sync::Mutex` geschützt.
///
/// # Fehlertypen
/// Gibt nur [`SessionControlError::Disconnected`] zurück, wenn der interne
/// Mutex vergiftet ist (Panics in anderen Threads).
///
/// # Spec
/// harw-operations Design §session_control — Null-Implementierung.
///
/// # Beispiel
/// ```rust
/// use harw_operations::session_control::{NullSessionController, SessionController};
/// use harw_types::ReasoningEffort;
///
/// let ctrl = NullSessionController::new();
/// ctrl.set_active_model("claude-opus-4".to_owned()).unwrap();
/// assert_eq!(ctrl.snapshot().active_model.as_deref(), Some("claude-opus-4"));
/// ```
#[derive(Debug, Default)]
pub struct NullSessionController {
    inner: std::sync::Mutex<SessionControlSnapshot>,
    context: std::sync::Mutex<Option<ContextUsageSnapshot>>,
}

impl NullSessionController {
    /// Erstellt einen neuen `NullSessionController` mit leerem Zustand.
    ///
    /// # Rückgabe
    /// Ein neuer [`NullSessionController`] — alle Snapshot-Felder `None`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::session_control::NullSessionController;
    /// let ctrl = NullSessionController::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Zeichnet eine Kontext-Momentaufnahme auf, die
    /// [`SessionController::context_usage`] anschließend liefert.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Interner Mutex ist vergiftet.
    pub fn set_context_usage(
        &self,
        usage: Option<ContextUsageSnapshot>,
    ) -> Result<(), SessionControlError> {
        self.context
            .lock()
            .map(|mut guard| {
                *guard = usage;
            })
            .map_err(|_| SessionControlError::Disconnected)
    }
}

impl Default for SessionControlSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}

impl SessionController for NullSessionController {
    /// Setzt den Reasoning-Effort-Level lokal im internen Snapshot.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Interner Mutex ist vergiftet.
    fn set_reasoning_effort(
        &self,
        effort: Option<ReasoningEffort>,
    ) -> Result<(), SessionControlError> {
        self.inner
            .lock()
            .map(|mut guard| {
                guard.reasoning_effort = effort;
            })
            .map_err(|_| SessionControlError::Disconnected)
    }

    /// Setzt die aktive Modell-ID lokal im internen Snapshot.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Interner Mutex ist vergiftet.
    fn set_active_model(&self, model_id: String) -> Result<(), SessionControlError> {
        self.inner
            .lock()
            .map(|mut guard| {
                guard.active_model = Some(model_id);
            })
            .map_err(|_| SessionControlError::Disconnected)
    }

    /// Setzt die aktive Provider-ID lokal im internen Snapshot.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Interner Mutex ist vergiftet.
    fn set_active_provider(&self, provider_id: String) -> Result<(), SessionControlError> {
        self.inner
            .lock()
            .map(|mut guard| {
                guard.active_provider = Some(provider_id);
            })
            .map_err(|_| SessionControlError::Disconnected)
    }

    /// Zeichnet die UIA-Auswahl als zusammengehörigen Wert auf.
    fn set_uia_selection(&self, selection: UiaSelection) -> Result<(), SessionControlError> {
        self.inner
            .lock()
            .map(|mut guard| {
                guard.uia_selection = selection;
            })
            .map_err(|_| SessionControlError::Disconnected)
    }

    /// Setzt den UIA-Provider unter demselben Mutex wie das Modell.
    fn set_uia_provider(&self, provider_id: String) -> Result<(), SessionControlError> {
        self.inner
            .lock()
            .map(|mut guard| {
                guard.uia_selection.provider = Some(provider_id);
            })
            .map_err(|_| SessionControlError::Disconnected)
    }

    /// Setzt das UIA-Modell unter demselben Mutex wie den Provider.
    fn set_uia_model(&self, model_id: String) -> Result<(), SessionControlError> {
        self.inner
            .lock()
            .map(|mut guard| {
                guard.uia_selection.model = Some(model_id);
            })
            .map_err(|_| SessionControlError::Disconnected)
    }

    /// Liefert die aufgezeichnete UIA-Auswahl.
    fn uia_selection(&self) -> UiaSelection {
        self.inner
            .lock()
            .map(|guard| guard.uia_selection.clone())
            .unwrap_or_else(|_| UiaSelection::empty())
    }

    /// Zeichnet den angeforderten Interaktionsmodus lokal im internen Snapshot auf.
    ///
    /// Überschreibt den fail-closed Trait-Default: der `NullSessionController` ist
    /// der Aufzeichnungs-Controller für Tests und CLI-Echo-Pfade und soll den
    /// Wunsch sichtbar machen, nicht abweisen. Ein echter Session-Effekt entsteht
    /// dadurch nicht — dafür ist die Oberfläche zuständig.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Interner Mutex ist vergiftet.
    fn request_mode(&self, mode: &str) -> Result<(), SessionControlError> {
        self.inner
            .lock()
            .map(|mut guard| {
                guard.interaction_mode = Some(mode.to_owned());
            })
            .map_err(|_| SessionControlError::Disconnected)
    }

    /// Gibt eine Momentaufnahme des aktuell aufgezeichneten Zustands zurück.
    ///
    /// # Rückgabe
    /// Geklonter [`SessionControlSnapshot`]; bei vergiftetem Mutex ein leerer Snapshot.
    fn snapshot(&self) -> SessionControlSnapshot {
        self.inner
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| SessionControlSnapshot::empty())
    }

    /// Liefert die über [`NullSessionController::set_context_usage`]
    /// aufgezeichnete Kontext-Momentaufnahme; bei vergiftetem Mutex `None`.
    fn context_usage(&self) -> Option<ContextUsageSnapshot> {
        self.context.lock().ok().and_then(|guard| guard.clone())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_snapshot_empty_returns_all_none() {
        let snap = SessionControlSnapshot::empty();
        assert!(
            snap.reasoning_effort.is_none(),
            "reasoning_effort should be None in empty snapshot"
        );
        assert!(
            snap.active_model.is_none(),
            "active_model should be None in empty snapshot"
        );
        assert!(
            snap.active_provider.is_none(),
            "active_provider should be None in empty snapshot"
        );
        assert!(snap.uia_selection.is_empty());
    }

    #[test]
    fn test_null_controller_records_reasoning_effort() -> TestResult {
        let ctrl = NullSessionController::new();
        ctrl.set_reasoning_effort(Some(ReasoningEffort::High))
            .map_err(ctx(
                "set_reasoning_effort should succeed on NullSessionController",
            ))?;
        let snap = ctrl.snapshot();
        assert_eq!(
            snap.reasoning_effort,
            Some(ReasoningEffort::High),
            "snapshot should reflect the recorded reasoning effort"
        );
        Ok(())
    }

    #[test]
    fn test_null_controller_records_active_model() -> TestResult {
        let ctrl = NullSessionController::new();
        ctrl.set_active_model("claude-opus-4".to_owned())
            .map_err(ctx(
                "set_active_model should succeed on NullSessionController",
            ))?;
        let snap = ctrl.snapshot();
        assert_eq!(
            snap.active_model.as_deref(),
            Some("claude-opus-4"),
            "snapshot should reflect the recorded active model"
        );
        Ok(())
    }

    #[test]
    fn test_null_controller_records_active_provider() -> TestResult {
        let ctrl = NullSessionController::new();
        ctrl.set_active_provider("anthropic".to_owned())
            .map_err(ctx(
                "set_active_provider should succeed on NullSessionController",
            ))?;
        let snap = ctrl.snapshot();
        assert_eq!(
            snap.active_provider.as_deref(),
            Some("anthropic"),
            "snapshot should reflect the recorded active provider"
        );
        Ok(())
    }

    #[test]
    fn test_uia_selection_prefers_specific_config_and_falls_back_per_axis() {
        let selection = UiaSelection::from_config(
            Some("uia-provider"),
            None,
            Some("default-provider"),
            Some("default-model"),
        );
        assert_eq!(selection.provider(), Some("uia-provider"));
        assert_eq!(selection.model(), Some("default-model"));
    }

    #[test]
    fn test_null_controller_keeps_uia_selection_separate_from_generic_state() -> TestResult {
        let ctrl = NullSessionController::new();
        ctrl.set_active_provider("generic-provider".to_owned())
            .map_err(ctx("generic provider setter should succeed"))?;
        ctrl.set_uia_selection(UiaSelection::new(
            Some("uia-provider".to_owned()),
            Some("uia-model".to_owned()),
        ))
        .map_err(ctx("UIA selection setter should succeed"))?;

        assert_eq!(
            ctrl.snapshot().active_provider.as_deref(),
            Some("generic-provider")
        );
        assert_eq!(
            ctrl.snapshot_uia_selection().provider(),
            Some("uia-provider")
        );
        assert_eq!(ctrl.snapshot_uia_selection().model(), Some("uia-model"));
        Ok(())
    }

    #[test]
    fn test_null_controller_snapshot_reflects_all_setters() -> TestResult {
        let ctrl = NullSessionController::new();
        ctrl.set_reasoning_effort(Some(ReasoningEffort::Medium))
            .map_err(ctx("set_reasoning_effort should succeed"))?;
        ctrl.set_active_model("claude-sonnet-4".to_owned())
            .map_err(ctx("set_active_model should succeed"))?;
        ctrl.set_active_provider("openai".to_owned())
            .map_err(ctx("set_active_provider should succeed"))?;

        let snap = ctrl.snapshot();
        assert_eq!(snap.reasoning_effort, Some(ReasoningEffort::Medium));
        assert_eq!(snap.active_model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(snap.active_provider.as_deref(), Some("openai"));
        Ok(())
    }

    #[test]
    fn test_error_display_variants_are_human_readable() {
        let unknown_provider = SessionControlError::UnknownProvider("acme".to_owned());
        let unknown_model = SessionControlError::UnknownModel("gpt-99".to_owned());
        let disconnected = SessionControlError::Disconnected;

        assert!(
            unknown_provider.to_string().contains("acme"),
            "UnknownProvider display must contain the provider id"
        );
        assert!(
            unknown_model.to_string().contains("gpt-99"),
            "UnknownModel display must contain the model id"
        );
        assert!(
            !disconnected.to_string().is_empty(),
            "Disconnected display must not be empty"
        );
    }

    #[test]
    fn test_null_controller_set_effort_none_clears_value() -> TestResult {
        let ctrl = NullSessionController::new();
        ctrl.set_reasoning_effort(Some(ReasoningEffort::Low))
            .map_err(ctx("initial set should succeed"))?;
        ctrl.set_reasoning_effort(None)
            .map_err(ctx("clearing effort should succeed"))?;
        let snap = ctrl.snapshot();
        assert_eq!(
            snap.reasoning_effort, None,
            "snapshot reasoning_effort should be None after clearing"
        );
        Ok(())
    }

    #[test]
    fn test_shared_controller_arc_clone_shares_state() -> TestResult {
        use std::sync::Arc;
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let ctrl2 = Arc::clone(&ctrl);

        ctrl.set_active_model("test-model".to_owned())
            .map_err(ctx("set_active_model should succeed"))?;

        let snap = ctrl2.snapshot();
        assert_eq!(
            snap.active_model.as_deref(),
            Some("test-model"),
            "Arc clone must share the same internal state"
        );
        Ok(())
    }

    #[test]
    fn test_session_control_error_implements_std_error() {
        fn accepts_std_error(_e: &dyn std::error::Error) {}
        let e = SessionControlError::Disconnected;
        accepts_std_error(&e);
    }

    #[test]
    fn null_controller_context_usage_defaults_to_none_and_records_values() -> TestResult {
        let ctrl = NullSessionController::new();
        assert_eq!(ctrl.context_usage(), None);
        let usage = ContextUsageSnapshot {
            used_tokens: 1_000,
            window_tokens: 10_000,
            threshold_tokens: Some(8_000),
            ..ContextUsageSnapshot::default()
        };
        ctrl.set_context_usage(Some(usage.clone()))
            .map_err(ctx("set_context_usage"))?;
        assert_eq!(ctrl.context_usage(), Some(usage));
        Ok(())
    }
}
