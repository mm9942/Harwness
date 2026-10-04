//! Netz-Scope der Rolle `researcher-web` — ausschließlich aus
//! `[network].researcher_web_hosts`.
//!
//! Spezifikationsquelle: Plan W5 RD, Annahme A5; Vertrag `NetworkSection`
//! (ledger W3/C-CFG), `EgressPolicy` (ledger W3/C-EGRESS).
//!
//! # Verantwortlichkeit
//! Dieses Modul übersetzt die Konfiguration in die Egress-Autorität der Rolle
//! `researcher-web`. Weitere Rollen führen ebenfalls `web.*`-Werkzeuge (siehe
//! `crate::profile`); ihr Netz bleibt durch den Sandbox-Scope des Elternteils
//! begrenzt.
//! - [`researcher_web_policy`] baut die `harw_egress::EgressPolicy`, die die
//!   Web-Werkzeuge der Rolle bekommen (`Arc`, geteilt über alle vier Tools).
//! - [`researcher_web_network_scope`] leitet daraus den `NetworkScope` der
//!   Kind-Sandbox ab — aus der **kanonisierten** Allowlist der Policy, damit
//!   Sandbox und Client dieselbe Hostmenge sehen.
//!
//! Die Prozess-Policy der Web-Werkzeuge baut [`install_web_tools`]: sie
//! installiert das Such-Backend (`[web.search]`) und die Einstellungen des
//! offenen Recherche-Netzes und übergibt die Policy über
//! `harw_tool_web::configure`. Diese Policy ist die **Obermenge** aller
//! erlaubten Ziele; welche davon ein einzelner Agent erreicht, bestimmt sein
//! Sandbox-`NetworkScope`.
//!
//! # Regeln
//! - Quelle ist **nur** `researcher_web_hosts`; `allow_hosts` (die allgemeine
//!   Harness-Liste) fließt nicht ein.
//! - Leere Liste = kein Netz: die Policy lässt dann kein Ziel zu
//!   (`EgressPolicy` mit leerer Allowlist, fail-closed) — außer im
//!   **offenen Recherche-Netz** (`[network].research_web = "open"`, Plan R9):
//!   dann zusätzlich jeder öffentliche DNS-Host
//!   (`EgressPolicy::with_open_public`, Scope-Ziel
//!   `harw_authority::EgressTarget::PublicDns`), nur lesend. Die erste Anfrage
//!   je Domain fragt unter `ask`/`auto` die Nutzerin
//!   ([`OpenWebApprovalPolicy`], für die Sitzung gemerkt in
//!   `harw_tool_web::open_web`), unter `full` nicht; offenes Web lesen nur
//!   Recherche-Rollen ([`OPEN_WEB_ROLES`] und davon abgeleitete Agenten wie
//!   `intel-web-researcher`).
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

use harw_authority::{EgressTarget, NetworkScope};
use harw_config::NetworkSection;
use harw_egress::EgressPolicy;
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{ApprovalDecision, ApprovalHandler, ExtFuture, ToolCall};
use harw_tool_web::open_web::{OpenWebAccess, OpenWebReview, OpenWebSettings};

use crate::profile::role_names;

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
        .map_err(|source| RegistryDefaultsError::ResearcherWebPolicy { source })?
        .with_open_public(network.research_web.is_open());
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
    let listed = NetworkScope::from_hosts(policy.allow_hosts().iter().cloned());
    if !policy.open_public() {
        return listed;
    }
    NetworkScope::from_targets(
        listed
            .targets()
            .cloned()
            .chain(std::iter::once(EgressTarget::PublicDns)),
    )
}

/// Die eingebauten Recherche-Rollen, die im offenen Recherche-Netz
/// (`[network].research_web = "open"`) öffentliche Hosts lesen dürfen.
/// Benutzerdefinierte Agenten gelten über ihre Basisrolle
/// (`intel-web-researcher` → `researcher-web`).
pub const OPEN_WEB_ROLES: &[&str] = &[
    role_names::RESEARCHER_WEB,
    role_names::RESEARCHER,
    role_names::DEPENDENCY_RESEARCHER,
];

/// Ob eine (Basis-)Rolle das offene Recherche-Netz nutzen darf.
///
/// # Examples
/// ```rust
/// use harw_registry_defaults::research_web::role_may_use_open_web;
///
/// assert!(role_may_use_open_web("researcher-web"));
/// assert!(!role_may_use_open_web("explorer"));
/// assert!(!role_may_use_open_web("matrix-player"));
/// ```
#[must_use]
pub fn role_may_use_open_web(base_role: &str) -> bool {
    OPEN_WEB_ROLES.contains(&base_role)
}

/// Freigabepolitik des offenen Recherche-Netzes (Plan R9).
///
/// # Beschreibung
/// Betrifft nur `web.fetch` auf einen öffentlichen, nicht gelisteten Host,
/// solange `[network].research_web = "open"` gilt
/// ([`harw_tool_web::open_web::OpenWebAccess::review_call`]); alles andere
/// lässt sie mit [`ApprovalDecision::Allow`] unberührt (die übrige Kette
/// entscheidet wie bisher). Sonst:
/// - Rolle ohne Recht auf offenes Web → immer [`ApprovalDecision::Deny`] mit
///   Hinweis auf die Recherche-Rollen (auch für eine bereits freigegebene
///   Domain: andere Rollen lesen nur gelistete Hosts);
/// - bereits freigegebene Domain → [`ApprovalDecision::Allow`];
/// - [`ApprovalMode::FullAccess`] → Domain sofort freigegeben, keine Frage;
/// - `ask`/`auto` → der Aufruf wird vermerkt
///   ([`OpenWebAccess::approve_call`]) und der Nutzerin vorgelegt
///   ([`ApprovalDecision::AskUser`]); führt das Werkzeug ihn aus, ist die
///   Domain für die Sitzung gemerkt. Der Dialog nennt die Domain
///   ([`open_web_approval_notice`]).
///
/// Die Aggregation `Deny` > `AskUser` > `Allow` der Kette sorgt dafür, dass
/// diese Politik nie etwas freigibt, was eine andere ablehnt.
#[derive(Debug)]
pub struct OpenWebApprovalPolicy {
    mode: ApprovalModeCell,
    research_role: bool,
    access: &'static OpenWebAccess,
}

impl OpenWebApprovalPolicy {
    /// Politik über der Modus-Zelle einer Sitzung, gegen den prozessweiten
    /// Freigabezustand ([`harw_tool_web::open_web::global`]).
    ///
    /// # Arguments
    /// - `mode`: die Freigabemodus-Zelle der Sitzung (live gelesen).
    /// - `research_role`: [`role_may_use_open_web`] der Basisrolle.
    #[must_use]
    pub fn new(mode: ApprovalModeCell, research_role: bool) -> Self {
        Self::with_access(mode, research_role, harw_tool_web::open_web::global())
    }

    /// Wie [`Self::new`], mit eigenem Freigabezustand (Tests).
    #[must_use]
    pub fn with_access(
        mode: ApprovalModeCell,
        research_role: bool,
        access: &'static OpenWebAccess,
    ) -> Self {
        Self {
            mode,
            research_role,
            access,
        }
    }

    /// Die synchrone Entscheidung (siehe Typdoku).
    #[must_use]
    pub fn decide(&self, call: &ToolCall) -> ApprovalDecision {
        let (domain, granted) = match self.access.review_call(call.name.as_str(), &call.arguments) {
            OpenWebReview::NotApplicable => return ApprovalDecision::Allow,
            OpenWebReview::Granted { domain } => (domain, true),
            OpenWebReview::NeedsApproval { domain } => (domain, false),
        };
        // Auch eine bereits (von einer Recherche-Rolle) freigegebene Domain
        // bleibt für andere Rollen zu: sie lesen nur gelistete Hosts.
        if !self.research_role {
            return ApprovalDecision::Deny(format!(
                "Offenes Web ({domain}) lesen nur Recherche-Rollen (researcher-web, \
                 intel-web-researcher, researcher, dependency-researcher) — delegiere die \
                 Frage an eine davon oder nutze einen gelisteten Host."
            ));
        }
        if granted {
            return ApprovalDecision::Allow;
        }
        if self.mode.get() == ApprovalMode::FullAccess {
            self.access.grant(&domain);
            return ApprovalDecision::Allow;
        }
        self.access
            .approve_call(call.name.as_str(), &call.arguments, &domain);
        ApprovalDecision::AskUser(Default::default())
    }
}

impl ApprovalHandler for OpenWebApprovalPolicy {
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        let decision = self.decide(call);
        Box::pin(async move { decision })
    }

    fn kind(&self) -> ApprovalHandlerKind {
        ApprovalHandlerKind::Other
    }

    fn label(&self) -> &'static str {
        "open-web"
    }
}

/// Hinweis für den Freigabedialog: nennt die Domain, wenn `call` die erste
/// Anfrage an eine Domain des offenen Recherche-Netzes ist; sonst `None`.
#[must_use]
pub fn open_web_approval_notice(call: &ToolCall) -> Option<String> {
    harw_tool_web::open_web::approval_notice(
        harw_tool_web::open_web::global(),
        call.name.as_str(),
        &call.arguments,
    )
}

/// Richtet die Netz-Werkzeuge (`web.fetch`, `web.docs_rs`, `web.crates_io`,
/// `web.search`) einmal je Prozess ein.
///
/// # Beschreibung
/// Ohne diesen Aufruf scheitert jeder Abruf mit `NotConfigured` (fail-closed).
/// Die Prozess-Policy ist die **Obermenge** der erlaubten Ziele:
/// `[network].allow_hosts` ∪ `[network].researcher_web_hosts` ∪
/// `[research].network_allow_hosts` ∪ der Host des konfigurierten
/// Such-Backends. Welche Ziele ein einzelner Agent tatsächlich erreicht,
/// bestimmt weiterhin sein Sandbox-`NetworkScope` (Schnittmenge je Aufruf).
/// Limits und Cache-TTL kommen aus `[research]`, das Such-Backend aus
/// `[web.search]` (Schlüssel nur aus der Umgebung).
///
/// Ein zweiter Aufruf im selben Prozess (z. B. nach `/resume`) ist ein
/// No-op für den Fetcher; die Such-Konfiguration wird aktualisiert.
///
/// # Fehler
/// [`RegistryDefaultsError::ResearcherWebPolicy`] bei einem ungültigen
/// Host-Eintrag.
pub fn install_web_tools(
    config: &harw_config::ResolvedConfig,
    cache_root: &std::path::Path,
) -> RegistryDefaultsResult<()> {
    use harw_tool_web::search::{SearchBackend, WebSearchConfig, install_search_config};

    let search = &config.web.search;
    let backend = match search.provider.as_str() {
        "brave" => SearchBackend::Brave,
        "tavily" => SearchBackend::Tavily,
        "searxng" => SearchBackend::Searxng,
        _ => SearchBackend::DuckDuckGo,
    };
    let search_host = match backend {
        SearchBackend::Brave => Some("api.search.brave.com".to_owned()),
        SearchBackend::Tavily => Some("api.tavily.com".to_owned()),
        SearchBackend::DuckDuckGo => Some("html.duckduckgo.com".to_owned()),
        SearchBackend::Searxng => search.endpoint.as_deref().and_then(url_host),
    };
    install_search_config(WebSearchConfig {
        provider: backend,
        endpoint: search.endpoint.clone(),
        api_key: search
            .api_key_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|key| !key.trim().is_empty()),
        max_results: search.max_results,
    });

    let mut hosts: Vec<String> = config
        .network
        .allow_hosts
        .iter()
        .chain(config.network.researcher_web_hosts.iter())
        .chain(config.harness.research.network_allow_hosts.iter())
        .cloned()
        .chain(search_host)
        .collect();
    hosts.sort();
    hosts.dedup();
    let open = config.network.research_web.is_open();
    // Plan R9: gelistete Hosts fragen im offenen Recherche-Netz nie.
    harw_tool_web::open_web::global().install(OpenWebSettings {
        open,
        allowlisted: hosts.clone(),
    });
    let policy = EgressPolicy::new(hosts, config.network.allow_private)
        .map_err(|source| RegistryDefaultsError::ResearcherWebPolicy { source })?
        .with_open_public(open);
    let research = &config.harness.research;
    let options = harw_tool_web::WebFetchOptions {
        ttl: std::time::Duration::from_secs(research.cache_ttl_secs),
        max_bytes: research.max_fetch_bytes,
        ..harw_tool_web::WebFetchOptions::default()
    };
    if let Err(error) = harw_tool_web::configure(Arc::new(policy), cache_root.join("web"), options)
    {
        tracing::debug!(%error, "web_tools.already_configured");
    }
    Ok(())
}

/// Host-Anteil einer `http(s)://host[:port]/…`-URL (ohne Userinfo).
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn section(allow_hosts: &[&str], allow_private: bool, researcher: &[&str]) -> NetworkSection {
        NetworkSection {
            allow_hosts: allow_hosts.iter().map(|host| (*host).to_owned()).collect(),
            allow_private,
            researcher_web_hosts: researcher.iter().map(|host| (*host).to_owned()).collect(),
            ..NetworkSection::default()
        }
    }

    fn open_section(researcher: &[&str]) -> NetworkSection {
        NetworkSection {
            research_web: harw_config::ResearchWebMode::Open,
            ..section(&[], true, researcher)
        }
    }

    fn call(tool: &str, url: &str) -> ToolCall {
        ToolCall {
            id: Default::default(),
            name: harw_extension_api::ToolName::new(tool),
            arguments: serde_json::json!({ "url": url }),
        }
    }

    /// Ein eigener, geleakter Freigabezustand je Test (die Politik hält
    /// `&'static`, wie der prozessweite).
    fn open_access(allowlisted: &[&str]) -> &'static OpenWebAccess {
        let access: &'static OpenWebAccess = Box::leak(Box::default());
        access.install(OpenWebSettings {
            open: true,
            allowlisted: allowlisted.iter().map(|host| (*host).to_owned()).collect(),
        });
        access
    }

    #[test]
    fn test_open_mode_allows_a_public_host_and_refuses_private_and_loopback() -> TestResult {
        let policy = researcher_web_policy(&open_section(&[])).map_err(ctx("offen"))?;
        assert!(policy.open_public());
        assert!(
            !policy.allow_private(),
            "Recherche nie privat, auch mit allow_private"
        );
        assert!(policy.check_url("https://www.destatis.de/DE/Home/").is_ok());
        for url in [
            "http://127.0.0.1/",
            "http://localhost:8080/",
            "http://10.1.2.3/",
            "http://169.254.169.254/latest/meta-data/",
            "http://printer.local/",
        ] {
            assert!(policy.check_url(url).is_err(), "{url}");
        }
        let scope = researcher_web_network_scope(&policy);
        assert!(scope.allows_public_dns());
        assert!(scope.allows("www.destatis.de"));
        assert!(!scope.allows("127.0.0.1"));
        assert!(!scope.allows("printer.local"));
        Ok(())
    }

    #[test]
    fn test_allowlist_mode_is_unchanged() -> TestResult {
        let policy =
            researcher_web_policy(&section(&[], false, &["docs.rs"])).map_err(ctx("allowlist"))?;
        assert!(!policy.open_public());
        assert!(policy.check_url("https://docs.rs/").is_ok());
        assert!(policy.check_url("https://www.destatis.de/").is_err());
        let scope = researcher_web_network_scope(&policy);
        assert!(!scope.allows_public_dns());
        assert!(!scope.allows("www.destatis.de"));
        // Allowlist-Modus: die Freigabepolitik greift nie ein.
        let access: &'static OpenWebAccess = Box::leak(Box::default());
        let gate = OpenWebApprovalPolicy::with_access(ApprovalModeCell::default(), true, access);
        assert!(matches!(
            gate.decide(&call("web.fetch", "https://www.destatis.de/")),
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    #[test]
    fn test_first_request_per_domain_asks_and_is_remembered() {
        let access = open_access(&["docs.rs"]);
        let mode = ApprovalModeCell::default();
        assert_ne!(mode.get(), ApprovalMode::FullAccess);
        let gate = OpenWebApprovalPolicy::with_access(mode, true, access);
        let first = call("web.fetch", "https://www.destatis.de/DE/Home/");
        assert!(matches!(gate.decide(&first), ApprovalDecision::AskUser(_)));
        // Gelistete Hosts und andere Werkzeuge fragen nie.
        assert!(matches!(
            gate.decide(&call("web.fetch", "https://docs.rs/serde")),
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            gate.decide(&call("web.search", "https://www.destatis.de/")),
            ApprovalDecision::Allow
        ));
        // Genehmigt: das Werkzeug führt genau diesen Aufruf aus …
        assert!(
            access
                .admit_fetch("https://www.destatis.de/DE/Home/")
                .is_ok()
        );
        // … und die Domain ist für die Sitzung gemerkt.
        assert!(matches!(
            gate.decide(&call("web.fetch", "https://destatis.de/andere-seite")),
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            gate.decide(&call("web.fetch", "https://www.bundesbank.de/")),
            ApprovalDecision::AskUser(_)
        ));
    }

    #[test]
    fn test_full_access_does_not_ask_for_a_new_domain() {
        let access = open_access(&[]);
        let mode = ApprovalModeCell::default();
        mode.set(ApprovalMode::FullAccess);
        let gate = OpenWebApprovalPolicy::with_access(mode, true, access);
        assert!(matches!(
            gate.decide(&call("web.fetch", "https://github.com/rust-lang/rust")),
            ApprovalDecision::Allow
        ));
        assert!(
            access
                .admit_fetch("https://github.com/rust-lang/rust")
                .is_ok()
        );
        assert_eq!(access.granted_domains(), ["github.com"]);
    }

    #[test]
    fn test_only_research_roles_may_read_the_open_web() -> TestResult {
        let access = open_access(&[]);
        let mode = ApprovalModeCell::default();
        mode.set(ApprovalMode::FullAccess);
        let gate = OpenWebApprovalPolicy::with_access(mode, false, access);
        let decision = gate.decide(&call("web.fetch", "https://www.destatis.de/"));
        let ApprovalDecision::Deny(reason) = decision else {
            return Err(TestError::Unexpected(
                "eine Nicht-Recherche-Rolle darf kein offenes Web lesen".to_owned(),
            ));
        };
        assert!(reason.contains("intel-web-researcher"), "{reason}");
        assert!(access.granted_domains().is_empty());
        // Auch eine von einer Recherche-Rolle freigegebene Domain bleibt zu.
        access.grant("destatis.de");
        assert!(matches!(
            gate.decide(&call("web.fetch", "https://www.destatis.de/")),
            ApprovalDecision::Deny(_)
        ));
        // Gelistete Hosts lesen alle Rollen mit Netz wie bisher.
        let listed = open_access(&["docs.rs"]);
        let gate = OpenWebApprovalPolicy::with_access(ApprovalModeCell::default(), false, listed);
        assert!(matches!(
            gate.decide(&call("web.fetch", "https://docs.rs/serde")),
            ApprovalDecision::Allow
        ));
        for role in OPEN_WEB_ROLES {
            assert!(role_may_use_open_web(role), "{role}");
        }
        for role in [
            role_names::EXPLORER,
            role_names::UIA_WORKER,
            role_names::MATRIX_PLAYER,
            role_names::MATRIX_UMPIRE,
            role_names::MATRIX_GAME_MASTER,
        ] {
            assert!(!role_may_use_open_web(role), "{role}");
        }
        Ok(())
    }

    #[test]
    fn test_url_host_extracts_plain_host() {
        assert_eq!(
            url_host("https://search.example.org:8443/x").as_deref(),
            Some("search.example.org")
        );
        assert_eq!(
            url_host("https://u:p@Host.test").as_deref(),
            Some("host.test")
        );
        assert_eq!(url_host("https://"), None);
    }

    #[test]
    fn test_researcher_web_policy_empty_hosts_means_no_network() -> TestResult {
        let policy = researcher_web_policy(&section(&[], false, &[]))
            .map_err(ctx("leere Liste ist gültig"))?;
        assert!(policy.allow_hosts().is_empty());
        assert!(policy.check_url("https://docs.rs/").is_err());
        assert!(researcher_web_network_scope(&policy).is_empty());
        Ok(())
    }

    #[test]
    fn test_researcher_web_policy_ignores_general_allow_hosts() -> TestResult {
        let policy = researcher_web_policy(&section(&["docs.rs", "example.com"], false, &[]))
            .map_err(ctx("gültig"))?;
        assert!(
            policy.check_url("https://docs.rs/").is_err(),
            "allow_hosts darf den Scope von researcher-web nicht erweitern"
        );
        Ok(())
    }

    #[test]
    fn test_researcher_web_policy_never_allows_private_networks() -> TestResult {
        let policy = researcher_web_policy(&section(&[], true, &["docs.rs", "127.0.0.1"]))
            .map_err(ctx("gültig"))?;
        assert!(!policy.allow_private());
        assert!(policy.check_url("https://docs.rs/serde").is_ok());
        assert!(
            policy.check_url("http://127.0.0.1/").is_err(),
            "Loopback bleibt gesperrt, auch wenn [network].allow_private = true"
        );
        Ok(())
    }

    #[test]
    fn test_researcher_web_policy_rejects_invalid_entry() -> TestResult {
        let Err(error) = researcher_web_policy(&section(&[], false, &["https://docs.rs"])) else {
            return Err(TestError::Unexpected(
                "Schema im Hosteintrag ist ungültig".to_owned(),
            ));
        };
        assert!(matches!(
            error,
            RegistryDefaultsError::ResearcherWebPolicy { .. }
        ));
        assert!(std::error::Error::source(&error).is_some());
        Ok(())
    }

    #[test]
    fn test_researcher_web_network_scope_matches_policy_hosts() -> TestResult {
        let policy = researcher_web_policy(&section(&[], false, &["crates.io", "Docs.RS"]))
            .map_err(ctx("gültig"))?;
        let scope = researcher_web_network_scope(&policy);
        let hosts: Vec<&str> = scope.hosts().collect();
        assert_eq!(hosts, vec!["crates.io", "docs.rs"]);
        assert!(scope.allows("docs.rs"));
        assert!(!scope.allows("example.com"));
        Ok(())
    }
}
