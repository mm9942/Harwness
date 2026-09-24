//! Freigabemodus.
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt genau eine Sache: die Antwort auf die Frage „wie viel
//! darf ein Werkzeugaufruf ohne Rückfrage tun?". Es entscheidet nicht selbst
//! über einzelne Aufrufe — das tut die Freigabepolitik, die den hier
//! hinterlegten Modus liest.
//!
//! # Schlüsseltypen
//! - [`ApprovalMode`] — die drei Stufen
//! - [`ApprovalModeCell`] — die Instanz, in der ein Modus lebt
//!
//! # Warum pro Sitzung
//! Der Modus lebt pro Session in einer [`ApprovalModeCell`], nicht
//! prozessweit: eine Sitzung und ihre Kind-Sitzungen teilen sich einen Klon
//! derselben Zelle, ohne dass Registry, Session und Politik dafür einen
//! globalen Zustand fädeln müssten. Die Umschaltung wirkt sofort, nicht erst
//! an der nächsten Turn-Grenze: eine Person, die gerade „alles fragen"
//! wählt, meint den nächsten Aufruf, nicht den übernächsten Turn.
//!
//! # Nebenläufigkeit
//! Siehe [`ApprovalModeCell`] für die Details zu Sperre und Vergiftung.
//!
//! # Fehler
//! Das Modul erzeugt keine Fehler. Ein unbekannter Name führt in
//! [`ApprovalMode::parse`] zu `None`, nie zu einem stillen Standardwert.
//!
//! # Beispiele
//! ```rust
//! use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
//!
//! assert_eq!(ApprovalMode::parse("full"), Some(ApprovalMode::FullAccess));
//!
//! let cell = ApprovalModeCell::new(ApprovalMode::AlwaysAsk);
//! assert_eq!(cell.get(), ApprovalMode::AlwaysAsk);
//! cell.set(ApprovalMode::Delegated);
//! assert_eq!(cell.get(), ApprovalMode::Delegated);
//! ```

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

    /// Die Autoritätsstufe als Zahl.
    ///
    /// # Rückgabe
    /// `0` für [`Self::AlwaysAsk`], `1` für [`Self::Delegated`], `2` für
    /// [`Self::FullAccess`] — je größer, desto weniger wird gefragt.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::AlwaysAsk => 0,
            Self::Delegated => 1,
            Self::FullAccess => 2,
        }
    }

    /// Deckelt diesen Modus auf höchstens `cap`.
    ///
    /// # Beschreibung
    /// Ein Modus mit mehr Autorität als `cap` wird auf `cap` gesenkt; ein
    /// strengerer Modus bleibt unverändert — eine Deckelung lockert nie.
    ///
    /// # Arguments
    /// - `cap` (`ApprovalMode`): die höchste zulässige Stufe.
    ///
    /// # Rückgabe
    /// Den strengeren der beiden Modi.
    #[must_use]
    pub fn capped_at(self, cap: ApprovalMode) -> ApprovalMode {
        if self.rank() > cap.rank() { cap } else { self }
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

/// Geteilte, klonbare Zelle für einen [`ApprovalMode`].
///
/// # Verantwortlichkeit
/// `ApprovalModeCell` trägt ihren Zustand selbst: mehrere Klone teilen sich
/// denselben Modus (nützlich, wenn z. B. eine Sitzung und ihre
/// Kind-Sitzungen denselben Modus sehen sollen), während [`Self::detached`]
/// eine unabhängige Kopie erzeugt, die ab diesem Zeitpunkt keinen Zustand
/// mehr teilt.
///
/// [`Self::follower`] erzeugt eine Folgezelle, die den Modus ihrer
/// Elternzelle live übernimmt, gedeckelt auf eine Obergrenze — so folgen
/// laufende Kind-Sitzungen einer Umstellung der Wurzel.
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
pub struct ApprovalModeCell(Storage);

/// Innerer Speicher einer [`ApprovalModeCell`].
///
/// # Beschreibung
/// - `Own`: die Zelle trägt ihren Modus selbst.
/// - `Follow`: die Zelle liest den Modus ihrer Elternzelle, gedeckelt auf
///   `cap`, bis ein lokales [`ApprovalModeCell::set`] sie abkoppelt.
#[derive(Clone)]
enum Storage {
    Own(Arc<RwLock<ApprovalMode>>),
    Follow {
        parent: Arc<Storage>,
        cap: ApprovalMode,
        local: Arc<RwLock<Option<ApprovalMode>>>,
    },
}

impl Storage {
    fn get(&self) -> ApprovalMode {
        match self {
            Self::Own(lock) => match lock.read() {
                Ok(guard) => *guard,
                Err(poisoned) => *poisoned.into_inner(),
            },
            Self::Follow { parent, cap, local } => {
                let local_mode = match local.read() {
                    Ok(guard) => *guard,
                    Err(poisoned) => *poisoned.into_inner(),
                };
                local_mode.unwrap_or_else(|| parent.get()).capped_at(*cap)
            }
        }
    }

    fn set(&self, mode: ApprovalMode) {
        match self {
            Self::Own(lock) => match lock.write() {
                Ok(mut guard) => *guard = mode,
                Err(poisoned) => *poisoned.into_inner() = mode,
            },
            Self::Follow { local, .. } => match local.write() {
                Ok(mut guard) => *guard = Some(mode),
                Err(poisoned) => *poisoned.into_inner() = Some(mode),
            },
        }
    }
}

impl ApprovalModeCell {
    /// Erzeugt eine neue Zelle mit `mode` als Startwert.
    ///
    /// # Arguments
    /// - `mode` (`ApprovalMode`): der anfängliche Modus dieser Zelle.
    #[must_use]
    pub fn new(mode: ApprovalMode) -> Self {
        Self(Storage::Own(Arc::new(RwLock::new(mode))))
    }

    /// Liest den aktuellen Modus dieser Zelle.
    ///
    /// # Rückgabe
    /// Den zuletzt über [`Self::set`] (auf dieser Zelle oder einem geteilten
    /// Klon davon) geschriebenen Modus. Eine Folgezelle ([`Self::follower`])
    /// liefert ihren lokalen Wert, sonst den der Elternzelle — beides
    /// gedeckelt auf ihre Obergrenze.
    ///
    /// # Nebenläufigkeit
    /// Blockiert nie dauerhaft: ein vergifteter Lock wird über `into_inner`
    /// aufgelöst statt weiterzureichen (siehe Typ-Doku).
    #[must_use]
    pub fn get(&self) -> ApprovalMode {
        self.0.get()
    }

    /// Setzt den Modus dieser Zelle. Jeder geteilte Klon sieht die Änderung
    /// beim nächsten [`Self::get`].
    ///
    /// # Beschreibung
    /// Auf einer Folgezelle ([`Self::follower`]) setzt der Aufruf den lokalen
    /// Wert: diese Zelle und ihre Klone folgen der Elternzelle danach nicht
    /// mehr, bleiben aber gedeckelt. Die Elternzelle bleibt unberührt.
    ///
    /// # Arguments
    /// - `mode` (`ApprovalMode`): der neue Modus.
    ///
    /// # Nebenläufigkeit
    /// Blockiert nie dauerhaft: ein vergifteter Lock wird über `into_inner`
    /// aufgelöst statt weiterzureichen (siehe Typ-Doku).
    pub fn set(&self, mode: ApprovalMode) {
        self.0.set(mode);
    }

    /// Erzeugt eine unabhängige Kopie: eine neue Zelle mit demselben
    /// aktuellen Wert, aber ohne geteilten Zustand mit `self`. Spätere
    /// [`Self::set`]-Aufrufe auf der einen Zelle sind in der anderen nicht
    /// sichtbar.
    ///
    /// # Rückgabe
    /// Eine neue, eigenständige `ApprovalModeCell` (Momentaufnahme; auch
    /// von einer Folgezelle aus folgt die Kopie niemandem).
    #[must_use]
    pub fn detached(&self) -> Self {
        Self::new(self.get())
    }

    /// Erzeugt eine Folgezelle, die den Modus von `self` live übernimmt,
    /// gedeckelt auf `cap`.
    ///
    /// # Beschreibung
    /// Solange niemand auf der Folgezelle [`Self::set`] aufruft, liefert
    /// ihr [`Self::get`] den aktuellen Modus von `self`, höchstens aber
    /// `cap`. Ein `set` auf der Folgezelle koppelt sie (und ihre Klone) ab;
    /// die Elternzelle und andere Folgezellen bleiben unberührt.
    ///
    /// # Arguments
    /// - `cap` (`ApprovalMode`): die höchste Stufe, die die Folgezelle je
    ///   liefert.
    ///
    /// # Rückgabe
    /// Eine neue Folgezelle.
    #[must_use]
    pub fn follower(&self, cap: ApprovalMode) -> Self {
        Self(Storage::Follow {
            parent: Arc::new(self.0.clone()),
            cap,
            local: Arc::new(RwLock::new(None)),
        })
    }

    /// Ob diese Zelle eine Folgezelle ist ([`Self::follower`]).
    ///
    /// # Rückgabe
    /// `true` für Folgezellen, auch nach einem lokalen [`Self::set`].
    #[must_use]
    pub fn is_follower(&self) -> bool {
        matches!(self.0, Storage::Follow { .. })
    }
}

impl std::fmt::Debug for ApprovalModeCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalModeCell")
            .field("mode", &self.get())
            .field("follower", &self.is_follower())
            .finish()
    }
}

impl Default for ApprovalModeCell {
    /// Startet mit [`ApprovalMode::default`] (`Delegated`).
    fn default() -> Self {
        Self::new(ApprovalMode::default())
    }
}

#[cfg(test)]
mod tests {
    use super::{ApprovalMode, ApprovalModeCell};

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

    #[test]
    fn test_cell_set_then_get_returns_the_set_mode() {
        let cell = ApprovalModeCell::default();

        cell.set(ApprovalMode::AlwaysAsk);
        assert_eq!(cell.get(), ApprovalMode::AlwaysAsk);

        cell.set(ApprovalMode::FullAccess);
        assert_eq!(cell.get(), ApprovalMode::FullAccess);

        cell.set(ApprovalMode::Delegated);
        assert_eq!(cell.get(), ApprovalMode::Delegated);
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

    #[test]
    fn rank_orders_by_authority() {
        assert!(ApprovalMode::AlwaysAsk.rank() < ApprovalMode::Delegated.rank());
        assert!(ApprovalMode::Delegated.rank() < ApprovalMode::FullAccess.rank());
    }

    #[test]
    fn capped_at_lowers_but_never_loosens() {
        assert_eq!(
            ApprovalMode::FullAccess.capped_at(ApprovalMode::Delegated),
            ApprovalMode::Delegated
        );
        assert_eq!(
            ApprovalMode::AlwaysAsk.capped_at(ApprovalMode::Delegated),
            ApprovalMode::AlwaysAsk
        );
        assert_eq!(
            ApprovalMode::Delegated.capped_at(ApprovalMode::FullAccess),
            ApprovalMode::Delegated
        );
    }

    #[test]
    fn follower_sees_parent_changes() {
        let parent = ApprovalModeCell::new(ApprovalMode::Delegated);
        let follower = parent.follower(ApprovalMode::FullAccess);
        assert!(follower.is_follower());
        assert!(!parent.is_follower());
        assert_eq!(follower.get(), ApprovalMode::Delegated);

        parent.set(ApprovalMode::AlwaysAsk);
        assert_eq!(follower.get(), ApprovalMode::AlwaysAsk);
    }

    #[test]
    fn follower_enforces_its_cap() {
        let parent = ApprovalModeCell::new(ApprovalMode::FullAccess);
        let follower = parent.follower(ApprovalMode::Delegated);
        assert_eq!(follower.get(), ApprovalMode::Delegated);

        follower.set(ApprovalMode::FullAccess);
        assert_eq!(follower.get(), ApprovalMode::Delegated);
        assert_eq!(parent.get(), ApprovalMode::FullAccess);
    }

    #[test]
    fn follower_local_set_decouples_it_and_its_clones() {
        let parent = ApprovalModeCell::new(ApprovalMode::Delegated);
        let follower = parent.follower(ApprovalMode::Delegated);
        let clone = follower.clone();

        follower.set(ApprovalMode::AlwaysAsk);
        parent.set(ApprovalMode::Delegated);

        assert_eq!(follower.get(), ApprovalMode::AlwaysAsk);
        assert_eq!(clone.get(), ApprovalMode::AlwaysAsk);
        assert_eq!(parent.get(), ApprovalMode::Delegated);
    }

    #[test]
    fn sibling_followers_are_independent() {
        let parent = ApprovalModeCell::new(ApprovalMode::Delegated);
        let a = parent.follower(ApprovalMode::Delegated);
        let b = parent.follower(ApprovalMode::Delegated);

        a.set(ApprovalMode::AlwaysAsk);

        assert_eq!(a.get(), ApprovalMode::AlwaysAsk);
        assert_eq!(b.get(), ApprovalMode::Delegated);
    }

    #[test]
    fn nested_follower_sees_the_root_through_its_parent() {
        let root = ApprovalModeCell::new(ApprovalMode::Delegated);
        let child = root.follower(ApprovalMode::Delegated);
        let grandchild = child.follower(ApprovalMode::FullAccess);

        root.set(ApprovalMode::AlwaysAsk);
        assert_eq!(grandchild.get(), ApprovalMode::AlwaysAsk);

        root.set(ApprovalMode::FullAccess);
        assert_eq!(grandchild.get(), ApprovalMode::Delegated);
    }

    #[test]
    fn detached_from_a_follower_is_a_plain_snapshot() {
        let parent = ApprovalModeCell::new(ApprovalMode::AlwaysAsk);
        let snapshot = parent.follower(ApprovalMode::Delegated).detached();
        assert!(!snapshot.is_follower());

        parent.set(ApprovalMode::Delegated);
        assert_eq!(snapshot.get(), ApprovalMode::AlwaysAsk);
    }
}
