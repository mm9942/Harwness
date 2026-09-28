//! Gemeinsame Infrastruktur-Metafläche: `GET /v1/health`, `/v1/version`,
//! `/v1/capabilities` (Crypto-Masterplan v2 §38, H9).
//!
//! # Wire-Vertrag (JSON, `application/json`)
//! Identisch zu dem, was `harw-infra-client` (`src/info.rs`) parst:
//!
//! ```text
//! GET /v1/health        200 {"status": "ok", "service": "harw-control"}
//! GET /v1/version       200 {"service": "harw-control", "version": "<semver>", "protocol": 1}
//! GET /v1/capabilities  200 {"service": "harw-control", "protocol": 1,
//!                            "operations": ["<registrierte Web-Operation>", …]}
//! ```
//!
//! `operations` sind die Namen aller Operationen, die in der
//! [`WebRouteTable`] mindestens eine `Surface::Web`-Route haben — sortiert und
//! ohne Duplikate. Die Liste ist **beschreibend, keine Autorität**: sie sagt
//! nichts darüber, ob der anfragende Peer eine Operation aufrufen darf; das
//! entscheidet weiterhin [`crate::router::decide_resolved_route`] — der
//! Prüfpfad, den der Server tatsächlich läuft ([`crate::router::decide_route`]
//! ist derselbe Kern, nur mit einem [`crate::authz::PeerAuthorizer`] statt
//! einer bereits aufgelösten Identität).
//!
//! # Autorisierung
//! - `/v1/health` braucht **keine** Berechtigungsstufe: jeder, der den Socket
//!   per `connect(2)` erreicht (Dateimodus/Gruppe des Sockets), darf die
//!   Lebendigkeit abfragen — wie bei `harw-auth-hub`. `harw-netsec` ist hier
//!   *kein* Vorbild: dessen Server lehnt jede Anfrage eines nicht per Uid
//!   erlaubten Peers ab, auch `/v1/health` (`403 peer_not_allowed`, noch vor
//!   der Dispatch-Entscheidung).
//! - `/v1/version` und `/v1/capabilities` verlangen die **niedrigste** Stufe,
//!   die die Routentabelle kennt ([`MINIMUM_DESCRIPTIVE_TIER`] =
//!   [`PermissionTier::Observer`]), aufgelöst über denselben
//!   [`crate::identity::LocalPeerIdentityResolver`] wie jede Route (der
//!   Server ruft ihn für die Metaflächen über denselben `resolve_identity`-
//!   Pfad wie für den Router). Ein unbekannter Peer bekommt
//!   `403 unknown_peer` — dieselbe Regel wie für `GET /events`.
//!
//! # Reservierte Pfade
//! Die Metapfade und `/events` werden vom Server vor der Routentabelle
//! beantwortet. Damit keine Operation still verschattet wird, lehnt
//! [`WebRouteTable::from_registry`] eine `Surface::Web`-Deklaration auf einem
//! dieser Pfade ab ([`is_reserved_path`]).

use std::collections::BTreeSet;

use harw_operations::operation::PermissionTier;

use crate::router::WebRouteTable;

/// Dienstname, den `/v1/health`, `/v1/version` und `/v1/capabilities` melden.
///
/// Die Web-Fläche ist die Kontrollebene (`control.sock`, Masterplan §6.1);
/// der Name entspricht der systemd-Unit `harw-control.service`.
pub const SERVICE_NAME: &str = "harw-control";

/// Protokollgeneration der Metafläche. Muss dem `protocol`-Wert entsprechen,
/// den `harw-infra-client` für die §38-Fläche dokumentiert (`1`).
pub const PROTOCOL_VERSION: u32 = 1;

/// Gesundheitszustand; die Web-Fläche kennt keinen `degraded`-Modus.
pub const HEALTH_STATUS: &str = "ok";

/// Pfad der Lebendigkeitsabfrage.
pub const HEALTH_PATH: &str = "/v1/health";
/// Pfad der Versionsabfrage.
pub const VERSION_PATH: &str = "/v1/version";
/// Pfad der Fähigkeitsabfrage.
pub const CAPABILITIES_PATH: &str = "/v1/capabilities";

/// Niedrigste Stufe der Routentabelle — Voraussetzung für `/v1/version` und
/// `/v1/capabilities`.
pub const MINIMUM_DESCRIPTIVE_TIER: PermissionTier = PermissionTier::Observer;

/// Eine der drei Metarouten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaRoute {
    /// `GET /v1/health`.
    Health,
    /// `GET /v1/version`.
    Version,
    /// `GET /v1/capabilities`.
    Capabilities,
}

impl MetaRoute {
    /// Ordnet einen exakten Pfad einer Metaroute zu.
    #[must_use]
    pub fn from_path(path: &str) -> Option<Self> {
        match path {
            HEALTH_PATH => Some(Self::Health),
            VERSION_PATH => Some(Self::Version),
            CAPABILITIES_PATH => Some(Self::Capabilities),
            _ => None,
        }
    }

    /// Die Stufe, die ein Peer für diese Route mindestens braucht — `None`
    /// heißt: keine Autorisierung nötig (nur `/v1/health`).
    #[must_use]
    pub fn required_tier(self) -> Option<PermissionTier> {
        match self {
            Self::Health => None,
            Self::Version | Self::Capabilities => Some(MINIMUM_DESCRIPTIVE_TIER),
        }
    }

    /// Das JSON-Dokument dieser Route.
    #[must_use]
    pub fn document(self, routes: &WebRouteTable) -> serde_json::Value {
        match self {
            Self::Health => health_document(),
            Self::Version => version_document(),
            Self::Capabilities => capabilities_document(routes),
        }
    }
}

/// Ob `path` vom Server selbst beantwortet wird und daher keiner Operation
/// gehören darf (Metapfade und der SSE-Strom).
#[must_use]
pub fn is_reserved_path(path: &str) -> bool {
    MetaRoute::from_path(path).is_some() || path == crate::server::EVENTS_PATH
}

/// `{"status": "ok", "service": "harw-control"}`.
#[must_use]
pub fn health_document() -> serde_json::Value {
    serde_json::json!({
        "status": HEALTH_STATUS,
        "service": SERVICE_NAME,
    })
}

/// `{"service", "version", "protocol"}`.
#[must_use]
pub fn version_document() -> serde_json::Value {
    serde_json::json!({
        "service": SERVICE_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL_VERSION,
    })
}

/// `{"service", "protocol", "operations"}` — `operations` sortiert und
/// dedupliziert aus der Routentabelle.
#[must_use]
pub fn capabilities_document(routes: &WebRouteTable) -> serde_json::Value {
    let operations: BTreeSet<&str> = routes.iter().map(|route| route.operation_name()).collect();
    serde_json::json!({
        "service": SERVICE_NAME,
        "protocol": PROTOCOL_VERSION,
        "operations": operations.into_iter().collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_operations::registry::OperationRegistry;

    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_meta_paths_map_to_routes_and_nothing_else() {
        assert_eq!(MetaRoute::from_path("/v1/health"), Some(MetaRoute::Health));
        assert_eq!(
            MetaRoute::from_path("/v1/version"),
            Some(MetaRoute::Version)
        );
        assert_eq!(
            MetaRoute::from_path("/v1/capabilities"),
            Some(MetaRoute::Capabilities)
        );
        assert_eq!(MetaRoute::from_path("/v1/health/"), None);
        assert_eq!(MetaRoute::from_path("/v1"), None);
    }

    #[test]
    fn test_only_health_is_unauthenticated() {
        assert_eq!(MetaRoute::Health.required_tier(), None);
        assert_eq!(
            MetaRoute::Version.required_tier(),
            Some(PermissionTier::Observer)
        );
        assert_eq!(
            MetaRoute::Capabilities.required_tier(),
            Some(PermissionTier::Observer)
        );
    }

    #[test]
    fn test_reserved_paths_cover_meta_and_events() {
        for path in ["/v1/health", "/v1/version", "/v1/capabilities", "/events"] {
            assert!(is_reserved_path(path), "{path}");
        }
        assert!(!is_reserved_path("/api/x"));
    }

    #[test]
    fn test_documents_match_the_infra_client_contract() -> TestResult {
        let health = health_document();
        assert_eq!(health["status"], "ok");
        assert_eq!(health["service"], SERVICE_NAME);

        let version = version_document();
        assert_eq!(version["service"], SERVICE_NAME);
        assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(version["protocol"], PROTOCOL_VERSION);

        let routes = WebRouteTable::from_registry(&OperationRegistry::new())
            .map_err(ctx("leere Routentabelle baubar"))?;
        let caps = capabilities_document(&routes);
        assert_eq!(caps["service"], SERVICE_NAME);
        assert_eq!(caps["protocol"], PROTOCOL_VERSION);
        assert_eq!(caps["operations"], serde_json::json!([]));
        Ok(())
    }
}
