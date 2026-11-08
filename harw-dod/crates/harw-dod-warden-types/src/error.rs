//! Fehlertyp von `harw-dod-warden-types` (Contract-Master §H.1: ein
//! Fehlertyp je Crate).
//!
//! # Verantwortungsbereich
//! [`WardenTypesError`] deckt den einzigen Fehlerpfad dieser Crate ab: eine
//! cgroup-Kernel-ID `0` ([`crate::target::CgroupKernelId::new`], ebenso beim
//! Dekodieren über `TryFrom<u64>`). Alle anderen Typen dieser Crate sind
//! unfehlbar konstruierbar.
//!
//! # Abbildung im Signierteil
//! Vor PL-100 WP 3.0 lieferte `CgroupKernelId::new` direkt
//! `harw_dod_warden_proto::ProofError::InvalidKernelId`. Diese Crate kennt den
//! Signierteil nicht; `harw-dod-warden-proto` bildet
//! [`WardenTypesError::InvalidKernelId`] deshalb per `From` auf
//! `ProofError::InvalidKernelId` ab (dort weiterhin `Denial::Malformed`).
//! Aufrufer, die mit `?` in einen `ProofError` umwandeln, bleiben unverändert.
//!
//! # Inhaltsfreie Meldung
//! Die `Display`-Meldung ist fest und interpoliert keinen Wert.
//!
//! # Nebenläufigkeit
//! `Copy`, keine geteilten Ressourcen.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden_types::{CgroupKernelId, WardenTypesError};
//!
//! assert!(matches!(CgroupKernelId::new(0), Err(WardenTypesError::InvalidKernelId)));
//! assert_eq!(
//!     WardenTypesError::InvalidKernelId.to_string(),
//!     "cgroup kernel id must not be zero"
//! );
//! ```

use std::fmt;

/// Fehler dieser Crate.
///
/// # Description
/// Feste, inhaltsfreie Meldungen; nie serialisiert, verlässt den Prozess
/// nie. Über die Leitung geht höchstens die daraus abgeleitete
/// `Denial`-Kategorie des Signierteils.
///
/// # Errors
/// Erzeugt von [`crate::target::CgroupKernelId::new`] und
/// `CgroupKernelId::try_from`.
#[derive(Clone, Copy, PartialEq, Eq, harw_macros::HarwError)]
pub enum WardenTypesError {
    /// Eine cgroup-Kernel-ID ist `0` (siehe [`crate::target::CgroupKernelId`]).
    #[msg("cgroup kernel id must not be zero")]
    InvalidKernelId,
}

/// Formatiert `WardenTypesError` über seine [`std::fmt::Display`]-Meldung.
///
/// # Description
/// `#[derive(harw_macros::HarwError)]` erzeugt kein `Debug`; diese Impl
/// delegiert an `Display`, statt eine zweite Formatierung zu pflegen.
impl fmt::Debug for WardenTypesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{WardenTypesError, WardenTypesResult};

    #[test]
    fn test_invalid_kernel_id_display_is_fixed_and_content_free() {
        let err = WardenTypesError::InvalidKernelId;
        assert_eq!(err.to_string(), "cgroup kernel id must not be zero");
        assert!(!err.to_string().chars().any(|c| c.is_ascii_digit()));
        assert!(err.source().is_none());
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let err = WardenTypesError::InvalidKernelId;
        assert_eq!(format!("{err:?}"), err.to_string());
    }

    #[test]
    fn test_result_alias_carries_warden_types_error() {
        fn always_fails() -> WardenTypesResult<()> {
            Err(WardenTypesError::InvalidKernelId)
        }

        assert_eq!(always_fails(), Err(WardenTypesError::InvalidKernelId));
    }
}
