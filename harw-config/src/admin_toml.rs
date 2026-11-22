//! `[admin]` — der Geräte-Enrollment-Webdienst `harw admin serve`.
//!
//! Alle Felder sind **global-only** (Home-Layer): weder ein Profil noch ein
//! nicht vertrauenswürdiges Projekt kann den Dienst verschieben, die öffentliche
//! Adresse ändern oder den Gateway-Neustart schalten. Die öffentliche Adresse
//! steht nie im Code, sondern nur hier (`public_url`).
//!
//! Durchsetzung liegt in `harw-cli` (`admin/`); dieses Modul ist deklarativ.

use serde::{Deserialize, Serialize};

/// Vorgabe-Bind-Adresse: nur Loopback.
pub const DEFAULT_ADMIN_ADDR: &str = "127.0.0.1:7450";

/// `[admin]`.
///
/// # Examples
/// ```rust
/// use harw_config::AdminSection;
///
/// let section: AdminSection = toml::from_str("public_url = \"https://admin.example.org\"").expect("valid");
/// assert_eq!(section.listen, "127.0.0.1:7450");
/// assert!(AdminSection::default().public_url.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSection {
    /// `host:port` des Webdienstes; ausschließlich Loopback erlaubt.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Öffentliche Basis-URL (`https://…`), unter der der Dienst erreichbar ist
    /// (z. B. über einen Cloudflare Tunnel). Alle ausgegebenen Links, Redirects,
    /// Deep-Link-Profile und QR-Codes werden daraus gebaut. `None`: nur die
    /// lokale URL, mit Hinweis.
    #[serde(default)]
    pub public_url: Option<String>,
    /// `host:port`, den das Handy für das Gateway nutzt (Profil-`endpoint`).
    /// `None`: `[session_listener] listen`.
    #[serde(default)]
    pub public_endpoint: Option<String>,
    /// Mandant neu enrollter Geräte. Standard `mobile`.
    #[serde(default = "default_tenant")]
    pub tenant: String,
    /// Startet das Gateway nach einer Freigabe neu (`systemctl --user restart`),
    /// damit der neue Schlüssel-Pin wirkt. Standard `false`.
    #[serde(default)]
    pub restart_gateway: bool,
    /// Name der systemd-User-Unit des Gateways. Standard `harw-gateway.service`.
    #[serde(default = "default_unit")]
    pub gateway_unit: String,
}

fn default_listen() -> String {
    DEFAULT_ADMIN_ADDR.to_owned()
}

fn default_tenant() -> String {
    "mobile".to_owned()
}

fn default_unit() -> String {
    "harw-gateway.service".to_owned()
}

impl Default for AdminSection {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            public_url: None,
            public_endpoint: None,
            tenant: default_tenant(),
            restart_gateway: false,
            gateway_unit: default_unit(),
        }
    }
}
