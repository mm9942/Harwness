//! Fehler von `harw-dod-procmon`: bewusst inhaltsfrei, wie
//! `harw_dod_cap::SensorError` und `harw_dod_bpf::BpfError`.
//!
//! # Verantwortungsbereich
//! [`ProcmonError`] ist der eine Fehlertyp dieser Crate. Er entsteht
//! ausschließlich in [`crate::event::parse_exec_payload`], wenn ein
//! `payload`-Puffer aus dem Ringpuffer nicht die erwartete Form hat — zu
//! kurz für ein Pflichtfeld, oder ein Feld außerhalb des darstellbaren
//! Bereichs. Die zugrunde liegenden Byte-Leser aus `harw-dod-bpf`
//! (`read_u32_le`, `read_fixed_c_str`) liefern dafür bereits
//! `harw_dod_bpf::BpfError::MalformedEvent` — diese Crate bildet das auf
//! eine **eigene**, gleichnamige Variante ab, statt den Fehlertyp einer
//! fremden Crate in der eigenen öffentlichen Fläche weiterzureichen. Beide
//! Varianten sind ohnehin inhaltsfrei; es geht hier nicht um verlorenen
//! Kontext, sondern um eine saubere Fehlergrenze zwischen den beiden Crates.
//!
//! **Inhaltsfrei heißt hier zusätzlich: keine rohen `argv`-Bytes.** Eine
//! Kommandozeile ist angreiferkontrolliert und enthält regelmäßig Geheimnisse
//! (Zugangstoken als Argument, eingebettete Passwörter, siehe
//! [`crate::event`]-Moduldoku für die volle Begründung). [`ProcmonError`]
//! besitzt strukturell **keinen** Konstruktionsweg, der `argv`-Bytes
//! aufnehmen könnte — es gibt kein Feld dafür.
//!
//! `Display`, `Debug`, `std::error::Error` entstehen über
//! `#[derive(harw_macros::HarwError)]` (Muster: `harw-dod-bpf/src/error.rs`).
//! Kein `anyhow`, kein `thiserror`. Diese Crate braucht kein `#[from]`: die
//! einzige Variante entsteht durch eigene Konstruktion in
//! [`crate::event::parse_exec_payload`], nie durch automatische Konvertierung
//! eines fremden Fehlertyps.
//!
//! # Exportierte Typen
//! [`ProcmonError`], sowie der vom Makro erzeugte Alias `ProcmonResult<T>`.
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_procmon::error::ProcmonError;
//!
//! let err = ProcmonError::MalformedEvent;
//! assert_eq!(err.to_string(), "process-exec payload buffer is malformed");
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate.
///
/// # Description
/// Eine einzige, inhaltsfreie Variante. Es gibt heute nur einen Fehlerpfad
/// in dieser Crate — die Deutung eines `payload`-Puffers in
/// [`crate::event::parse_exec_payload`] — deshalb genügt eine Variante ohne
/// eingebetteten Ursache-Fehler. [`crate::sensor::ProcmonSensor::poll`]
/// entpackt daraus (und aus `harw_dod_bpf::BpfError`) das vom
/// `harw_dod_signals::Sensor`-Vertrag verlangte `harw_dod_cap::SensorError`
/// (private Abbildungsfunktion in `crate::sensor`, Muster:
/// `harw-dod-authlog/src/sensor.rs::into_sensor_error`).
#[derive(Debug, HarwError)]
pub enum ProcmonError {
    /// Ein `payload`-Puffer eines Prozessstart-Ereignisses hat nicht die in
    /// [`crate::event`] dokumentierte Form.
    ///
    /// Zum Beispiel kürzer als der feste, art-spezifische Kopfbereich
    /// (`pid`/`ppid`/`uid`/`comm`/`filename`), oder ein an `argv`
    /// angrenzendes Feld, dessen Breite über das Pufferende hinausreicht.
    /// **Inhaltsfrei:** nennt nie die Länge, die Bytes oder den Inhalt des
    /// betroffenen Puffers — insbesondere nie ein Byte von `argv`.
    #[msg("process-exec payload buffer is malformed")]
    MalformedEvent,
}

#[cfg(test)]
mod tests {
    use super::{ProcmonError, ProcmonResult};

    #[test]
    fn test_malformed_event_display_is_exact_and_content_free() {
        let err = ProcmonError::MalformedEvent;
        assert_eq!(err.to_string(), "process-exec payload buffer is malformed");
        // Inhaltsfrei: die Meldung ist ein fester String ohne interpolierte
        // Felder, kann also strukturell keine Rohbytes (insbesondere kein
        // `argv`) enthalten.
    }

    #[test]
    fn test_malformed_event_has_no_source() {
        assert!(std::error::Error::source(&ProcmonError::MalformedEvent).is_none());
    }

    #[test]
    fn test_procmon_result_alias_carries_procmon_error() {
        fn always_fails() -> ProcmonResult<()> {
            Err(ProcmonError::MalformedEvent)
        }
        assert!(always_fails().is_err());
    }
}
