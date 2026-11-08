//! Fehlertyp von `harw-dod-warden` (Contract-Master §H.1: ein Fehlertyp je
//! Crate).
//!
//! # Verantwortungsbereich
//! [`WardenError`] deckt drei Gruppen von Fehlerpfaden ab:
//!
//! 1. **Ausführung** (`executor.rs`, `observe.rs`): eine an sich autorisierte
//!    Aktion scheitert an der eigentlichen Systemoperation (z. B. ein
//!    Dateisystemzugriff auf `/sys/fs/cgroup/...`) oder das Ziel lässt sich
//!    nicht beobachten.
//! 2. **Start** (`config.rs`, `keys.rs`, `trust.rs`): die Warden-eigene
//!    Policy `/etc/harw-dod/warden.toml` oder ein Schlüssel aus
//!    `proof_key_dir` ist syntaktisch falsch, unvollständig oder liegt nicht
//!    root-eigen und schreibgeschützt vor (PL-100 WP 3.1).
//! 3. **Zustand** (`nonce_ledger.rs`, `action_ledger.rs`, `holds.rs`,
//!    `state.rs`): der persistente Zustand im StateDirectory ist beschädigt,
//!    voll oder nicht schreibbar — der Warden arbeitet dann fail closed.
//!
//! Die Nachprüfung einer Autorisierung selbst liefert ihre eigenen Fehler
//! bereits fertig als [`harw_dod_warden_proto::ProofError`] bzw. dessen
//! inhaltsfreie Ablehnungskategorie [`harw_dod_warden_proto::Denial`] —
//! dieser Typ dupliziert das nicht; [`WardenError::Proof`] trägt einen
//! `ProofError` nur dort, wo er beim **Aufbau** (Key-ID, Schlüssel, Policy)
//! entsteht.
//!
//! # Inhaltsfreie Meldungen
//! Keine Meldung interpoliert einen Pfad, einen Schlüssel, eine Kennung oder
//! Bytes aus einer Datei: die Meldungen landen im Journal, Pfade und
//! Schlüsselmaterial gehören dort nicht hin. Zeilennummern und feste
//! Gründe (`&'static str`) reichen für die Diagnose.
//!
//! # Dieser Typ verlässt den Prozess nie
//! Wie `harw-dod-warden-proto::WardenProtoError` implementiert auch dieser
//! Typ absichtlich **kein** `Serialize`. Was der Warden nach außen meldet,
//! ist ausschließlich [`crate::warden::WardenOutcome`] bzw. dessen
//! `to_wire`-Abbildung auf [`harw_dod_warden_proto::WardenResponse`].
//!
//! # Nebenläufigkeit
//! Trägt keinen inneren Zustand außer den Fehlerdaten selbst; kein Locking,
//! keine geteilten Ressourcen. `Send + Sync` (jedes Feld ist es).

use std::fmt;

use harw_dod_warden_proto::ProofError;

/// Fehler dieser Crate.
///
/// # Description
/// Siehe Moduldoku für die drei Gruppen. `#[derive(harw_macros::HarwError)]`
/// erzeugt daraus `Display`, `std::error::Error` (mit `source()` für
/// [`Self::Io`], [`Self::Proof`] und [`Self::Encode`]) und den Typalias
/// [`WardenResult`] — **kein** `Debug` (siehe `impl Debug` unterhalb).
///
/// # Errors
/// Wird von den Ausführungs-Traits, dem Konfigurations- und Schlüssellader
/// und den Zustandsdateien zurückgegeben.
#[derive(harw_macros::HarwError)]
pub enum WardenError {
    /// Eine cgroup-Kennung ist kein gefahrloser relativer Pfad unterhalb der
    /// cgroup-Wurzel: ein leeres Segment (also ein führender, abschließender
    /// oder doppelter `/`), ein Segment `.` oder `..` oder ein Steuerzeichen
    /// (einschließlich NUL). Mehrstufige Pfade wie `harw.slice/job-1` sind
    /// zulässig; geprüft wird jedes `/`-getrennte Segment einzeln. `CgroupId`
    /// garantiert laut `harw-types` nur „nicht leer, nicht nur
    /// Leerzeichen" — keine Pfadsicherheit. Da der Warden der Gegenseite
    /// nichts glaubt (Crate-Moduldoku, `lib.rs`), prüft
    /// [`crate::executor::CgroupV2Executor`] das selbst, bevor die Kennung
    /// in einen Pfad eingebettet wird.
    #[msg("cgroup-Kennung enthält unzulässige Zeichen für einen Dateisystempfad")]
    InvalidCgroupId,

    /// Ein Dateisystemzugriff (cgroup-Operation, Zustandsdatei, Policy- oder
    /// Schlüsseldatei) ist fehlgeschlagen.
    #[msg("Dateisystemzugriff für die cgroup-Operation ist fehlgeschlagen: {0}")]
    #[from]
    Io(std::io::Error),

    /// Das beobachtete Ziel ist kein cgroup-Verzeichnis (oder hat eine
    /// Kernel-ID `0`) — der Warden handelt dann nicht.
    #[msg("Ziel ist kein cgroup-Verzeichnis")]
    TargetNotCgroup,

    /// Unter dem signierten Pfad liegt beim Eingriff eine andere cgroup als
    /// die geprüfte (Kernel-ID am geöffneten Verzeichnis weicht ab, B21).
    /// Der Ausführer hat nichts geschrieben.
    #[msg("Ziel ist nicht mehr die geprüfte cgroup")]
    TargetChanged,

    /// Eine stillgelegte Aktion (`ReleaseCgroup`) erreichte die Ausführung.
    /// Unerreichbar, solange `SignedAuthorization::verify` sie vorher
    /// ablehnt; der Ausführer verweigert sie trotzdem selbst.
    #[msg("stillgelegte Aktion wird nie ausgeführt")]
    RetiredAction,

    /// Eine Datei oder ein Verzeichnis, dem der Warden vertrauen muss
    /// (Policy, Schlüssel, StateDirectory), erfüllt die Eigentums- oder
    /// Modusregeln nicht (z. B. nicht root-eigen, gruppen- oder
    /// weltschreibbar, Symlink, kein reguläres File).
    #[msg("{what} ist nicht vertrauenswürdig abgelegt: {reason}")]
    UntrustedFile {
        /// Welche Datei (fester Name, kein Pfad), z. B. `"warden policy"`.
        what: &'static str,
        /// Fester Grund, z. B. `"group or world writable"`.
        reason: &'static str,
    },

    /// `warden.toml` verletzt die unterstützte TOML-Teilmenge oder nennt
    /// einen unbekannten Schlüssel.
    #[msg("warden policy: Fehler in Zeile {line}: {reason}")]
    ConfigSyntax {
        /// 1-basierte Zeilennummer.
        line: usize,
        /// Fester Grund (kein Dateiinhalt).
        reason: &'static str,
    },

    /// `warden.toml` ist syntaktisch korrekt, aber inhaltlich unzulässig
    /// (fehlender Pflichtwert, unbekannte Aktion, widersprüchliche Angaben).
    #[msg("warden policy ist unzulässig: {reason}")]
    ConfigInvalid {
        /// Fester Grund (kein Dateiinhalt).
        reason: &'static str,
    },

    /// Der Schlüsselordner `proof_key_dir` ist unbrauchbar (leer, zu viele
    /// Dateien, ungültiger Dateiname, falsches Schlüsselformat).
    #[msg("proof key store ist unbrauchbar: {reason}")]
    KeyStore {
        /// Fester Grund (kein Schlüsselmaterial, kein Pfad).
        reason: &'static str,
    },

    /// Ein Proof-v2-Baustein (Key-ID, Schlüssel, Schlüsselring, Policy)
    /// ließ sich nicht konstruieren.
    #[msg("proof v2 setup failed: {0}")]
    #[from]
    Proof(ProofError),

    /// Eine Zustandsdatei im StateDirectory ist beschädigt (Hash-Kette
    /// gebrochen, Sequenzlücke, unlesbare Zeile). Der Warden startet dann
    /// nicht (fail closed).
    #[msg("Zustandsdatei {ledger} ist beschädigt (Zeile {line})")]
    LedgerCorrupt {
        /// Welche Datei (fester Name: `"nonce"`, `"action"`, `"holds"`).
        ledger: &'static str,
        /// 1-basierte Zeilennummer (`0` für eine Datei ohne Zeilenstruktur).
        line: usize,
    },

    /// Eine Zustandsdatei hat ihre Obergrenze erreicht oder ist nach einem
    /// Schreibfehler gesperrt; neue Einträge werden fail closed abgelehnt.
    #[msg("Zustandsdatei {ledger} nimmt keine Einträge mehr an: {reason}")]
    LedgerUnavailable {
        /// Welche Datei (fester Name).
        ledger: &'static str,
        /// Fester Grund (`"full"`, `"locked after a write error"`, ...).
        reason: &'static str,
    },

    /// Ein Zustandsdatensatz ließ sich nicht als JSON kodieren.
    #[msg("Zustandsdatensatz ließ sich nicht kodieren: {0}")]
    Encode(#[source] serde_json::Error),

    /// Ein interner Mutex ist vergiftet (ein Thread ist mit gehaltener
    /// Sperre abgebrochen) — fail closed.
    #[msg("interner Zustand des Wardens ist gesperrt (vergifteter Mutex)")]
    StatePoisoned,
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

impl WardenError {
    /// Returns whether this error means "the target does not exist (any more)".
    ///
    /// # Description
    /// `true` für [`Self::TargetNotCgroup`] und ein [`Self::Io`] mit
    /// `ErrorKind::NotFound`. Der Warden meldet ein verschwundenes Ziel als
    /// geändertes Ziel (`Denial::TargetChanged`), jeden anderen
    /// Beobachtungsfehler als vorübergehend nicht prüfbar
    /// (`Denial::Unavailable`).
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden::WardenError;
    ///
    /// let gone = WardenError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
    /// assert!(gone.is_target_gone());
    /// assert!(!WardenError::StatePoisoned.is_target_gone());
    /// ```
    #[must_use]
    pub fn is_target_gone(&self) -> bool {
        match self {
            Self::TargetNotCgroup => true,
            Self::Io(error) => error.kind() == std::io::ErrorKind::NotFound,
            _ => false,
        }
    }

    /// Returns whether this error means "the target is not the verified cgroup any more".
    ///
    /// # Description
    /// `true` für [`Self::TargetChanged`] (andere Kernel-ID am geöffneten
    /// Verzeichnis) und für jedes [`Self::is_target_gone`]. Der Warden meldet
    /// einen solchen Ausführungsfehler als `Denial::TargetChanged` statt als
    /// `ExecutionFailed`: nichts wurde ausgeführt, die Ursache ist das Ziel.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden::WardenError;
    ///
    /// assert!(WardenError::TargetChanged.is_target_replaced_or_gone());
    /// assert!(WardenError::TargetNotCgroup.is_target_replaced_or_gone());
    /// assert!(!WardenError::InvalidCgroupId.is_target_replaced_or_gone());
    /// ```
    #[must_use]
    pub fn is_target_replaced_or_gone(&self) -> bool {
        matches!(self, Self::TargetChanged) || self.is_target_gone()
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
        assert!(
            err.to_string()
                .starts_with("Dateisystemzugriff für die cgroup-Operation ist fehlgeschlagen:")
        );
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

    #[test]
    fn test_setup_errors_render_only_fixed_reasons() {
        let untrusted = WardenError::UntrustedFile {
            what: "warden policy",
            reason: "group or world writable",
        };
        assert_eq!(
            untrusted.to_string(),
            "warden policy ist nicht vertrauenswürdig abgelegt: group or world writable"
        );
        let syntax = WardenError::ConfigSyntax {
            line: 7,
            reason: "unknown key",
        };
        assert_eq!(
            syntax.to_string(),
            "warden policy: Fehler in Zeile 7: unknown key"
        );
        let corrupt = WardenError::LedgerCorrupt {
            ledger: "action",
            line: 3,
        };
        assert!(corrupt.to_string().contains("action"));
        assert!(corrupt.source().is_none());
    }

    #[test]
    fn test_is_target_gone_distinguishes_missing_from_other_failures() {
        assert!(WardenError::TargetNotCgroup.is_target_gone());
        assert!(
            WardenError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)).is_target_gone()
        );
        assert!(!WardenError::from(sample_io_error()).is_target_gone());
        assert!(!WardenError::InvalidCgroupId.is_target_gone());
        assert!(!WardenError::TargetChanged.is_target_gone());
    }

    #[test]
    fn test_target_changed_is_content_free_and_counts_as_replaced() {
        let err = WardenError::TargetChanged;
        assert_eq!(err.to_string(), "Ziel ist nicht mehr die geprüfte cgroup");
        assert!(err.source().is_none());
        assert!(err.is_target_replaced_or_gone());
        assert!(
            WardenError::Io(std::io::Error::from(std::io::ErrorKind::NotFound))
                .is_target_replaced_or_gone()
        );
        assert!(!WardenError::from(sample_io_error()).is_target_replaced_or_gone());
    }
}
