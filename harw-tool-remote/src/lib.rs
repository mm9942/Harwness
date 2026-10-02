//! `harw-tool-remote` — der entfernte Werkzeug-Proxy (R18 D-A).
//!
//! # Beschreibung
//! Ein Agentenprozess, der an ein Gateway angebunden ist, hat **keine**
//! lokalen `ToolProvider`s: er montiert `RegistryProfile::NoTools` plus genau
//! den [`RemoteToolProvider`] dieses Crates (Vertrag
//! `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §1 D-A,
//! §10 P2). Jeder Aufruf geht als `tool.call` über
//! [`harw_protocol::session_port::ToolPort`] an den Gateway-Tool-Host und
//! läuft dort in der gateway-seitigen Sandbox.
//!
//! - [`RemoteToolProvider`] entsteht aus `tool.list`
//!   ([`RemoteToolProvider::connect`]) und bietet exakt die Deskriptoren des
//!   Gateways an — nichts dazu, nichts weg.
//! - [`RemoteToolExecutor`] leitet jeden Aufruf weiter, kopiert die
//!   Platzierung der Ergebnis-Frame in die Fertigmeldung
//!   (`ToolExecutor::execute_placed`) und sendet `tool.cancel` genau einmal,
//!   wenn der Turn abgebrochen wird — sowohl wenn der Ausführer den
//!   `CancelToken` selbst sieht als auch wenn der Turn-Loop das laufende
//!   Future verwirft.
//!
//! # Kein Rückfall (fail closed)
//! Jede Ablehnung (`PortError`, darunter jede `ToolRefusal`) wird zu einer
//! `ToolOutput::Error`, deren Text die Ablehnung benennt. Der Proxy führt
//! nie etwas lokal aus und registriert keinen lokalen Ausführer neben sich.

#![forbid(unsafe_code)]

mod error;
mod executor;
mod provider;

pub use error::RemoteToolError;
pub use executor::{RemoteToolExecutor, refusal_message};
pub use provider::RemoteToolProvider;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
