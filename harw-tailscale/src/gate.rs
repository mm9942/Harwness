//! Admission of tailnet peers.
//!
//! # Rules (fail closed)
//! 1. The remote address must be a tailnet address ([`is_tailnet_ip`]).
//! 2. It must not be one of this node's own addresses: a local process that
//!    connects to the node's tailnet IP would otherwise pass as a peer.
//! 3. `tailscaled` must answer `whois` for it with a login.
//!
//! Every tailnet login is admitted (owner decision: the whole tailnet may
//! connect); the login and node are returned for the audit log.

use crate::addr::is_tailnet_ip;
use crate::localapi::{LocalApi, LocalApiError, WhoIs};
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;

/// An admitted tailnet peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetPeer {
    /// Remote address of the connection.
    pub addr: SocketAddr,
    /// Owner login (`whois`).
    pub login: String,
    /// Node name (`whois`).
    pub node: String,
}

/// Why a peer was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// The address is not in a tailnet range.
    NotTailnet(IpAddr),
    /// The address belongs to this node.
    SelfNode(IpAddr),
    /// `whois` failed or named no login.
    Unidentified(String),
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTailnet(ip) => write!(f, "{ip} is not a tailnet address"),
            Self::SelfNode(ip) => write!(f, "{ip} is this node's own address"),
            Self::Unidentified(detail) => write!(f, "peer not identified: {detail}"),
        }
    }
}

impl std::error::Error for GateError {}

/// The pure decision behind [`TailnetGate`].
///
/// # Arguments
/// - `addr`: remote address of the connection.
/// - `self_ips`: this node's tailnet addresses.
/// - `whois`: `None` when not asked yet (the address was already refused),
///   otherwise `tailscaled`'s answer.
///
/// # Errors
/// See [`GateError`].
pub fn decide(
    addr: SocketAddr,
    self_ips: &[IpAddr],
    whois: Option<Result<WhoIs, LocalApiError>>,
) -> Result<TailnetPeer, GateError> {
    let ip = addr.ip().to_canonical();
    if !is_tailnet_ip(ip) {
        return Err(GateError::NotTailnet(ip));
    }
    if self_ips.contains(&ip) {
        return Err(GateError::SelfNode(ip));
    }
    match whois {
        Some(Ok(who)) if !who.login.trim().is_empty() => Ok(TailnetPeer {
            addr,
            login: who.login,
            node: who.node,
        }),
        Some(Ok(_)) => Err(GateError::Unidentified("whois without login".to_owned())),
        Some(Err(error)) => Err(GateError::Unidentified(error.to_string())),
        None => Err(GateError::Unidentified("whois not asked".to_owned())),
    }
}

/// Future returned by [`Admission::admit`].
pub type AdmissionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TailnetPeer, GateError>> + Send + 'a>>;

/// Decides whether a TCP peer may reach the control plane.
pub trait Admission: Send + Sync {
    /// Admits or refuses the peer at `addr`.
    fn admit(&self, addr: SocketAddr) -> AdmissionFuture<'_>;
}

/// [`Admission`] backed by `tailscaled`.
#[derive(Debug, Clone)]
pub struct TailnetGate {
    api: LocalApi,
    self_ips: Vec<IpAddr>,
}

impl TailnetGate {
    /// Gate for a node with the addresses `self_ips`
    /// (from [`LocalApi::status`]).
    #[must_use]
    pub fn new(api: LocalApi, self_ips: Vec<IpAddr>) -> Self {
        Self { api, self_ips }
    }
}

impl Admission for TailnetGate {
    fn admit(&self, addr: SocketAddr) -> AdmissionFuture<'_> {
        Box::pin(async move {
            // Refuse cheap cases before asking tailscaled.
            match decide(addr, &self.self_ips, None) {
                Err(GateError::Unidentified(_)) => {}
                Err(other) => return Err(other),
                Ok(peer) => return Ok(peer),
            }
            let whois = self.api.whois(addr).await;
            decide(addr, &self.self_ips, Some(whois))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(text: &str) -> Result<SocketAddr, String> {
        text.parse().map_err(|error| format!("{text}: {error}"))
    }

    fn who(login: &str) -> WhoIs {
        WhoIs {
            login: login.to_owned(),
            display_name: String::new(),
            node: "s24.tail1234.ts.net".to_owned(),
        }
    }

    #[test]
    fn decide_admits_only_identified_foreign_tailnet_peers() -> Result<(), String> {
        let own: IpAddr = "100.101.1.2".parse().map_err(|error| format!("{error}"))?;
        let peer = decide(
            addr("100.101.9.9:50000")?,
            &[own],
            Some(Ok(who("mia@example.com"))),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(peer.login, "mia@example.com");

        assert!(matches!(
            decide(addr("192.168.1.5:1")?, &[own], Some(Ok(who("x")))),
            Err(GateError::NotTailnet(_))
        ));
        assert!(matches!(
            decide(addr("100.101.1.2:1")?, &[own], Some(Ok(who("x")))),
            Err(GateError::SelfNode(_))
        ));
        assert!(matches!(
            decide(addr("100.101.9.9:1")?, &[own], Some(Ok(who(" ")))),
            Err(GateError::Unidentified(_))
        ));
        assert!(matches!(
            decide(
                addr("100.101.9.9:1")?,
                &[own],
                Some(Err(LocalApiError::NotFound))
            ),
            Err(GateError::Unidentified(_))
        ));
        Ok(())
    }

    #[test]
    fn ipv4_mapped_ipv6_is_canonicalised() -> Result<(), String> {
        let mapped = addr("[::ffff:192.168.1.5]:1")?;
        assert!(matches!(
            decide(mapped, &[], Some(Ok(who("x")))),
            Err(GateError::NotTailnet(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn gate_refuses_non_tailnet_without_asking_tailscaled() -> Result<(), String> {
        // The socket does not exist: a refusal proves tailscaled was not asked.
        let gate = TailnetGate::new(LocalApi::new("/nonexistent/tailscaled.sock"), Vec::new());
        assert!(matches!(
            gate.admit(addr("10.0.0.1:1")?).await,
            Err(GateError::NotTailnet(_))
        ));
        assert!(matches!(
            gate.admit(addr("100.101.9.9:1")?).await,
            Err(GateError::Unidentified(_))
        ));
        Ok(())
    }
}
