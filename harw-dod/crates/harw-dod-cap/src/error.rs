//! Fehler eines Sensors: inhaltsfrei, damit Logging nie Host-Daten preisgibt.
//!
//! # Verantwortungsbereich
//! [`SensorError`] ist der eine Fehlertyp dieser Crate (Vertrag Abschnitt H.1,
//! Abschnitt F). Kein Feldwert, kein Pfad, keine gelesene Zeile erscheint in
//! einer seiner Meldungen: ein Sensorfehler wird geloggt, und was geloggt
//! wird, verlässt den Host. [`Permanence`] klassifiziert, ob ein erneuter
//! Versuch sinnvoll ist.
//!
//! Zwei Varianten lassen sich leicht verwechseln, meinen aber Verschiedenes:
//! [`SensorError::MalformedSource`] sagt etwas über die überwachte Quelle
//! (sie hat nicht die erwartete Form), [`SensorError::ToolFault`] sagt etwas
//! über **unser** Werkzeug (es kann eine an sich unauffällige Quelle nicht
//! verarbeiten). Korrektur K79 ist der Anlass: nach der Vereinheitlichung auf
//! `version.workspace = true` meldete ein Sensor monatelang „Quelle
//! fehlerhaft", obwohl der überwachte Baum in Ordnung war und der Parser das
//! Problem hatte.
//!
//! `Display`, `Debug`, `std::error::Error` und die `From`-Konvertierung für
//! `#[from]`-Varianten entstehen über `#[derive(harw_macros::HarwError)]`
//! (Muster: `harw-plan/src/error.rs`). Kein `anyhow`, kein `thiserror`.
//!
//! # Exportierte Typen
//! [`SensorError`], [`Permanence`].
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_cap::error::{Permanence, SensorError};
//!
//! let err = SensorError::OutsideScope;
//! assert_eq!(err.permanence(), Permanence::Permanent);
//! assert_eq!(err.to_string(), "path resolves outside the sensor read scope");
//! ```

use harw_macros::HarwError;

/// Ob ein Fehler wiederholbar ist.
///
/// # Description
/// Steuert, ob ein Sentinel einen Sensor nach einem Fehler erneut pollen darf
/// oder ihn abmeldet. `Permanent` bedeutet: eine Wiederholung kann das
/// Ergebnis grundsätzlich nicht ändern, solange sich der Host oder der
/// konfigurierte Bereich nicht ändert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permanence {
    /// Ein erneuter Versuch kann gelingen (z. B. ein flüchtiger Lesefehler).
    Transient,
    /// Ein erneuter Versuch wird mit an Sicherheit grenzender Wahrscheinlichkeit
    /// wieder scheitern.
    Permanent,
}

/// Fehler eines Sensors.
///
/// # Description
/// **Inhaltsfrei:** kein Feldwert, kein Pfad, keine gelesene Zeile erscheint
/// in einer dieser Varianten oder ihrer `Display`-Ausgabe. Ein Sensorfehler
/// wird geloggt, und was geloggt wird, verlässt den Host — deshalb trägt
/// keine Variante mehr Kontext, als zum Unterscheiden der fünf Fälle nötig
/// ist.
#[derive(Debug, HarwError)]
pub enum SensorError {
    /// Ein aufgelöster Pfad liegt außerhalb des konfigurierten Lesebereichs.
    ///
    /// Wird von [`crate::scope::ReadScope::open`] geliefert, nachdem alle
    /// Symlinks aufgelöst wurden. Nennt **nie** das Ziel.
    #[msg("path resolves outside the sensor read scope")]
    OutsideScope,

    /// Die Quelle dieses Sensors existiert auf diesem Host nicht.
    ///
    /// Zum Beispiel ein `/sys`-Pfad, den der laufende Kernel nicht anbietet.
    #[msg("sensor source is unavailable on this host")]
    SourceUnavailable,

    /// Die Quelle existiert, hat aber nicht die erwartete Form.
    ///
    /// Zum Beispiel eine `/proc`-Datei mit einer unerwarteten Spaltenzahl.
    #[msg("sensor source has an unexpected shape")]
    MalformedSource,

    /// Unser Werkzeug kann eine an sich unauffällige Quelle dauerhaft nicht
    /// verarbeiten.
    ///
    /// Anders als [`Self::MalformedSource`] trifft diese Variante keine
    /// Aussage über den überwachten Baum — der kann in bester Ordnung sein.
    /// Sie beschreibt eine Lücke im **eigenen** Parser oder Interpreter
    /// dieses Sensors: eine Eingabeform, die es tatsächlich gibt, mit der
    /// unser Code aber nicht umgehen kann. Der Anlass war Korrektur K79:
    /// nach der Vereinheitlichung auf `version.workspace = true` meldete ein
    /// Sensor monatelang „Quelle fehlerhaft", obwohl der überwachte Baum in
    /// Ordnung war und der Parser das Problem hatte. Diese Variante gibt dem
    /// tatsächlichen Fall — unser Werkzeug versagt, nicht die Quelle — einen
    /// eigenen Namen, statt ihn wieder unter `MalformedSource` zu verstecken.
    #[msg("sensor tool cannot process this source")]
    ToolFault,

    /// Ein Lese- oder Auflösungsvorgang ist mit einem I/O-Fehler gescheitert.
    ///
    /// # Arguments
    /// - `source` (`std::io::Error`): der zugrunde liegende Betriebssystem-
    ///   Fehler. Wird über `std::error::Error::source()` verlinkt, aber nie
    ///   in die `Display`-Meldung eingebettet (inhaltsfrei).
    ///
    /// # Warum eine Tupel-Variante
    /// `#[derive(harw_macros::HarwError)]` erzeugt das `From`-Impl **nur** für
    /// eine Variante, die (a) das `#[from]`-Attribut auf sich selbst trägt und
    /// (b) genau ein unbenanntes Feld hat. Auf einem benannten Feld ist
    /// `#[from]` als Helfer-Attribut inert: es kompiliert, erzeugt aber still
    /// kein `From` — und `?` funktioniert dann nicht, ohne dass der Compiler
    /// etwas sagt. `harw-dod-readfs` und die elf Sensor-Crates reichen
    /// I/O-Fehler über `?` durch und brauchen dieses Impl.
    /// **Inhaltsfrei, auch hier.** Die Meldung wiederholt den Text des
    /// eingebetteten Fehlers nicht. Rusts `std::io::Error` hängt zwar von sich
    /// aus keinen Pfad an, aber die Zusage dieser Crate gilt unabhängig davon,
    /// was ein fremder Fehlertyp trägt — sonst hinge sie an einer Eigenschaft,
    /// die niemand garantiert. Wer den Detailwert braucht, holt ihn über
    /// `std::error::Error::source()`.
    #[msg("sensor read failed")]
    #[from]
    Io(std::io::Error),
}

impl SensorError {
    /// Ob ein erneuter Versuch sinnvoll ist.
    ///
    /// # Description
    /// `Permanent` führt beim Sentinel zur Abmeldung des Sensors, `Transient`
    /// erlaubt einen weiteren Poll-Versuch. Die Einteilung:
    ///
    /// - [`Self::OutsideScope`] → `Permanent`: [`crate::scope::ReadScope`] ist
    ///   ein Halbverband, der nur schrumpfen kann — ein Pfad, der heute
    ///   außerhalb liegt, kann durch keine künftige Operation wieder
    ///   hineinrutschen.
    /// - [`Self::SourceUnavailable`] → `Permanent`: „unavailable on this
    ///   host“ beschreibt eine Eigenschaft der Hardware-/Kernel-Konfiguration
    ///   dieses Hosts, die sich innerhalb eines Sensor-Laufs nicht ändert.
    /// - [`Self::MalformedSource`] → `Transient`: `/proc`- und `/sys`-Dateien
    ///   sind Momentaufnahmen; eine unerwartete Form kann eine flüchtige,
    ///   kernel-interne Inkonsistenz sein, die beim nächsten Poll verschwunden
    ///   ist.
    /// - [`Self::ToolFault`] → `Permanent`: ein Werkzeugfehler wiederholt
    ///   sich beim nächsten Poll identisch, weil dieselbe Eingabeform auf
    ///   denselben Codepfad trifft — anders als bei [`Self::MalformedSource`]
    ///   ändert sich hier nichts von selbst, solange niemand den Parser
    ///   korrigiert (Korrektur K79).
    /// - [`Self::Io`] → `Transient`: I/O-Fehler sind die klassische Kategorie
    ///   kurzfristiger Störungen (Sperre, Unterbrechung, kurzzeitige
    ///   Nichtverfügbarkeit).
    ///
    /// # Returns
    /// Die [`Permanence`] dieser Fehlervariante.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::error::{Permanence, SensorError};
    ///
    /// assert_eq!(SensorError::OutsideScope.permanence(), Permanence::Permanent);
    /// assert_eq!(SensorError::ToolFault.permanence(), Permanence::Permanent);
    /// assert_eq!(SensorError::MalformedSource.permanence(), Permanence::Transient);
    /// ```
    #[must_use]
    pub const fn permanence(&self) -> Permanence {
        match self {
            Self::OutsideScope | Self::SourceUnavailable | Self::ToolFault => Permanence::Permanent,
            Self::MalformedSource | Self::Io(_) => Permanence::Transient,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Permanence, SensorError};

    #[test]
    fn test_permanence_outside_scope_is_permanent() {
        assert_eq!(
            SensorError::OutsideScope.permanence(),
            Permanence::Permanent
        );
    }

    #[test]
    fn test_permanence_source_unavailable_is_permanent() {
        assert_eq!(
            SensorError::SourceUnavailable.permanence(),
            Permanence::Permanent
        );
    }

    #[test]
    fn test_permanence_malformed_source_is_transient() {
        assert_eq!(
            SensorError::MalformedSource.permanence(),
            Permanence::Transient
        );
    }

    #[test]
    fn test_permanence_tool_fault_is_permanent() {
        assert_eq!(SensorError::ToolFault.permanence(), Permanence::Permanent);
    }

    #[test]
    fn test_permanence_io_is_transient() {
        let source = std::io::Error::other("boom");
        let err = SensorError::Io(source);
        assert_eq!(err.permanence(), Permanence::Transient);
    }

    #[test]
    fn test_outside_scope_display_is_exact_and_content_free() {
        assert_eq!(
            SensorError::OutsideScope.to_string(),
            "path resolves outside the sensor read scope"
        );
    }

    #[test]
    fn test_source_unavailable_display_is_exact() {
        assert_eq!(
            SensorError::SourceUnavailable.to_string(),
            "sensor source is unavailable on this host"
        );
    }

    #[test]
    fn test_malformed_source_display_is_exact() {
        assert_eq!(
            SensorError::MalformedSource.to_string(),
            "sensor source has an unexpected shape"
        );
    }

    #[test]
    fn test_tool_fault_display_is_exact_and_content_free() {
        assert_eq!(
            SensorError::ToolFault.to_string(),
            "sensor tool cannot process this source"
        );
    }

    #[test]
    fn test_tool_fault_display_differs_from_malformed_source() {
        assert_ne!(
            SensorError::ToolFault.to_string(),
            SensorError::MalformedSource.to_string()
        );
    }

    #[test]
    fn test_io_display_never_contains_the_underlying_os_message() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "/etc/shadow missing");
        let err = SensorError::Io(source);
        // Inhaltsfrei: die Display-Meldung dieser Crate darf den Text des
        // eingebetteten I/O-Fehlers nicht wiederholen, auch wenn er selbst
        // (hier künstlich) einen Pfad enthält.
        assert_eq!(err.to_string(), "sensor read failed");
        assert!(!err.to_string().contains("shadow"));
    }

    #[test]
    fn test_io_source_links_to_underlying_error() {
        let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = SensorError::Io(source);
        assert!(std::error::Error::source(&err).is_some());
    }
}
