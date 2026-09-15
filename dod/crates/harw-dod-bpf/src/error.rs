//! Fehler der eBPF-Ladeschicht: bewusst inhaltsfrei, wie `harw_dod_cap::SensorError`.
//!
//! # Verantwortungsbereich
//! [`BpfError`] ist der eine Fehlertyp dieser Crate. Jede Variante trägt
//! genau so viel Kontext, wie zum Unterscheiden der Fälle nötig ist — keine
//! Rohbytes, keine Pfade, keine gelesenen Zeilen. Das gilt insbesondere für
//! [`BpfError::MalformedEvent`]: ein zu kurzer oder falsch geformter
//! Ereignispuffer erzeugt diese Variante, nie eine Meldung, die den Puffer
//! selbst zitiert.
//!
//! `Display`, `Debug`, `std::error::Error` und die `From`-Konvertierung für
//! [`BpfError::Io`] entstehen über `#[derive(harw_macros::HarwError)]`
//! (Muster: `harw-dod-cap/src/error.rs`). Kein `anyhow`, kein `thiserror`.
//!
//! # Exportierte Typen
//! [`BpfError`], [`BpfResult`] (vom Makro erzeugter Typalias).
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::error::BpfError;
//!
//! let err = BpfError::CapabilityUnavailable;
//! assert_eq!(err.to_string(), "bpf capability is unavailable on this host");
//! ```

use harw_macros::HarwError;

/// Fehler der eBPF-Ladeschicht.
///
/// # Description
/// Deckt zwei Quellen ab: den Ladeteil (`load`/`read_events` auf
/// [`crate::loader::BpfLoader`]) und den Formungsteil (`crate::event`'s
/// reine `&[u8]`-Deutungsfunktionen). [`Self::CapabilityUnavailable`] ist der
/// dokumentierte, lauffähige Fall eines Hosts ohne `CAP_BPF` — kein
/// Absturz, sondern ein Rückgabewert, den ein Sentinel als Degradierung
/// des Sensors meldet. [`Self::MalformedEvent`] ist **inhaltsfrei**: kein
/// Feldwert, kein Byte des betroffenen Puffers erscheint in der Meldung.
#[derive(Debug, HarwError)]
pub enum BpfError {
    /// Der Host bietet `CAP_BPF` (oder die Fähigkeit, ein Programm zu laden)
    /// nicht an.
    ///
    /// Wird von [`crate::loader::BpfLoader::load`] geliefert. Kein Fehler im
    /// Sinne eines Absturzes — der dokumentierte, lauffähige Betriebsfall
    /// eines Hosts ohne die Fähigkeit `harw_dod_cap::Capability::LoadBpfProgram`.
    /// Ein Sentinel meldet den betroffenen Sensor als degradiert, statt den
    /// Prozess scheitern zu lassen.
    #[msg("bpf capability is unavailable on this host")]
    CapabilityUnavailable,

    /// Ein Ereignispuffer aus dem Ringpuffer hat nicht die erwartete Form.
    ///
    /// Zum Beispiel kürzer als der feste Kopf-Bereich, oder ein Zeitstempel
    /// außerhalb des darstellbaren Bereichs. Wird ausschließlich vom
    /// Formungsteil (`crate::event`) geliefert. **Inhaltsfrei:** nennt nie
    /// die Länge, die Bytes oder den Inhalt des betroffenen Puffers.
    #[msg("bpf event buffer is malformed")]
    MalformedEvent,

    /// Der Programmrumpf konnte nicht von seiner Quelle gelesen werden.
    ///
    /// Liefert [`crate::spec::BpfProgramSource::resolve`], wenn die Quelle
    /// [`crate::spec::BpfProgramSource::Path`] ist und die Datei nicht lesbar
    /// ist. Betrifft **nie** [`crate::spec::BpfProgramSource::Embedded`] —
    /// eingebettete Bytes sind unfehlbar verfügbar.
    ///
    /// # Warum eine Tupel-Variante ohne eigenes `#[msg]`
    /// `#[derive(harw_macros::HarwError)]` erzeugt das `From`-Impl nur für
    /// eine Variante, die (a) `#[from]` auf sich selbst trägt und (b) genau
    /// ein unbenanntes Feld hat — auf einem benannten Feld ist `#[from]`
    /// inert. Ohne ein eigenes `#[msg]` bindet das Makro die eine gebundene
    /// Variable (`f0`) tatsächlich, indem es an `Display::fmt` des inneren
    /// Fehlers delegiert; mit einem `#[msg]`-Text ohne `{0}` bliebe `f0`
    /// ungenutzt und `-D warnings` schlüge auf `unused_variables` an.
    #[from]
    Io(std::io::Error),

    /// Der reale (`aya`-gestützte) Ladeteil ([`crate::real::RealBpfLoader`])
    /// konnte ein Programm nicht laden, nicht anheften, oder die erwartete
    /// Ringpuffer-Map (`"EVENTS"`) nicht finden bzw. nicht als solche
    /// deuten.
    ///
    /// **Inhaltsfrei und absichtlich grob gebündelt:** `aya`s eigene
    /// Fehlertypen (`EbpfError`, `ProgramError`, `MapError`) dürfen laut
    /// CI-Gate an keiner öffentlichen Signatur dieser Crate erscheinen (siehe
    /// [`crate`]-Moduldoku, Abschnitt „Warum kein `aya`-Typ nach außen
    /// dringt“) — deshalb bündelt diese eine Variante jede dieser Quellen,
    /// statt für jede einen eigenen, `aya`-typisierten Fall vorzusehen. Wird
    /// ausschließlich von [`crate::real::RealBpfLoader::load`] geliefert.
    #[msg("a real bpf loader failed to load, attach, or locate the event map of a program")]
    ProgramLoadFailed,

    /// [`crate::spec::BpfProgramKind::SocketFilter`] wird vom realen
    /// Ladeteil nicht unterstützt.
    ///
    /// `SocketFilter::attach` verlangt einen bereits offenen, vom Aufrufer
    /// besessenen Socket (`T: AsFd`), den eine generische Ladeschicht nicht
    /// besitzen darf — siehe [`crate::real`]-Moduldoku, Abschnitt „Was der
    /// reale Ladeteil nicht tut“, für die vollständige Begründung und die
    /// Bedingung, unter der das nachgeliefert werden könnte.
    #[msg("the real bpf loader does not support the SocketFilter program kind")]
    UnsupportedProgramKind,

    /// [`crate::loader::BpfLoader::read_events`] wurde mit einem Griff
    /// aufgerufen, den [`crate::real::RealBpfLoader`] nicht kennt — weder von
    /// [`crate::real::RealBpfLoader::load`] vergeben, noch (mehr) im
    /// internen Register vorhanden.
    #[msg("bpf handle is unknown to this loader")]
    UnknownHandle,
}

/// Bildet einen [`BpfError`] auf die passende `SensorError`-Variante ab.
///
/// # Description
/// **Die eine Wahrheit dieser Abbildung.** Sie stand zuvor in drei Kopien —
/// in `harw-dod-procmon`, `harw-dod-flow` und `harw-probe-bpf` —, jede als
/// private Funktion mit eigenem Doc-Kommentar, und alle drei brachen
/// gleichzeitig, als [`BpfError`] um drei Varianten wuchs. Genau das ist der
/// Grund, warum eine Regel nicht in Kopien lebt: sie driftet, und der Bruch
/// tritt an so vielen Stellen zugleich auf, wie es Kopien gibt.
///
/// Die Zuordnung folgt der **Dauerhaftigkeit** des Fehlers, nicht seiner
/// Herkunft:
///
/// - [`BpfError::CapabilityUnavailable`] → `SourceUnavailable`: ein Host ohne
///   `CAP_BPF` bekommt sie auch beim nächsten Versuch nicht.
/// - [`BpfError::ProgramLoadFailed`] → `SourceUnavailable`: entsteht nur in
///   `load()`; ein gescheiterter Ladevorgang gelingt beim Wiederholen nicht.
/// - [`BpfError::UnsupportedProgramKind`] → `SourceUnavailable`: ein
///   Programmtyp, den der Lader nicht kennt, wird ihm nicht bekannt.
/// - [`BpfError::UnknownHandle`] → `SourceUnavailable`: ein Sensor hält einen
///   Griff sein ganzes Leben lang unverändert; erkennt der Lader ihn nicht
///   mehr, scheitert jeder weitere Aufruf identisch.
/// - [`BpfError::MalformedEvent`] → `MalformedSource`: **kein**
///   `SourceUnavailable` — die Quelle ist erreichbar, ihr Inhalt ist es
///   nicht. Die Unterscheidung entscheidet, ob ein Sentinel den Sensor
///   abschaltet oder nur dieses eine Ereignis verwirft.
/// - [`BpfError::Io`] wird unverändert durchgereicht.
///
/// # Arguments
/// - `err` (`BpfError`): der abzubildende Fehler der Ladeschicht.
///
/// # Returns
/// Die entsprechende `SensorError`-Variante.
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::error::BpfError;
/// use harw_dod_cap::SensorError;
///
/// let mapped = SensorError::from(BpfError::UnsupportedProgramKind);
/// assert!(matches!(mapped, SensorError::SourceUnavailable));
/// ```
impl From<BpfError> for harw_dod_cap::SensorError {
    fn from(err: BpfError) -> Self {
        match err {
            BpfError::CapabilityUnavailable
            | BpfError::ProgramLoadFailed
            | BpfError::UnsupportedProgramKind
            | BpfError::UnknownHandle => Self::SourceUnavailable,
            BpfError::MalformedEvent => Self::MalformedSource,
            BpfError::Io(io) => Self::Io(io),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::BpfError;

    #[test]
    fn test_capability_unavailable_display_is_exact() {
        assert_eq!(
            BpfError::CapabilityUnavailable.to_string(),
            "bpf capability is unavailable on this host"
        );
    }

    #[test]
    fn test_malformed_event_display_is_exact_and_content_free() {
        let err = BpfError::MalformedEvent;
        assert_eq!(err.to_string(), "bpf event buffer is malformed");
        // Inhaltsfrei: die Meldung ist ein fester String ohne interpolierte
        // Felder, kann also strukturell keine Rohbytes enthalten.
    }

    #[test]
    fn test_io_variant_converts_from_std_io_error_via_from() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err: BpfError = source.into();
        assert!(matches!(err, BpfError::Io(_)));
    }

    #[test]
    fn test_io_display_delegates_to_inner_error_display() {
        let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let expected = source.to_string();
        let err = BpfError::Io(source);
        assert_eq!(err.to_string(), expected);
    }

    #[test]
    fn test_io_source_links_to_underlying_error() {
        let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = BpfError::Io(source);
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_capability_unavailable_and_malformed_event_have_no_source() {
        assert!(std::error::Error::source(&BpfError::CapabilityUnavailable).is_none());
        assert!(std::error::Error::source(&BpfError::MalformedEvent).is_none());
    }

    #[test]
    fn test_program_load_failed_display_is_exact_and_content_free() {
        assert_eq!(
            BpfError::ProgramLoadFailed.to_string(),
            "a real bpf loader failed to load, attach, or locate the event map of a program"
        );
        assert!(std::error::Error::source(&BpfError::ProgramLoadFailed).is_none());
    }

    #[test]
    fn test_unsupported_program_kind_display_is_exact() {
        assert_eq!(
            BpfError::UnsupportedProgramKind.to_string(),
            "the real bpf loader does not support the SocketFilter program kind"
        );
    }

    #[test]
    fn test_unknown_handle_display_is_exact() {
        assert_eq!(BpfError::UnknownHandle.to_string(), "bpf handle is unknown to this loader");
    }
}
