//! Fehler dieser Crate: inhaltsfrei, damit ein geloggter Fehler nie einen
//! Benutzernamen, Hostnamen oder Terminalnamen aus einem Anmelde-Record
//! preisgibt.
//!
//! # Verantwortungsbereich
//! [`AuthlogError`] ist der eine Fehlertyp von `harw-dod-authlog`. Er reicht
//! ausschließlich `harw_dod_cap::SensorError` durch — das gemeinsame,
//! bereits inhaltsfreie Zugriffsvokabular aller Sensor-Crates (Contract-
//! Master Abschnitt F). Ein Fehler aus dem Audit-Backend
//! (`harw_dod_netlink::NetlinkError`) wird an der Backend-Grenze
//! (`crate::audit_backend`) auf genau diese eine Variante abgebildet, statt
//! eine zweite, redundante Fehlermenge einzuführen.
//!
//! `Display`, `Debug`, `std::error::Error` und die `From`-Konvertierung für
//! die `#[from]`-Variante entstehen über `#[derive(harw_macros::HarwError)]`
//! (Muster: `harw-dod-cap/src/error.rs`, `harw-dod-netlink/src/error.rs`).
//! Kein `anyhow`, kein `thiserror`.
//!
//! # Exportierte Typen
//! [`AuthlogError`] sowie der vom Makro erzeugte Alias `AuthlogResult<T>`.
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_authlog::error::AuthlogError;
//! use harw_dod_cap::SensorError;
//!
//! let err: AuthlogError = SensorError::MalformedSource.into();
//! assert_eq!(err.to_string(), "sensor source has an unexpected shape");
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate.
///
/// # Description
/// Eine einzige Variante, die das gemeinsame Zugriffsvokabular unverändert
/// durchreicht. Kein eigenes `#[msg]`: die `Display`-Ausgabe delegiert
/// unverändert an `harw_dod_cap::SensorError`, das selbst bereits
/// inhaltsfrei ist (Muster: `harw_dod_netlink::NetlinkError::Sensor`).
#[derive(Debug, HarwError)]
pub enum AuthlogError {
    /// Ein Fehler aus dem gemeinsamen Zugriffsvokabular — insbesondere
    /// [`harw_dod_cap::SensorError::MalformedSource`] für einen Record, der
    /// nicht dem erwarteten Format entspricht oder ein Pflichtfeld
    /// (`uid`, Zeitstempel) vermissen lässt, und
    /// [`harw_dod_cap::SensorError::SourceUnavailable`], wenn eine Quelle auf
    /// diesem Host nicht verfügbar ist.
    ///
    /// # Arguments
    /// - `0` (`harw_dod_cap::SensorError`): der zugrunde liegende Fehler.
    #[from]
    Sensor(harw_dod_cap::SensorError),
}

#[cfg(test)]
mod tests {
    use super::{AuthlogError, AuthlogResult};
    use harw_dod_cap::SensorError;

    #[test]
    fn test_from_sensor_error_delegates_display() {
        let err: AuthlogError = SensorError::MalformedSource.into();
        assert_eq!(err.to_string(), "sensor source has an unexpected shape");
    }

    #[test]
    fn test_sensor_variant_links_source() {
        let err: AuthlogError = SensorError::SourceUnavailable.into();
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_authlog_result_alias_carries_authlog_error() {
        fn always_fails() -> AuthlogResult<()> {
            Err(AuthlogError::from(SensorError::OutsideScope))
        }
        assert!(always_fails().is_err());
    }

    #[test]
    fn test_display_never_contains_sensitive_looking_literal() {
        // Inhaltsfrei bedeutet strukturell: es gibt keinen Konstruktionsweg,
        // der Recordinhalt in die Meldung einbettet. Diese Prüfung dient als
        // Regressionsschutz, sollte künftig ein Feld mit Inhalt hinzukommen.
        let err: AuthlogError = SensorError::MalformedSource.into();
        let message = err.to_string();
        assert!(!message.contains("alice"));
        assert!(!message.contains("pts/"));
    }
}
