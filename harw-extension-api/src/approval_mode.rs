//! Prozessweiter Freigabemodus.
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt genau eine Sache: die Antwort auf die Frage „wie viel
//! darf ein Werkzeugaufruf ohne Rückfrage tun?". Es entscheidet nicht selbst
//! über einzelne Aufrufe — das tut die Freigabepolitik, die den hier
//! hinterlegten Modus liest.
//!
//! # Schlüsseltypen
//! - [`ApprovalMode`] — die drei Stufen
//! - [`current`] / [`set`] — Lesen und Setzen des aktiven Modus
//!
//! # Warum prozessweit
//! Ein harw-Prozess führt genau eine interaktive Sitzung, und der Modus ist
//! eine Aussage dieser einen Person über diese eine Sitzung. Ihn stattdessen
//! durch Registry, Session und Politik zu fädeln, würde jede dieser Schichten
//! um einen Parameter erweitern, den nur eine einzige Stelle liest. Die
//! Umschaltung wirkt sofort, nicht erst an der nächsten Turn-Grenze: eine
//! Person, die gerade „alles fragen" wählt, meint den nächsten Aufruf, nicht
//! den übernächsten Turn.
//!
//! # Nebenläufigkeit
//! Der Zustand liegt in einem [`AtomicU8`] und ist von jedem Thread aus les-
//! und schreibbar, ohne Sperre.
//!
//! # Fehler
//! Das Modul erzeugt keine Fehler. Ein unbekannter Name führt in
//! [`ApprovalMode::parse`] zu `None`, nie zu einem stillen Standardwert.
//!
//! # Beispiele
//! ```rust
//! use harw_extension_api::approval_mode::{self, ApprovalMode};
//!
//! assert_eq!(ApprovalMode::parse("full"), Some(ApprovalMode::FullAccess));
//! approval_mode::set(ApprovalMode::AlwaysAsk);
//! assert_eq!(approval_mode::current(), ApprovalMode::AlwaysAsk);
//! approval_mode::set(ApprovalMode::Delegated);
//! ```

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

/// Wie viel ein Werkzeugaufruf ohne Rückfrage tun darf.
///
/// # Beschreibung
/// Die drei Stufen unterscheiden sich nur darin, wer entscheidet:
/// - [`Self::AlwaysAsk`]: die Person, bei jedem einzelnen Aufruf.
/// - [`Self::Delegated`]: harw entscheidet die harmlosen Fälle selbst und
///   fragt beim Rest.
/// - [`Self::FullAccess`]: harw entscheidet alles selbst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Jeder Werkzeugaufruf wird bestätigt, auch ein lesender.
    AlwaysAsk,
    /// Lesende und andere unkritische Werkzeuge laufen durch, alles
    /// Verändernde oder Ausführende fragt nach. Voreinstellung.
    Delegated,
    /// Kein Werkzeugaufruf fragt nach.
    FullAccess,
}

impl ApprovalMode {
    /// Der kanonische Kurzname, wie ihn `/permissions set` entgegennimmt.
    ///
    /// # Rückgabe
    /// `"ask"`, `"auto"` oder `"full"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlwaysAsk => "ask",
            Self::Delegated => "auto",
            Self::FullAccess => "full",
        }
    }

    /// Eine Zeile Klartext, die den Modus für eine Person beschreibt.
    ///
    /// # Rückgabe
    /// Die Beschreibung ohne abschließenden Punkt.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::AlwaysAsk => "jeder Werkzeugaufruf wird einzeln bestätigt",
            Self::Delegated => {
                "harw gibt lesende Werkzeuge selbst frei und fragt bei schreibenden und ausführenden"
            }
            Self::FullAccess => "kein Werkzeugaufruf fragt nach",
        }
    }

    /// Liest einen Modus aus seinem Namen.
    ///
    /// # Beschreibung
    /// Erkennt den Kurznamen und die gebräuchlichen Langformen. Groß- und
    /// Kleinschreibung sowie umgebender Leerraum sind egal.
    ///
    /// # Arguments
    /// - `name` (`&str`): der eingegebene Name.
    ///
    /// # Rückgabe
    /// Den Modus, oder `None` bei unbekanntem Namen.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "ask" | "always" | "always-ask" | "always_ask" => Some(Self::AlwaysAsk),
            "auto" | "delegated" | "harw" => Some(Self::Delegated),
            "full" | "full-access" | "full_access" | "all" => Some(Self::FullAccess),
            _ => None,
        }
    }

    /// Alle Stufen in der Reihenfolge zunehmender Autorität.
    pub const ALL: [Self; 3] = [Self::AlwaysAsk, Self::Delegated, Self::FullAccess];

    // Numerische Darstellung für den atomaren Zustand.
    fn to_repr(self) -> u8 {
        match self {
            Self::AlwaysAsk => 0,
            Self::Delegated => 1,
            Self::FullAccess => 2,
        }
    }

    // Umkehrung von `to_repr`. Ein unerwarteter Wert fällt auf die
    // Voreinstellung zurück, statt weiter zu öffnen.
    fn from_repr(repr: u8) -> Self {
        match repr {
            0 => Self::AlwaysAsk,
            2 => Self::FullAccess,
            _ => Self::Delegated,
        }
    }
}

impl Default for ApprovalMode {
    /// Liefert [`ApprovalMode::Delegated`] — dieselbe Grenze, die harw ohne
    /// jede Einstellung zieht.
    fn default() -> Self {
        Self::Delegated
    }
}

impl std::fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Geteilte, klonbare Zelle für einen [`ApprovalMode`] — die Instanz-Variante
/// zum prozessweiten [`current`]/[`set`].
///
/// # Verantwortlichkeit
/// Wo [`current`]/[`set`] einen einzigen, globalen Zustand für den gesamten
/// Prozess führen, trägt `ApprovalModeCell` ihren Zustand selbst: mehrere
/// Klone teilen sich denselben Modus (nützlich, wenn z. B. eine Sitzung und
/// ihre Kind-Sitzungen denselben Modus sehen sollen, ohne den globalen
/// Prozesszustand zu berühren), während [`Self::detached`] eine unabhängige
/// Kopie erzeugt, die ab diesem Zeitpunkt keinen Zustand mehr teilt.
///
/// # Nebenläufigkeit
/// Innen ein `Arc<RwLock<ApprovalMode>>`: viele gleichzeitige Leser, ein
/// Schreiber. Ein vergifteter Lock (ein anderer Thread ist während des
/// Haltens der Sperre paniert) blockiert [`Self::get`]/[`Self::set`] nicht —
/// beide entnehmen den zuletzt geschriebenen Wert über `into_inner` aus dem
/// vergifteten Guard, statt ihrerseits zu paniken. Ein vergifteter Lock
/// bedeutet hier nur „ein Leser/Schreiber ist mittendrin abgebrochen", nicht
/// „der Wert ist beschädigt" — `ApprovalMode` ist ein einfaches `Copy`-Enum,
/// das keine Invariante über mehrere Felder hinweg halten muss.
///
/// # Beispiele
/// ```rust
/// use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
///
/// let cell = ApprovalModeCell::new(ApprovalMode::AlwaysAsk);
/// let shared = cell.clone();
/// shared.set(ApprovalMode::FullAccess);
/// assert_eq!(cell.get(), ApprovalMode::FullAccess);
///
/// let detached = cell.detached();
/// detached.set(ApprovalMode::AlwaysAsk);
/// assert_eq!(cell.get(), ApprovalMode::FullAccess);
/// ```
#[derive(Clone)]
pub struct ApprovalModeCell(Arc<RwLock<ApprovalMode>>);

impl ApprovalModeCell {
    /// Erzeugt eine neue Zelle mit `mode` als Startwert.
    ///
    /// # Arguments
    /// - `mode` (`ApprovalMode`): der anfängliche Modus dieser Zelle.
    #[must_use]
    pub fn new(mode: ApprovalMode) -> Self {
        Self(Arc::new(RwLock::new(mode)))
    }

    /// Liest den aktuellen Modus dieser Zelle.
    ///
    /// # Rückgabe
    /// Den zuletzt über [`Self::set`] (auf dieser Zelle oder einem geteilten
    /// Klon davon) geschriebenen Modus.
    ///
    /// # Nebenläufigkeit
    /// Blockiert nie dauerhaft: ein vergifteter Lock wird über `into_inner`
    /// aufgelöst statt weiterzureichen (siehe Typ-Doku).
    #[must_use]
    pub fn get(&self) -> ApprovalMode {
        match self.0.read() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// Setzt den Modus dieser Zelle. Jeder geteilte Klon sieht die Änderung
    /// beim nächsten [`Self::get`].
    ///
    /// # Arguments
    /// - `mode` (`ApprovalMode`): der neue Modus.
    ///
    /// # Nebenläufigkeit
    /// Blockiert nie dauerhaft: ein vergifteter Lock wird über `into_inner`
    /// aufgelöst statt weiterzureichen (siehe Typ-Doku).
    pub fn set(&self, mode: ApprovalMode) {
        match self.0.write() {
            Ok(mut guard) => *guard = mode,
            Err(poisoned) => *poisoned.into_inner() = mode,
        }
    }

    /// Erzeugt eine unabhängige Kopie: eine neue Zelle mit demselben
    /// aktuellen Wert, aber ohne geteilten Zustand mit `self`. Spätere
    /// [`Self::set`]-Aufrufe auf der einen Zelle sind in der anderen nicht
    /// sichtbar.
    ///
    /// # Rückgabe
    /// Eine neue, eigenständige `ApprovalModeCell`.
    #[must_use]
    pub fn detached(&self) -> Self {
        Self::new(self.get())
    }
}

impl std::fmt::Debug for ApprovalModeCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalModeCell").field("mode", &self.get()).finish()
    }
}

impl Default for ApprovalModeCell {
    /// Startet mit [`ApprovalMode::default`] (`Delegated`) — derselbe
    /// Startwert wie das prozessweite [`current`]/[`set`].
    fn default() -> Self {
        Self::new(ApprovalMode::default())
    }
}

// Der aktive Modus. Startwert 1 ist `Delegated`.
static ACTIVE: AtomicU8 = AtomicU8::new(1);

/// Liest den aktiven Freigabemodus.
///
/// # Rückgabe
/// Den zuletzt über [`set`] gewählten Modus, sonst [`ApprovalMode::default`].
///
/// # Nebenläufigkeit
/// Sperrenfrei, von jedem Thread aus aufrufbar.
#[must_use]
pub fn current() -> ApprovalMode {
    ApprovalMode::from_repr(ACTIVE.load(Ordering::Relaxed))
}

/// Setzt den aktiven Freigabemodus.
///
/// # Beschreibung
/// Die Änderung gilt ab dem nächsten Werkzeugaufruf, auch innerhalb eines
/// bereits laufenden Turns.
///
/// # Arguments
/// - `mode` (`ApprovalMode`): die neue Stufe.
///
/// # Nebenläufigkeit
/// Sperrenfrei, von jedem Thread aus aufrufbar.
pub fn set(mode: ApprovalMode) {
    ACTIVE.store(mode.to_repr(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::{ApprovalMode, ApprovalModeCell, current, set};

    #[test]
    fn test_parse_recognizes_all_short_names() {
        assert_eq!(ApprovalMode::parse("ask"), Some(ApprovalMode::AlwaysAsk));
        assert_eq!(ApprovalMode::parse("auto"), Some(ApprovalMode::Delegated));
        assert_eq!(ApprovalMode::parse("full"), Some(ApprovalMode::FullAccess));
    }

    #[test]
    fn test_parse_recognizes_all_long_names_case_and_whitespace_insensitive() {
        let cases = [
            (" Always-Ask ", ApprovalMode::AlwaysAsk),
            ("ALWAYS_ASK", ApprovalMode::AlwaysAsk),
            ("always", ApprovalMode::AlwaysAsk),
            (" Delegated ", ApprovalMode::Delegated),
            ("HARW", ApprovalMode::Delegated),
            ("Full-Access", ApprovalMode::FullAccess),
            ("full_access", ApprovalMode::FullAccess),
            ("ALL", ApprovalMode::FullAccess),
        ];
        for (input, expected) in cases {
            assert_eq!(
                ApprovalMode::parse(input),
                Some(expected),
                "input {input:?} should parse to {expected:?}"
            );
        }
    }

    #[test]
    fn test_parse_unknown_name_returns_none() {
        assert_eq!(ApprovalMode::parse("quatsch"), None);
        assert_eq!(ApprovalMode::parse(""), None);
    }

    #[test]
    fn test_as_str_round_trips_over_all() {
        for mode in ApprovalMode::ALL {
            assert_eq!(ApprovalMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn test_default_is_delegated() {
        assert_eq!(ApprovalMode::default(), ApprovalMode::Delegated);
    }

    // `set`/`current` teilen sich einen prozessweiten `AtomicU8`. Tests laufen
    // parallel im selben Prozess, darum bündelt dieser eine Test jeden
    // Übergang und stellt am Ende ausdrücklich `Delegated` wieder her, damit
    // andere Tests im selben Binary den Startwert vorfinden.
    #[test]
    fn test_set_then_current_returns_the_set_mode() {
        set(ApprovalMode::AlwaysAsk);
        assert_eq!(current(), ApprovalMode::AlwaysAsk);

        set(ApprovalMode::FullAccess);
        assert_eq!(current(), ApprovalMode::FullAccess);

        set(ApprovalMode::Delegated);
        assert_eq!(current(), ApprovalMode::Delegated);
    }

    #[test]
    fn cell_new_returns_the_given_start_value() {
        let cell = ApprovalModeCell::new(ApprovalMode::FullAccess);
        assert_eq!(cell.get(), ApprovalMode::FullAccess);
    }

    #[test]
    fn cell_default_is_delegated() {
        assert_eq!(ApprovalModeCell::default().get(), ApprovalMode::Delegated);
    }

    #[test]
    fn cell_clone_shares_state() {
        let cell = ApprovalModeCell::new(ApprovalMode::AlwaysAsk);
        let clone = cell.clone();

        clone.set(ApprovalMode::FullAccess);

        assert_eq!(
            cell.get(),
            ApprovalMode::FullAccess,
            "a clone must share state with its origin"
        );
    }

    #[test]
    fn cell_detached_does_not_share_state() {
        let cell = ApprovalModeCell::new(ApprovalMode::AlwaysAsk);
        let detached = cell.detached();

        detached.set(ApprovalMode::FullAccess);

        assert_eq!(
            cell.get(),
            ApprovalMode::AlwaysAsk,
            "detached() must copy the current value but not share future writes"
        );
        assert_eq!(detached.get(), ApprovalMode::FullAccess);
    }

    #[test]
    fn cell_detached_starts_at_current_value_not_default() {
        let cell = ApprovalModeCell::new(ApprovalMode::FullAccess);
        assert_eq!(cell.detached().get(), ApprovalMode::FullAccess);
    }
}
