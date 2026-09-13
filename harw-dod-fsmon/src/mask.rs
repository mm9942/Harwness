//! Deutung roher fanotify-Ereignismasken: welche Art von Zugriff geschah.
//!
//! # Verantwortungsbereich
//! Reine Funktion auf einer `u64`-Bitmaske und einem bereits aufgelösten
//! Pfad: [`interpret_mask`] entscheidet, ob eine Maske einen Schreibzugriff
//! trägt, und formt daraus ein `harw_dod_signals::EventKind`. Kein
//! Betriebssystemaufruf, keine `fanotify`-Quelle — die Maskenwerte selbst
//! sind stabile, öffentlich dokumentierte Kernel-ABI-Konstanten (`man 7
//! fanotify`), keine zur Laufzeit ermittelten Werte.
//!
//! # Was diese Funktion NICHT deutet
//! `harw_dod_signals::EventKind` kennt heute nur `FileWrite` als
//! dateisystembezogene Variante. Masken, die ausschließlich Lese- oder
//! Metadaten-Zugriffe tragen (`FAN_ACCESS`, `FAN_OPEN`, `FAN_ATTRIB` u. Ä.),
//! sind deshalb bewusst nicht interpretierbar — eine neue `EventKind`-
//! Variante dafür ist Sache von `harw-dod-signals` (Contract-Master), nicht
//! dieser Crate.
//!
//! # Exportierte Elemente
//! [`FAN_MODIFY`], [`FAN_CLOSE_WRITE`], [`interpret_mask`].
//!
//! # Nebenläufigkeit
//! Zustandslos, `Send + Sync`, ohne innere Veränderlichkeit.
//!
//! # Fehler
//! [`crate::error::FsMonError::MalformedSource`], wenn keine der
//! unterstützten Zugriffsarten in der Maske gesetzt ist. Die rohe Maske
//! erscheint **nicht** in der Fehlermeldung (siehe
//! [`crate::error::FsMonError`]-Moduldoku).
//!
//! # Examples
//! ```rust
//! use harw_dod_fsmon::mask::{FAN_MODIFY, interpret_mask};
//! use harw_dod_signals::EventKind;
//!
//! let kind = interpret_mask(FAN_MODIFY, "/etc/passwd".to_owned())
//!     .expect("Schreibmaske muss interpretierbar sein");
//! assert!(matches!(kind, EventKind::FileWrite { .. }));
//! ```

use harw_dod_signals::EventKind;

use crate::error::FsMonError;

/// `FAN_MODIFY`: eine Datei wurde inhaltlich verändert (`man 7 fanotify`).
pub const FAN_MODIFY: u64 = 0x0000_0002;

/// `FAN_CLOSE_WRITE`: eine zum Schreiben geöffnete Datei wurde geschlossen
/// (`man 7 fanotify`) — der häufigste, zuverlässigste Schreibindikator, da er
/// erst nach Abschluss des Schreibvorgangs feuert.
pub const FAN_CLOSE_WRITE: u64 = 0x0000_0008;

/// Deutet eine rohe fanotify-Ereignismaske als [`EventKind`].
///
/// # Description
/// Trägt die Maske eine der beiden unterstützten Schreib-Bits ([`FAN_MODIFY`]
/// oder [`FAN_CLOSE_WRITE`], einzeln oder kombiniert), entsteht
/// `EventKind::FileWrite { path }`. Andernfalls ist die Ereignisform für
/// diese Crate unerwartet — siehe Moduldoku für die Begründung, warum das
/// kein Rückgabewert, sondern ein Fehler ist.
///
/// # Arguments
/// - `mask` (`u64`): die rohe fanotify-Ereignismaske, unverändert wie vom
///   Kernel geliefert.
/// - `path` (`String`): der bereits aufgelöste Dateisystempfad (siehe
///   [`crate::fdpath::interpret_fd_target`]), der in `EventKind::FileWrite`
///   übernommen wird.
///
/// # Returns
/// `EventKind::FileWrite { path }`, wenn die Maske einen Schreibzugriff
/// trägt.
///
/// # Errors
/// [`FsMonError::MalformedSource`], wenn `mask` keines der unterstützten Bits
/// gesetzt hat.
///
/// # Examples
/// ```rust
/// use harw_dod_fsmon::mask::{FAN_CLOSE_WRITE, interpret_mask};
///
/// let kind = interpret_mask(FAN_CLOSE_WRITE, "/var/log/app.log".to_owned())
///     .expect("FAN_CLOSE_WRITE muss interpretierbar sein");
/// assert_eq!(
///     serde_json::to_string(&kind).expect("serialisiert"),
///     r#"{"kind":"file-write","path":"/var/log/app.log"}"#
/// );
/// ```
pub fn interpret_mask(mask: u64, path: String) -> Result<EventKind, FsMonError> {
    if mask & (FAN_MODIFY | FAN_CLOSE_WRITE) != 0 {
        Ok(EventKind::FileWrite { path })
    } else {
        Err(FsMonError::MalformedSource)
    }
}

#[cfg(test)]
mod tests {
    use harw_dod_signals::EventKind;

    use super::{FAN_CLOSE_WRITE, FAN_MODIFY, interpret_mask};
    use crate::error::FsMonError;

    #[test]
    fn test_interpret_mask_close_write_yields_file_write() {
        let kind = interpret_mask(FAN_CLOSE_WRITE, "/etc/passwd".to_owned())
            .expect("FAN_CLOSE_WRITE muss interpretierbar sein");
        match kind {
            EventKind::FileWrite { path } => assert_eq!(path, "/etc/passwd"),
            other => panic!("erwartet FileWrite, erhalten {other:?}"),
        }
    }

    #[test]
    fn test_interpret_mask_modify_yields_file_write() {
        let kind = interpret_mask(FAN_MODIFY, "/etc/shadow".to_owned())
            .expect("FAN_MODIFY muss interpretierbar sein");
        assert!(matches!(kind, EventKind::FileWrite { .. }));
    }

    #[test]
    fn test_interpret_mask_combined_write_bits_yield_file_write() {
        let kind = interpret_mask(FAN_MODIFY | FAN_CLOSE_WRITE, "/tmp/x".to_owned())
            .expect("kombinierte Schreib-Bits müssen interpretierbar sein");
        assert!(matches!(kind, EventKind::FileWrite { .. }));
    }

    #[test]
    fn test_interpret_mask_unrecognized_bits_return_malformed_source() {
        // FAN_ACCESS (0x01) allein trägt keinen Schreibzugriff.
        let err = interpret_mask(0x01, "/etc/passwd".to_owned())
            .expect_err("reiner Lesezugriff darf nicht interpretierbar sein");
        assert!(matches!(err, FsMonError::MalformedSource));
    }

    #[test]
    fn test_interpret_mask_zero_returns_malformed_source_without_raw_bytes_in_message() {
        let err = interpret_mask(0, "/etc/passwd".to_owned())
            .expect_err("eine leere Maske muss scheitern");
        let message = err.to_string();
        assert!(matches!(err, FsMonError::MalformedSource));
        assert!(!message.contains('0'), "Meldung darf keine rohe Maske enthalten: {message}");
    }
}
