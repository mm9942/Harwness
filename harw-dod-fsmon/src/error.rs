//! Fehlertyp von `harw-dod-fsmon`: inhaltsfrei, wie `harw_dod_cap::SensorError`.
//!
//! # Verantwortungsbereich
//! [`FsMonError`] ist der eine Fehlertyp dieser Crate. Er deckt genau die
//! Fehlerfälle ab, die beim **Lesen** roher fanotify-Ereignisse entstehen
//! können ([`crate::raw::FsEventSource::read_events`]) — nicht die Fälle, die
//! beim **Formen** eines Ereignisses entstehen (eine unerwartete
//! Ereignismaske, siehe [`crate::mask`]) oder beim **Anreichern** mit der
//! loginuid ([`crate::loginuid::resolve_loginuid`], die bewusst
//! fehlerfrei/`Option`-basiert ist und nie hierher zurückmeldet).
//!
//! # Warum ein eigener Typ statt `harw_dod_cap::SensorError`
//! Die Fläche von [`crate::raw::FsEventSource`] ist im Vertrag dieses Knotens
//! wörtlich mit `Result<Vec<RawFsEvent>, FsMonError>` festgelegt — sowohl der
//! Binary-Knoten (AW4-02b, echte fanotify-Bindung) als auch die Regel-Crate
//! (AW4-03) schreiben gegen genau diesen Namen. Inhaltlich spiegelt
//! [`FsMonError`] drei der vier Varianten von `harw_dod_cap::SensorError`
//! ([`Self::SourceUnavailable`], [`Self::MalformedSource`], [`Self::Io`]) —
//! die vierte, `OutsideScope`, gehört nicht hierher: ein Ereignis außerhalb
//! des Lesebereichs wird beim Formen stillschweigend verworfen (siehe
//! [`crate::shape::shape_event`]), nicht als Fehler gemeldet.
//!
//! # Exportierte Typen
//! [`FsMonError`] sowie `FsMonResult<T>` (vom `HarwError`-Derive erzeugter
//! Alias).
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_fsmon::error::FsMonError;
//!
//! let err = FsMonError::MalformedSource;
//! assert_eq!(err.to_string(), "fanotify event has an unexpected shape");
//! ```

use harw_macros::HarwError;

/// Fehler beim Lesen roher fanotify-Ereignisse.
///
/// # Description
/// **Inhaltsfrei mit einer dokumentierten Ausnahme:** [`Self::SourceUnavailable`]
/// und [`Self::MalformedSource`] tragen keine Felder und sind damit garantiert
/// inhaltsfrei. [`Self::Io`] zeigt die `Display`-Meldung des gekapselten
/// `std::io::Error` an — für echte Betriebssystemfehler (z. B.
/// `std::io::ErrorKind::NotFound` ohne eigenen Text) ist das eine generische,
/// pfadfreie Kernel-Formulierung wie „entity not found"; ein von einem
/// Aufrufer selbst mit `std::io::Error::new(kind, "eigener Text")`
/// konstruierter Fehler kann dagegen beliebigen Text tragen. Diese Crate
/// konstruiert an keiner eigenen Stelle einen solchen `Custom`-Fehler; die
/// Eigenschaft ist dieselbe, die `harw_dod_cap::SensorError::Io` bereits hat.
#[derive(Debug, HarwError)]
pub enum FsMonError {
    /// Die fanotify-Quelle ist auf diesem Host nicht verfügbar (z. B. ein
    /// Kernel ohne `CONFIG_FANOTIFY` oder eine fehlende Berechtigung, den
    /// Mechanismus überhaupt zu initialisieren).
    #[msg("fanotify source is unavailable on this host")]
    SourceUnavailable,

    /// Ein rohes Ereignis hat nicht die erwartete Form (z. B. eine
    /// Ereignismaske ohne eine der unterstützten Zugriffsarten, siehe
    /// [`crate::mask::interpret_mask`]).
    #[msg("fanotify event has an unexpected shape")]
    MalformedSource,

    /// Das Lesen der zugrunde liegenden Quelle ist mit einem I/O-Fehler
    /// gescheitert.
    ///
    /// # Arguments
    /// - `source` (`std::io::Error`): der zugrunde liegende
    ///   Betriebssystem-Fehler. Über `std::error::Error::source()` verlinkt.
    ///   Erscheint **auch** in der `Display`-Meldung dieser Variante (siehe
    ///   die Ausnahme in der Enum-Dokumentation) — für echte, vom
    ///   Betriebssystem erzeugte Fehler ist das eine generische,
    ///   inhaltsfreie Formulierung wie „entity not found", da diese Crate an
    ///   keiner Stelle selbst einen `std::io::Error` mit eigenem Freitext
    ///   konstruiert.
    #[msg("fanotify read failed: {0}")]
    #[from]
    Io(std::io::Error),
}

impl From<FsMonError> for harw_dod_cap::SensorError {
    /// Bildet [`FsMonError`] auf `harw_dod_cap::SensorError` ab.
    ///
    /// # Description
    /// Ermöglicht [`crate::sensor::FsMonSensor::poll`], den Fehler dieser
    /// Crate über `?` in den von `harw_dod_signals::Sensor::poll`
    /// geforderten Fehlertyp zu überführen. Strukturelle Eins-zu-eins-
    /// Abbildung ohne Textkonkatenation: jede [`FsMonError`]-Variante hat
    /// eine inhaltlich entsprechende `SensorError`-Variante.
    ///
    /// # Arguments
    /// - `err` (`FsMonError`): der zu übersetzende Fehler.
    ///
    /// # Returns
    /// Die inhaltlich entsprechende `harw_dod_cap::SensorError`-Variante.
    ///
    /// # Errors
    /// Keine — diese Konvertierung ist total, jede [`FsMonError`]-Variante
    /// hat eine Entsprechung.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::SensorError;
    /// use harw_dod_fsmon::error::FsMonError;
    ///
    /// let mapped: SensorError = FsMonError::MalformedSource.into();
    /// assert!(matches!(mapped, SensorError::MalformedSource));
    /// ```
    fn from(err: FsMonError) -> Self {
        match err {
            FsMonError::SourceUnavailable => Self::SourceUnavailable,
            FsMonError::MalformedSource => Self::MalformedSource,
            FsMonError::Io(source) => Self::Io(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use harw_dod_cap::SensorError;

    use super::{FsMonError, FsMonResult};

    #[test]
    fn test_source_unavailable_display_is_exact() {
        assert_eq!(
            FsMonError::SourceUnavailable.to_string(),
            "fanotify source is unavailable on this host"
        );
    }

    #[test]
    fn test_malformed_source_display_is_exact_and_content_free() {
        assert_eq!(
            FsMonError::MalformedSource.to_string(),
            "fanotify event has an unexpected shape"
        );
    }

    /// `std::io::Error`s, wie sie echte Betriebssystemaufrufe liefern (kein
    /// selbst getippter `Custom`-Text), zeigen eine generische,
    /// kernel-standardisierte Formulierung ohne Pfad oder Dateiname — siehe
    /// die Enum-Dokumentation für die Abgrenzung zum `Custom`-Fall, den
    /// diese Crate an keiner Stelle selbst erzeugt.
    #[test]
    fn test_io_display_for_a_genuine_os_error_kind_is_generic_and_path_free() {
        let source = std::io::Error::from(std::io::ErrorKind::NotFound);
        let err = FsMonError::Io(source);
        let message = err.to_string();
        assert!(message.starts_with("fanotify read failed: "));
        assert!(!message.contains('/'), "generische OS-Meldung darf keinen Pfad enthalten: {message}");
    }

    #[test]
    fn test_io_source_links_to_underlying_error() {
        let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = FsMonError::Io(source);
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_from_source_unavailable_maps_to_sensor_error() {
        let mapped: SensorError = FsMonError::SourceUnavailable.into();
        assert!(matches!(mapped, SensorError::SourceUnavailable));
    }

    #[test]
    fn test_from_malformed_source_maps_to_sensor_error() {
        let mapped: SensorError = FsMonError::MalformedSource.into();
        assert!(matches!(mapped, SensorError::MalformedSource));
    }

    #[test]
    fn test_from_io_maps_to_sensor_error_io_and_preserves_source() {
        let source = std::io::Error::other("boom");
        let mapped: SensorError = FsMonError::Io(source).into();
        assert!(matches!(mapped, SensorError::Io(_)));
        assert!(mapped.source().is_some());
    }

    #[test]
    fn test_fs_mon_result_alias_carries_fs_mon_error() {
        fn always_fails() -> FsMonResult<()> {
            Err(FsMonError::MalformedSource)
        }

        assert!(always_fails().is_err());
    }
}
