//! Fehlertyp dieses Binaries: nur die Startpfade, die den Prozess wirklich
//! beenden dürfen.
//!
//! # Verantwortungsbereich
//! `harw-sentinel` trifft an zwei Stellen die asymmetrische Entscheidung aus
//! der Aufgabenstellung: eine fehlende Landlock-Unterstützung (Modul
//! [`crate::sandbox`]) und ein nicht bindbarer IPC-Empfangssocket (Modul
//! [`crate::ipc`]) degradieren, statt den Prozess zu beenden — ein
//! Sicherheitssammler, der auf einem Host ohne diese Kernelfunktionen gar
//! nicht erst startet, verliert genau die Stufen, die dort noch funktioniert
//! hätten. [`SentinelBinError`] trägt deshalb ausschließlich die Pfade, für
//! die es **keinen** sinnvollen Degradationspfad gibt: ohne einen aufgelösten
//! Root-Space oder einen schreibbaren Telemetrie-Sink hat der Prozess keine
//! Grundlage, überhaupt zu laufen.
//!
//! # Exportierte Typen
//! [`SentinelBinError`], sowie der über `#[derive(harw_macros::HarwError)]`
//! erzeugte Alias `SentinelBinResult<T>`.
//!
//! # Nebenläufigkeit
//! `SentinelBinError` ist `Send + Sync`, weil jedes seiner Felder es ist.
//! Kein internes Locking, keine geteilten Ressourcen.
//!
//! # Examples
//! ```rust
//! use harw_sentinel::error::SentinelBinError;
//!
//! fn describe(err: &SentinelBinError) -> String {
//!     err.to_string()
//! }
//! ```

use harw_macros::HarwError;

/// Fehler dieses Binaries, die den Prozess mit einem Fehlschlag beenden.
///
/// # Description
/// Jede Variante entspricht genau einem Startschritt in `main`, der keine
/// Degradation kennt (siehe Moduldoku für die Abgrenzung zu den Pfaden, die
/// stattdessen ein `SensorDegraded`-Ereignis erzeugen).
#[derive(Debug, HarwError)]
pub enum SentinelBinError {
    /// Der Root-Space (`~/.harw` oder `--home`) konnte nicht aufgelöst
    /// werden.
    ///
    /// # Arguments
    /// - `0` (`harw_home::HomeError`): die zugrunde liegende Ursache (kein
    ///   Home-Verzeichnis auffindbar, oder `HARW_HOME`/`--home` zeigt auf
    ///   eine existierende Nicht-Verzeichnis-Datei).
    #[from]
    Home(harw_home::HomeError),

    /// Der Telemetrie-Sink (`harw_observe_file::FileSink`) konnte unter
    /// `harw_home::paths::telemetry_dir(home)` nicht geöffnet werden.
    ///
    /// # Arguments
    /// - `0` (`harw_observe_file::ObserveFileError`): die zugrunde liegende
    ///   Ursache (Verzeichnis nicht anlegbar, aktive Datei nicht öffenbar).
    #[from]
    Sink(harw_observe_file::ObserveFileError),

    /// Die Kommandozeile konnte nicht geparst werden.
    ///
    /// # Arguments
    /// - `0` (`clap::Error`): clap trägt bereits eine für Menschen lesbare
    ///   Nutzungsmeldung; sie wird unverändert durchgereicht.
    #[from]
    Cli(clap::Error),

    /// Das Unterkommando `completions` ist fehlgeschlagen.
    ///
    /// # Arguments
    /// - `0` (`harw_completions::CompletionError`): die zugrunde liegende
    ///   Ursache (Shell nicht erkennbar, fremde Zieldatei, I/O-Fehler).
    #[msg("shell completions failed: {0}")]
    #[from]
    Completions(harw_completions::CompletionError),
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{SentinelBinError, SentinelBinResult};

    fn home_error() -> harw_home::HomeError {
        harw_home::HomeError::io(
            "/no/such/path",
            std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        )
    }

    #[test]
    fn test_home_variant_display_delegates_to_inner_error() {
        let err = SentinelBinError::from(home_error());
        assert!(err.to_string().contains("/no/such/path"));
    }

    #[test]
    fn test_home_variant_source_returns_inner_error() {
        let err = SentinelBinError::from(home_error());
        assert!(err.source().is_some());
    }

    #[test]
    fn test_cli_variant_display_is_clap_usage_message() {
        let clap_err =
            clap::Error::raw(clap::error::ErrorKind::InvalidValue, "invalid --log value");
        let err = SentinelBinError::from(clap_err);
        assert!(err.to_string().contains("invalid --log value"));
    }

    #[test]
    fn test_sentinel_bin_result_alias_carries_sentinel_bin_error() {
        fn always_fails() -> SentinelBinResult<()> {
            Err(SentinelBinError::from(home_error()))
        }

        assert!(always_fails().is_err());
    }
}
