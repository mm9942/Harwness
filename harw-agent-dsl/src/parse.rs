//! TOML-Parser für Harwness Agent Definitions (§17 DSL-Spec, Schritt "parse").
//!
//! Dieses Modul stellt [`parse_toml`] bereit, die einzige öffentliche Funktion
//! zum Umwandeln eines TOML-Quelltexts in eine [`RawAgentDefinition`].
//!
//! # Verantwortlichkeit
//! Nur syntaktisches Parsing und grundlegende Deserialisierung.
//! Semantische Validierung (ID-Konsistenz, Authority-Prüfung) findet
//! in `resolve.rs` statt.
//!
//! # Nebenläufigkeit
//! `parse_toml` ist zustandslos und thread-sicher.

use crate::error::DslError;
use crate::raw::RawAgentDefinition;

/// Parst eine TOML-Zeichenkette in eine [`RawAgentDefinition`].
///
/// # Beschreibung
/// Führt nur syntaktisches TOML-Parsing und `serde`-Deserialisierung durch.
/// Das Ergebnis ist eine roh geparste Struktur ohne Semantik-Validierung.
///
/// # Argumente
/// - `source` (`&str`): TOML-Quelltext einer Agentendefinition.
///
/// # Rückgabe
/// `Ok(RawAgentDefinition)` bei erfolgreichem Parsing.
///
/// # Fehler
/// - [`DslError::Parse`]: wenn der TOML-Quelltext syntaktisch fehlerhaft ist.
/// - [`DslError::Toml`]: wenn die Deserialisierung in [`RawAgentDefinition`] scheitert.
///
/// # Nebenläufigkeit
/// Zustandslos und thread-sicher.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::parse::parse_toml;
///
/// let src = r#"
/// schema = "harwness.agent/v1"
/// id = "harwness.agent.test@1"
/// version = "1.0.0"
/// role = "worker"
/// specialization = "test"
/// "#;
/// let raw = parse_toml(src).unwrap();
/// assert_eq!(raw.specialization, "test");
/// ```
pub fn parse_toml(source: &str) -> Result<RawAgentDefinition, DslError> {
    // Direkte Deserialisierung; toml::from_str gibt bei Syntaxfehlern DslError::Toml zurück.
    // Eine separate erste Validierungsphase entfällt, da toml::from_str beide Fehlerarten abdeckt.
    toml::from_str::<RawAgentDefinition>(source).map_err(|e| {
        let msg = e.to_string();
        // Syntaxfehler → DslError::Parse; Strukturfehler → DslError::Toml
        if msg.contains("TOML parse error") || msg.contains("unexpected") {
            DslError::Parse(msg)
        } else {
            DslError::Toml(msg)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_invalid_toml_syntax() {
        let bad = "schema = [unclosed";
        let result = parse_toml(bad);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_missing_required_field() {
        // role fehlt → Deserialisierungsfehler
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.test@1"
version = "1.0.0"
specialization = "test"
"#;
        let result = parse_toml(src);
        assert!(result.is_err());
    }
}
