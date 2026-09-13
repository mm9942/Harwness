//! Fehlertyp von `harw-dod-warden` (Contract-Master §H.1: ein Fehlertyp je
//! Crate).
//!
//! # Verantwortungsbereich
//! [`WardenError`] deckt genau die Fehlerpfade ab, die **nach** einer
//! erfolgreichen Beleg-Nachprüfung entstehen können — also innerhalb der
//! Ausführungs-Traits (`executor.rs`), wenn eine an sich autorisierte und
//! zulässige Aktion an der eigentlichen Systemoperation scheitert (z. B. ein
//! Dateisystemzugriff auf `/sys/fs/cgroup/...`). Die Nachprüfung selbst
//! (Bindung, Zulässigkeit) liefert ihre eigenen Fehler bereits fertig als
//! [`harw_dod_warden_proto::WardenProtoError`] bzw. dessen inhaltsfreie
//! Ablehnungskategorie [`harw_dod_warden_proto::Denial`] — dieser Typ
//! dupliziert das nicht.
//!
//! # Dieser Typ verlässt den Prozess nie
//! Wie `harw-dod-warden-proto::WardenProtoError` implementiert auch dieser
//! Typ absichtlich **kein** `Serialize` — er beschreibt einen internen
//! Ausführungsfehler, nicht etwas, das über die Leitung ginge. Was der
//! Warden nach außen meldet, ist ausschließlich
//! [`crate::warden::WardenOutcome`] bzw. dessen `to_wire`-Abbildung auf
//! [`harw_dod_warden_proto::WardenResponse`] — beide bewusst ohne
//! Fehlerdetail (siehe `warden.rs`-Moduldoku).
//!
//! # Nebenläufigkeit
//! Trägt keinen inneren Zustand außer den Fehlerdaten selbst; kein Locking,
//! keine geteilten Ressourcen. `Send + Sync` (jedes Feld ist es).

use std::fmt;

/// Fehler dieser Crate.
///
/// # Description
/// Zwei Varianten: eine cgroup-Kennung, die sich nicht gefahrlos in einen
/// Dateisystempfad einbetten lässt ([`Self::InvalidCgroupId`]), und ein
/// fehlgeschlagener Dateisystemzugriff während einer Ausführungsoperation
/// ([`Self::Io`]). `#[derive(harw_macros::HarwError)]` erzeugt daraus
/// `Display`, `std::error::Error` (mit `source()` für [`Self::Io`]) und den
/// Typalias [`WardenResult`] — **kein** `Debug` (siehe `impl Debug`
/// unterhalb).
///
/// # Errors
/// Wird von den Ausführungs-Traits in `executor.rs` zurückgegeben.
#[derive(harw_macros::HarwError)]
pub enum WardenError {
    /// Eine cgroup-Kennung enthält Zeichen, die als Dateisystempfad-Segment
    /// gefährlich wären (`/`, `..`, ein eingebettetes NUL-Byte). `CgroupId`
    /// garantiert laut `harw-types` nur „nicht leer, nicht nur
    /// Leerzeichen" — keine Pfadsicherheit. Da der Warden der Gegenseite
    /// nichts glaubt (Crate-Moduldoku, `lib.rs`), prüft
    /// [`crate::executor::CgroupV2Executor`] das selbst, bevor die Kennung
    /// in einen Pfad eingebettet wird.
    #[msg("cgroup-Kennung enthält unzulässige Zeichen für einen Dateisystempfad")]
    InvalidCgroupId,

    /// Der Dateisystemzugriff für eine cgroup-Operation (Einfrieren,
    /// Freigeben, Beenden) ist fehlgeschlagen.
    #[msg("Dateisystemzugriff für die cgroup-Operation ist fehlgeschlagen: {0}")]
    #[from]
    Io(std::io::Error),
}

/// Formatiert `WardenError` über seine [`std::fmt::Display`]-Meldung.
///
/// # Description
/// `#[derive(harw_macros::HarwError)]` erzeugt kein `Debug` — diese Impl
/// schließt die Lücke, indem sie an `Display` delegiert, statt eine zweite,
/// potenziell auseinanderlaufende Formatierung zu pflegen (dieselbe
/// Konvention wie `harw_dod_warden_proto::WardenProtoError`).
impl fmt::Debug for WardenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::WardenError;

    fn sample_io_error() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied")
    }

    #[test]
    fn test_invalid_cgroup_id_display_is_fixed_and_content_free() {
        let err = WardenError::InvalidCgroupId;
        assert_eq!(
            err.to_string(),
            "cgroup-Kennung enthält unzulässige Zeichen für einen Dateisystempfad"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn test_io_display_interpolates_inner_message() {
        let err = WardenError::from(sample_io_error());
        assert!(err
            .to_string()
            .starts_with("Dateisystemzugriff für die cgroup-Operation ist fehlgeschlagen:"));
    }

    #[test]
    fn test_io_source_returns_inner_error() {
        let err: WardenError = sample_io_error().into();
        assert!(err.source().is_some());
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let err = WardenError::InvalidCgroupId;
        assert_eq!(format!("{err:?}"), err.to_string());
    }

    #[test]
    fn test_warden_result_alias_carries_warden_error() {
        fn always_fails() -> super::WardenResult<()> {
            Err(WardenError::InvalidCgroupId)
        }
        assert!(always_fails().is_err());
    }
}
