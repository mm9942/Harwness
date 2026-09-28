//! Ausgehende Netzverbindungen des Harness unter einer prüfbaren Policy.
//!
//! # Verantwortung
//! Dieses Crate ist die eine Stelle, an der sowohl In-Process-Tools (z. B.
//! `web.fetch`) als auch sandboxed Fremdprozesse (geckodriver/Firefox)
//! entscheiden, *wohin* sie sich verbinden dürfen (Plan Teil B, Design
//! „Egress ohne Landlock“; Befunde F-168, F-062, F-035):
//!
//! - [`classify`] ordnet jede IP-Adresse einer [`AddrClass`] zu (Loopback,
//!   privat, Link-Local, Cloud-Metadaten, CGNAT, ULA, …). IPv4-gemappte und
//!   NAT64-Adressen werden nach ihrer eingebetteten IPv4-Adresse bewertet,
//!   6to4/Teredo konservativ als [`AddrClass::Reserved`].
//! - [`EgressPolicy`] kombiniert eine Host-Allowlist (Punktgrenzen-Suffixregel
//!   aus [`harw_authority::host_matches_suffix`]) mit der
//!   Adressklassen-Prüfung und liefert einen stabilen [`EgressPolicy::digest`].
//! - [`build_client`] baut einen `reqwest::Client`, dessen DNS-Resolver jede
//!   aufgelöste Adresse per [`EgressPolicy::check_resolved`] filtert — für
//!   einen Namen, der nur über das offene öffentliche Web erlaubt ist, nur
//!   [`AddrClass::Public`], sonst wie [`EgressPolicy::check_addr`]. Der Client
//!   verbindet sich nur mit genau den geprüften Adressen (kein
//!   DNS-Rebinding-Fenster), liest keine Proxy-Umgebungsvariablen und folgt
//!   keinen Redirects.
//! - [`EgressProxy`] (Funktion [`serve`]) ist ein SOCKS5-Proxy (RFC 1928) für
//!   Fremdprozesse ohne eigenes Netz (`bwrap --unshare-net`); er setzt
//!   dieselbe [`EgressPolicy`] durch wie [`build_client`] und ist ausschließlich
//!   über einen Unix-Socket erreichbar, nie über TCP.
//! - Das Modul `relay` (Binary `harw-netns-relay`) läuft innerhalb der
//!   netzlosen Namespace des Fremdprozesses und leitet dessen TCP-Verbindungen
//!   Byte für Byte an den Unix-Socket des [`EgressProxy`] weiter. Es enthält
//!   selbst keine Policy-Logik.
//!
//! URL-Parsing und Host-Normalisierung werden aus `harw_sandbox::egress`
//! wiederverwendet ([`EgressUrl`], [`EgressHost`], [`EgressUrlError`]) und hier
//! nur re-exportiert.
//!
//! # Aufrufer-Pflichten
//! - **Jede** URL — auch jedes Redirect-Ziel (`Location`) — läuft vor dem
//!   Senden durch [`EgressPolicy::check_url`]; gesendet wird
//!   [`EgressUrl::as_url`] der geprüften URL. Bei IP-Literalen überspringt der
//!   HTTP-Connector den Resolver; nur `check_url` prüft sie.
//! - Redirects (`3xx`) folgt der Aufrufer manuell, Hop für Hop.
//!
//! # Zentrale Typen
//! [`AddrClass`], [`EgressPolicy`], [`EgressError`], [`EgressUrl`],
//! [`EgressProxy`], [`ProxyLimits`], [`RelayConfig`].
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`. Die Policy ist unveränderlich und wird als
//! `Arc<EgressPolicy>` geteilt; Resolver und Proxy halten nur einen
//! `Arc`-Klon.
//!
//! # Fehler
//! Ablehnungen und Aufbaufehler des In-Process-Clients und des SOCKS5-Proxys
//! sind Varianten von [`EgressError`]; Fehler des Relays (Argumente, Bind,
//! Kindprozess) sind Varianten von [`RelayError`].
//!
//! # Beispiele
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_egress::{EgressPolicy, build_client};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let policy = Arc::new(EgressPolicy::new(vec!["docs.rs".to_owned()], false)?);
//! let url = policy.check_url("https://docs.rs/serde")?;
//! let client = build_client(Arc::clone(&policy))?;
//! let response = client.get(url.as_url().clone()).send().await?;
//! # let _ = response;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod classify;
mod client;
mod error;
mod policy;
mod proxy;
mod relay;

pub use classify::{AddrClass, classify};
pub use client::build_client;
pub use error::EgressError;
pub use harw_sandbox::{EgressHost, EgressUrl, EgressUrlError};
pub use policy::EgressPolicy;
pub use proxy::{EgressProxy, ProxyLimits, serve};
pub use relay::{
    ChildCommand, DEFAULT_RELAY_IDLE_BUDGET, DEFAULT_RELAY_IDLE_POLL,
    DEFAULT_RELAY_MAX_CONNECTIONS, EXIT_BIND_FAILED, EXIT_CHILD_FAILED, EXIT_CHILD_NOT_EXECUTABLE,
    EXIT_CHILD_NOT_FOUND, EXIT_USAGE, RELAY_USAGE, RelayConfig, RelayDirection, RelayError,
    RelayReporter, bind_relay, exit_code_from_status, relay_connection, run_child, run_relay,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use crate::test_support::TestResult;

    // Re-Export-Probe: `DEFAULT_RELAY_IDLE_POLL` und `DEFAULT_RELAY_IDLE_BUDGET`
    // müssen am Crate-Root ankommen, sonst können Aufrufer die in
    // `relay_connection`s Doku genannten Standardwerte nicht referenzieren; das
    // Budget muss länger sein als eine einzelne Lesefrist (Sibling
    // `DEFAULT_RELAY_MAX_CONNECTIONS` diente als Vorbild).
    #[test]
    fn test_crate_root_reexports_relay_idle_defaults() -> TestResult {
        let idle_poll = crate::DEFAULT_RELAY_IDLE_POLL;
        let idle_budget = crate::DEFAULT_RELAY_IDLE_BUDGET;
        let max_connections = crate::DEFAULT_RELAY_MAX_CONNECTIONS;
        assert!(idle_poll.as_millis() > 0, "{idle_poll:?}");
        assert!(idle_budget > idle_poll, "{idle_budget:?} <= {idle_poll:?}");
        assert!(max_connections > 0, "{max_connections}");
        Ok(())
    }
}
