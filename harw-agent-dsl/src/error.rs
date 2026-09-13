//! Fehlertypen der Harwness Agent Definition DSL.
//!
//! Dieses Modul definiert [`DslError`], den zentralen Error-Enum für das gesamte
//! `harw-agent-dsl`-Crate. Alle Varianten tragen ausreichend Kontext, um die
//! Fehlerursache ohne Blick in den Quellcode zu verstehen (§20 DSL-Spec).
//!
//! # Schlüsseltypen
//! - [`DslError`] — vollständige Fehlerbeschreibung
//! - [`DslResult<T>`] — Alias für `Result<T, DslError>`
//! - [`DiagLocation`] — optionaler Schicht- und Feldpfad-Kontext für Fehlervarianten
//!
//! # Fehlerquellen
//! - TOML-Parsing (`Parse`, `Toml`)
//! - Namensraum-ID-Parsing (`InvalidId`)
//! - Auflösung fehlender Basen/Mixins (`MissingBase`, `MissingMixin`)
//! - Zyklische Vererbung (`InheritanceCycle`)
//! - Authority-Verletzungen (`AuthorityElevation`)
//! - Rollenkonflikte bei Mixins (`IllegalRoleForMixin`)
//! - Unbekannte Merge-Operatoren (`UnknownMergeOp`)
//! - I/O-Fehler (`Io`)
//!
//! # Nebenläufigkeit
//! `DslError` implementiert `Send + Sync` (alle Felder sind es).

use std::path::PathBuf;

use crate::ids::{DefinitionId, DefinitionRef};
use crate::roles::AgentRoleId;

/// Optionaler Schicht- und Feldpfad-Kontext für diagnostische Fehlervarianten (§20).
///
/// # Beschreibung
/// Trägt den Kontext, der zur Fehlerstelle zeigt: in welcher Definitionsschicht
/// (`layer`) und an welchem Feldpfad (`field_path`) der Fehler aufgetreten ist.
/// Beide Felder sind `Option`, damit bestehende Aufrufstellen mit `DiagLocation::none()`
/// unverändert bleiben können.
///
/// # Nebenläufigkeit
/// `Send + Sync`.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::error::DiagLocation;
///
/// let loc = DiagLocation {
///     layer: Some("Project".to_owned()),
///     field_path: Some("authority.capabilities[3]".to_owned()),
/// };
/// assert!(loc.field_path.as_deref().unwrap().contains("authority"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagLocation {
    /// Name der Definitionsschicht, in der der Fehler aufgetreten ist (z. B. `"BuiltIn"`, `"Project"`).
    pub layer: Option<String>,
    /// Feldpfad innerhalb der Definition (z. B. `"authority.capabilities[3]"`).
    pub field_path: Option<String>,
}

impl DiagLocation {
    /// Erzeugt einen leeren Diagnose-Ort (beide Felder `None`).
    ///
    /// # Rückgabe
    /// `DiagLocation` mit `layer = None` und `field_path = None`.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_agent_dsl::error::DiagLocation;
    ///
    /// let loc = DiagLocation::none();
    /// assert!(loc.layer.is_none());
    /// assert!(loc.field_path.is_none());
    /// ```
    pub fn none() -> Self {
        Self {
            layer: None,
            field_path: None,
        }
    }

    /// Erzeugt einen Diagnose-Ort mit einem Feldpfad (kein Schicht-Kontext).
    ///
    /// # Argumente
    /// - `field_path` (`impl Into<String>`): der Feldpfad zum Fehlerort.
    ///
    /// # Rückgabe
    /// `DiagLocation` mit `layer = None` und dem angegebenen `field_path`.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_agent_dsl::error::DiagLocation;
    ///
    /// let loc = DiagLocation::field("authority.capabilities[0]");
    /// assert_eq!(loc.field_path.as_deref(), Some("authority.capabilities[0]"));
    /// ```
    pub fn field(field_path: impl Into<String>) -> Self {
        Self {
            layer: None,
            field_path: Some(field_path.into()),
        }
    }

    /// Formatiert den Diagnose-Ort als String-Suffix für Fehlermeldungen.
    ///
    /// # Rückgabe
    /// Leerer String wenn beide Felder leer sind; andernfalls ein String wie
    /// `" (layer: Project, field: authority.capabilities[3])"`.
    fn format_suffix(&self) -> String {
        match (&self.layer, &self.field_path) {
            (None, None) => String::new(),
            (Some(l), None) => format!(" (layer: {l})"),
            (None, Some(f)) => format!(" (field: {f})"),
            (Some(l), Some(f)) => format!(" (layer: {l}, field: {f})"),
        }
    }
}

/// Vollständige Fehlerbeschreibung der Harwness Agent Definition DSL.
///
/// Jede Variante enthält alle Informationen, die zur Diagnose des Fehlers
/// notwendig sind. Erbschaftsketten und Quell-Provenienz werden gemäß §20
/// (Diagnostics) der DSL-Spezifikation transportiert.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::error::DslError;
///
/// let err = DslError::Parse("ungültiges TOML".to_owned());
/// assert!(err.to_string().contains("TOML"));
/// ```
#[derive(Debug)]
pub enum DslError {
    /// TOML-Quelldatei konnte nicht geparst werden.
    Parse(String),

    /// Namensraum-ID hat ein ungültiges Format (§5).
    InvalidId {
        /// Die Eingabe, die den Fehler verursacht hat.
        input: String,
        /// Grund, warum die ID ungültig ist.
        reason: &'static str,
    },

    /// Die deklarierte Basisdefinition existiert nicht in den verfügbaren Schichten.
    MissingBase {
        /// ID der Definition, die die Basis referenziert (geboxt, um Enum-Größe zu reduzieren).
        of: Box<DefinitionId>,
        /// Referenz auf die nicht auffindbare Basisdefinition (geboxt).
        referenced: Box<DefinitionRef>,
        /// Optionaler Schicht- und Feldpfad-Kontext (§20).
        location: DiagLocation,
    },

    /// Ein deklariertes Mixin existiert nicht in den verfügbaren Schichten.
    MissingMixin {
        /// ID der Definition, die das Mixin referenziert (geboxt).
        of: Box<DefinitionId>,
        /// Referenz auf das nicht auffindbare Mixin (geboxt).
        referenced: Box<DefinitionRef>,
        /// Optionaler Schicht- und Feldpfad-Kontext (§20).
        location: DiagLocation,
    },

    /// Eine Vererbungskette enthält einen Zyklus (§6).
    InheritanceCycle {
        /// Die vollständige erkannte Zyklusstrecke, einschließlich des wiederholten Startpunkts.
        cycle: Vec<DefinitionId>,
        /// Optionaler Schicht- und Feldpfad-Kontext (§20).
        location: DiagLocation,
    },

    /// Ein Patch versucht, neue Capabilities hinzuzufügen (§7, §22 Invariante 7).
    AuthorityElevation {
        /// ID der Definition, die die Elevation versucht (geboxt).
        of: Box<DefinitionId>,
        /// Capabilities, die unzulässig hinzugefügt werden sollten.
        added_capabilities: Vec<String>,
        /// Schicht- und Feldpfad-Kontext, z. B. `"authority.capabilities[3]"` (§20).
        location: DiagLocation,
    },

    /// Ein Mixin hat eine inkompatible Rolle für den Zielagenten (§6).
    IllegalRoleForMixin {
        /// ID des Mixins mit der falschen Rolle (geboxt).
        mixin: Box<DefinitionId>,
        /// Die inkompatible Rolle des Mixins.
        role: AgentRoleId,
        /// Optionaler Schicht- und Feldpfad-Kontext (§20).
        location: DiagLocation,
    },

    /// Eine unbekannte Merge-Operation wurde angetroffen (§7).
    UnknownMergeOp {
        /// Name der unbekannten Operation.
        name: String,
    },

    /// TOML-Deserialisierungsfehler.
    Toml(String),

    /// Semver-Versionsfehler.
    Semver(String),

    /// I/O-Fehler beim Lesen oder Schreiben einer Definitionsdatei.
    Io {
        /// Pfad der Datei, bei der der Fehler aufgetreten ist.
        path: PathBuf,
        /// Ursächlicher I/O-Fehler.
        source: std::io::Error,
    },
}

/// Alias für `Result<T, DslError>`.
///
/// # Beschreibung
/// Vereinfacht Funktionssignaturen im gesamten Crate.
pub type DslResult<T> = Result<T, DslError>;

impl std::fmt::Display for DslError {
    /// Gibt eine menschenlesbare Fehlerbeschreibung aus.
    ///
    /// # Beschreibung
    /// Jede Variante erzeugt eine vollständige Fehlermeldung ohne internen Jargon.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DslError::Parse(msg) => {
                write!(f, "TOML-Parsing-Fehler: {msg}")
            }
            DslError::InvalidId { input, reason } => {
                write!(f, "Ungültige Definition-ID '{input}': {reason}")
            }
            DslError::MissingBase {
                of,
                referenced,
                location,
            } => {
                write!(
                    f,
                    "Definition '{}' referenziert nicht vorhandene Basis '{}' (Version: {:?}){}",
                    of,
                    referenced.id,
                    referenced.version,
                    location.format_suffix()
                )
            }
            DslError::MissingMixin {
                of,
                referenced,
                location,
            } => {
                write!(
                    f,
                    "Definition '{}' referenziert nicht vorhandenes Mixin '{}' (Version: {:?}){}",
                    of,
                    referenced.id,
                    referenced.version,
                    location.format_suffix()
                )
            }
            DslError::InheritanceCycle { cycle, location } => {
                let path = cycle
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                write!(
                    f,
                    "Zyklus in der Vererbungskette: {path}{}",
                    location.format_suffix()
                )
            }
            DslError::AuthorityElevation {
                of,
                added_capabilities,
                location,
            } => {
                write!(
                    f,
                    "Definition '{}' versucht unzulässig, Capabilities hinzuzufügen: [{}]{}",
                    of,
                    added_capabilities.join(", "),
                    location.format_suffix()
                )
            }
            DslError::IllegalRoleForMixin {
                mixin,
                role,
                location,
            } => {
                write!(
                    f,
                    "Mixin '{}' hat eine inkompatible Rolle '{:?}' für den Zielagenten{}",
                    mixin,
                    role,
                    location.format_suffix()
                )
            }
            DslError::UnknownMergeOp { name } => {
                write!(f, "Unbekannte Merge-Operation: '{name}'")
            }
            DslError::Toml(msg) => {
                write!(f, "TOML-Deserialisierungsfehler: {msg}")
            }
            DslError::Semver(msg) => {
                write!(f, "Semver-Versionsfehler: {msg}")
            }
            DslError::Io { path, source } => {
                write!(f, "I/O-Fehler bei '{}': {source}", path.display())
            }
        }
    }
}

impl std::error::Error for DslError {
    /// Gibt die zugrunde liegende Fehlerursache zurück, sofern vorhanden.
    ///
    /// # Beschreibung
    /// Nur [`DslError::Io`] besitzt eine verkettete Ursache; diagnostische
    /// Varianten wie [`DslError::InheritanceCycle`] tragen ihren Kontext direkt.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DslError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<toml::de::Error> for DslError {
    /// Konvertiert einen TOML-Deserialisierungsfehler in [`DslError::Toml`].
    fn from(e: toml::de::Error) -> Self {
        DslError::Toml(e.to_string())
    }
}

impl From<semver::Error> for DslError {
    /// Konvertiert einen Semver-Fehler in [`DslError::Semver`].
    fn from(e: semver::Error) -> Self {
        DslError::Semver(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ids::{DefinitionId, DefinitionRef};
    use crate::roles::AgentRoleId;

    fn make_id() -> DefinitionId {
        DefinitionId::parse("harwness.agent.test-worker@1").unwrap()
    }

    fn make_ref() -> DefinitionRef {
        DefinitionRef {
            id: DefinitionId::parse("harwness.agent.worker-base@1").unwrap(),
            version: None,
        }
    }

    #[test]
    fn test_display_all_variants() {
        let cases: Vec<DslError> = vec![
            DslError::Parse("fehler beim parsen".to_owned()),
            DslError::InvalidId {
                input: "schlecht".to_owned(),
                reason: "kein @-Zeichen",
            },
            DslError::MissingBase {
                of: Box::new(make_id()),
                referenced: Box::new(make_ref()),
                location: DiagLocation::none(),
            },
            DslError::MissingMixin {
                of: Box::new(make_id()),
                referenced: Box::new(make_ref()),
                location: DiagLocation::none(),
            },
            DslError::InheritanceCycle {
                cycle: vec![make_id(), make_ref().id],
                location: DiagLocation::none(),
            },
            DslError::AuthorityElevation {
                of: Box::new(make_id()),
                added_capabilities: vec!["spawn.child".to_owned()],
                location: DiagLocation::none(),
            },
            DslError::IllegalRoleForMixin {
                mixin: Box::new(make_id()),
                role: AgentRoleId::Worker,
                location: DiagLocation::none(),
            },
            DslError::UnknownMergeOp {
                name: "supermerge".to_owned(),
            },
            DslError::Toml("ungültige Tabelle".to_owned()),
            DslError::Semver("kein semver".to_owned()),
            DslError::Io {
                path: PathBuf::from("/tmp/test.toml"),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "nicht gefunden"),
            },
        ];

        for err in &cases {
            let msg = err.to_string();
            assert!(
                !msg.is_empty(),
                "Display für {:?} sollte nicht leer sein",
                std::mem::discriminant(err)
            );
        }
    }

    #[test]
    fn test_io_has_source() {
        use std::error::Error;
        let err = DslError::Io {
            path: PathBuf::from("/tmp/x.toml"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "kein Zugriff"),
        };
        assert!(err.source().is_some());
    }

    #[test]
    fn test_parse_no_source() {
        use std::error::Error;
        let err = DslError::Parse("fehler".to_owned());
        assert!(err.source().is_none());
    }

    #[test]
    fn test_inheritance_cycle_display_includes_path_and_location() {
        use std::error::Error;

        let err = DslError::InheritanceCycle {
            cycle: vec![make_id(), make_ref().id],
            location: DiagLocation::field("extends"),
        };
        let msg = err.to_string();
        assert!(msg.contains("harwness.agent.test-worker@1"), "got: {msg}");
        assert!(msg.contains("harwness.agent.worker-base@1"), "got: {msg}");
        assert!(msg.contains("extends"), "got: {msg}");
        assert!(err.source().is_none());
    }

    #[test]
    fn test_authority_elevation_display_contains_field_path_when_set() {
        let err = DslError::AuthorityElevation {
            of: Box::new(make_id()),
            added_capabilities: vec!["agent.spawn.child".to_owned()],
            location: DiagLocation::field("authority.capabilities[3]"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("authority.capabilities"),
            "Display should contain 'authority.capabilities', got: {msg}"
        );
    }

    #[test]
    fn test_diag_location_none_produces_no_suffix() {
        let loc = DiagLocation::none();
        assert_eq!(loc.format_suffix(), "");
    }

    #[test]
    fn test_diag_location_field_produces_field_suffix() {
        let loc = DiagLocation::field("authority.capabilities[0]");
        let suffix = loc.format_suffix();
        assert!(
            suffix.contains("authority.capabilities[0]"),
            "got: {suffix}"
        );
    }

    #[test]
    fn test_diag_location_layer_and_field() {
        let loc = DiagLocation {
            layer: Some("Project".to_owned()),
            field_path: Some("authority.capabilities".to_owned()),
        };
        let suffix = loc.format_suffix();
        assert!(suffix.contains("Project"), "got: {suffix}");
        assert!(suffix.contains("authority.capabilities"), "got: {suffix}");
    }

    #[test]
    fn test_missing_base_display_includes_location_when_present() {
        let err = DslError::MissingBase {
            of: Box::new(make_id()),
            referenced: Box::new(make_ref()),
            location: DiagLocation {
                layer: Some("BuiltIn".to_owned()),
                field_path: None,
            },
        };
        let msg = err.to_string();
        assert!(
            msg.contains("BuiltIn"),
            "expected layer in message, got: {msg}"
        );
    }

    #[test]
    fn test_authority_elevation_display_no_field_path_when_none() {
        let err = DslError::AuthorityElevation {
            of: Box::new(make_id()),
            added_capabilities: vec!["x".to_owned()],
            location: DiagLocation::none(),
        };
        let msg = err.to_string();
        // No location suffix should be appended
        assert!(!msg.contains("field:"), "unexpected 'field:' in: {msg}");
        assert!(!msg.contains("layer:"), "unexpected 'layer:' in: {msg}");
    }
}
