//! Offenes Recherche-Netz: Domain-Freigaben für `web.fetch`.
//!
//! # Verantwortung
//! Mit `[network].research_web = "open"` dürfen Recherche-Rollen jeden
//! **öffentlichen** DNS-Host lesen (`harw_authority::EgressTarget::PublicDns`
//! im Sandbox-Scope, `harw_egress::EgressPolicy::with_open_public` im
//! Prozess). Dieses Modul hält, welche Domains dafür in dieser Sitzung
//! bereits freigegeben sind, und setzt die Regel „erste Anfrage je Domain
//! fragt“ am Werkzeug selbst durch:
//!
//! 1. **Vor dem Dispatch** fragt die Freigabepolitik
//!    (`harw_registry_defaults::research_web::OpenWebApprovalPolicy`)
//!    [`OpenWebAccess::review_call`]. Eine neue Domain liefert
//!    [`OpenWebReview::NeedsApproval`]; die Politik vermerkt den Aufruf mit
//!    [`OpenWebAccess::approve_call`] und legt ihn der Nutzerin vor (unter
//!    `full` gibt sie die Domain sofort mit [`OpenWebAccess::grant`] frei).
//! 2. **Im Werkzeug** prüft `web.fetch` mit [`OpenWebAccess::admit_fetch`]:
//!    gelisteter Host, freigegebene Domain oder ein vermerkter, also
//!    genehmigter Aufruf (wird verbraucht, die Domain gemerkt). Sonst lehnt
//!    das Werkzeug ab — fail-closed auch in einer Sitzung ohne diese
//!    Freigabepolitik. Ein abgelehnter Aufruf läuft nie; sein Vermerk verfällt.
//! 3. **Bei Weiterleitungen** prüft `web.fetch` jedes Redirect-Ziel mit
//!    [`OpenWebAccess::admit_redirect`]
//!    (`crate::fetch::WebFetcher::with_redirect_approvals`): gelisteter Host,
//!    freigegebene Domain oder kein öffentlicher DNS-Name (dann entscheiden
//!    Egress-Policy und Scope). Eine neue Domain bricht den Abruf ab und wird
//!    genannt, damit das Modell sie selbst anfragt. Dabei wird weder ein
//!    Vermerk verbraucht noch eine Domain gemerkt: das Ziel wählt der Server,
//!    nicht die Nutzerin.
//!
//! Gemerkt wird bis Prozessende (eine harw-Sitzung), wie die
//! Symlink-Freigaben von `harw-tool-fs`. Domain heißt: der Host ohne
//! führendes `www.`; eine Freigabe gilt auch für Subdomains.
//!
//! # Grenzen
//! - Nur `web.fetch` trägt eine frei wählbare URL; `web.search`,
//!   `web.docs_rs` und `web.crates_io` sprechen feste Hosts an.
//! - Weiterleitungen prüft `crate::hop::check_hop` gegen Egress-Policy und
//!   Scope, unter `web.fetch` zusätzlich [`OpenWebAccess::admit_redirect`]:
//!   eine Freigabe gilt der angefragten Domain, nicht jedem Redirect-Ziel;
//!   ein Redirect auf eine noch nicht freigegebene Domain bricht ab, statt zu
//!   fragen (Cache-Treffer werden genauso geprüft).
//! - Gesendet wird nur `GET` ohne Zugangsdaten, Cookies oder eigene Header
//!   (`crate::fetch`); URLs mit Userinfo lehnt der Parser ab.
//!
//! # Nebenläufigkeit
//! [`OpenWebAccess`] ist `Send + Sync` (ein `Mutex`); [`global`] ist die
//! prozessweite Instanz, die Freigabepolitik und Werkzeug teilen.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use harw_authority::{host_matches_suffix, is_public_dns_name};
use harw_egress::EgressUrl;
use serde_json::Value;

/// Das einzige Werkzeug mit frei wählbarer URL.
pub const OPEN_WEB_TOOL: &str = "web.fetch";

/// Wie lange ein vor dem Dispatch vermerkter Aufruf auf seine Ausführung
/// wartet (die Nutzerin kann sich Zeit lassen).
const CALL_APPROVAL_TTL: Duration = Duration::from_secs(15 * 60);

/// Obergrenze offener Vermerke (älteste fallen heraus).
const MAX_PENDING_APPROVALS: usize = 256;

/// Präfix jeder Ablehnung einer noch nicht freigegebenen Domain.
pub const NOT_APPROVED_PREFIX: &str = "Domain für die offene Web-Recherche nicht freigegeben";

/// Einstellungen des offenen Recherche-Netzes (aus `[network]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenWebSettings {
    /// `[network].research_web = "open"`.
    pub open: bool,
    /// Ausdrücklich gelistete Hosts (`allow_hosts`, `researcher_web_hosts`,
    /// `[research].network_allow_hosts`, Such-Backend) — sie fragen nie.
    pub allowlisted: Vec<String>,
}

/// Ergebnis von [`OpenWebAccess::review_url`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenWebReview {
    /// Keine Domain-Freigabe nötig: Modus `allowlist`, anderes Werkzeug,
    /// gelisteter Host oder kein öffentlicher DNS-Name (dann entscheiden
    /// Egress-Policy und Scope wie bisher).
    NotApplicable,
    /// Die Domain ist in dieser Sitzung bereits freigegeben.
    Granted {
        /// Die freigegebene Domain.
        domain: String,
    },
    /// Erste Anfrage an diese Domain: Freigabe nötig (unter `full` ohne
    /// Rückfrage).
    NeedsApproval {
        /// Die Domain, die der Freigabedialog nennt.
        domain: String,
    },
}

/// Ein vor dem Dispatch vermerkter Aufruf.
#[derive(Debug)]
struct Pending {
    url: String,
    domain: String,
    at: Instant,
}

#[derive(Debug, Default)]
struct State {
    settings: OpenWebSettings,
    granted: BTreeSet<String>,
    pending: VecDeque<Pending>,
}

/// Freigabezustand des offenen Recherche-Netzes (siehe Moduldoku).
#[derive(Debug, Default)]
pub struct OpenWebAccess {
    state: Mutex<State>,
}

/// Die prozessweite Instanz, die Freigabepolitik und `web.fetch` teilen.
#[must_use]
pub fn global() -> &'static OpenWebAccess {
    static GLOBAL: OnceLock<OpenWebAccess> = OnceLock::new();
    GLOBAL.get_or_init(OpenWebAccess::default)
}

/// Die Domain, unter der eine Freigabe für `host` gemerkt wird: der
/// normalisierte Host ohne führendes `www.`.
///
/// # Examples
/// ```rust
/// use harw_tool_web::open_web::approval_domain;
///
/// assert_eq!(approval_domain("WWW.Destatis.de."), "destatis.de");
/// assert_eq!(approval_domain("api.github.com"), "api.github.com");
/// ```
#[must_use]
pub fn approval_domain(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    match host.strip_prefix("www.") {
        Some(rest) if rest.contains('.') => rest.to_owned(),
        _ => host,
    }
}

/// Der normalisierte Host einer `http(s)`-URL, sonst `None`.
fn url_host(url: &str) -> Option<String> {
    EgressUrl::parse(url.trim())
        .ok()
        .map(|parsed| parsed.host_str())
}

impl OpenWebAccess {
    // Ein vergifteter Mutex hält nur Listen: der letzte Stand bleibt gültig.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Setzt die Einstellungen (idempotent; bestehende Freigaben bleiben).
    pub fn install(&self, mut settings: OpenWebSettings) {
        settings.allowlisted = settings
            .allowlisted
            .iter()
            .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        settings.allowlisted.sort();
        settings.allowlisted.dedup();
        self.state().settings = settings;
    }

    /// Die aktuellen Einstellungen.
    #[must_use]
    pub fn settings(&self) -> OpenWebSettings {
        self.state().settings.clone()
    }

    /// Ob das offene Recherche-Netz eingeschaltet ist.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.state().settings.open
    }

    /// Ob `host` ausdrücklich gelistet ist (fragt nie).
    #[must_use]
    pub fn is_allowlisted(&self, host: &str) -> bool {
        self.state()
            .settings
            .allowlisted
            .iter()
            .any(|allowed| host_matches_suffix(allowed, host))
    }

    /// Ob die Domain von `host` in dieser Sitzung freigegeben ist (auch als
    /// Subdomain einer freigegebenen Domain).
    #[must_use]
    pub fn is_granted(&self, host: &str) -> bool {
        let domain = approval_domain(host);
        self.state()
            .granted
            .iter()
            .any(|granted| host_matches_suffix(granted, &domain))
    }

    /// Merkt `domain` als freigegeben (bis Prozessende).
    pub fn grant(&self, domain: &str) {
        let domain = approval_domain(domain);
        if !domain.is_empty() {
            self.state().granted.insert(domain);
        }
    }

    /// Alle freigegebenen Domains, sortiert.
    #[must_use]
    pub fn granted_domains(&self) -> Vec<String> {
        self.state().granted.iter().cloned().collect()
    }

    /// Prüft eine URL: braucht sie eine Domain-Freigabe?
    #[must_use]
    pub fn review_url(&self, url: &str) -> OpenWebReview {
        if !self.is_open() {
            return OpenWebReview::NotApplicable;
        }
        let Some(host) = url_host(url) else {
            return OpenWebReview::NotApplicable;
        };
        if self.is_allowlisted(&host) || !is_public_dns_name(&host) {
            return OpenWebReview::NotApplicable;
        }
        let domain = approval_domain(&host);
        if self.is_granted(&host) {
            OpenWebReview::Granted { domain }
        } else {
            OpenWebReview::NeedsApproval { domain }
        }
    }

    /// Wie [`Self::review_url`] für einen Werkzeugaufruf; nur `web.fetch`
    /// (Feld `url`) ist betroffen.
    #[must_use]
    pub fn review_call(&self, tool: &str, arguments: &Value) -> OpenWebReview {
        if tool != OPEN_WEB_TOOL {
            return OpenWebReview::NotApplicable;
        }
        match arguments.get("url").and_then(Value::as_str) {
            Some(url) => self.review_url(url),
            None => OpenWebReview::NotApplicable,
        }
    }

    /// Vermerkt einen Aufruf, den die Freigabepolitik eben der Nutzerin
    /// vorlegt: läuft das Werkzeug genau diesen Aufruf danach, gilt die
    /// Domain als freigegeben. Ein abgelehnter Aufruf läuft nie; sein Vermerk
    /// verfällt nach [`CALL_APPROVAL_TTL`].
    pub fn approve_call(&self, tool: &str, arguments: &Value, domain: &str) {
        if tool != OPEN_WEB_TOOL {
            return;
        }
        let Some(url) = arguments.get("url").and_then(Value::as_str) else {
            return;
        };
        let mut state = self.state();
        let now = Instant::now();
        state
            .pending
            .retain(|pending| now.duration_since(pending.at) < CALL_APPROVAL_TTL);
        while state.pending.len() >= MAX_PENDING_APPROVALS {
            state.pending.pop_front();
        }
        state.pending.push_back(Pending {
            url: url.trim().to_owned(),
            domain: approval_domain(domain),
            at: now,
        });
    }

    /// Die Werkzeug-Seite: darf `web.fetch` die URL jetzt abrufen?
    ///
    /// # Errors
    /// Eine deutsche Meldung mit der Domain, wenn das offene Netz an ist, der
    /// Host weder gelistet noch freigegeben ist und kein genehmigter Vermerk
    /// für genau diesen Aufruf vorliegt.
    pub fn admit_fetch(&self, url: &str) -> Result<(), String> {
        let domain = match self.review_url(url) {
            OpenWebReview::NotApplicable | OpenWebReview::Granted { .. } => return Ok(()),
            OpenWebReview::NeedsApproval { domain } => domain,
        };
        let now = Instant::now();
        let trimmed = url.trim();
        let consumed = {
            let mut state = self.state();
            let position = state.pending.iter().position(|pending| {
                pending.url == trimmed
                    && pending.domain == domain
                    && now.duration_since(pending.at) < CALL_APPROVAL_TTL
            });
            position.and_then(|position| state.pending.remove(position))
        };
        if consumed.is_some() {
            self.grant(&domain);
            return Ok(());
        }
        Err(format!(
            "{NOT_APPROVED_PREFIX}: {domain}. Die erste Anfrage je Domain braucht die Freigabe \
             der Nutzerin (unter `full` automatisch); offenes Web lesen nur Recherche-Rollen \
             (researcher-web, intel-web-researcher, researcher)."
        ))
    }

    /// Die Redirect-Seite: darf `web.fetch` einer Weiterleitung auf `url` folgen?
    ///
    /// Anders als [`Self::admit_fetch`] verbraucht sie keinen Vermerk und gibt
    /// nie frei: vermerkt und freigegeben wird nur, was das Modell selbst
    /// anfragt, nie ein Ziel, das der Server wählt.
    ///
    /// # Errors
    /// Eine deutsche Meldung, beginnend mit [`NOT_APPROVED_PREFIX`] und der
    /// Domain, wenn das offene Netz an ist und die Domain des Ziels weder
    /// gelistet noch freigegeben ist. Sie nennt nur die Domain, nie Pfad oder
    /// Query des Ziels.
    pub fn admit_redirect(&self, url: &str) -> Result<(), String> {
        match self.review_url(url) {
            OpenWebReview::NotApplicable | OpenWebReview::Granted { .. } => Ok(()),
            OpenWebReview::NeedsApproval { domain } => Err(format!(
                "{NOT_APPROVED_PREFIX}: {domain} (Ziel einer Weiterleitung). Eine Freigabe gilt \
                 nur der angefragten Domain, nicht dem Redirect-Ziel: diese Domain zuerst selbst \
                 mit `web.fetch` anfragen (die erste Anfrage je Domain braucht die Freigabe der \
                 Nutzerin, unter `full` automatisch), danach den Abruf wiederholen."
            )),
        }
    }
}

/// Einzeiliger Hinweis für den Freigabedialog (TUI), wenn `tool`/`arguments`
/// eine noch nicht freigegebene Domain des offenen Recherche-Netzes abrufen
/// würde; sonst `None`.
#[must_use]
pub fn approval_notice(access: &OpenWebAccess, tool: &str, arguments: &Value) -> Option<String> {
    match access.review_call(tool, arguments) {
        OpenWebReview::NeedsApproval { domain } => Some(format!(
            "Offenes Web: erster Abruf von Domain {domain} (nur lesend, ohne Zugangsdaten). \
             Eine Freigabe gilt für diese Domain bis zum Ende der Sitzung."
        )),
        OpenWebReview::NotApplicable | OpenWebReview::Granted { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use serde_json::json;

    fn open_access(allowlisted: &[&str]) -> OpenWebAccess {
        let access = OpenWebAccess::default();
        access.install(OpenWebSettings {
            open: true,
            allowlisted: allowlisted.iter().map(|host| (*host).to_owned()).collect(),
        });
        access
    }

    #[test]
    fn allowlist_mode_never_needs_a_domain_approval() {
        let access = OpenWebAccess::default();
        assert!(!access.is_open());
        assert_eq!(
            access.review_url("https://www.destatis.de/"),
            OpenWebReview::NotApplicable
        );
        assert!(access.admit_fetch("https://www.destatis.de/").is_ok());
    }

    #[test]
    fn listed_hosts_and_non_public_names_are_not_domain_gated() {
        let access = open_access(&["docs.rs"]);
        assert_eq!(
            access.review_url("https://static.docs.rs/x.css"),
            OpenWebReview::NotApplicable
        );
        // Private Ziele entscheidet die Egress-Policy (Ablehnung), nie eine
        // Domain-Freigabe.
        for url in ["http://127.0.0.1/", "http://printer.local/", "not a url"] {
            assert_eq!(
                access.review_url(url),
                OpenWebReview::NotApplicable,
                "{url}"
            );
        }
    }

    #[test]
    fn first_request_to_a_domain_needs_approval_and_is_remembered() {
        let access = open_access(&[]);
        let call = json!({ "url": "https://www.destatis.de/DE/Themen/_inhalt.html" });
        assert_eq!(
            access.review_call(OPEN_WEB_TOOL, &call),
            OpenWebReview::NeedsApproval {
                domain: "destatis.de".to_owned()
            }
        );
        // Ohne Vermerk (Sitzung ohne Freigabepolitik) lehnt das Werkzeug ab.
        let refused = access
            .admit_fetch("https://www.destatis.de/DE/Themen/_inhalt.html")
            .err()
            .unwrap_or_default();
        assert!(refused.starts_with(NOT_APPROVED_PREFIX), "{refused}");
        assert!(refused.contains("destatis.de"), "{refused}");

        // Vorgelegt und genehmigt: der Aufruf läuft, die Domain ist gemerkt.
        access.approve_call(OPEN_WEB_TOOL, &call, "destatis.de");
        assert!(
            access
                .admit_fetch("https://www.destatis.de/DE/Themen/_inhalt.html")
                .is_ok()
        );
        assert_eq!(access.granted_domains(), ["destatis.de"]);
        assert_eq!(
            access.review_url("https://destatis.de/andere-seite"),
            OpenWebReview::Granted {
                domain: "destatis.de".to_owned()
            }
        );
        assert_eq!(
            access.review_url("https://service.destatis.de/"),
            OpenWebReview::Granted {
                domain: "service.destatis.de".to_owned()
            }
        );
        // Eine andere Domain fragt erneut.
        assert!(matches!(
            access.review_url("https://www.bundesbank.de/"),
            OpenWebReview::NeedsApproval { .. }
        ));
    }

    #[test]
    fn a_pending_approval_covers_only_its_own_call() {
        let access = open_access(&[]);
        let call = json!({ "url": "https://example.org/a" });
        access.approve_call(OPEN_WEB_TOOL, &call, "example.org");
        assert!(access.admit_fetch("https://example.org/b").is_err());
        assert!(access.admit_fetch("https://example.org/a").is_ok());
        // Danach gilt die Domain.
        assert!(access.admit_fetch("https://example.org/b").is_ok());
    }

    #[test]
    fn full_access_grant_skips_the_question() {
        let access = open_access(&[]);
        access.grant("www.github.com");
        assert!(access.is_granted("github.com"));
        assert!(access.admit_fetch("https://github.com/rust-lang").is_ok());
    }

    #[test]
    fn other_tools_are_never_domain_gated_and_the_notice_names_the_domain() {
        let access = open_access(&[]);
        let call = json!({ "url": "https://www.destatis.de/" });
        assert_eq!(
            access.review_call("web.search", &call),
            OpenWebReview::NotApplicable
        );
        let notice = approval_notice(&access, OPEN_WEB_TOOL, &call).unwrap_or_default();
        assert!(notice.contains("destatis.de"), "{notice}");
        access.grant("destatis.de");
        assert!(approval_notice(&access, OPEN_WEB_TOOL, &call).is_none());
    }

    #[test]
    fn a_redirect_needs_its_own_domain_approval_and_never_grants() -> TestResult {
        let access = open_access(&["docs.rs"]);
        access.grant("destatis.de");
        // Subdomain einer freigegebenen Domain, gelisteter Host, kein
        // öffentlicher Name (dann entscheidet die Egress-Policy).
        for url in [
            "https://service.destatis.de/x",
            "https://static.docs.rs/x.css",
            "http://127.0.0.1/",
        ] {
            access.admit_redirect(url).map_err(TestError::Unexpected)?;
        }

        // Eine neue Domain bricht ab; die Meldung nennt nur die Domain.
        let target = "https://exfil.example.org/leak?q=geheim";
        let Err(refused) = access.admit_redirect(target) else {
            return Err(TestError::Unexpected(
                "Err erwartet: Redirect auf neue Domain".into(),
            ));
        };
        assert!(refused.starts_with(NOT_APPROVED_PREFIX), "{refused}");
        assert!(refused.contains("exfil.example.org"), "{refused}");
        for leaked in ["leak", "geheim"] {
            assert!(!refused.contains(leaked), "{leaked} in {refused}");
        }

        // Ein Vermerk für genau diese URL gibt den Redirect nicht frei, wird
        // von ihm nicht verbraucht, und gemerkt wird nichts.
        access.approve_call(
            OPEN_WEB_TOOL,
            &json!({ "url": target }),
            "exfil.example.org",
        );
        if access.admit_redirect(target).is_ok() {
            return Err(TestError::Unexpected(
                "Err erwartet: ein Vermerk gibt keinen Redirect frei".into(),
            ));
        }
        assert_eq!(access.granted_domains(), ["destatis.de"]);
        // Der Vermerk liegt noch: der vom Modell angefragte Abruf läuft.
        access.admit_fetch(target).map_err(TestError::Unexpected)?;
        Ok(())
    }

    #[test]
    fn allowlist_mode_never_gates_a_redirect() -> TestResult {
        OpenWebAccess::default()
            .admit_redirect("https://exfil.example.org/")
            .map_err(TestError::Unexpected)
    }
}
