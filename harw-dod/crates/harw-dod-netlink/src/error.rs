//! Fehler dieser Crate: inhaltsfrei, damit ein geloggter Fehler nie eine
//! Kommandozeile oder einen Dateipfad aus einem Audit-Record preisgibt.
//!
//! # Verantwortungsbereich
//! [`NetlinkError`] ist der eine Fehlertyp von `harw-dod-netlink`. Er trägt
//! vier Fälle: [`NetlinkError::MalformedRecord`] für einen Audit-Record, der
//! nicht dem erwarteten `key=value`-Format entspricht oder ein Pflichtfeld
//! vermissen lässt, [`NetlinkError::Overrun`], wenn der Kernel Records
//! verworfen hat, weil der Empfangspuffer des Sockets voll war,
//! [`NetlinkError::Truncated`], wenn ein Datagramm größer als der
//! Lesepuffer war und sein Record deshalb verloren ist, und
//! [`NetlinkError::Sensor`] für alles, was aus dem
//! gemeinsamen Zugriffsvokabular [`harw_dod_cap::SensorError`] kommt —
//! insbesondere [`harw_dod_cap::SensorError::SourceUnavailable`], wenn der
//! `AUDIT`-Netlink-Socket auf diesem Host nicht geöffnet oder gebunden werden
//! kann (fehlende Berechtigung, Kernel ohne Audit-Unterstützung, nicht-Linux-
//! Plattform).
//!
//! **Inhaltsfrei:** [`NetlinkError::MalformedRecord`] trägt absichtlich kein
//! Feld für den Recordinhalt. Audit-Records enthalten Kommandozeilen,
//! Dateipfade und Umgebungsvariablen; eine Fehlermeldung, die den Rohtext
//! eines fehlerhaften Records zitiert, würde genau diese Daten ins Log und
//! damit vom Host weg tragen — der Fehlerfall selbst ist meldenswert, sein
//! Inhalt nicht.
//!
//! `Display`, `Debug`, `std::error::Error` und die `From`-Konvertierung für
//! die `#[from]`-Variante entstehen über `#[derive(harw_macros::HarwError)]`
//! (Muster: `harw-plan/src/error.rs`, `harw-dod-cap/src/error.rs`). Kein
//! `anyhow`, kein `thiserror`.
//!
//! # Exportierte Typen
//! [`NetlinkError`] sowie der vom Makro erzeugte Alias `NetlinkResult<T>`.
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_netlink::error::NetlinkError;
//! use harw_dod_cap::SensorError;
//!
//! let err: NetlinkError = SensorError::SourceUnavailable.into();
//! assert_eq!(err.to_string(), "sensor source is unavailable on this host");
//!
//! let malformed = NetlinkError::MalformedRecord;
//! assert_eq!(malformed.to_string(), "audit record is malformed");
//! ```

use harw_dod_cap::SensorError;
use harw_macros::HarwError;

/// Fehler dieser Crate.
///
/// # Description
/// Vier Varianten: ein lokal erkannter Formfehler eines Audit-Records
/// ([`Self::MalformedRecord`]), ein vom Kernel gemeldeter Verlust von
/// Records ([`Self::Overrun`]), ein beim Lesen abgeschnittenes Datagramm
/// ([`Self::Truncated`]) und alles, was aus dem gemeinsamen
/// Zugriffsvokabular [`harw_dod_cap::SensorError`] stammt ([`Self::Sensor`]).
/// Alle sind inhaltsfrei — siehe Moduldokumentation.
#[derive(Debug, HarwError)]
pub enum NetlinkError {
    /// Ein Audit-Record entspricht nicht dem erwarteten `key=value`-Format,
    /// ein numerisches Feld enthält keine gültige Zahl, oder ein
    /// Pflichtfeld (`type`) fehlt.
    ///
    /// Nennt **nie** den Recordinhalt — siehe Moduldokumentation.
    #[msg("audit record is malformed")]
    MalformedRecord,

    /// Der Kernel hat Audit-Records verworfen, weil der Empfangspuffer des
    /// Netlink-Sockets voll war (`ENOBUFS`).
    ///
    /// **Vorübergehend:** der Socket bleibt benutzbar, der nächste Lesevorgang
    /// liefert wieder Records. Der Verlust selbst ist aber nicht
    /// rückgängig zu machen; ein Aufrufer meldet ihn (etwa als
    /// `SensorDegraded`), statt ihn still zu übergehen.
    #[msg("audit netlink socket overran; records were lost")]
    Overrun,

    /// Ein empfangenes Datagramm war größer als der Lesepuffer; der Kernel
    /// hat den Rest beim `recv()` verworfen (erkannt über `MSG_TRUNC`).
    ///
    /// **Vorübergehend** wie [`Self::Overrun`]: der Socket bleibt benutzbar,
    /// aber der Record in diesem Datagramm ist verloren. Ein Aufrufer zählt
    /// und meldet ihn wie einen Überlauf, statt ihn still zu übergehen.
    /// Trägt weder Länge noch Inhalt.
    #[msg("audit netlink datagram exceeded the receive buffer; a record was lost")]
    Truncated,

    /// Ein Fehler aus dem gemeinsamen Zugriffsvokabular.
    ///
    /// Kein eigenes `#[msg]`: die `Display`-Ausgabe delegiert unverändert an
    /// [`harw_dod_cap::SensorError`], das selbst bereits inhaltsfrei ist.
    ///
    /// # Arguments
    /// - `0` (`harw_dod_cap::SensorError`): der zugrunde liegende Fehler,
    ///   z. B. [`harw_dod_cap::SensorError::SourceUnavailable`], wenn der
    ///   `AUDIT`-Netlink-Socket auf diesem Host nicht geöffnet werden kann.
    #[from]
    Sensor(SensorError),
}

#[cfg(test)]
mod tests {
    use super::NetlinkError;
    use harw_dod_cap::SensorError;

    #[test]
    fn test_malformed_record_display_is_exact_and_content_free() {
        assert_eq!(
            NetlinkError::MalformedRecord.to_string(),
            "audit record is malformed"
        );
    }

    #[test]
    fn test_from_sensor_error_delegates_display() {
        let err: NetlinkError = SensorError::SourceUnavailable.into();
        assert_eq!(err.to_string(), "sensor source is unavailable on this host");
    }

    #[test]
    fn test_sensor_variant_links_source() {
        let err: NetlinkError = SensorError::OutsideScope.into();
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_overrun_display_is_exact_and_content_free() {
        let err = NetlinkError::Overrun;
        assert_eq!(
            err.to_string(),
            "audit netlink socket overran; records were lost"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn test_truncated_display_is_exact_and_content_free() {
        let err = NetlinkError::Truncated;
        assert_eq!(
            err.to_string(),
            "audit netlink datagram exceeded the receive buffer; a record was lost"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn test_malformed_record_has_no_source() {
        let err = NetlinkError::MalformedRecord;
        assert!(std::error::Error::source(&err).is_none());
    }
}
