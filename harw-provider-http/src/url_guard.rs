//! URL-Guard fuer remote angelegte eigene Provider und fuer `scan` (Welle 3, Scheibe 3, C9b).
//!
//! # Verantwortung
//! Vom Handy kommende Provider-Adressen duerfen das Gateway nicht als SSRF-Sprungbrett
//! missbrauchen (Vertrag K-W3b, Regel R4). Dieses Modul prueft deshalb:
//! - Syntax ohne DNS ([`check_remote_url_syntax`]): Laenge, WHATWG-Parsing ueber
//!   [`EgressUrl::parse`], nur `https`, keine Zugangsdaten, keine Query/Fragment, keine
//!   Platzhalter/Leerraeume, kein lokaler Host (`localhost`, Endungen aus
//!   [`BLOCKED_HOST_SUFFIXES`], Namen ohne Punkt) und kein privates IP-Literal.
//! - Aufloesung ([`guard_remote_url_with`]): ALLE aufgeloesten Adressen muessen oeffentlich
//!   sein; sonst [`UrlGuardError::PrivateAddress`].
//! - Pinning ([`GuardedEndpoint::client`]): der HTTP-Client verbindet nur zu genau den
//!   geprueften Adressen (kein zweites DNS, kein DNS-Rebinding), folgt keinen Redirects,
//!   benutzt keinen Proxy und spricht nur `https`.
//!
//! # Fehlertexte
//! Alle [`UrlGuardError`]-Texte sind deutsch und wiederholen weder die Eingabe-URL
//! (Pfad, Query) noch eine aufgeloeste IP-Adresse.
//!
//! # Nebenlaeufigkeit
//! Reine Funktionen und unveraenderliche Daten; [`HostResolver`] ist `Send + Sync`.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use harw_sandbox::{EgressHost, EgressUrl, EgressUrlError};

/// Hoechstlaenge einer remote angegebenen Provider-Adresse in Bytes.
pub const MAX_URL_LEN: usize = 2048;
/// Hoechstzahl gepinnter Adressen je Endpunkt (nach Pruefung ALLER aufgeloesten Adressen).
pub const MAX_PINNED_ADDRS: usize = 8;
/// Verbindungsaufbau-Timeout des gepinnten Clients.
pub const GUARD_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Gesamt-Timeout je Request des gepinnten Clients.
pub const GUARD_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout der DNS-Aufloesung.
pub const GUARD_RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Host-Endungen, die nie oeffentlich sind (zusaetzlich zur IP-Pruefung).
///
/// `.test`/`.example` sind bewusst NICHT gesperrt (Tests nutzen `*.example.test` mit
/// Fake-Resolver; oeffentlich aufloesen koennen diese Namen ohnehin nicht).
pub const BLOCKED_HOST_SUFFIXES: &[&str] = &[
    ".localhost",
    ".local",
    ".internal",
    ".lan",
    ".home.arpa",
    ".intranet",
    ".corp",
    ".onion",
    ".test.invalid",
];

/// Ablehnungsgruende des URL-Guards.
///
/// Die `Display`-Texte nennen weder die Eingabe noch eine aufgeloeste Adresse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlGuardError {
    /// Eingabe laenger als [`MAX_URL_LEN`].
    TooLong,
    /// Keine parsebare absolute URL bzw. kein gueltiger Host, oder Steuerzeichen.
    Malformed,
    /// Schema ist nicht `https`.
    NotHttps,
    /// Benutzer/Passwort in der URL.
    Credentials,
    /// Query (`?`) oder Fragment (`#`) in der URL.
    QueryOrFragment,
    /// `<`/`>` oder Leerraum.
    Placeholder,
    /// Host `localhost`, Endung aus `BLOCKED_HOST_SUFFIXES`, Name ohne Punkt, oder IP-Literal
    /// privat/speziell.
    BlockedHost,
    /// Mindestens eine aufgeloeste Adresse ist privat/speziell (Adresse wird NICHT genannt).
    PrivateAddress,
    /// DNS-Aufloesung schlug fehl oder lief in den Timeout.
    ResolveFailed,
    /// DNS lieferte keine Adresse.
    NoAddress,
    /// Der gepinnte HTTP-Client liess sich nicht bauen.
    ClientBuild,
}

impl std::fmt::Display for UrlGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::TooLong => "Provider-Adresse ist zu lang",
            Self::Malformed => "Provider-Adresse ist keine gültige URL",
            Self::NotHttps => "Provider-Adresse muss https verwenden",
            Self::Credentials => "Provider-Adresse darf keine Zugangsdaten enthalten",
            Self::QueryOrFragment => {
                "Provider-Adresse darf keine Abfrage (?) und keinen Anker (#) enthalten"
            }
            Self::Placeholder => "Provider-Adresse enthält Platzhalter oder Leerzeichen",
            Self::BlockedHost => "Provider-Adresse zeigt auf ein lokales oder privates Ziel",
            Self::PrivateAddress => {
                "Provider-Adresse löst auf eine private oder spezielle Netzadresse auf"
            }
            Self::ResolveFailed => "Provider-Adresse konnte nicht aufgelöst werden",
            Self::NoAddress => "Provider-Adresse liefert keine Netzadresse",
            Self::ClientBuild => "HTTP-Client für die Provider-Adresse konnte nicht gebaut werden",
        };
        f.write_str(text)
    }
}

impl std::error::Error for UrlGuardError {}

/// Future-Typ von [`HostResolver::resolve`].
pub type ResolveFuture<'a> =
    Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>>;

/// DNS-Aufloesung, austauschbar fuer Tests.
pub trait HostResolver: Send + Sync {
    /// Loest `host` (normalisierte Domain, ohne Klammern) fuer `port` auf.
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a>;
}

/// `tokio::net::lookup_host((host, port))` mit [`GUARD_RESOLVE_TIMEOUT`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl HostResolver for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a> {
        Box::pin(async move {
            match tokio::time::timeout(GUARD_RESOLVE_TIMEOUT, tokio::net::lookup_host((host, port)))
                .await
            {
                Ok(Ok(addrs)) => Ok(addrs.collect()),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "DNS-Aufloesung: Timeout",
                )),
            }
        })
    }
}

/// Eine gepruefte, an Adressen gepinnte https-Basis-URL.
#[derive(Debug, Clone)]
pub struct GuardedEndpoint {
    url: String,
    host: String,
    is_domain: bool,
    port: u16,
    addrs: Vec<SocketAddr>,
}

impl GuardedEndpoint {
    /// Die Basis-URL wie eingegeben, ohne abschliessende `/`.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Der normalisierte Host (wie [`EgressUrl::host_str`]).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Der effektive Port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Die gepinnten (gepruefte, oeffentliche) Adressen, Port = [`Self::port`].
    #[must_use]
    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// `true`, wenn `url` dasselbe Schema (`https`), denselben Host und Port hat und keine
    /// Zugangsdaten traegt (fuer abgeleitete URLs wie `{base}/models`).
    #[must_use]
    pub fn admits(&self, url: &str) -> bool {
        match EgressUrl::parse(url) {
            Ok(parsed) => {
                parsed.is_https() && parsed.host_str() == self.host && parsed.port() == self.port
            }
            Err(_) => false,
        }
    }

    /// Baut den gepinnten reqwest-Client.
    ///
    /// `.https_only(true)`, keine Redirects, kein Proxy, `resolve_to_addrs(host, addrs)` bei
    /// Domain-Host, Connect-/Request-Timeouts aus den Konstanten.
    ///
    /// # Errors
    /// [`UrlGuardError::ClientBuild`], wenn reqwest den Client nicht bauen kann.
    pub fn client(&self) -> Result<reqwest::Client, UrlGuardError> {
        let mut builder = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(GUARD_CONNECT_TIMEOUT)
            .timeout(GUARD_REQUEST_TIMEOUT);
        if self.is_domain {
            builder = builder.resolve_to_addrs(&self.host, &self.addrs);
        }
        builder.build().map_err(|_| UrlGuardError::ClientBuild)
    }
}

/// `EgressHost::Ip(ip).is_private_or_special()` (harw-sandbox-Tabellen: RFC1918, CGNAT
/// 100.64/10, Loopback, Link-Local, Doku-Netze, Multicast, ULA, NAT64, 6to4, Teredo,
/// v4-mapped).
#[must_use]
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    EgressHost::Ip(ip).is_private_or_special()
}

/// Syntaxpruefung ohne DNS (fuer `plan_change`).
///
/// # Errors
/// Siehe [`UrlGuardError`]; nie [`UrlGuardError::PrivateAddress`], `ResolveFailed`,
/// `NoAddress` oder `ClientBuild`.
pub fn check_remote_url_syntax(raw: &str) -> Result<(), UrlGuardError> {
    parse_remote(raw).map(|_| ())
}

/// Syntax + Aufloesung mit [`SystemResolver`].
///
/// # Errors
/// Siehe [`guard_remote_url_with`].
pub async fn guard_remote_url(raw: &str) -> Result<GuardedEndpoint, UrlGuardError> {
    guard_remote_url_with(raw, &SystemResolver).await
}

/// Syntax + Aufloesung; ALLE Adressen muessen oeffentlich sein, sonst `PrivateAddress`;
/// leer → `NoAddress`; mehr als [`MAX_PINNED_ADDRS`] → die ersten `MAX_PINNED_ADDRS`
/// (nach Pruefung ALLER). Gepinnt wird immer auf den Port der URL, nie auf einen vom
/// Resolver gemeldeten anderen Port.
///
/// # Errors
/// Syntaxfehler wie [`check_remote_url_syntax`], dazu `ResolveFailed`, `NoAddress`,
/// `PrivateAddress`.
pub async fn guard_remote_url_with(
    raw: &str,
    resolver: &dyn HostResolver,
) -> Result<GuardedEndpoint, UrlGuardError> {
    let parsed = parse_remote(raw)?;
    let port = parsed.port();
    let host = parsed.host_str();
    let (is_domain, resolved) = match parsed.host() {
        // IP-Literal: bereits in `parse_remote` als oeffentlich geprueft.
        EgressHost::Ip(ip) => (false, vec![SocketAddr::new(*ip, port)]),
        EgressHost::Domain(name) => {
            let answer = tokio::time::timeout(GUARD_RESOLVE_TIMEOUT, resolver.resolve(name, port))
                .await
                .map_err(|_| UrlGuardError::ResolveFailed)?
                .map_err(|_| UrlGuardError::ResolveFailed)?;
            (true, answer)
        }
    };
    if resolved.is_empty() {
        return Err(UrlGuardError::NoAddress);
    }
    if resolved.iter().any(|addr| is_forbidden_ip(addr.ip())) {
        return Err(UrlGuardError::PrivateAddress);
    }
    let mut addrs: Vec<SocketAddr> = Vec::with_capacity(resolved.len().min(MAX_PINNED_ADDRS));
    for addr in resolved {
        let pinned = SocketAddr::new(addr.ip(), port);
        if !addrs.contains(&pinned) {
            addrs.push(pinned);
        }
        if addrs.len() == MAX_PINNED_ADDRS {
            break;
        }
    }
    Ok(GuardedEndpoint {
        url: raw.trim_end_matches('/').to_owned(),
        host,
        is_domain,
        port,
        addrs,
    })
}

/// Remote-Scan eines Providers ueber den Guard (fuer `models.rs` C5b, Fremd-Scope).
///
/// Prueft die effektive Basis (`provider.base_url`, bei Codex-Route der feste Codex-Endpunkt)
/// mit [`guard_remote_url_with`], baut [`GuardedEndpoint::client`] und ruft
/// `crate::discovery::list_models_with_client(..)`. Vor bestandenem Guard geht kein Request
/// hinaus.
///
/// # Errors
/// Guard-Fehler als [`crate::discovery::DiscoveryError::Network`] mit dem deutschen
/// `Display`-Text; sonst die Fehler der Discovery.
pub async fn list_models_guarded(
    provider_name: &str,
    provider: &harw_config::ProviderToml,
    api_key: Option<&str>,
    home: Option<&std::path::Path>,
    resolver: &dyn HostResolver,
) -> Result<Vec<crate::discovery::DiscoveredModel>, crate::discovery::DiscoveryError> {
    use crate::discovery::DiscoveryError;

    let codex_route =
        crate::codex::CodexRoute::from_provider(provider).map_err(|_| DiscoveryError::Decode {
            detail: "Invalid Codex credential route".into(),
        })?;
    let base = if codex_route.is_some() {
        crate::codex::BASE_URL
    } else {
        provider.base_url.trim_end_matches('/')
    };
    let network = |error: UrlGuardError| DiscoveryError::Network {
        detail: error.to_string(),
    };
    let endpoint = guard_remote_url_with(base, resolver)
        .await
        .map_err(network)?;
    let client = endpoint.client().map_err(network)?;
    crate::discovery::list_models_with_client(provider_name, provider, api_key, home, client).await
}

/// Gemeinsame Syntaxpruefung; liefert die geparste URL fuer die Aufloesung.
fn parse_remote(raw: &str) -> Result<EgressUrl, UrlGuardError> {
    if raw.len() > MAX_URL_LEN {
        return Err(UrlGuardError::TooLong);
    }
    if raw.chars().any(char::is_control) {
        return Err(UrlGuardError::Malformed);
    }
    if raw
        .chars()
        .any(|c| c == '<' || c == '>' || c.is_whitespace())
    {
        return Err(UrlGuardError::Placeholder);
    }
    let parsed = EgressUrl::parse(raw).map_err(|error| match error {
        EgressUrlError::UnsupportedScheme(_) => UrlGuardError::NotHttps,
        EgressUrlError::UserinfoPresent => UrlGuardError::Credentials,
        EgressUrlError::Parse | EgressUrlError::MissingHost | EgressUrlError::InvalidHost => {
            UrlGuardError::Malformed
        }
    })?;
    if !parsed.is_https() {
        return Err(UrlGuardError::NotHttps);
    }
    // `raw`-Pruefung zusaetzlich zum Parser: auch ein leeres `?`/`#` ist abgelehnt.
    if parsed.as_url().query().is_some()
        || parsed.as_url().fragment().is_some()
        || raw.contains(['?', '#'])
    {
        return Err(UrlGuardError::QueryOrFragment);
    }
    match parsed.host() {
        EgressHost::Ip(ip) => {
            if is_forbidden_ip(*ip) {
                return Err(UrlGuardError::BlockedHost);
            }
        }
        EgressHost::Domain(name) => {
            if domain_is_blocked(name) {
                return Err(UrlGuardError::BlockedHost);
            }
        }
    }
    Ok(parsed)
}

/// `localhost`, `*.localhost`, Endungen aus [`BLOCKED_HOST_SUFFIXES`] (auch die nackte
/// Endung ohne Punkt) und Namen ohne Punkt (Suchdomain-Aufloesung ins interne Netz).
fn domain_is_blocked(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    if !name.contains('.') || EgressHost::Domain(name.clone()).is_loopback() {
        return true;
    }
    BLOCKED_HOST_SUFFIXES.iter().any(|suffix| {
        name.ends_with(suffix) || suffix.strip_prefix('.').is_some_and(|bare| name == bare)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Synthetische, nicht private Adresse (kein Doku-Netz; siehe Vertrag 6.3 Hinweis).
    const PUBLIC_TEST_IP: &str = "203.0.114.10";

    enum Answer {
        /// Adressen mit dem angefragten Port.
        Ips(Vec<IpAddr>),
        /// Adressen mit festem Port (z. B. Auffang-Server).
        Addrs(Vec<SocketAddr>),
        Fail,
    }

    struct FakeResolver {
        answer: Answer,
        calls: AtomicUsize,
        hosts: Mutex<Vec<String>>,
    }

    impl FakeResolver {
        fn new(answer: Answer) -> Self {
            Self {
                answer,
                calls: AtomicUsize::new(0),
                hosts: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl HostResolver for FakeResolver {
        fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut hosts) = self.hosts.lock() {
                hosts.push(host.to_owned());
            }
            let out = match &self.answer {
                Answer::Ips(ips) => Ok(ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect()),
                Answer::Addrs(addrs) => Ok(addrs.clone()),
                Answer::Fail => Err(std::io::Error::other("fake resolver failure")),
            };
            Box::pin(async move { out })
        }
    }

    fn ip(text: &str) -> TestResult<IpAddr> {
        text.parse::<IpAddr>().map_err(ctx("IP-Literal parsen"))
    }

    fn public_ip() -> TestResult<IpAddr> {
        let addr = ip(PUBLIC_TEST_IP)?;
        // Vorabpruefung (Vertrag 6.3): die Test-Adresse muss als oeffentlich gelten.
        if is_forbidden_ip(addr) {
            return Err(TestError::Unexpected("Test-Adresse gilt als privat".into()));
        }
        Ok(addr)
    }

    fn provider(base_url: &str) -> TestResult<harw_config::ProviderToml> {
        serde_json::from_value(serde_json::json!({
            "name": "custom-test",
            "api": "openai-chat",
            "base_url": base_url,
        }))
        .map_err(ctx("ProviderToml bauen"))
    }

    #[test]
    fn rejects_plain_http_even_for_public_host() {
        assert_eq!(
            check_remote_url_syntax("http://api.example.test/v1"),
            Err(UrlGuardError::NotHttps)
        );
        assert_eq!(
            check_remote_url_syntax(&format!("http://{PUBLIC_TEST_IP}/v1")),
            Err(UrlGuardError::NotHttps)
        );
        assert_eq!(
            check_remote_url_syntax("ftp://api.example.test/"),
            Err(UrlGuardError::NotHttps)
        );
        assert_eq!(
            check_remote_url_syntax("https://api.example.test/v1"),
            Ok(())
        );
    }

    #[test]
    fn rejects_credentials_query_fragment_placeholder() {
        assert_eq!(
            check_remote_url_syntax("https://user:pw@api.example.test/v1"),
            Err(UrlGuardError::Credentials)
        );
        assert_eq!(
            check_remote_url_syntax("https://user@api.example.test/v1"),
            Err(UrlGuardError::Credentials)
        );
        for raw in [
            "https://api.example.test/v1?key=x",
            "https://api.example.test/v1?",
            "https://api.example.test/v1#frag",
            "https://api.example.test/v1#",
        ] {
            assert_eq!(
                check_remote_url_syntax(raw),
                Err(UrlGuardError::QueryOrFragment),
                "{raw}"
            );
        }
        for raw in [
            "https://<host>/v1",
            "https://api.example.test/<path>",
            "https://api.example.test/v 1",
            " https://api.example.test/v1",
        ] {
            assert_eq!(
                check_remote_url_syntax(raw),
                Err(UrlGuardError::Placeholder),
                "{raw}"
            );
        }
        assert_eq!(
            check_remote_url_syntax("https://api.example.test/\u{7}"),
            Err(UrlGuardError::Malformed)
        );
        assert_eq!(
            check_remote_url_syntax("not a url"),
            Err(UrlGuardError::Placeholder)
        );
        assert_eq!(
            check_remote_url_syntax("api.example.test"),
            Err(UrlGuardError::Malformed)
        );
        let long = format!("https://api.example.test/{}", "a".repeat(MAX_URL_LEN));
        assert_eq!(check_remote_url_syntax(&long), Err(UrlGuardError::TooLong));
    }

    #[test]
    fn rejects_private_loopback_linklocal_literals() {
        for raw in [
            "https://127.0.0.1",
            "https://10.1.2.3",
            "https://169.254.169.254",
            "https://[::1]",
            "https://[fe80::1]",
            "https://100.64.0.1",
            "https://[::ffff:192.168.0.1]",
            "https://192.168.1.10/v1",
            "https://172.16.0.1",
            "https://0.0.0.0",
            "https://[fd00::1]",
            "https://0x7f.1",
        ] {
            assert_eq!(
                check_remote_url_syntax(raw),
                Err(UrlGuardError::BlockedHost),
                "{raw}"
            );
        }
    }

    #[test]
    fn rejects_blocked_suffixes() {
        for raw in [
            "https://router.local",
            "https://svc.internal",
            "https://localhost",
            "https://api.localhost/v1",
            "https://LOCALHOST./v1",
            "https://nas.home.arpa",
            "https://box.lan",
            "https://hidden.onion",
            "https://intranetserver",
            "https://local",
        ] {
            assert_eq!(
                check_remote_url_syntax(raw),
                Err(UrlGuardError::BlockedHost),
                "{raw}"
            );
        }
        // `.test` ist bewusst nicht gesperrt.
        assert_eq!(check_remote_url_syntax("https://api.example.test"), Ok(()));
    }

    #[tokio::test]
    async fn rejects_when_any_resolved_address_is_private() -> TestResult {
        let public = public_ip()?;
        let private = ip("10.0.0.5")?;
        let resolver = FakeResolver::new(Answer::Ips(vec![public, private]));
        let result = guard_remote_url_with("https://api.example.test/v1", &resolver).await;
        assert_eq!(result.err(), Some(UrlGuardError::PrivateAddress));
        assert_eq!(resolver.calls(), 1);
        let hosts = resolver
            .hosts
            .lock()
            .map_err(|_| TestError::Unexpected("Mutex vergiftet".into()))?
            .clone();
        assert_eq!(hosts, vec!["api.example.test".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn accepts_public_resolution_and_pins_url_port() -> TestResult {
        let public = public_ip()?;
        // Resolver meldet einen anderen Port; gepinnt wird der Port der URL.
        let resolver = FakeResolver::new(Answer::Addrs(vec![SocketAddr::new(public, 9)]));
        let endpoint = guard_remote_url_with("https://API.example.test/v1/", &resolver)
            .await
            .map_err(ctx("Guard"))?;
        assert_eq!(endpoint.url(), "https://API.example.test/v1");
        assert_eq!(endpoint.host(), "api.example.test");
        assert_eq!(endpoint.port(), 443);
        assert_eq!(endpoint.addrs(), &[SocketAddr::new(public, 443)]);
        Ok(())
    }

    #[tokio::test]
    async fn public_ip_literal_needs_no_resolver() -> TestResult {
        let public = public_ip()?;
        let resolver = FakeResolver::new(Answer::Fail);
        let endpoint =
            guard_remote_url_with(&format!("https://{PUBLIC_TEST_IP}:8443/v1"), &resolver)
                .await
                .map_err(ctx("Guard"))?;
        assert_eq!(resolver.calls(), 0);
        assert_eq!(endpoint.addrs(), &[SocketAddr::new(public, 8443)]);
        endpoint.client().map_err(ctx("Client bauen"))?;
        Ok(())
    }

    #[tokio::test]
    async fn resolve_failure_and_empty_answer_are_rejected() {
        let failing = FakeResolver::new(Answer::Fail);
        assert_eq!(
            guard_remote_url_with("https://api.example.test/v1", &failing)
                .await
                .err(),
            Some(UrlGuardError::ResolveFailed)
        );
        let empty = FakeResolver::new(Answer::Ips(Vec::new()));
        assert_eq!(
            guard_remote_url_with("https://api.example.test/v1", &empty)
                .await
                .err(),
            Some(UrlGuardError::NoAddress)
        );
        // Syntaxfehler: Resolver wird gar nicht gefragt.
        let unused = FakeResolver::new(Answer::Fail);
        assert_eq!(
            guard_remote_url_with("https://router.local/v1", &unused)
                .await
                .err(),
            Some(UrlGuardError::BlockedHost)
        );
        assert_eq!(unused.calls(), 0);
    }

    #[tokio::test]
    async fn pins_at_most_max_and_checks_all() -> TestResult {
        let mut ips = Vec::new();
        for last in 1..=10u8 {
            let candidate = IpAddr::from([203, 0, 114, last]);
            if is_forbidden_ip(candidate) {
                return Err(TestError::Unexpected("Test-Adresse gilt als privat".into()));
            }
            ips.push(candidate);
        }
        let resolver = FakeResolver::new(Answer::Ips(ips.clone()));
        let endpoint = guard_remote_url_with("https://api.example.test", &resolver)
            .await
            .map_err(ctx("Guard"))?;
        assert_eq!(endpoint.addrs().len(), MAX_PINNED_ADDRS);

        // Private Adresse jenseits der Pinning-Grenze wird trotzdem erkannt.
        ips.push(ip("192.168.0.9")?);
        let resolver = FakeResolver::new(Answer::Ips(ips));
        assert_eq!(
            guard_remote_url_with("https://api.example.test", &resolver)
                .await
                .err(),
            Some(UrlGuardError::PrivateAddress)
        );
        Ok(())
    }

    #[tokio::test]
    async fn private_address_at_scan_is_rejected() -> TestResult {
        let resolver = FakeResolver::new(Answer::Ips(vec![ip("192.168.1.10")?]));
        let provider = provider("https://api.example.test/v1")?;
        let result = list_models_guarded(
            "custom-test",
            &provider,
            Some("sk-test-FAKE-0000000000000000"),
            None,
            &resolver,
        )
        .await;
        match result {
            Err(crate::discovery::DiscoveryError::Network { detail }) => {
                assert_eq!(detail, UrlGuardError::PrivateAddress.to_string());
                assert!(!detail.contains("192.168"));
                assert!(!detail.contains("sk-test"));
            }
            Err(other) => return Err(TestError::Unexpected(format!("falscher Fehler: {other}"))),
            Ok(_) => return Err(TestError::Unexpected("Scan trotz privater Adresse".into())),
        }
        assert_eq!(resolver.calls(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn capture_server_receives_nothing() -> TestResult {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(ctx("Auffang-Server binden"))?;
        let local = listener.local_addr().map_err(ctx("Auffang-Adresse"))?;
        let raw = format!("https://api.example.test:{}/v1", local.port());

        let resolver = FakeResolver::new(Answer::Addrs(vec![local]));
        assert_eq!(
            guard_remote_url_with(&raw, &resolver).await.err(),
            Some(UrlGuardError::PrivateAddress)
        );

        // Auch der Scan-Pfad schickt nichts an den Auffang-Server.
        let resolver = FakeResolver::new(Answer::Addrs(vec![local]));
        let provider = provider(&raw)?;
        let scan = list_models_guarded(
            "custom-test",
            &provider,
            Some("sk-test-FAKE-0000000000000000"),
            None,
            &resolver,
        )
        .await;
        assert!(scan.is_err());

        let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "Auffang-Server hat eine Verbindung erhalten"
        );
        Ok(())
    }

    /// Vertrag [OFFEN-12]: ein echter Redirect-Test braucht einen oeffentlich erreichbaren
    /// TLS-Server, den es im Test nicht gibt (Loopback lehnt der Guard ab). Geprueft wird
    /// daher nur, dass der gepinnte Client mit `Policy::none()` baut; die Redirect-Politik
    /// selbst ist in [`GuardedEndpoint::client`] fest verdrahtet.
    #[tokio::test]
    async fn client_does_not_follow_redirects() -> TestResult {
        let resolver = FakeResolver::new(Answer::Ips(vec![public_ip()?]));
        let endpoint = guard_remote_url_with("https://api.example.test/v1", &resolver)
            .await
            .map_err(ctx("Guard"))?;
        endpoint.client().map_err(ctx("Client bauen"))?;
        Ok(())
    }

    #[tokio::test]
    async fn admits_only_same_origin() -> TestResult {
        let resolver = FakeResolver::new(Answer::Ips(vec![public_ip()?]));
        let endpoint = guard_remote_url_with("https://api.example.test/v1", &resolver)
            .await
            .map_err(ctx("Guard"))?;
        assert!(endpoint.admits("https://api.example.test/v1/models"));
        assert!(endpoint.admits("https://API.example.test:443/v1/models?after_id=x"));
        assert!(endpoint.admits("https://api.example.test/other"));
        assert!(!endpoint.admits("http://api.example.test/v1/models"));
        assert!(!endpoint.admits("https://api.example.test:8443/v1/models"));
        assert!(!endpoint.admits("https://other.example.test/v1/models"));
        assert!(!endpoint.admits("https://evil.example.test\\@api.example.test/v1"));
        assert!(!endpoint.admits("https://user@api.example.test/v1"));
        assert!(!endpoint.admits("kein url"));
        Ok(())
    }

    #[tokio::test]
    async fn error_texts_echo_neither_url_nor_ip() -> TestResult {
        let resolver = FakeResolver::new(Answer::Ips(vec![public_ip()?, ip("10.0.0.5")?]));
        let error = guard_remote_url_with("https://api.example.test/secret-path", &resolver)
            .await
            .err()
            .ok_or(TestError::Missing("Guard-Fehler"))?;
        let msg = error.to_string();
        assert!(!msg.contains("10.0.0.5"));
        assert!(!msg.contains(PUBLIC_TEST_IP));
        assert!(!msg.contains("secret-path"));
        assert!(!msg.contains("example.test"));

        for variant in [
            UrlGuardError::TooLong,
            UrlGuardError::Malformed,
            UrlGuardError::NotHttps,
            UrlGuardError::Credentials,
            UrlGuardError::QueryOrFragment,
            UrlGuardError::Placeholder,
            UrlGuardError::BlockedHost,
            UrlGuardError::PrivateAddress,
            UrlGuardError::ResolveFailed,
            UrlGuardError::NoAddress,
            UrlGuardError::ClientBuild,
        ] {
            let text = variant.to_string();
            assert!(!text.is_empty());
            assert!(!text.contains("://"), "{text}");
        }
        Ok(())
    }

    #[test]
    fn forbidden_ip_table_matches_sandbox() -> TestResult {
        assert!(is_forbidden_ip(ip("127.0.0.1")?));
        assert!(is_forbidden_ip(ip("169.254.169.254")?));
        assert!(is_forbidden_ip(ip("::ffff:10.0.0.1")?));
        assert!(is_forbidden_ip(ip("2002:a00:1::")?));
        assert!(!is_forbidden_ip(public_ip()?));
        Ok(())
    }
}
