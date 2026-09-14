//! Netz-Scope der Rolle `researcher-web` — ausschließlich aus
//! `[network].researcher_web_hosts`.
//!
//! Spezifikationsquelle: Plan W5 RD, Annahme A5; Vertrag `NetworkSection`
//! (ledger W3/C-CFG), `EgressPolicy` (ledger W3/C-EGRESS).
//!
//! # Verantwortlichkeit
//! Dieses Modul übersetzt die Konfiguration in die Egress-Autorität der
//! einzigen eingebauten Rolle mit Netz:
//! - [`researcher_web_policy`] baut die `harw_egress::EgressPolicy`, die die
//!   Web-Werkzeuge der Rolle bekommen (`Arc`, geteilt über alle drei Tools).
//! - [`researcher_web_network_scope`] leitet daraus den `NetworkScope` der
//!   Kind-Sandbox ab — aus der **kanonisierten** Allowlist der Policy, damit
//!   Sandbox und Client dieselbe Hostmenge sehen.
//!
//! Die Registrierung der Web-Werkzeuge **mit** dieser Policy liegt nicht hier:
//! `harw_tool_web::WebToolProvider` hat (Stand W5 RD) noch keinen Konstruktor,
//! der eine Policy annimmt (N-WEB parallel). Das Durchreichen ist
//! Folgearbeit W6 I-CONTRIB.
//!
//! # Regeln
//! - Quelle ist **nur** `researcher_web_hosts`; `allow_hosts` (die allgemeine
//!   Harness-Liste) fließt nicht ein.
//! - Leere Liste = kein Netz: die Policy lässt dann kein Ziel zu
//!   (`EgressPolicy` mit leerer Allowlist, fail-closed).
//! - `allow_private` ist für diese Rolle immer `false`, unabhängig von
//!   `[network].allow_private`: eine Web-Recherche braucht keine Ziele in
//!   privaten Netzen, und Loopback/LAN wären genau der Exfiltrations- bzw.
//!   SSRF-Weg, den A5 schließt.
//!
//! # Fehler
//! [`crate::RegistryDefaultsError::ResearcherWebPolicy`] bei einem ungültigen
//! Host-Eintrag.
//!
//! # Nebenläufigkeit
//! Rein; die Policy ist unveränderlich und wird als `Arc` geteilt.
//!
//! # Beispiele
//! ```rust
//! use harw_config::NetworkSection;
//! use harw_registry_defaults::research_web::researcher_web_policy;
//!
//! let network = NetworkSection {
//!     researcher_web_hosts: vec!["docs.rs".to_owned()],
//!     ..NetworkSection::default()
//! };
//! let policy = researcher_web_policy(&network)?;
//! assert!(policy.check_url("https://docs.rs/serde").is_ok());
//! assert!(policy.check_url("https://example.com/").is_err());
//! # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
//! ```

use std::sync::Arc;

use harw_config::NetworkSection;
use harw_egress::EgressPolicy;
use harw_sandbox::NetworkScope;

use crate::error::{RegistryDefaultsError, RegistryDefaultsResult};

/// Baut die Egress-Policy der Rolle `researcher-web`.
///
/// # Beschreibung
/// Allowlist = `network.researcher_web_hosts`, `allow_private = false`. Die
/// Hosteinträge normalisiert und prüft `EgressPolicy::new`
/// (`harw-egress/src/policy.rs:91`).
///
/// # Argumente
/// - `network` (`&NetworkSection`): die aufgelöste `[network]`-Sektion; nur
///   geliehen.
///
/// # Rückgabe
/// `Ok(Arc<EgressPolicy>)` — bei leerer Liste eine Policy, die kein Ziel zulässt.
///
/// # Fehler
/// - [`RegistryDefaultsError::ResearcherWebPolicy`]: ein Eintrag ist kein reiner
///   Hostname bzw. keine IP (Schema, Port, Userinfo, Wildcard, leeres Label …).
///
/// # Nebenläufigkeit
/// Rein; von jedem Thread aus sicher.
///
/// # Beispiele
/// ```rust
/// use harw_config::NetworkSection;
/// use harw_registry_defaults::research_web::researcher_web_policy;
///
/// let policy = researcher_web_policy(&NetworkSection::default())?;
/// assert!(policy.allow_hosts().is_empty());
/// assert!(policy.check_url("https://docs.rs/").is_err());
/// # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
/// ```
pub fn researcher_web_policy(
    network: &NetworkSection,
) -> RegistryDefaultsResult<Arc<EgressPolicy>> {
    let policy = EgressPolicy::new(network.researcher_web_hosts.to_vec(), false)
        .map_err(|source| RegistryDefaultsError::ResearcherWebPolicy { source })?;
    Ok(Arc::new(policy))
}

/// Leitet den `NetworkScope` der `researcher-web`-Sandbox aus ihrer Policy ab.
///
/// # Beschreibung
/// Nimmt die kanonisierten Einträge aus `EgressPolicy::allow_hosts`, nicht die
/// Rohkonfiguration — Sandbox-Scope und Egress-Client beruhen damit auf
/// derselben, bereits validierten Hostmenge.
///
/// # Argumente
/// - `policy` (`&EgressPolicy`): Ergebnis von [`researcher_web_policy`].
///
/// # Rückgabe
/// Ein `NetworkScope` mit genau den Hosts der Policy; leer, wenn die Policy
/// leer ist.
///
/// # Nebenläufigkeit
/// Rein.
///
/// # Beispiele
/// ```rust
/// use harw_config::NetworkSection;
/// use harw_registry_defaults::research_web::{
///     researcher_web_network_scope, researcher_web_policy,
/// };
///
/// let network = NetworkSection {
///     researcher_web_hosts: vec!["Docs.RS".to_owned()],
///     ..NetworkSection::default()
/// };
/// let policy = researcher_web_policy(&network)?;
/// let scope = researcher_web_network_scope(&policy);
/// assert_eq!(scope.hosts().collect::<Vec<_>>(), vec!["docs.rs"]);
/// # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
/// ```
#[must_use]
pub fn researcher_web_network_scope(policy: &EgressPolicy) -> NetworkScope {
    NetworkScope::from_hosts(policy.allow_hosts().iter().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(allow_hosts: &[&str], allow_private: bool, researcher: &[&str]) -> NetworkSection {
        NetworkSection {
            allow_hosts: allow_hosts.iter().map(|host| (*host).to_owned()).collect(),
            allow_private,
            researcher_web_hosts: researcher.iter().map(|host| (*host).to_owned()).collect(),
        }
    }

    #[test]
    fn test_researcher_web_policy_empty_hosts_means_no_network() {
        let policy =
            researcher_web_policy(&section(&[], false, &[])).expect("leere Liste ist gültig");
        assert!(policy.allow_hosts().is_empty());
        assert!(policy.check_url("https://docs.rs/").is_err());
        assert!(researcher_web_network_scope(&policy).is_empty());
    }

    #[test]
    fn test_researcher_web_policy_ignores_general_allow_hosts() {
        let policy = researcher_web_policy(&section(&["docs.rs", "example.com"], false, &[]))
            .expect("gültig");
        assert!(
            policy.check_url("https://docs.rs/").is_err(),
            "allow_hosts darf den Scope von researcher-web nicht erweitern"
        );
    }

    #[test]
    fn test_researcher_web_policy_never_allows_private_networks() {
        let policy = researcher_web_policy(&section(&[], true, &["docs.rs", "127.0.0.1"]))
            .expect("gültig");
        assert!(!policy.allow_private());
        assert!(policy.check_url("https://docs.rs/serde").is_ok());
        assert!(
            policy.check_url("http://127.0.0.1/").is_err(),
            "Loopback bleibt gesperrt, auch wenn [network].allow_private = true"
        );
    }

    #[test]
    fn test_researcher_web_policy_rejects_invalid_entry() {
        let error = researcher_web_policy(&section(&[], false, &["https://docs.rs"]))
            .expect_err("Schema im Hosteintrag ist ungültig");
        assert!(matches!(error, RegistryDefaultsError::ResearcherWebPolicy { .. }));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn test_researcher_web_network_scope_matches_policy_hosts() {
        let policy = researcher_web_policy(&section(&[], false, &["crates.io", "Docs.RS"]))
            .expect("gültig");
        let scope = researcher_web_network_scope(&policy);
        let hosts: Vec<&str> = scope.hosts().collect();
        assert_eq!(hosts, vec!["crates.io", "docs.rs"]);
        assert!(scope.allows("docs.rs"));
        assert!(!scope.allows("example.com"));
    }
}
