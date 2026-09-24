//! Ausgehende Netzverbindungen des Harness unter einer prüfbaren Policy.
//!
//! # Verantwortung
//! Dieses Crate ist die eine Stelle, an der In-Process-Tools (z. B.
//! `web.fetch`) entscheiden, *wohin* sie sich verbinden dürfen (Plan Teil B,
//! Design „Egress ohne Landlock“; Befunde F-168, F-062, F-035):
//!
//! - [`classify`] ordnet jede IP-Adresse einer [`AddrClass`] zu (Loopback,
//!   privat, Link-Local, Cloud-Metadaten, CGNAT, ULA, …). IPv4-gemappte und
//!   NAT64-Adressen werden nach ihrer eingebetteten IPv4-Adresse bewertet,
//!   6to4/Teredo konservativ als [`AddrClass::Reserved`].
//! - [`EgressPolicy`] kombiniert eine Host-Allowlist (Punktgrenzen-Suffixregel
//!   aus [`harw_sandbox::egress::host_matches_suffix`]) mit der
//!   Adressklassen-Prüfung und liefert einen stabilen [`EgressPolicy::digest`].
//! - [`build_client`] baut einen `reqwest::Client`, dessen DNS-Resolver jede
//!   aufgelöste Adresse per [`EgressPolicy::check_addr`] filtert. Der Client
//!   verbindet sich nur mit genau den geprüften Adressen (kein
//!   DNS-Rebinding-Fenster), liest keine Proxy-Umgebungsvariablen und folgt
//!   keinen Redirects.
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
//! [`AddrClass`], [`EgressPolicy`], [`EgressError`], [`EgressUrl`].
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`. Die Policy ist unveränderlich und wird als
//! `Arc<EgressPolicy>` geteilt; der Resolver hält nur einen `Arc`-Klon.
//!
//! # Fehler
//! Alle Ablehnungen und Aufbaufehler sind Varianten von [`EgressError`].
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
    ChildCommand, DEFAULT_RELAY_MAX_CONNECTIONS, EXIT_BIND_FAILED, EXIT_CHILD_FAILED,
    EXIT_CHILD_NOT_EXECUTABLE, EXIT_CHILD_NOT_FOUND, EXIT_USAGE, RELAY_USAGE, RelayConfig,
    RelayDirection, RelayError, RelayReporter, bind_relay, exit_code_from_status, relay_connection,
    run_child, run_relay,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
