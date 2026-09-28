//! Fehler beim Aufbau des entfernten Werkzeugsatzes.
//!
//! Ein fehlgeschlagener oder abgelehnter **Aufruf** ist kein Fehler dieses
//! Typs: er wird zu einer `ToolOutput::Error`, damit das Modell reagieren
//! kann (Vertrag R18 §2.4/§4.2).

use std::fmt;

use harw_protocol::session_port::PortError;

/// Warum der entfernte Werkzeugsatz nicht aufgebaut werden konnte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteToolError {
    /// `tool.list` wurde abgelehnt oder die Verbindung schlug fehl.
    List(PortError),
    /// Ein Deskriptor des Gateways ist unbrauchbar (fail closed: der ganze
    /// Satz wird verworfen statt still ein Werkzeug auszulassen).
    InvalidDescriptor {
        /// Name des Werkzeugs, wie das Gateway ihn meldete.
        tool: String,
        /// Was an ihm falsch ist.
        reason: String,
    },
}

impl fmt::Display for RemoteToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List(error) => write!(f, "gateway tool.list failed: {error}"),
            Self::InvalidDescriptor { tool, reason } => {
                write!(f, "gateway tool descriptor '{tool}' is invalid: {reason}")
            }
        }
    }
}

impl std::error::Error for RemoteToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::List(error) => Some(error),
            Self::InvalidDescriptor { .. } => None,
        }
    }
}
