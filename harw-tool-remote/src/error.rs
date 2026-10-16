//! Fehler beim Aufbau des entfernten Werkzeugsatzes.
//!
//! Ein fehlgeschlagener oder abgelehnter **Aufruf** ist kein Fehler dieses
//! Typs: er wird zu einer `ToolOutput::Error`, damit das Modell reagieren
//! kann (Vertrag R18 §2.4/§4.2).

use harw_macros::HarwError;
use harw_protocol::session_port::PortError;

/// Warum der entfernte Werkzeugsatz nicht aufgebaut werden konnte.
#[derive(Debug, Clone, PartialEq, Eq, HarwError)]
pub enum RemoteToolError {
    /// `tool.list` wurde abgelehnt oder die Verbindung schlug fehl.
    #[msg("gateway tool.list failed: {0}")]
    List(#[source] PortError),
    /// Ein Deskriptor des Gateways ist unbrauchbar (fail closed: der ganze
    /// Satz wird verworfen statt still ein Werkzeug auszulassen).
    #[msg("gateway tool descriptor '{tool}' is invalid: {reason}")]
    InvalidDescriptor {
        /// Name des Werkzeugs, wie das Gateway ihn meldete.
        tool: String,
        /// Was an ihm falsch ist.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn display_and_source_are_stable() {
        let list = RemoteToolError::List(PortError::NotFound);
        assert_eq!(
            list.to_string(),
            format!("gateway tool.list failed: {}", PortError::NotFound)
        );
        assert!(list.source().is_some());
        let bad = RemoteToolError::InvalidDescriptor {
            tool: "t".to_owned(),
            reason: "r".to_owned(),
        };
        assert_eq!(bad.to_string(), "gateway tool descriptor 't' is invalid: r");
        assert!(bad.source().is_none());
    }
}
