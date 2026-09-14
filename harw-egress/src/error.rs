//! Fehlertyp des Egress-Crates.
//!
//! # Verantwortung
//! [`EgressError`] beschreibt jede Ablehnung durch [`crate::EgressPolicy`] und
//! jeden Aufbaufehler von [`crate::build_client`]. Die `Display`-Texte
//! enthalten nur normalisierte Hosts, Adressen und Allowlist-Einträge aus der
//! Konfiguration — nie die rohe URL-Eingabe, die Userinfo tragen kann.
//!
//! # Nebenläufigkeit
//! `EgressError` ist `Send + Sync + 'static`; der Resolver reicht ihn als
//! `Box<dyn Error + Send + Sync>` an `reqwest` weiter, Aufrufer finden ihn über
//! die `source()`-Kette wieder.

use std::fmt;
use std::net::IpAddr;

use harw_sandbox::egress::EgressUrlError;

use crate::classify::AddrClass;

/// Ablehnungs- und Aufbaufehler des Egress-Crates.
///
/// # Beschreibung
/// Jede Variante trägt den Kontext, der zum Verständnis nötig ist (Host,
/// Adresse, Adressklasse, Allowlist-Eintrag), aber keine Roh-URL.
#[derive(Debug)]
pub enum EgressError {
    /// Die URL ist für ausgehende Verbindungen unzulässig (Parser, Schema,
    /// Userinfo, Host); Details in [`EgressUrlError`].
    InvalidUrl(EgressUrlError),
    /// Ein Allowlist-Eintrag aus der Konfiguration ist kein gültiger Host.
    InvalidAllowEntry {
        /// Der Eintrag wie übergeben.
        entry: String,
        /// Warum der Eintrag abgelehnt wurde.
        reason: &'static str,
    },
    /// Der (normalisierte) Host passt auf keinen Allowlist-Eintrag.
    HostNotAllowed {
        /// Normalisierter Host.
        host: String,
    },
    /// Der Host ist ein lokaler Name (`localhost`, `*.localhost`), und die
    /// Policy erlaubt keine privaten Ziele.
    LocalHostName {
        /// Normalisierter Host.
        host: String,
    },
    /// Die Adresse gehört zu einer Klasse, die die Policy nicht erlaubt.
    AddressDenied {
        /// Die geprüfte Adresse (bei IPv4-gemappt/NAT64 die volle IPv6-Form).
        addr: IpAddr,
        /// Ihre Klassifikation.
        class: AddrClass,
    },
    /// Die DNS-Auflösung lieferte keine zulässige Adresse.
    NoPermittedAddress {
        /// Der aufgelöste Host.
        host: String,
        /// Alle verworfenen Adressen mit ihrer Klasse (leer, wenn die
        /// Auflösung gar keine Adresse lieferte).
        denied: Vec<(IpAddr, AddrClass)>,
    },
    /// Die DNS-Auflösung selbst schlug fehl.
    Lookup {
        /// Der aufzulösende Host.
        host: String,
        /// Ursache aus dem System-Resolver.
        source: std::io::Error,
    },
    /// `reqwest::ClientBuilder::build` schlug fehl (z. B. TLS-Backend).
    ClientBuild(reqwest::Error),
}

impl fmt::Display for EgressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(err) => write!(f, "Egress-URL abgelehnt: {err}"),
            Self::InvalidAllowEntry { entry, reason } => {
                write!(f, "ungültiger Egress-Allowlist-Eintrag {entry:?}: {reason}")
            }
            Self::HostNotAllowed { host } => {
                write!(f, "Host {host} ist nicht in der Egress-Allowlist")
            }
            Self::LocalHostName { host } => write!(
                f,
                "Host {host} bezeichnet die eigene Maschine; private Ziele sind nicht erlaubt"
            ),
            Self::AddressDenied { addr, class } => {
                write!(f, "Zieladresse {addr} ({class}) ist für Egress nicht erlaubt")
            }
            Self::NoPermittedAddress { host, denied } => {
                if denied.is_empty() {
                    write!(f, "DNS-Auflösung von {host} lieferte keine Adresse")
                } else {
                    write!(f, "DNS-Auflösung von {host} lieferte nur unzulässige Adressen: ")?;
                    for (index, (addr, class)) in denied.iter().enumerate() {
                        if index > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{addr} ({class})")?;
                    }
                    Ok(())
                }
            }
            Self::Lookup { host, source } => {
                write!(f, "DNS-Auflösung von {host} fehlgeschlagen: {source}")
            }
            Self::ClientBuild(err) => write!(f, "Egress-HTTP-Client nicht baubar: {err}"),
        }
    }
}

impl std::error::Error for EgressError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidUrl(err) => Some(err),
            Self::Lookup { source, .. } => Some(source),
            Self::ClientBuild(err) => Some(err),
            Self::InvalidAllowEntry { .. }
            | Self::HostNotAllowed { .. }
            | Self::LocalHostName { .. }
            | Self::AddressDenied { .. }
            | Self::NoPermittedAddress { .. } => None,
        }
    }
}

impl From<EgressUrlError> for EgressError {
    fn from(err: EgressUrlError) -> Self {
        Self::InvalidUrl(err)
    }
}

impl From<reqwest::Error> for EgressError {
    fn from(err: reqwest::Error) -> Self {
        Self::ClientBuild(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn test_display_address_denied_names_address_and_class() {
        let err = EgressError::AddressDenied {
            addr: "169.254.169.254".parse().unwrap(),
            class: AddrClass::CloudMetadata,
        };
        let text = err.to_string();
        assert!(text.contains("169.254.169.254"), "{text}");
        assert!(text.contains("cloud-metadata"), "{text}");
    }

    #[test]
    fn test_display_no_permitted_address_lists_denied() {
        let err = EgressError::NoPermittedAddress {
            host: "rebind.example".to_owned(),
            denied: vec![
                ("10.0.0.1".parse().unwrap(), AddrClass::Private),
                ("::1".parse().unwrap(), AddrClass::Loopback),
            ],
        };
        assert_eq!(
            err.to_string(),
            "DNS-Auflösung von rebind.example lieferte nur unzulässige Adressen: \
             10.0.0.1 (private), ::1 (loopback)"
        );
        let empty = EgressError::NoPermittedAddress {
            host: "x.example".to_owned(),
            denied: Vec::new(),
        };
        assert_eq!(empty.to_string(), "DNS-Auflösung von x.example lieferte keine Adresse");
    }

    #[test]
    fn test_from_egress_url_error_keeps_source() {
        let err = EgressError::from(EgressUrlError::UserinfoPresent);
        assert!(matches!(err, EgressError::InvalidUrl(EgressUrlError::UserinfoPresent)));
        assert!(err.source().is_some());
    }
}
