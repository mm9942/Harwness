//! HTTP-Client mit Scoped-Resolver ohne DNS-Rebinding-Fenster.
//!
//! # Verantwortung
//! [`build_client`] liefert einen `reqwest::Client`, der
//! - Namen über einen eigenen [`reqwest::dns::Resolve`] auflöst, der zuerst
//!   den Host gegen die Allowlist prüft und danach **jede** aufgelöste Adresse
//!   per [`EgressPolicy::check_addr`] filtert. `reqwest`/`hyper-util`
//!   verbinden sich ausschließlich mit den zurückgegebenen Adressen; eine
//!   zweite Auflösung zwischen Prüfung und Verbindung gibt es nicht.
//! - keine Proxys nutzt (`no_proxy()`: weder konfigurierte noch
//!   `HTTP(S)_PROXY`/`ALL_PROXY` aus der Umgebung),
//! - keinen Redirects folgt (`redirect::Policy::none()`); Aufrufer prüfen jedes
//!   `Location`-Ziel mit [`EgressPolicy::check_url`] und senden neu.
//!
//! # Grenzen
//! Für URLs mit IP-Literal ruft `hyper-util` den Resolver nicht auf
//! (`HttpConnector::call_async`, hyper-util 0.1.20
//! `src/client/legacy/connect/http.rs:538-541`). IP-Literale deckt
//! ausschließlich [`EgressPolicy::check_url`] ab — deshalb ist sie vor jedem
//! Senden Pflicht.
//!
//! # Nebenläufigkeit
//! Der Resolver ist `Send + Sync` und hält `Arc<EgressPolicy>`; die
//! System-Auflösung läuft über `tokio::net::lookup_host` (blockierendes
//! `getaddrinfo` im Tokio-Blocking-Pool) und benötigt eine laufende
//! Tokio-Runtime, wie jeder `reqwest`-Aufruf.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::classify::classify;
use crate::error::EgressError;
use crate::policy::EgressPolicy;

// Obergrenze für den TCP-Verbindungsaufbau je Adresse; Gesamt-Deadlines setzen
// die Aufrufer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// Fehlertyp, den `reqwest::dns::Resolving` erwartet.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

// Zukunft einer Namensauflösung.
pub(crate) type LookupFuture =
    Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;

// Namensauflösung hinter dem Scoped-Resolver; im Produktivpfad das System, in
// Tests ein netzfreier Stub.
pub(crate) trait HostLookup: Send + Sync {
    // Löst `host` auf; Ports der Ergebnisse sind bedeutungslos (reqwest setzt
    // den Port der URL).
    fn lookup(&self, host: &str) -> LookupFuture;
}

// System-Resolver über `tokio::net::lookup_host`.
struct SystemLookup;

impl HostLookup for SystemLookup {
    fn lookup(&self, host: &str) -> LookupFuture {
        let host = host.to_owned();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok::<Vec<SocketAddr>, std::io::Error>(addrs.collect())
        })
    }
}

/// Baut den Egress-HTTP-Client zu einer Policy.
///
/// # Description
/// Konfiguriert `reqwest::ClientBuilder` mit
/// `dns_resolver(Arc<ScopedResolver>)`, `no_proxy()`,
/// `redirect(Policy::none())` und einem Verbindungs-Timeout von 10 s. Der
/// Resolver lehnt Namen außerhalb der Allowlist ab und gibt nur Adressen
/// zurück, die [`EgressPolicy::check_addr`] besteht; bleibt keine übrig,
/// scheitert die Anfrage mit [`EgressError::NoPermittedAddress`] in der
/// `source()`-Kette des `reqwest::Error`.
///
/// # Arguments
/// - `policy` (`Arc<EgressPolicy>`): geteilte Policy (Eigentum am `Arc`
///   geht an den Client über).
///
/// # Returns
/// Einen `reqwest::Client`; Klone teilen Resolver und Verbindungspool.
///
/// # Errors
/// - [`EgressError::ClientBuild`]: `reqwest` kann den Client nicht bauen.
///
/// # Concurrency
/// Der Client ist `Send + Sync` und für parallele Anfragen gedacht.
///
/// # Examples
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_egress::{EgressPolicy, build_client};
///
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// let policy = Arc::new(EgressPolicy::new(vec!["crates.io".to_owned()], false)?);
/// let client = build_client(Arc::clone(&policy))?;
/// let url = policy.check_url("https://crates.io/api/v1/crates/serde")?;
/// let status = client.get(url.as_url().clone()).send().await?.status();
/// # let _ = status;
/// # Ok(())
/// # }
/// ```
pub fn build_client(policy: Arc<EgressPolicy>) -> Result<reqwest::Client, EgressError> {
    build_client_with_lookup(policy, Arc::new(SystemLookup))
}

// `build_client` mit austauschbarer Namensauflösung (Tests).
pub(crate) fn build_client_with_lookup(
    policy: Arc<EgressPolicy>,
    lookup: Arc<dyn HostLookup>,
) -> Result<reqwest::Client, EgressError> {
    let resolver = Arc::new(ScopedResolver { policy, lookup });
    reqwest::Client::builder()
        .dns_resolver(resolver)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(EgressError::ClientBuild)
}

// DNS-Resolver, der Allowlist und Adressklassen durchsetzt.
struct ScopedResolver {
    policy: Arc<EgressPolicy>,
    lookup: Arc<dyn HostLookup>,
}

impl Resolve for ScopedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        let lookup = Arc::clone(&self.lookup);
        let host = name.as_str().to_ascii_lowercase();
        Box::pin(async move {
            policy.check_host(&host).map_err(boxed)?;
            let resolved = lookup
                .lookup(&host)
                .await
                .map_err(|source| boxed(EgressError::Lookup { host: host.clone(), source }))?;
            let permitted = filter_resolved(&policy, &host, resolved).map_err(boxed)?;
            let addrs: Addrs = Box::new(permitted.into_iter());
            Ok::<Addrs, BoxError>(addrs)
        })
    }
}

fn boxed(err: EgressError) -> BoxError {
    Box::new(err)
}

// Filtert aufgelöste Adressen durch `EgressPolicy::check_addr`.
//
// Liefert die zulässigen Adressen in Auflösungsreihenfolge. Verworfene
// Adressen werden mit ihrer Klasse geloggt; bleibt keine Adresse übrig (auch
// bei leerer Auflösung), ist das Ergebnis `NoPermittedAddress` mit allen
// verworfenen Adressen. Netzfrei und synchron, damit Resolver und künftiger
// SOCKS5-Proxy (N-EGRESS) dieselbe Regel nutzen.
pub(crate) fn filter_resolved<I>(
    policy: &EgressPolicy,
    host: &str,
    resolved: I,
) -> Result<Vec<SocketAddr>, EgressError>
where
    I: IntoIterator<Item = SocketAddr>,
{
    let mut permitted = Vec::new();
    let mut denied = Vec::new();
    for addr in resolved {
        if policy.check_addr(addr).is_ok() {
            permitted.push(addr);
        } else {
            let class = classify(addr.ip());
            tracing::warn!(
                host,
                addr = %addr.ip(),
                class = %class,
                "egress: aufgelöste Adresse verworfen"
            );
            denied.push((addr.ip(), class));
        }
    }
    if permitted.is_empty() {
        tracing::warn!(host, denied = denied.len(), "egress: keine zulässige Adresse");
        return Err(EgressError::NoPermittedAddress { host: host.to_owned(), denied });
    }
    tracing::debug!(
        host,
        permitted = permitted.len(),
        denied = denied.len(),
        "egress: aufgelöst"
    );
    Ok(permitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::AddrClass;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{IpAddr, TcpListener};

    // Netzfreier Resolver: feste Antworten je Host, zählt Aufrufe nicht.
    struct StubLookup(HashMap<String, Vec<SocketAddr>>);

    impl StubLookup {
        fn new(entries: &[(&str, &[&str])]) -> Arc<Self> {
            let map = entries
                .iter()
                .map(|(host, addrs)| {
                    let parsed = addrs
                        .iter()
                        .map(|raw| SocketAddr::new(raw.parse::<IpAddr>().expect("Test-IP"), 0))
                        .collect();
                    ((*host).to_owned(), parsed)
                })
                .collect();
            Arc::new(Self(map))
        }
    }

    impl HostLookup for StubLookup {
        fn lookup(&self, host: &str) -> LookupFuture {
            let result = self.0.get(host).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "stub: unbekannter Host")
            });
            Box::pin(std::future::ready(result))
        }
    }

    fn policy(hosts: &[&str], allow_private: bool) -> Arc<EgressPolicy> {
        let hosts = hosts.iter().map(|h| (*h).to_owned()).collect();
        Arc::new(EgressPolicy::new(hosts, allow_private).expect("gültige Test-Policy"))
    }

    fn sock(raw: &str) -> SocketAddr {
        SocketAddr::new(raw.parse::<IpAddr>().expect("Test-IP"), 0)
    }

    // Sucht einen `EgressError` in der `source()`-Kette.
    fn find_egress_error<'a>(
        err: &'a (dyn std::error::Error + 'static),
    ) -> Option<&'a EgressError> {
        let mut current = Some(err);
        while let Some(e) = current {
            if let Some(found) = e.downcast_ref::<EgressError>() {
                return Some(found);
            }
            current = e.source();
        }
        None
    }

    fn name(host: &str) -> Name {
        host.parse::<Name>().expect("gültiger Name")
    }

    #[test]
    fn test_filter_resolved_drops_private_answers() {
        let p = policy(&["rebind.example"], false);
        let resolved = [sock("10.0.0.1"), sock("93.184.216.34"), sock("::ffff:127.0.0.1")];
        let permitted = filter_resolved(&p, "rebind.example", resolved).expect("öffentlich");
        assert_eq!(permitted, [sock("93.184.216.34")]);
    }

    #[test]
    fn test_filter_resolved_all_private_is_error_with_classes() {
        let p = policy(&["rebind.example"], false);
        let resolved = [
            sock("127.0.0.1"),
            sock("169.254.169.254"),
            sock("64:ff9b::a00:1"),
            sock("fd00::1"),
        ];
        let result = filter_resolved(&p, "rebind.example", resolved);
        let (host, denied) = match result {
            Err(EgressError::NoPermittedAddress { host, denied }) => (host, denied),
            other => panic!("erwartet NoPermittedAddress, erhalten {other:?}"),
        };
        assert_eq!(host, "rebind.example");
        let classes: Vec<AddrClass> = denied.iter().map(|(_, class)| *class).collect();
        let expected = [
            AddrClass::Loopback,
            AddrClass::CloudMetadata,
            AddrClass::Private,
            AddrClass::UniqueLocal,
        ];
        assert_eq!(classes, expected);
    }

    #[test]
    fn test_filter_resolved_empty_answer_is_error() {
        let p = policy(&["x.example"], false);
        let result = filter_resolved(&p, "x.example", Vec::new());
        let empty_denied = matches!(
            &result,
            Err(EgressError::NoPermittedAddress { denied, .. }) if denied.is_empty()
        );
        assert!(empty_denied, "{result:?}");
    }

    #[test]
    fn test_filter_resolved_allow_private_keeps_lan_but_not_metadata() {
        let p = policy(&["nas.lan"], true);
        let resolved = [sock("192.168.1.10"), sock("169.254.169.254")];
        let permitted = filter_resolved(&p, "nas.lan", resolved).expect("LAN erlaubt");
        assert_eq!(permitted, [sock("192.168.1.10")]);
    }

    #[tokio::test]
    async fn test_scoped_resolver_filters_private_answer() {
        let lookup = StubLookup::new(&[("api.docs.rs", &["10.0.0.7", "2606:4700::1"])]);
        let resolver = ScopedResolver { policy: policy(&["docs.rs"], false), lookup };
        let addrs: Vec<SocketAddr> = match resolver.resolve(name("api.docs.rs")).await {
            Ok(addrs) => addrs.collect(),
            Err(err) => panic!("Auflösung muss gelingen: {err}"),
        };
        assert_eq!(addrs, [sock("2606:4700::1")]);
    }

    #[tokio::test]
    async fn test_scoped_resolver_rejects_host_outside_allowlist_before_lookup() {
        // Der Stub kennt den Host nicht: ein Lookup ergäbe `Lookup`, nicht
        // `HostNotAllowed`.
        let lookup = StubLookup::new(&[]);
        let resolver = ScopedResolver { policy: policy(&["docs.rs"], false), lookup };
        let Err(err) = resolver.resolve(name("evil.com")).await else {
            panic!("evil.com darf nicht aufgelöst werden");
        };
        let found = find_egress_error(&*err);
        let rejected = matches!(
            found,
            Some(EgressError::HostNotAllowed { host }) if host == "evil.com"
        );
        assert!(rejected, "{found:?}");
    }

    #[tokio::test]
    async fn test_build_client_request_to_rebound_name_fails_without_connecting() {
        let lookup = StubLookup::new(&[("rebind.test", &["127.0.0.1"])]);
        let client = build_client_with_lookup(policy(&["rebind.test"], false), lookup)
            .expect("Client baubar");
        let err = client.get("http://rebind.test:9/").send().await.expect_err("muss scheitern");
        let loopback = ("127.0.0.1".parse::<IpAddr>().expect("IP"), AddrClass::Loopback);
        let found = find_egress_error(&err);
        let denied_loopback = matches!(
            found,
            Some(EgressError::NoPermittedAddress { denied, .. }) if denied.as_slice() == [loopback]
        );
        assert!(denied_loopback, "{err:?}");
    }

    #[tokio::test]
    async fn test_build_client_connects_only_to_checked_address() {
        // Loopback-Server (kein externes Netz); `allow_private` erlaubt ihn.
        let listener = TcpListener::bind("127.0.0.1:0").expect("Loopback-Bind");
        let port = listener.local_addr().expect("lokale Adresse").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("Verbindung");
            let mut buf = [0_u8; 1024];
            let mut seen: Vec<u8> = Vec::new();
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).expect("Anfrage lesen");
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
            }
            let response = b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\n\
                             Content-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response).expect("Antwort schreiben");
            String::from_utf8_lossy(&seen).into_owned()
        });

        let lookup = StubLookup::new(&[("svc.test", &["127.0.0.1"])]);
        let client =
            build_client_with_lookup(policy(&["svc.test"], true), lookup).expect("Client baubar");
        let response = client
            .get(format!("http://svc.test:{port}/probe"))
            .send()
            .await
            .expect("Anfrage an geprüfte Adresse");
        // Redirects werden nicht verfolgt: der 302 kommt beim Aufrufer an.
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        let request = server.join().expect("Server-Thread");
        assert!(request.starts_with("GET /probe HTTP/1.1\r\n"), "{request}");
    }
}
