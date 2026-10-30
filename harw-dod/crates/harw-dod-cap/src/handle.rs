//! Sensor-Griff mit Typestate: ein `Unbound`-Griff kann nicht lesen.
//!
//! # Verantwortungsbereich
//! [`SensorHandle`] verbindet eine `harw_types::SensorId`, eine
//! [`crate::capability::Capability`] und — sobald gebunden — einen
//! [`crate::scope::ReadScope`]. [`Unbound`] und [`Bound`] sind
//! Zero-Sized-Marker (`PhantomData`), die den Zustand zur Compile-Zeit
//! tragen: `scope()`, `id()` und `capability()` existieren nur auf
//! `SensorHandle<Bound>`. Ein ungebundener Griff kann nicht lesen — nicht als
//! Laufzeitfehler, sondern als fehlender Methodenname (Vorbild:
//! `harw-provider/src/marker.rs` und `harw-provider/src/provider.rs`).
//!
//! # Exportierte Typen
//! [`SensorHandle`], [`Unbound`], [`Bound`].
//!
//! # Nebenläufigkeit
//! `harw_types::SensorId` und [`crate::capability::Capability`] sind reine
//! Werttypen; [`crate::scope::ReadScope`] ebenso. `SensorHandle<S>` hält
//! keine innere Veränderlichkeit und ist `Send + Sync`, sofern `S` es ist —
//! beide Marker sind zustandslose `Copy`-Typen und damit `Send + Sync`.
//!
//! # Fehler
//! Keine — beide Methoden `new` und `bind` sind total (kein `Result`); die
//! Typestate-Regel selbst verhindert Fehlbedienung bereits beim Kompilieren.
//!
//! # Examples
//! ```rust
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/sys/class/thermal")]);
//! let handle = SensorHandle::new(SensorId::from_str("thermal-0"), Capability::ReadSysfsThermal)
//!     .bind(scope.clone());
//!
//! assert_eq!(handle.scope(), &scope);
//! assert_eq!(handle.capability(), Capability::ReadSysfsThermal);
//! ```

use std::marker::PhantomData;

use harw_types::SensorId;

use crate::capability::Capability;
use crate::scope::ReadScope;

/// Marker: der Sensor kennt seinen Bereich noch nicht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unbound;

/// Marker: der Sensor ist an einen Bereich gebunden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound;

// Versiegeltes Supertrait: nur Typen aus diesem Modul dürfen `HandleState`
// implementieren. Verhindert, dass eine fremde Crate einen dritten Zustand
// erfindet, für den `SensorHandle<S>` nicht vorgesehen ist.
mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Unbound {}
    impl Sealed for super::Bound {}
}

/// Internes Bindeglied zwischen Zustands-Marker und dem Datenfeld, das genau
/// dieser Zustand trägt.
///
/// # Description
/// `Unbound` trägt keinen Bereich (`Scope = ()`), `Bound` genau einen
/// (`Scope = ReadScope`). Das hält [`SensorHandle::scope`] frei von
/// `Option`/`unwrap`: die Abwesenheit eines Bereichs im Zustand `Unbound` ist
/// eine Typ-Eigenschaft, die der Compiler durchsetzt, kein
/// Laufzeit-Sonderfall, den eine Methode erst prüfen müsste. Nicht Teil der
/// in Vertrag Abschnitt F vereinbarten `pub`-Fläche dieser Crate — deshalb
/// `#[doc(hidden)]` und nicht am Crate-Wurzel reexportiert; öffentlich nur,
/// weil ein privates Trait als Bound auf einem `pub`-Typ sonst einen
/// Sichtbarkeits-Lint auslöst (`private_bounds`).
#[doc(hidden)]
pub trait HandleState: sealed::Sealed {
    /// Der pro Zustand vorhandene Datentyp.
    type Scope: std::fmt::Debug;
}

impl HandleState for Unbound {
    type Scope = ();
}

impl HandleState for Bound {
    type Scope = ReadScope;
}

/// Ein Sensor-Griff im Zustand `S`.
///
/// # Description
/// `id()`, `capability()` und `scope()` gibt es **nur** auf
/// `SensorHandle<Bound>`. Ein `SensorHandle<Unbound>` besitzt schlicht keine
/// dieser Methoden — der Compiler lehnt den Aufruf ab, bevor das Programm
/// läuft.
pub struct SensorHandle<S: HandleState> {
    id: SensorId,
    capability: Capability,
    scope: S::Scope,
    _state: PhantomData<S>,
}

// Manuell statt `#[derive(Debug)]`: eine generisch gebundene Struktur
// (`struct SensorHandle<S: HandleState>`) bekäme von `derive(Debug)` nur die
// Bound `S: Debug`, nicht die hier tatsächlich nötige `S::Scope: Debug` — der
// generierte Impl würde also nicht kompilieren. Die manuelle Fassung trägt
// die richtige Bound direkt.
impl<S: HandleState> std::fmt::Debug for SensorHandle<S>
where
    S::Scope: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SensorHandle")
            .field("id", &self.id)
            .field("capability", &self.capability)
            .field("scope", &self.scope)
            .finish()
    }
}

impl SensorHandle<Unbound> {
    /// Erzeugt einen ungebundenen Griff.
    ///
    /// # Arguments
    /// - `id` (`harw_types::SensorId`): Kennung dieses Sensors.
    /// - `capability` (`Capability`): die Fähigkeit, die dieser Sensor nutzt.
    ///
    /// # Returns
    /// Einen `SensorHandle<Unbound>` ohne Lesebereich. Erst [`Self::bind`]
    /// macht daraus einen lesefähigen Griff.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::{Capability, SensorHandle};
    /// use harw_types::SensorId;
    ///
    /// let _handle = SensorHandle::new(SensorId::from_str("thermal-0"), Capability::ReadSysfsThermal);
    /// ```
    #[must_use]
    pub fn new(id: SensorId, capability: Capability) -> Self {
        Self {
            id,
            capability,
            scope: (),
            _state: PhantomData,
        }
    }

    /// Bindet den Griff an einen Lesebereich.
    ///
    /// # Description
    /// Verbraucht `self` (Typestate-Übergang): ein `SensorHandle<Unbound>`
    /// kann nach `bind` nicht mehr als ungebundener Griff verwendet werden,
    /// weil er nicht mehr existiert.
    ///
    /// # Arguments
    /// - `scope` (`ReadScope`): der Bereich, an den der Griff gebunden wird.
    ///
    /// # Returns
    /// Einen `SensorHandle<Bound>`, dessen [`SensorHandle::scope`] exakt
    /// `scope` zurückgibt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/sys/class/thermal")]);
    /// let bound = SensorHandle::new(SensorId::from_str("thermal-0"), Capability::ReadSysfsThermal)
    ///     .bind(scope.clone());
    /// assert_eq!(bound.scope(), &scope);
    /// ```
    #[must_use]
    pub fn bind(self, scope: ReadScope) -> SensorHandle<Bound> {
        SensorHandle {
            id: self.id,
            capability: self.capability,
            scope,
            _state: PhantomData,
        }
    }
}

impl SensorHandle<Bound> {
    /// Der Lesebereich dieses Griffs.
    ///
    /// # Returns
    /// Der bei [`SensorHandle::bind`] übergebene [`ReadScope`].
    #[must_use]
    pub fn scope(&self) -> &ReadScope {
        &self.scope
    }

    /// Die Kennung dieses Sensors.
    ///
    /// # Returns
    /// Die `harw_types::SensorId`, mit der dieser Griff erzeugt wurde.
    #[must_use]
    pub fn id(&self) -> &SensorId {
        &self.id
    }

    /// Die Fähigkeit dieses Sensors.
    ///
    /// # Returns
    /// Die [`Capability`], mit der dieser Griff erzeugt wurde.
    #[must_use]
    pub fn capability(&self) -> Capability {
        self.capability
    }
}

#[cfg(test)]
mod tests {
    use super::{Bound, SensorHandle};
    use crate::capability::Capability;
    use crate::scope::ReadScope;
    use harw_types::SensorId;
    use std::path::PathBuf;

    #[test]
    fn test_bind_returns_handle_whose_scope_matches_bound_scope() {
        let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
        let handle: SensorHandle<Bound> =
            SensorHandle::new(SensorId::from_str("proc-stat-0"), Capability::ReadProcStat)
                .bind(scope.clone());

        assert_eq!(handle.scope(), &scope);
    }

    #[test]
    fn test_bind_preserves_id_and_capability() {
        let id = SensorId::from_str("thermal-0");
        let handle = SensorHandle::new(id.clone(), Capability::ReadSysfsThermal)
            .bind(ReadScope::from_roots([PathBuf::from("/sys/class/thermal")]));

        assert_eq!(handle.id(), &id);
        assert_eq!(handle.capability(), Capability::ReadSysfsThermal);
    }

    #[test]
    fn test_debug_format_does_not_panic_for_bound_handle() {
        let handle = SensorHandle::new(SensorId::from_str("cgroup-0"), Capability::ReadCgroupV2)
            .bind(ReadScope::from_roots([PathBuf::from("/sys/fs/cgroup")]));

        let debug = format!("{handle:?}");
        assert!(debug.contains("SensorHandle"));
    }
}
