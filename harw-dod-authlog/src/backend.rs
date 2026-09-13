//! `trait AuthBackend`: die eine Schnittstelle für beide Anmelde-Quellen.
//!
//! # Verantwortungsbereich
//! Dieser Trait ist die dokumentierte Ausnahme von „genau eine Quelle je
//! Sensor-Crate" (siehe Crate-Dokumentation für die volle Begründung): das
//! Journal- und das Audit-Backend beantworten dieselbe Frage — wer hat sich
//! wann erfolgreich oder erfolglos angemeldet? — aus zwei verschiedenen
//! Quellen, hinter demselben Trait. Jede Implementierung hält für sich
//! trotzdem **genau eine** `harw_dod_cap::Capability`; die Rechtematrix
//! bleibt damit exakt.
//!
//! # Exportierte Typen
//! [`AuthBackend`].
//!
//! # Nebenläufigkeit
//! `Send + Sync`: ein Sentinel darf ein Backend aus einem beliebigen Thread
//! pollen.
//!
//! # Fehler
//! [`crate::error::AuthlogError`], inhaltsfrei.
//!
//! # Examples
//! ```rust
//! use harw_dod_authlog::{AuthBackend, AuthRecord, FixtureAuthBackend};
//! use harw_dod_cap::Capability;
//!
//! let backend: Box<dyn AuthBackend> = Box::new(FixtureAuthBackend::new(
//!     Vec::<AuthRecord>::new(),
//!     Capability::ReadAuditNetlink,
//! ));
//! assert_eq!(backend.capability(), Capability::ReadAuditNetlink);
//! ```

use jiff::Timestamp;

use harw_dod_cap::Capability;

use crate::error::AuthlogError;
use crate::record::AuthRecord;

/// Die eine Schnittstelle für beide Anmelde-Quellen.
///
/// # Description
/// Siehe Moduldokumentation für die Begründung, warum genau zwei
/// Implementierungen hinter diesem einen Trait zulässig sind. Ein Aufrufer
/// programmiert ausschließlich gegen diesen Trait und bindet sich nie an
/// eine konkrete Implementierung.
pub trait AuthBackend: Send + Sync {
    /// Liest Anmeldeereignisse seit `since`.
    ///
    /// # Arguments
    /// - `since` (`jiff::Timestamp`): injizierte untere Zeitgrenze. Nur
    ///   Ereignisse mit `observed_at >= since` erscheinen im Ergebnis.
    ///
    /// # Returns
    /// Die seit `since` beobachteten [`AuthRecord`]e, in der Reihenfolge, in
    /// der die zugrunde liegende Quelle sie liefert. Kann leer sein.
    ///
    /// # Errors
    /// [`AuthlogError`], wenn die Quelle nicht lesbar ist, außerhalb des
    /// Lesebereichs liegt oder eine unerwartete Form hat. Inhaltsfrei — siehe
    /// Crate-Dokumentation, Abschnitt „Der Inhalt ist heikel".
    fn read_events(&self, since: Timestamp) -> Result<Vec<AuthRecord>, AuthlogError>;

    /// Die Fähigkeit, die dieses Backend benutzt.
    ///
    /// # Returns
    /// Genau eine `harw_dod_cap::Capability` — nie mehr als eine pro
    /// Implementierung (siehe Moduldokumentation).
    fn capability(&self) -> Capability;
}

#[cfg(test)]
mod tests {
    use super::AuthBackend;
    use crate::fixture_backend::FixtureAuthBackend;
    use crate::record::AuthRecord;
    use harw_dod_cap::Capability;

    #[test]
    fn test_auth_backend_is_object_safe_as_boxed_trait_object() {
        let backend: Box<dyn AuthBackend> = Box::new(FixtureAuthBackend::new(
            Vec::<AuthRecord>::new(),
            Capability::ReadJournal,
        ));
        assert_eq!(backend.capability(), Capability::ReadJournal);
    }

    #[test]
    fn test_boxed_backends_can_be_held_in_a_heterogeneous_collection() {
        let backends: Vec<Box<dyn AuthBackend>> = vec![
            Box::new(FixtureAuthBackend::new(
                Vec::<AuthRecord>::new(),
                Capability::ReadJournal,
            )),
            Box::new(FixtureAuthBackend::new(
                Vec::<AuthRecord>::new(),
                Capability::ReadAuditNetlink,
            )),
        ];
        assert_eq!(backends.len(), 2);
    }
}
