//! `NetRule`: eine einzelne, inspizierbare Egress-Regel.
//!
//! # Verantwortungsbereich
//! Ein [`NetRule`] ist die Plan-Ebene-Entsprechung genau eines
//! `harw_sandbox::EgressTarget`. Diese Crate übernimmt jede Zielart
//! verlustfrei — keine Zielart wird in eine andere übersetzt, keine wird
//! erweitert (siehe die Moduldoc von `crate` zum `DnsSuffix`-Fall). Die
//! Übersetzung in echte Firewall-Mechanik bleibt Sache eines
//! [`crate::NetBackend`].
//!
//! # Nebenläufigkeit
//! [`NetRule`] ist ein reiner Wert ohne innere Veränderlichkeit: `Send +
//! Sync`, beliebig teilbar, keine Sperren.

use std::net::IpAddr;

use harw_sandbox::EgressTarget;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};

/// Eine einzelne Egress-Regel eines [`crate::NetPlan`].
///
/// # Description
/// Jede Variante entspricht genau einer `EgressTarget`-Variante aus
/// `harw-sandbox`: [`Self::AllowHost`] ↔ `EgressTarget::Host`,
/// [`Self::AllowDnsSuffix`] ↔ `EgressTarget::DnsSuffix`, [`Self::AllowCidr`]
/// ↔ `EgressTarget::Cidr`. Die Zuordnung ist verlustfrei — die `From`-Impl
/// unten kopiert Name bzw. Adressbereich unverändert, ohne Namensauflösung, ohne
/// Erweiterung. Nur [`Self::AllowCidr`] lässt sich direkt von einem
/// paketfilterbasierten Backend vollständig durchsetzen; [`Self::AllowHost`]
/// und [`Self::AllowDnsSuffix`] sind namensbasiert. Siehe die Moduldoc von
/// `crate` für die volle Begründung dieser Entscheidung und ihre Grenzen.
///
/// # Vergleich
/// [`Ord`]/[`PartialOrd`] sind abgeleitet (zuerst nach Variante in
/// Deklarationsreihenfolge, dann nach Inhalt), damit ein `Vec<NetRule>`
/// stabil sortiert werden kann — Voraussetzung für
/// [`crate::plan_for_scope`]s Determinismus-Zusage: gleiche Bereiche ergeben
/// dieselbe Regelreihenfolge, weil die zugrunde liegende
/// `BTreeSet<EgressTarget>` in `harw-sandbox` bereits dieselbe Ordnung trägt.
///
/// # Serialisierung
/// `#[serde(deny_unknown_fields)]` lässt ein unbekanntes Feld in einer
/// Variante beim Deserialisieren fehlschlagen statt es stillschweigend zu
/// verwerfen — fail-closed bei Konfigurationsdrift, konsistent mit
/// [`crate::NetPlan`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum NetRule {
    /// Erlaubt ausgehende Verbindungen zu genau diesem Hostnamen, ohne
    /// Subdomains. Namensbasiert — siehe Typ-Dokumentation.
    AllowHost {
        /// Der exakte, aus dem Bereich ererbte Hostname.
        host: String,
    },
    /// Erlaubt ausgehende Verbindungen zu diesem Namen und allem darunter,
    /// an Punktgrenzen — identische Semantik zu
    /// `harw_sandbox::NetworkScope::allows`. Namensbasiert — siehe
    /// Typ-Dokumentation.
    AllowDnsSuffix {
        /// Das aus dem Bereich ererbte DNS-Suffix.
        suffix: String,
    },
    /// Erlaubt ausgehende Verbindungen zu Adressen in diesem Bereich. Die
    /// einzige Regelart, die ein paketfilterbasiertes Backend direkt und
    /// vollständig durchsetzen kann.
    AllowCidr {
        /// Der aus dem Bereich ererbte Adressbereich.
        cidr: IpNet,
    },
}

impl From<EgressTarget> for NetRule {
    /// Übernimmt ein `harw_sandbox::EgressTarget` unverändert als `NetRule`.
    ///
    /// # Description
    /// Reine, verlustfreie Struktur-Umbenennung: kein Feld wird verändert,
    /// keine Zielart wird in eine andere übersetzt. Diese
    /// Eins-zu-eins-Treue ist die Grundlage dafür, dass
    /// [`crate::plan_for_scope`] nie mehr erlaubt als der Quell-Bereich.
    ///
    /// # Arguments
    /// - `target` (`EgressTarget`): das zu übernehmende Ziel.
    ///
    /// # Returns
    /// Die semantisch entsprechende `NetRule`-Variante.
    fn from(target: EgressTarget) -> Self {
        match target {
            EgressTarget::Host(host) => Self::AllowHost { host },
            EgressTarget::DnsSuffix(suffix) => Self::AllowDnsSuffix { suffix },
            EgressTarget::Cidr(cidr) => Self::AllowCidr { cidr },
        }
    }
}

impl NetRule {
    /// Prüft, ob diese eine Regel den gegebenen (bereits normalisierten)
    /// Hostnamen zulässt.
    ///
    /// # Description
    /// Repliziert exakt die Vergleichsregel von
    /// `harw_sandbox::NetworkScope::allows` für die jeweilige Zielart:
    /// [`Self::AllowHost`] vergleicht case-insensitiv exakt,
    /// [`Self::AllowDnsSuffix`] verlangt einen Treffer an einer Punktgrenze
    /// (siehe [`host_matches`]). [`Self::AllowCidr`] ist nie ein
    /// Host-Treffer. `harw-sandbox` exportiert diese Vergleichsfunktionen
    /// nicht öffentlich; siehe die Moduldoc von `crate` zum daraus
    /// entstehenden Kopplungsrisiko.
    ///
    /// # Arguments
    /// - `needle` (`&str`): bereits über [`normalize_host`] normalisierter
    ///   Hostname.
    ///
    /// # Returns
    /// `true`, wenn diese eine Regel den Host zulässt.
    #[must_use]
    pub(crate) fn allows_host(&self, needle: &str) -> bool {
        match self {
            Self::AllowHost { host } => needle.eq_ignore_ascii_case(host),
            Self::AllowDnsSuffix { suffix } => host_matches(suffix, needle),
            Self::AllowCidr { .. } => false,
        }
    }

    /// Prüft, ob diese eine Regel die gegebene Adresse zulässt.
    ///
    /// # Arguments
    /// - `addr` (`std::net::IpAddr`): die zu prüfende Adresse, IPv4 oder
    ///   IPv6.
    ///
    /// # Returns
    /// `true` nur für [`Self::AllowCidr`], wenn `addr` im Bereich liegt.
    #[must_use]
    pub(crate) fn allows_addr(&self, addr: IpAddr) -> bool {
        match self {
            Self::AllowCidr { cidr } => cidr.contains(&addr),
            Self::AllowHost { .. } | Self::AllowDnsSuffix { .. } => false,
        }
    }
}

/// Kanonische Vergleichsform eines Hostnamens.
///
/// # Description
/// Identische Regel zu `harw_sandbox::NetworkScope`s (nicht exportierter)
/// Normalisierung: Leerzeichen entfernen, ASCII-Kleinschreibung, führende
/// Punkte entfernen. Dupliziert, weil `harw-sandbox` diese Funktion nicht
/// öffentlich macht — siehe die Moduldoc von `crate` zum Kopplungsrisiko.
///
/// # Arguments
/// - `host` (`&str`): der zu normalisierende Hostname.
///
/// # Returns
/// Den normalisierten Hostnamen als neuen `String`.
pub(crate) fn normalize_host(host: &str) -> String {
    host.trim().trim_start_matches('.').to_ascii_lowercase()
}

// Exakter Treffer oder Suffix-Treffer an einer Punktgrenze. `needle` muss
// bereits über `normalize_host` normalisiert sein; `allowed` wird defensiv
// behandelt. Identische Semantik zu harw_sandbox's privatem `host_matches`
// (siehe dortige Begründung der Punktgrenzen-Regel).
fn host_matches(allowed: &str, needle: &str) -> bool {
    if allowed.is_empty() {
        return false;
    }
    if needle.eq_ignore_ascii_case(allowed) {
        return true;
    }
    if needle.len() <= allowed.len() {
        return false;
    }
    let start = needle.len() - allowed.len();
    let Some(suffix) = needle.get(start..) else {
        return false;
    };
    suffix.eq_ignore_ascii_case(allowed) && needle.as_bytes()[start - 1] == b'.'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_egress_target_host_maps_to_allow_host() {
        let target = EgressTarget::Host("api.example.com".to_owned());
        assert_eq!(
            NetRule::from(target),
            NetRule::AllowHost {
                host: "api.example.com".to_owned()
            }
        );
    }

    #[test]
    fn test_from_egress_target_dns_suffix_maps_to_allow_dns_suffix() {
        let target = EgressTarget::DnsSuffix("docs.rs".to_owned());
        assert_eq!(
            NetRule::from(target),
            NetRule::AllowDnsSuffix {
                suffix: "docs.rs".to_owned()
            }
        );
    }

    #[test]
    fn test_from_egress_target_cidr_maps_to_allow_cidr() {
        let net: IpNet = "10.0.0.0/8".parse().expect("valid CIDR literal");
        let target = EgressTarget::Cidr(net);
        assert_eq!(NetRule::from(target), NetRule::AllowCidr { cidr: net });
    }

    #[test]
    fn test_allow_host_rule_matches_only_the_exact_name() {
        let rule = NetRule::AllowHost {
            host: "api.example.com".to_owned(),
        };
        assert!(rule.allows_host("api.example.com"));
        assert!(rule.allows_host("API.EXAMPLE.COM"));
        assert!(!rule.allows_host("sub.api.example.com"));
        assert!(!rule.allows_addr("10.0.0.1".parse().expect("valid IP")));
    }

    #[test]
    fn test_allow_dns_suffix_rule_matches_at_a_dot_boundary() {
        let rule = NetRule::AllowDnsSuffix {
            suffix: "docs.rs".to_owned(),
        };
        assert!(rule.allows_host("docs.rs"));
        assert!(rule.allows_host("static.docs.rs"));
        assert!(!rule.allows_host("evildocs.rs"));
        assert!(!rule.allows_addr("10.0.0.1".parse().expect("valid IP")));
    }

    #[test]
    fn test_allow_cidr_rule_matches_only_addresses_never_hosts() {
        let net: IpNet = "10.0.0.0/8".parse().expect("valid CIDR literal");
        let rule = NetRule::AllowCidr { cidr: net };
        assert!(rule.allows_addr("10.1.2.3".parse().expect("valid IP")));
        assert!(!rule.allows_addr("11.0.0.0".parse().expect("valid IP")));
        assert!(!rule.allows_host("docs.rs"));
    }

    #[test]
    fn test_normalize_host_trims_case_and_leading_dot() {
        assert_eq!(normalize_host(" .Docs.RS "), "docs.rs");
    }
}
