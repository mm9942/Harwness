//! Konfiguration eines [`crate::OtlpSink`]: Endpunkt, Header,
//! Stapel- und Puffergrößen.
//!
//! # Verantwortungsbereich
//! Trägt [`OtlpConfig`] und [`HeaderEntry`]. Reine Werttypen ohne
//! Verhalten — `endpoint` und `headers` werden von
//! [`crate::HttpTransport`] gelesen (siehe dessen Moduldoc): `endpoint`
//! muss mit `http://` beginnen (kein TLS, siehe dort Abschnitt „TLS").
//! [`crate::RecordingTransport`] liest beide Felder nicht.
//!
//! # Nebenläufigkeit
//! Beide Typen sind `Clone` und ohne Interior Mutability — beliebig
//! zwischen Threads teilbar.
//!
//! # Fehler
//! Keine eigenen — `serde_json::Error`/`toml::de::Error` einer
//! fehlschlagenden Deserialisierung entstehen an der Aufrufstelle, nicht in
//! dieser Crate.
//!
//! # Examples
//! ```
//! use harw_observe_otlp::OtlpConfig;
//!
//! let json = r#"{
//!     "endpoint": "http://127.0.0.1:4318/v1/metrics",
//!     "resource_service_name": "harw-sentinel"
//! }"#;
//! let config: OtlpConfig = serde_json::from_str(json).unwrap();
//! assert_eq!(config.batch_size, 100);
//! assert_eq!(config.max_buffer, 10_000);
//! ```

use serde::{Deserialize, Serialize};

fn default_batch_size() -> usize {
    100
}

fn default_max_buffer() -> usize {
    10_000
}

/// Ein einzelner statischer HTTP-Header, den [`crate::HttpTransport`] auf
/// jeden ausgelieferten Stapel anwendet.
///
/// # Description
/// Ein `(String, String)`-Tupel wäre in TOML unhandlich zu schreiben
/// (`[["Authorization", "Bearer x"]]`); dieser benannte Typ liest sich in
/// Konfigurationsdateien als `[[headers]]` `name = "..."` `value = "..."`.
/// `deny_unknown_fields` (K19), weil dieser Typ ausschließlich aus von außen
/// kommender Konfiguration entsteht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderEntry {
    /// Der Headername, z. B. `"Authorization"`.
    pub name: String,
    /// Der Headerwert, z. B. `"Bearer <token>"`.
    pub value: String,
}

/// Konfiguration eines [`crate::OtlpSink`].
///
/// # Description
/// `deny_unknown_fields` (K19): ein Tippfehler in einem Feldnamen einer
/// Konfigurationsdatei soll beim Laden auffallen, nicht als stillschweigend
/// ignoriertes Feld verschwinden. `batch_size` und `max_buffer` tragen
/// Voreinstellungen (100 bzw. 10 000), damit eine minimale Konfiguration nur
/// `endpoint` und `resource_service_name` nennen muss.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtlpConfig {
    /// Die Ziel-URL für `POST .../v1/metrics`, z. B.
    /// `"http://127.0.0.1:4318/v1/metrics"`. Muss mit `http://` beginnen —
    /// [`crate::HttpTransport::new`] verweigert `https://` (siehe dessen
    /// Moduldoc, Abschnitt „TLS"). Von [`crate::RecordingTransport`] nicht
    /// gelesen (siehe Moduldoc).
    pub endpoint: String,

    /// Statische Header für jede Zustellung (z. B. eine
    /// Authentifizierung). Leer in der Voreinstellung. Von
    /// [`crate::HttpTransport`] auf jeden Stapel angewendet; von
    /// [`crate::RecordingTransport`] nicht gelesen (siehe Moduldoc).
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,

    /// Höchstzahl an Datenpunkten je zugestelltem Stapel.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Obergrenze noch nicht zugestellter, gepufferter Datenpunkte. Ein
    /// eingehender Messpunkt wird verworfen und gezählt
    /// (`OtlpSink::buffer_overflow_dropped_count`), sobald der Puffer diese
    /// Grenze erreicht — siehe Crate-Doc, Abschnitt „Puffern und Zustellen".
    #[serde(default = "default_max_buffer")]
    pub max_buffer: usize,

    /// Wert des `service.name`-Ressourcenattributs, das jedem exportierten
    /// Stapel beiliegt (siehe `crate::schema`).
    pub resource_service_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_deserialize_minimal_config_applies_defaults() -> TestResult {
        let json = r#"{
            "endpoint": "http://127.0.0.1:4318/v1/metrics",
            "resource_service_name": "harw-sentinel"
        }"#;
        let config: OtlpConfig =
            serde_json::from_str(json).map_err(ctx("Config deserialisieren"))?;
        assert_eq!(config.endpoint, "http://127.0.0.1:4318/v1/metrics");
        assert_eq!(config.resource_service_name, "harw-sentinel");
        assert!(config.headers.is_empty());
        assert_eq!(config.batch_size, 100);
        assert_eq!(config.max_buffer, 10_000);
        Ok(())
    }

    #[test]
    fn test_deserialize_rejects_unknown_field() {
        let json = r#"{
            "endpoint": "http://127.0.0.1:4318/v1/metrics",
            "resource_service_name": "harw-sentinel",
            "not_a_real_field": true
        }"#;
        let result: Result<OtlpConfig, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_header_entry_deserialize_rejects_unknown_field() {
        let json = r#"{"name": "Authorization", "value": "x", "extra": "y"}"#;
        let result: Result<HeaderEntry, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_full_config_round_trips_through_serialize() -> TestResult {
        let config = OtlpConfig {
            endpoint: "http://127.0.0.1:4318/v1/metrics".to_owned(),
            headers: vec![HeaderEntry {
                name: "Authorization".to_owned(),
                value: "Bearer secret".to_owned(),
            }],
            batch_size: 50,
            max_buffer: 500,
            resource_service_name: "harw-sentinel".to_owned(),
        };
        let json = serde_json::to_string(&config).map_err(ctx("Config serialisieren"))?;
        let back: OtlpConfig =
            serde_json::from_str(&json).map_err(ctx("Config deserialisieren"))?;
        assert_eq!(config, back);
        Ok(())
    }
}
