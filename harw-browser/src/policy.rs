//! # policy
//!
//! ## Responsibility
//! This module owns the **security policy vocabulary** for opening and
//! driving a browser session: the same-origin navigation allowlist
//! ([`OriginPolicy`] built from [`OriginRule`]s), the private-network veto,
//! the host-owned resource limits ([`BrowserLimits`]), the profile
//! persistence choice ([`ProfilePolicy`]), and the complete host-authorized
//! open request ([`OpenBrowserRequest`]). It does not enforce anything at
//! runtime by itself: backends (`harw-browser-thirtyfour`) and the tool layer
//! (`harw-tool-browser`) call the `check_*`/`validate` functions defined here
//! before and after every navigation-capable operation.
//!
//! ## Origin semantics (remediation C-BROWSER, F-009, F-114)
//! - An origin is the triple **scheme + host + port** (port defaulted per
//!   scheme). Only `http` and `https` can ever be allowed.
//! - A rule `https://erp.example.com` matches exactly that origin — never a
//!   subdomain, never `http://`, never another port.
//! - Subdomains are opt-in with a leading wildcard label:
//!   `https://*.example.com` matches `a.example.com` and `b.a.example.com`
//!   but **not** the apex `example.com` (add a second exact rule for that).
//!   Wildcards on IP literals or on single-label suffixes (`*.com`) are
//!   rejected.
//! - URLs carrying credentials (`https://user:pw@host/`) are never allowed.
//! - With `deny_private_networks`, loopback, RFC 1918, link-local (including
//!   cloud metadata `169.254.169.254`), CGNAT, unspecified, ULA, IPv4-mapped
//!   and `localhost`/`*.localhost` hosts are vetoed even when listed. DNS
//!   names resolving to such addresses are the egress layer's concern.
//! - [`OpenBrowserRequest::authentication_origins`] are **not** navigation
//!   targets: they are accepted only as *observed* locations (redirects during
//!   login) via [`OpenBrowserRequest::check_observed_location`].
//!
//! Host comparison mirrors `harw_sandbox::egress::host_matches_suffix`
//! (ASCII case-insensitive, one trailing dot tolerated, label boundary
//! required, IP literals exact). It is re-implemented here because this crate
//! is the dependency-free L0 contract layer and must not depend on
//! `harw-sandbox`.
//!
//! ## Key types exported
//! - [`OriginRule`], [`OriginScheme`] — one validated allowlist entry.
//! - [`OriginPolicy`] — allowlist plus private-network veto.
//! - [`BrowserLimits`] — clamped per-session resource ceilings.
//! - [`ProfilePolicy`], [`Viewport`], [`BiDiRequirement`].
//! - [`OpenBrowserRequest`] — host-authorized open request.
//!
//! ## Concurrency
//! Pure data and pure functions; every type is `Send + Sync`, no locking.
//!
//! ## Errors
//! - [`Error::InvalidArgument`] for malformed rules, out-of-range limits,
//!   invalid viewports or profile bindings.
//! - [`Error::OriginNotAllowed`] when a URL is outside the policy.
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::policy::OriginPolicy;
//!
//! # fn main() -> harw_browser::Result<()> {
//! let policy = OriginPolicy::from_origins(["https://erp.example.com"], true)?;
//! let url = url::Url::parse("https://erp.example.com/login")?;
//! policy.check(&url)?;
//! assert!(!policy.is_allowed(&url::Url::parse("http://erp.example.com/")?));
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::error::{Error, Result};

/// Maximum number of rules a single [`OriginPolicy`] may hold.
pub const MAX_ORIGIN_RULES: usize = 64;
/// Maximum byte length of one textual origin rule.
pub const MAX_ORIGIN_RULE_BYTES: usize = 512;
/// Maximum byte length of a persistent profile binding name.
pub const MAX_PROFILE_BINDING_BYTES: usize = 128;
/// Maximum width or height of a requested viewport, in CSS pixels.
pub const MAX_VIEWPORT_DIMENSION: u32 = 8_192;

/// Default ceiling for actions per browser session.
pub const DEFAULT_MAX_ACTIONS_PER_SESSION: u32 = 200;
/// Absolute ceiling for actions per browser session.
pub const HARD_MAX_ACTIONS_PER_SESSION: u32 = 5_000;
/// Default ceiling for a single wait, in milliseconds.
pub const DEFAULT_MAX_WAIT_MS: u64 = 30_000;
/// Absolute ceiling for a single wait, in milliseconds.
pub const HARD_MAX_WAIT_MS: u64 = 120_000;
/// Default ceiling for typed text / match patterns, in bytes.
pub const DEFAULT_MAX_TEXT_BYTES: usize = 4_096;
/// Absolute ceiling for typed text / match patterns, in bytes.
pub const HARD_MAX_TEXT_BYTES: usize = 65_536;
/// Default ceiling for one selector string, in bytes.
pub const DEFAULT_MAX_SELECTOR_BYTES: usize = 512;
/// Absolute ceiling for one selector string, in bytes.
pub const HARD_MAX_SELECTOR_BYTES: usize = 4_096;
/// Default ceiling for fallback selectors per target.
pub const DEFAULT_MAX_SELECTOR_FALLBACKS: usize = 4;
/// Absolute ceiling for fallback selectors per target.
pub const HARD_MAX_SELECTOR_FALLBACKS: usize = 16;
/// Default ceiling for a navigation URL, in bytes.
pub const DEFAULT_MAX_URL_BYTES: usize = 2_048;
/// Absolute ceiling for a navigation URL, in bytes.
pub const HARD_MAX_URL_BYTES: usize = 8_192;

/// How a browser profile is provisioned for a session.
///
/// # Description
/// `Ephemeral` starts from an empty profile that is discarded on close.
/// `Persistent` seeds the profile from a host-configured `binding`; the
/// binding name is host-owned and never chosen by the model.
///
/// # Concurrency
/// Plain data, `Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::policy::ProfilePolicy;
///
/// assert!(ProfilePolicy::Ephemeral.validate().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ProfilePolicy {
    Ephemeral,
    Persistent { binding: String },
}

impl ProfilePolicy {
    /// Validates the profile binding name.
    ///
    /// # Returns
    /// `Ok(())` for `Ephemeral` or a well-formed binding.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: binding empty, longer than
    ///   [`MAX_PROFILE_BINDING_BYTES`], or containing characters outside
    ///   `[A-Za-z0-9._-]`, or equal to `.`/`..`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::ProfilePolicy;
    ///
    /// let bad = ProfilePolicy::Persistent { binding: "../x".to_owned() };
    /// assert!(bad.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<()> {
        let Self::Persistent { binding } = self else {
            return Ok(());
        };
        let well_formed = !binding.is_empty()
            && binding.len() <= MAX_PROFILE_BINDING_BYTES
            && binding != "."
            && binding != ".."
            && binding
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if well_formed {
            Ok(())
        } else {
            Err(Error::InvalidArgument {
                detail: format!(
                    "profile binding must be 1..={MAX_PROFILE_BINDING_BYTES} bytes of [A-Za-z0-9._-] and not '.' or '..'"
                ),
            })
        }
    }
}

/// Requested browser viewport size in CSS pixels.
///
/// # Concurrency
/// Plain data, `Send + Sync`, `Copy`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::policy::Viewport;
///
/// assert!(Viewport { width: 1280, height: 720 }.validate().is_ok());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    /// Checks that both dimensions are within `1..=MAX_VIEWPORT_DIMENSION`.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: a dimension is zero or too large.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::Viewport;
    ///
    /// assert!(Viewport { width: 0, height: 720 }.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<()> {
        let valid = |d: u32| (1..=MAX_VIEWPORT_DIMENSION).contains(&d);
        if valid(self.width) && valid(self.height) {
            Ok(())
        } else {
            Err(Error::InvalidArgument {
                detail: format!(
                    "viewport {}x{} outside 1..={MAX_VIEWPORT_DIMENSION}",
                    self.width, self.height
                ),
            })
        }
    }
}

/// Whether the session requires WebDriver BiDi support.
///
/// # Concurrency
/// Plain data, `Send + Sync`, `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum BiDiRequirement {
    Required,
    Preferred,
    NotRequired,
}

/// URL scheme an [`OriginRule`] can admit. Only web schemes are
/// representable; `file`, `data`, `javascript`, `ftp`, `ws` etc. can never be
/// allowed.
///
/// # Concurrency
/// Plain data, `Send + Sync`, `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OriginScheme {
    Http,
    Https,
}

impl OriginScheme {
    /// Returns the lowercase scheme name as used in URLs.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::OriginScheme;
    ///
    /// assert_eq!(OriginScheme::Https.as_str(), "https");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    /// Returns the scheme's default port (80 or 443).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::OriginScheme;
    ///
    /// assert_eq!(OriginScheme::Http.default_port(), 80);
    /// ```
    pub fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

// Normalized host of a rule: lowercase domain without trailing dot, or an IP.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RuleHost {
    Domain(String),
    Ip(IpAddr),
}

/// One validated same-origin allowlist entry (scheme + host + port, with an
/// optional leading-wildcard subdomain opt-in).
///
/// # Description
/// Textual form: `http[s]://host[:port][/]` or `http[s]://*.domain[:port][/]`.
/// The scheme must be lowercase `http`/`https`; paths other than a single
/// trailing `/`, queries, fragments and userinfo are rejected. Hosts are
/// normalized with the WHATWG URL parser (IDNA, lowercase, IPv4 forms).
/// Serialized as the canonical string (default ports omitted).
///
/// # Concurrency
/// Plain data, `Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::policy::OriginRule;
///
/// # fn main() -> harw_browser::Result<()> {
/// let rule = OriginRule::parse("https://*.example.com")?;
/// assert!(rule.matches(&url::Url::parse("https://a.example.com/x")?));
/// assert!(!rule.matches(&url::Url::parse("https://example.com/")?));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OriginRule {
    scheme: OriginScheme,
    host: RuleHost,
    port: u16,
    include_subdomains: bool,
}

impl OriginRule {
    /// Parses and validates one textual origin rule.
    ///
    /// # Arguments
    /// - `input` (`&str`): the rule text, borrowed.
    ///
    /// # Returns
    /// The normalized [`OriginRule`].
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: empty/too long, scheme not `http://` or
    ///   `https://`, path/query/fragment/userinfo present, misplaced `*`,
    ///   wildcard on an IP literal or single-label suffix, or unparsable host.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::OriginRule;
    ///
    /// assert!(OriginRule::parse("https://erp.example.com:8443").is_ok());
    /// assert!(OriginRule::parse("erp.example.com").is_err());
    /// ```
    pub fn parse(input: &str) -> Result<Self> {
        let invalid = |reason: &str| Error::InvalidArgument {
            detail: format!("invalid origin rule: {reason}"),
        };
        if input.is_empty() || input.len() > MAX_ORIGIN_RULE_BYTES {
            return Err(invalid("must be 1..=512 bytes"));
        }
        let (scheme, rest) = if let Some(rest) = input.strip_prefix("https://") {
            (OriginScheme::Https, rest)
        } else if let Some(rest) = input.strip_prefix("http://") {
            (OriginScheme::Http, rest)
        } else {
            return Err(invalid("scheme must be lowercase http:// or https://"));
        };
        let (include_subdomains, authority) = match rest.strip_prefix("*.") {
            Some(tail) => (true, tail),
            None => (false, rest),
        };
        let authority = authority.strip_suffix('/').unwrap_or(authority);
        if authority.is_empty() {
            return Err(invalid("host is missing"));
        }
        if authority.contains(['*', '@', '/', '?', '#', '\\', ' ']) {
            return Err(invalid(
                "only scheme, host and port are allowed; '*' only as leading label",
            ));
        }
        let parsed = url::Url::parse(&format!("{}://{authority}/", scheme.as_str()))
            .map_err(|error| invalid(&format!("unparsable host or port ({error})")))?;
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| invalid("port cannot be determined"))?;
        let host = match parsed.host() {
            Some(url::Host::Domain(domain)) => {
                let domain = domain.strip_suffix('.').unwrap_or(domain);
                if domain.is_empty() {
                    return Err(invalid("host is empty"));
                }
                RuleHost::Domain(domain.to_ascii_lowercase())
            }
            Some(url::Host::Ipv4(v4)) => RuleHost::Ip(IpAddr::V4(v4)),
            Some(url::Host::Ipv6(v6)) => RuleHost::Ip(IpAddr::V6(v6)),
            None => return Err(invalid("host is missing")),
        };
        if include_subdomains {
            match &host {
                RuleHost::Ip(_) => return Err(invalid("wildcard not allowed on IP literals")),
                RuleHost::Domain(domain) if !domain.contains('.') => {
                    return Err(invalid("wildcard suffix needs at least two labels"));
                }
                RuleHost::Domain(_) => {}
            }
        }
        Ok(Self {
            scheme,
            host,
            port,
            include_subdomains,
        })
    }

    /// Returns the rule's scheme.
    pub fn scheme(&self) -> OriginScheme {
        self.scheme
    }

    /// Returns the rule's effective port (explicit or scheme default).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns whether the rule admits strict subdomains instead of the exact host.
    pub fn include_subdomains(&self) -> bool {
        self.include_subdomains
    }

    /// Returns whether `url`'s origin is admitted by this rule.
    ///
    /// # Description
    /// Scheme and effective port must be equal; hosts are compared as
    /// described in the module docs. Credentials in `url` never match.
    /// The private-network veto is **not** applied here (see
    /// [`OriginPolicy::is_allowed`]).
    ///
    /// # Arguments
    /// - `url` (`&url::Url`): the candidate location, borrowed.
    ///
    /// # Returns
    /// `true` if the origin matches.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::OriginRule;
    ///
    /// # fn main() -> harw_browser::Result<()> {
    /// let rule = OriginRule::parse("https://erp.example.com")?;
    /// assert!(!rule.matches(&url::Url::parse("https://erp.example.com:8443/")?));
    /// # Ok(())
    /// # }
    /// ```
    pub fn matches(&self, url: &url::Url) -> bool {
        if url.scheme() != self.scheme.as_str()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port_or_known_default() != Some(self.port)
        {
            return false;
        }
        match (url.host(), &self.host) {
            (Some(url::Host::Domain(domain)), RuleHost::Domain(rule)) => {
                domain_matches(rule, domain, self.include_subdomains)
            }
            (Some(url::Host::Ipv4(v4)), RuleHost::Ip(IpAddr::V4(rule))) => v4 == *rule,
            (Some(url::Host::Ipv6(v6)), RuleHost::Ip(IpAddr::V6(rule))) => v6 == *rule,
            _ => false,
        }
    }
}

// Exact or strict-subdomain comparison, ASCII case-insensitive, one trailing
// dot tolerated, label boundary required. Mirrors
// `harw_sandbox::egress::host_matches_suffix` minus the IP branch (handled by
// the caller) and plus the apex exclusion for wildcard rules.
fn domain_matches(rule: &str, host: &str, include_subdomains: bool) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || rule.is_empty() {
        return false;
    }
    if !include_subdomains {
        return host.eq_ignore_ascii_case(rule);
    }
    if host.len() <= rule.len() + 1 {
        return false;
    }
    let start = host.len() - rule.len();
    let Some(suffix) = host.get(start..) else {
        return false;
    };
    suffix.eq_ignore_ascii_case(rule) && host.as_bytes().get(start - 1) == Some(&b'.')
}

impl fmt::Display for OriginRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.scheme.as_str())?;
        if self.include_subdomains {
            f.write_str("*.")?;
        }
        match &self.host {
            RuleHost::Domain(domain) => f.write_str(domain)?,
            RuleHost::Ip(IpAddr::V4(v4)) => write!(f, "{v4}")?,
            RuleHost::Ip(IpAddr::V6(v6)) => write!(f, "[{v6}]")?,
        }
        if self.port != self.scheme.default_port() {
            write!(f, ":{}", self.port)?;
        }
        Ok(())
    }
}

impl TryFrom<String> for OriginRule {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl From<OriginRule> for String {
    fn from(rule: OriginRule) -> Self {
        rule.to_string()
    }
}

// Deserialization shape of `OriginPolicy`; converted with validation.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginPolicyRepr {
    allow: Vec<OriginRule>,
    deny_private_networks: bool,
}

/// Same-origin navigation allowlist plus an independent private-network veto.
///
/// # Description
/// Holds at most [`MAX_ORIGIN_RULES`] validated [`OriginRule`]s. A URL is
/// allowed only if (a) its scheme is `http`/`https`, (b) it carries no
/// credentials, (c) the private-network veto does not apply, and (d) at least
/// one rule matches. The default policy is empty with the veto enabled, i.e.
/// deny-all.
///
/// # Concurrency
/// Plain data, `Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::policy::OriginPolicy;
///
/// # fn main() -> harw_browser::Result<()> {
/// let policy = OriginPolicy::from_origins(["https://*.example.com"], true)?;
/// assert!(policy.is_allowed(&url::Url::parse("https://erp.example.com/")?));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "OriginPolicyRepr")]
pub struct OriginPolicy {
    allow: Vec<OriginRule>,
    deny_private_networks: bool,
}

impl Default for OriginPolicy {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            deny_private_networks: true,
        }
    }
}

impl TryFrom<OriginPolicyRepr> for OriginPolicy {
    type Error = Error;

    fn try_from(repr: OriginPolicyRepr) -> Result<Self> {
        Self::new(repr.allow, repr.deny_private_networks)
    }
}

impl OriginPolicy {
    /// Builds a policy from already-validated rules.
    ///
    /// # Arguments
    /// - `allow` (`Vec<OriginRule>`): the rules, ownership transferred;
    ///   duplicates are removed preserving first occurrence.
    /// - `deny_private_networks` (`bool`): enables the private-network veto.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: more than [`MAX_ORIGIN_RULES`] rules.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::{OriginPolicy, OriginRule};
    ///
    /// # fn main() -> harw_browser::Result<()> {
    /// let policy = OriginPolicy::new(vec![OriginRule::parse("https://a.example")?], true)?;
    /// assert_eq!(policy.rules().len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(allow: Vec<OriginRule>, deny_private_networks: bool) -> Result<Self> {
        if allow.len() > MAX_ORIGIN_RULES {
            return Err(Error::InvalidArgument {
                detail: format!(
                    "origin policy has {} rules, maximum is {MAX_ORIGIN_RULES}",
                    allow.len()
                ),
            });
        }
        let mut deduplicated: Vec<OriginRule> = Vec::with_capacity(allow.len());
        for rule in allow {
            if !deduplicated.contains(&rule) {
                deduplicated.push(rule);
            }
        }
        Ok(Self {
            allow: deduplicated,
            deny_private_networks,
        })
    }

    /// Parses textual rules and builds a policy.
    ///
    /// # Arguments
    /// - `origins` (`IntoIterator<Item = impl AsRef<str>>`): rule texts.
    /// - `deny_private_networks` (`bool`): enables the private-network veto.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: any rule is invalid or there are too many.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::OriginPolicy;
    ///
    /// assert!(OriginPolicy::from_origins(["ftp://example.com"], true).is_err());
    /// ```
    pub fn from_origins<I, S>(origins: I, deny_private_networks: bool) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut rules = Vec::new();
        for origin in origins {
            if rules.len() == MAX_ORIGIN_RULES {
                return Err(Error::InvalidArgument {
                    detail: format!("origin policy exceeds {MAX_ORIGIN_RULES} rules"),
                });
            }
            rules.push(OriginRule::parse(origin.as_ref())?);
        }
        Self::new(rules, deny_private_networks)
    }

    /// Returns the validated rules in configuration order.
    pub fn rules(&self) -> &[OriginRule] {
        &self.allow
    }

    /// Returns the canonical textual rules (for scopes, logs and audits).
    pub fn origins(&self) -> Vec<String> {
        self.allow.iter().map(ToString::to_string).collect()
    }

    /// Returns whether the private-network veto is enabled.
    pub fn deny_private_networks(&self) -> bool {
        self.deny_private_networks
    }

    /// Returns whether the policy has no rules (and therefore denies everything).
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty()
    }

    /// Returns `self` with the private-network veto enabled. The veto can only
    /// be tightened this way, never relaxed.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::OriginPolicy;
    ///
    /// # fn main() -> harw_browser::Result<()> {
    /// let policy =
    ///     OriginPolicy::from_origins(["https://a.example"], false)?.with_private_network_veto();
    /// assert!(policy.deny_private_networks());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_private_network_veto(mut self) -> Self {
        self.deny_private_networks = true;
        self
    }

    /// Returns whether `url` is inside this policy.
    ///
    /// # Description
    /// Security boundary: untrusted navigation must never silently escape the
    /// allowlist. Non-web schemes, missing hosts and credentials are denied;
    /// the private-network veto runs before any rule and is never overridden
    /// by a rule naming the same host.
    ///
    /// # Arguments
    /// - `url` (`&url::Url`): candidate location, borrowed.
    ///
    /// # Returns
    /// `true` if allowed.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::OriginPolicy;
    ///
    /// # fn main() -> harw_browser::Result<()> {
    /// let policy = OriginPolicy::from_origins(["http://127.0.0.1:8080"], true)?;
    /// assert!(!policy.is_allowed(&url::Url::parse("http://127.0.0.1:8080/")?));
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_allowed(&self, url: &url::Url) -> bool {
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        let Some(host) = url.host() else {
            return false;
        };
        if self.deny_private_networks && is_private_network_host(&host) {
            return false;
        }
        self.allow.iter().any(|rule| rule.matches(url))
    }

    /// Checks `url` against the policy.
    ///
    /// # Errors
    /// - [`Error::OriginNotAllowed`]: `url` is outside the policy; `origin`
    ///   carries the URL's ASCII origin serialization (never path/query).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::OriginPolicy;
    ///
    /// # fn main() -> harw_browser::Result<()> {
    /// let policy = OriginPolicy::default();
    /// assert!(policy.check(&url::Url::parse("https://example.com/")?).is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn check(&self, url: &url::Url) -> Result<()> {
        if self.is_allowed(url) {
            Ok(())
        } else {
            Err(origin_not_allowed(url))
        }
    }
}

fn origin_not_allowed(url: &url::Url) -> Error {
    Error::OriginNotAllowed {
        origin: url.origin().ascii_serialization(),
    }
}

// Private-network veto for a parsed URL host.
fn is_private_network_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(domain) => domain_is_localhost(domain),
        url::Host::Ipv4(v4) => is_private_ipv4(*v4),
        url::Host::Ipv6(v6) => is_private_ipv6(*v6),
    }
}

// `localhost` or `*.localhost`, case-insensitive, one trailing dot tolerated.
fn domain_is_localhost(domain: &str) -> bool {
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    domain.eq_ignore_ascii_case("localhost") || domain_matches("localhost", domain, true)
}

// Non-public IPv4 ranges a browser must never reach under the veto.
fn is_private_ipv4(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && v4.octets()[2] == 0)
        || (a == 198 && (18..=19).contains(&b))
        || a >= 240
}

// Non-public IPv6 ranges, including embedded IPv4 forms.
fn is_private_ipv6(v6: Ipv6Addr) -> bool {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_private_ipv4(v4);
    }
    let segments = v6.segments();
    // Deprecated IPv4-compatible (::a.b.c.d) and NAT64 (64:ff9b::/96).
    let compatible = segments[..6].iter().all(|s| *s == 0);
    let nat64 =
        segments[0] == 0x64 && segments[1] == 0xff9b && segments[2..6].iter().all(|s| *s == 0);
    if (compatible || nat64) && !v6.is_loopback() && !v6.is_unspecified() {
        let [.., hi, lo] = segments;
        let v4 = Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
        return is_private_ipv4(v4);
    }
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
}

// Deserialization shape of `BrowserLimits`; converted with range validation.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserLimitsRepr {
    max_actions_per_session: u32,
    max_wait_ms: u64,
    max_text_bytes: usize,
    max_selector_bytes: usize,
    max_selector_fallbacks: usize,
    max_url_bytes: usize,
}

/// Host-owned per-session resource ceilings.
///
/// # Description
/// Every value is kept within `1..=HARD_MAX_*` (fallbacks: `0..=HARD_MAX`).
/// The `with_*` builders clamp; deserialization rejects out-of-range values.
/// Consumers: [`crate::action::BrowserAction::validate`],
/// [`crate::action::ActionBudget`], [`crate::wait::WaitCondition::validate`],
/// [`crate::wait::WaitTimeout::validate`].
///
/// # Concurrency
/// Plain data, `Send + Sync`, `Copy`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::policy::{BrowserLimits, HARD_MAX_WAIT_MS};
///
/// let limits = BrowserLimits::default().with_max_wait_ms(u64::MAX);
/// assert_eq!(limits.max_wait_ms(), HARD_MAX_WAIT_MS);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "BrowserLimitsRepr")]
pub struct BrowserLimits {
    max_actions_per_session: u32,
    max_wait_ms: u64,
    max_text_bytes: usize,
    max_selector_bytes: usize,
    max_selector_fallbacks: usize,
    max_url_bytes: usize,
}

impl Default for BrowserLimits {
    fn default() -> Self {
        Self {
            max_actions_per_session: DEFAULT_MAX_ACTIONS_PER_SESSION,
            max_wait_ms: DEFAULT_MAX_WAIT_MS,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
            max_selector_bytes: DEFAULT_MAX_SELECTOR_BYTES,
            max_selector_fallbacks: DEFAULT_MAX_SELECTOR_FALLBACKS,
            max_url_bytes: DEFAULT_MAX_URL_BYTES,
        }
    }
}

impl TryFrom<BrowserLimitsRepr> for BrowserLimits {
    type Error = Error;

    fn try_from(repr: BrowserLimitsRepr) -> Result<Self> {
        fn in_range<T: PartialOrd + fmt::Display>(
            name: &str,
            value: T,
            min: T,
            max: T,
        ) -> Result<T> {
            if value < min || value > max {
                return Err(Error::InvalidArgument {
                    detail: format!("browser limit {name}={value} outside {min}..={max}"),
                });
            }
            Ok(value)
        }
        Ok(Self {
            max_actions_per_session: in_range(
                "max_actions_per_session",
                repr.max_actions_per_session,
                1,
                HARD_MAX_ACTIONS_PER_SESSION,
            )?,
            max_wait_ms: in_range("max_wait_ms", repr.max_wait_ms, 1, HARD_MAX_WAIT_MS)?,
            max_text_bytes: in_range(
                "max_text_bytes",
                repr.max_text_bytes,
                1,
                HARD_MAX_TEXT_BYTES,
            )?,
            max_selector_bytes: in_range(
                "max_selector_bytes",
                repr.max_selector_bytes,
                1,
                HARD_MAX_SELECTOR_BYTES,
            )?,
            max_selector_fallbacks: in_range(
                "max_selector_fallbacks",
                repr.max_selector_fallbacks,
                0,
                HARD_MAX_SELECTOR_FALLBACKS,
            )?,
            max_url_bytes: in_range("max_url_bytes", repr.max_url_bytes, 1, HARD_MAX_URL_BYTES)?,
        })
    }
}

impl BrowserLimits {
    /// Sets the per-session action ceiling, clamped to `1..=HARD_MAX_ACTIONS_PER_SESSION`.
    #[must_use]
    pub fn with_max_actions_per_session(mut self, value: u32) -> Self {
        self.max_actions_per_session = value.clamp(1, HARD_MAX_ACTIONS_PER_SESSION);
        self
    }

    /// Sets the wait ceiling in milliseconds, clamped to `1..=HARD_MAX_WAIT_MS`.
    #[must_use]
    pub fn with_max_wait_ms(mut self, value: u64) -> Self {
        self.max_wait_ms = value.clamp(1, HARD_MAX_WAIT_MS);
        self
    }

    /// Sets the text/pattern ceiling in bytes, clamped to `1..=HARD_MAX_TEXT_BYTES`.
    #[must_use]
    pub fn with_max_text_bytes(mut self, value: usize) -> Self {
        self.max_text_bytes = value.clamp(1, HARD_MAX_TEXT_BYTES);
        self
    }

    /// Sets the selector ceiling in bytes, clamped to `1..=HARD_MAX_SELECTOR_BYTES`.
    #[must_use]
    pub fn with_max_selector_bytes(mut self, value: usize) -> Self {
        self.max_selector_bytes = value.clamp(1, HARD_MAX_SELECTOR_BYTES);
        self
    }

    /// Sets the fallback-selector ceiling, clamped to `0..=HARD_MAX_SELECTOR_FALLBACKS`.
    #[must_use]
    pub fn with_max_selector_fallbacks(mut self, value: usize) -> Self {
        self.max_selector_fallbacks = value.min(HARD_MAX_SELECTOR_FALLBACKS);
        self
    }

    /// Sets the URL ceiling in bytes, clamped to `1..=HARD_MAX_URL_BYTES`.
    #[must_use]
    pub fn with_max_url_bytes(mut self, value: usize) -> Self {
        self.max_url_bytes = value.clamp(1, HARD_MAX_URL_BYTES);
        self
    }

    /// Returns the per-session action ceiling.
    pub fn max_actions_per_session(&self) -> u32 {
        self.max_actions_per_session
    }

    /// Returns the wait ceiling in milliseconds.
    pub fn max_wait_ms(&self) -> u64 {
        self.max_wait_ms
    }

    /// Returns the text/pattern ceiling in bytes.
    pub fn max_text_bytes(&self) -> usize {
        self.max_text_bytes
    }

    /// Returns the selector ceiling in bytes.
    pub fn max_selector_bytes(&self) -> usize {
        self.max_selector_bytes
    }

    /// Returns the fallback-selector ceiling.
    pub fn max_selector_fallbacks(&self) -> usize {
        self.max_selector_fallbacks
    }

    /// Returns the URL ceiling in bytes.
    pub fn max_url_bytes(&self) -> usize {
        self.max_url_bytes
    }

    /// Checks a URL's serialized length against [`Self::max_url_bytes`].
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: the URL is too long.
    pub fn check_url_len(&self, url: &url::Url) -> Result<()> {
        let len = url.as_str().len();
        if len > self.max_url_bytes {
            return Err(Error::InvalidArgument {
                detail: format!("url is {len} bytes, maximum is {}", self.max_url_bytes),
            });
        }
        Ok(())
    }
}

/// Host-authorized request to open a browser session.
///
/// # Description
/// Built by the host/tool layer from a host-owned grant plus the model's
/// start URL; it is not model-facing. `allowed_origins` bounds every
/// model-initiated navigation *and* every observed location;
/// `authentication_origins` are additionally accepted **only** as observed
/// locations (e.g. SSO redirects), never as navigation targets or start URL.
///
/// # Concurrency
/// Plain data, `Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::policy::*;
///
/// # fn main() -> harw_browser::Result<()> {
/// let request = OpenBrowserRequest {
///     start_url: url::Url::parse("https://erp.example.com/")?,
///     headless: true,
///     profile: ProfilePolicy::Ephemeral,
///     bidi: BiDiRequirement::Preferred,
///     allowed_origins: OriginPolicy::from_origins(["https://erp.example.com"], true)?,
///     authentication_origins: OriginPolicy::from_origins(["https://sso.example.com"], true)?,
///     viewport: None,
///     limits: BrowserLimits::default(),
/// };
/// request.validate()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenBrowserRequest {
    pub start_url: url::Url,
    pub headless: bool,
    pub profile: ProfilePolicy,
    pub bidi: BiDiRequirement,
    pub allowed_origins: OriginPolicy,
    pub authentication_origins: OriginPolicy,
    pub viewport: Option<Viewport>,
    pub limits: BrowserLimits,
}

impl OpenBrowserRequest {
    /// Validates the whole request before a session is started.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: start URL too long, invalid viewport or
    ///   profile binding.
    /// - [`Error::OriginNotAllowed`]: start URL outside `allowed_origins`
    ///   (authentication origins are not valid start URLs).
    pub fn validate(&self) -> Result<()> {
        self.check_navigation_target(&self.start_url)?;
        if let Some(viewport) = &self.viewport {
            viewport.validate()?;
        }
        self.profile.validate()
    }

    /// Checks a model-initiated navigation target (`Navigate`, start URL).
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: URL too long.
    /// - [`Error::OriginNotAllowed`]: outside `allowed_origins`.
    pub fn check_navigation_target(&self, url: &url::Url) -> Result<()> {
        self.limits.check_url_len(url)?;
        self.allowed_origins.check(url)
    }

    /// Checks a location observed after any action, redirect or script-driven
    /// navigation.
    ///
    /// # Description
    /// Accepts `allowed_origins` and `authentication_origins`. Backends must
    /// call this after every action and navigation and treat an error as a
    /// policy breach (stop the context, do not return page content).
    ///
    /// # Errors
    /// - [`Error::OriginNotAllowed`]: outside both policies.
    pub fn check_observed_location(&self, url: &url::Url) -> Result<()> {
        if self.allowed_origins.is_allowed(url) || self.authentication_origins.is_allowed(url) {
            Ok(())
        } else {
            Err(origin_not_allowed(url))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    // Test helper: test inputs are literals, a parse failure is a test bug.
    fn url(s: &str) -> TestResult<url::Url> {
        url::Url::parse(s).map_err(ctx("valid test url"))
    }

    fn policy(origins: &[&str], deny: bool) -> TestResult<OriginPolicy> {
        OriginPolicy::from_origins(origins, deny).map_err(ctx("valid test policy"))
    }

    fn open_request() -> TestResult<OpenBrowserRequest> {
        Ok(OpenBrowserRequest {
            start_url: url("https://erp.example.com/login")?,
            headless: true,
            profile: ProfilePolicy::Persistent {
                binding: "sales-team".to_owned(),
            },
            bidi: BiDiRequirement::Preferred,
            allowed_origins: policy(&["https://erp.example.com"], true)?,
            authentication_origins: policy(&["https://sso.example.net"], true)?,
            viewport: Some(Viewport {
                width: 1920,
                height: 1080,
            }),
            limits: BrowserLimits::default(),
        })
    }

    #[test]
    fn test_origin_rule_parse_accepts_exact_port_and_wildcard() -> TestResult {
        let exact = OriginRule::parse("https://ERP.example.com/").map_err(ctx("exact"))?;
        assert_eq!(exact.to_string(), "https://erp.example.com");
        assert_eq!(exact.port(), 443);
        let port = OriginRule::parse("http://erp.example.com:8080").map_err(ctx("port"))?;
        assert_eq!(port.to_string(), "http://erp.example.com:8080");
        let wildcard = OriginRule::parse("https://*.example.com").map_err(ctx("wildcard"))?;
        assert!(wildcard.include_subdomains());
        let v6 = OriginRule::parse("https://[::1]:8443").map_err(ctx("v6"))?;
        assert_eq!(v6.to_string(), "https://[::1]:8443");
        Ok(())
    }

    #[test]
    fn test_origin_rule_parse_rejects_malformed_rules() {
        for bad in [
            "",
            "erp.example.com",
            "HTTPS://erp.example.com",
            "ftp://erp.example.com",
            "file:///etc/passwd",
            "javascript://erp.example.com",
            "https://erp.example.com/path",
            "https://erp.example.com?q=1",
            "https://erp.example.com#frag",
            "https://user@erp.example.com",
            "https://*.com",
            "https://*",
            "https://a.*.example.com",
            "https://*.10.0.0.1",
            "https://*.[::1]",
            "https://erp.example.com:99999",
        ] {
            assert!(
                OriginRule::parse(bad).is_err(),
                "rule {bad:?} must be rejected"
            );
        }
        let long = format!("https://{}.example.com", "a".repeat(MAX_ORIGIN_RULE_BYTES));
        assert!(OriginRule::parse(&long).is_err());
    }

    #[test]
    fn test_is_allowed_exact_origin_match() -> TestResult {
        let policy = policy(&["https://erp.example.com"], false)?;
        assert!(policy.is_allowed(&url("https://erp.example.com/path?x=1")?));
        assert!(policy.is_allowed(&url("https://ERP.EXAMPLE.COM./path")?));
        assert!(policy.is_allowed(&url("https://erp.example.com:443/")?));
        Ok(())
    }

    #[test]
    fn test_is_allowed_scheme_change_denied() -> TestResult {
        let policy = policy(&["https://erp.example.com"], false)?;
        assert!(!policy.is_allowed(&url("http://erp.example.com/")?));
        assert!(!policy.is_allowed(&url("ws://erp.example.com/")?));
        assert!(!policy.is_allowed(&url("ftp://erp.example.com/")?));
        Ok(())
    }

    #[test]
    fn test_is_allowed_port_change_denied() -> TestResult {
        let policy = policy(&["https://erp.example.com"], false)?;
        assert!(!policy.is_allowed(&url("https://erp.example.com:8443/")?));
        let with_port = policy_with(&["http://erp.example.com:8080"])?;
        assert!(with_port.is_allowed(&url("http://erp.example.com:8080/")?));
        assert!(!with_port.is_allowed(&url("http://erp.example.com/")?));
        Ok(())
    }

    fn policy_with(origins: &[&str]) -> TestResult<OriginPolicy> {
        policy(origins, false)
    }

    #[test]
    fn test_is_allowed_host_change_denied() -> TestResult {
        let policy = policy(&["https://erp.example.com"], false)?;
        assert!(!policy.is_allowed(&url("https://evil.com/")?));
        assert!(!policy.is_allowed(&url("https://erp.example.com.evil.com/")?));
        assert!(!policy.is_allowed(&url("https://evilerp.example.com/")?));
        Ok(())
    }

    #[test]
    fn test_is_allowed_exact_rule_does_not_admit_subdomains() -> TestResult {
        let policy = policy(&["https://example.com"], false)?;
        assert!(policy.is_allowed(&url("https://example.com/")?));
        assert!(!policy.is_allowed(&url("https://erp.example.com/")?));
        Ok(())
    }

    #[test]
    fn test_is_allowed_wildcard_rule_admits_strict_subdomains_only() -> TestResult {
        let policy = policy(&["https://*.example.com"], false)?;
        assert!(policy.is_allowed(&url("https://erp.example.com/")?));
        assert!(policy.is_allowed(&url("https://a.b.example.com/")?));
        assert!(!policy.is_allowed(&url("https://example.com/")?));
        assert!(!policy.is_allowed(&url("https://notexample.com/")?));
        assert!(!policy.is_allowed(&url("https://example.com.evil.net/")?));
        assert!(!policy.is_allowed(&url("http://erp.example.com/")?));
        Ok(())
    }

    #[test]
    fn test_is_allowed_credentials_in_url_denied() -> TestResult {
        let policy = policy(&["https://erp.example.com"], false)?;
        assert!(!policy.is_allowed(&url("https://user:pw@erp.example.com/")?));
        assert!(!policy.is_allowed(&url("https://user@erp.example.com/")?));
        Ok(())
    }

    #[test]
    fn test_is_allowed_hostless_and_non_web_schemes_denied() -> TestResult {
        let policy = policy(&["https://example.com"], false)?;
        assert!(!policy.is_allowed(&url("data:text/plain,hello")?));
        assert!(!policy.is_allowed(&url("about:blank")?));
        assert!(!policy.is_allowed(&url("file:///etc/passwd")?));
        Ok(())
    }

    #[test]
    fn test_is_allowed_private_hosts_vetoed_even_when_listed() -> TestResult {
        let cases = [
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080/"),
            ("http://192.168.1.5", "http://192.168.1.5/"),
            (
                "http://169.254.169.254",
                "http://169.254.169.254/latest/meta-data",
            ),
            ("http://100.64.0.1", "http://100.64.0.1/"),
            ("http://0.0.0.0", "http://0.0.0.0/"),
            ("http://[::1]", "http://[::1]/"),
            ("http://[fd00::1]", "http://[fd00::1]/"),
            ("http://[fe80::1]", "http://[fe80::1]/"),
            ("http://[::ffff:10.0.0.1]", "http://[::ffff:10.0.0.1]/"),
            (
                "http://[64:ff9b::a9fe:a9fe]",
                "http://[64:ff9b::a9fe:a9fe]/",
            ),
            ("http://localhost:8080", "http://localhost:8080/"),
            ("http://localhost.", "http://localhost./"),
            ("http://*.app.localhost", "http://x.app.localhost/"),
        ];
        for (rule, target) in cases {
            let vetoed = policy(&[rule], true)?;
            assert!(!vetoed.is_allowed(&url(target)?), "{target} must be vetoed");
            let open = policy(&[rule], false)?;
            assert!(
                open.is_allowed(&url(target)?),
                "{target} must match {rule} without veto"
            );
        }
        Ok(())
    }

    #[test]
    fn test_is_allowed_public_ip_literal_exact_only() -> TestResult {
        let policy = policy(&["https://93.184.216.34"], true)?;
        assert!(policy.is_allowed(&url("https://93.184.216.34/")?));
        assert!(!policy.is_allowed(&url("https://93.184.216.35/")?));
        Ok(())
    }

    #[test]
    fn test_check_returns_origin_not_allowed_without_path() -> TestResult {
        let policy = policy(&["https://erp.example.com"], true)?;
        let result = policy.check(&url("https://evil.com/secret?token=1")?);
        let Err(error) = result else {
            return Err(TestError::Unexpected("must be denied".to_owned()));
        };
        match error {
            Error::OriginNotAllowed { origin } => assert_eq!(origin, "https://evil.com"),
            other => return Err(TestError::Unexpected(format!("unexpected error {other}"))),
        }
        Ok(())
    }

    #[test]
    fn test_origin_policy_default_denies_everything() -> TestResult {
        let policy = OriginPolicy::default();
        assert!(policy.is_empty());
        assert!(policy.deny_private_networks());
        assert!(!policy.is_allowed(&url("https://example.com/")?));
        Ok(())
    }

    #[test]
    fn test_origin_policy_from_origins_rejects_too_many_rules() {
        let origins: Vec<String> = (0..=MAX_ORIGIN_RULES)
            .map(|i| format!("https://h{i}.example.com"))
            .collect();
        assert!(OriginPolicy::from_origins(&origins, true).is_err());
        assert!(OriginPolicy::from_origins(&origins[..MAX_ORIGIN_RULES], true).is_ok());
    }

    #[test]
    fn test_origin_policy_new_deduplicates_rules() -> TestResult {
        let rule = OriginRule::parse("https://a.example").map_err(ctx("rule"))?;
        let policy = OriginPolicy::new(vec![rule.clone(), rule], true).map_err(ctx("policy"))?;
        assert_eq!(policy.origins(), vec!["https://a.example".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_origin_policy_with_private_network_veto_tightens() -> TestResult {
        let policy = policy(&["http://10.0.0.1"], false)?.with_private_network_veto();
        assert!(!policy.is_allowed(&url("http://10.0.0.1/")?));
        Ok(())
    }

    #[test]
    fn test_origin_policy_serde_round_trip_and_strictness() -> TestResult {
        let policy = policy(
            &["https://*.example.com", "http://erp.example.com:8080"],
            true,
        )?;
        let json = serde_json::to_value(&policy).map_err(ctx("serializes"))?;
        assert_eq!(
            json,
            serde_json::json!({
                "allow": ["https://*.example.com", "http://erp.example.com:8080"],
                "deny_private_networks": true
            })
        );
        let decoded: OriginPolicy = serde_json::from_value(json).map_err(ctx("deserializes"))?;
        assert_eq!(decoded, policy);

        let unknown = serde_json::json!({"allow": [], "deny_private_networks": true, "extra": 1});
        assert!(serde_json::from_value::<OriginPolicy>(unknown).is_err());
        let bare_host =
            serde_json::json!({"allow": ["example.com"], "deny_private_networks": true});
        assert!(serde_json::from_value::<OriginPolicy>(bare_host).is_err());
        Ok(())
    }

    #[test]
    fn test_browser_limits_default_within_hard_ceilings() {
        let limits = BrowserLimits::default();
        assert!(limits.max_actions_per_session() <= HARD_MAX_ACTIONS_PER_SESSION);
        assert!(limits.max_wait_ms() <= HARD_MAX_WAIT_MS);
        assert!(limits.max_text_bytes() <= HARD_MAX_TEXT_BYTES);
        assert!(limits.max_selector_bytes() <= HARD_MAX_SELECTOR_BYTES);
        assert!(limits.max_selector_fallbacks() <= HARD_MAX_SELECTOR_FALLBACKS);
        assert!(limits.max_url_bytes() <= HARD_MAX_URL_BYTES);
    }

    #[test]
    fn test_browser_limits_with_builders_clamp() {
        let limits = BrowserLimits::default()
            .with_max_actions_per_session(0)
            .with_max_wait_ms(u64::MAX)
            .with_max_text_bytes(usize::MAX)
            .with_max_selector_bytes(0)
            .with_max_selector_fallbacks(usize::MAX)
            .with_max_url_bytes(usize::MAX);
        assert_eq!(limits.max_actions_per_session(), 1);
        assert_eq!(limits.max_wait_ms(), HARD_MAX_WAIT_MS);
        assert_eq!(limits.max_text_bytes(), HARD_MAX_TEXT_BYTES);
        assert_eq!(limits.max_selector_bytes(), 1);
        assert_eq!(limits.max_selector_fallbacks(), HARD_MAX_SELECTOR_FALLBACKS);
        assert_eq!(limits.max_url_bytes(), HARD_MAX_URL_BYTES);
    }

    #[test]
    fn test_browser_limits_serde_rejects_out_of_range_and_unknown_fields() -> TestResult {
        let limits = BrowserLimits::default();
        let mut json = serde_json::to_value(limits).map_err(ctx("serializes"))?;
        let decoded: BrowserLimits =
            serde_json::from_value(json.clone()).map_err(ctx("deserializes"))?;
        assert_eq!(decoded, limits);

        json["max_wait_ms"] = serde_json::json!(HARD_MAX_WAIT_MS + 1);
        assert!(serde_json::from_value::<BrowserLimits>(json.clone()).is_err());
        json["max_wait_ms"] = serde_json::json!(1_000);
        json["unbounded"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BrowserLimits>(json).is_err());
        Ok(())
    }

    #[test]
    fn test_browser_limits_check_url_len() -> TestResult {
        let limits = BrowserLimits::default().with_max_url_bytes(30);
        assert!(limits.check_url_len(&url("https://a.example/")?).is_ok());
        assert!(
            limits
                .check_url_len(&url("https://a.example/a-very-long-path-over-limit")?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn test_profile_policy_validate_rejects_path_like_bindings() {
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "a b"] {
            let policy = ProfilePolicy::Persistent {
                binding: bad.to_owned(),
            };
            assert!(
                policy.validate().is_err(),
                "binding {bad:?} must be rejected"
            );
        }
        let ok = ProfilePolicy::Persistent {
            binding: "sales-team_1.v2".to_owned(),
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn test_viewport_validate_bounds() {
        assert!(
            Viewport {
                width: 1,
                height: 1
            }
            .validate()
            .is_ok()
        );
        assert!(
            Viewport {
                width: MAX_VIEWPORT_DIMENSION + 1,
                height: 10
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn test_open_browser_request_validate_accepts_allowed_start() -> TestResult {
        assert!(open_request()?.validate().is_ok());
        Ok(())
    }

    #[test]
    fn test_open_browser_request_validate_rejects_authentication_origin_as_start() -> TestResult {
        let mut request = open_request()?;
        request.start_url = url("https://sso.example.net/login")?;
        assert!(matches!(
            request.validate(),
            Err(Error::OriginNotAllowed { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_open_browser_request_navigation_vs_observed_location() -> TestResult {
        let request = open_request()?;
        let sso = url("https://sso.example.net/authorize")?;
        assert!(request.check_navigation_target(&sso).is_err());
        assert!(request.check_observed_location(&sso).is_ok());
        let evil = url("https://evil.example.org/")?;
        assert!(request.check_navigation_target(&evil).is_err());
        assert!(request.check_observed_location(&evil).is_err());
        assert!(
            request
                .check_navigation_target(&url("https://erp.example.com/next")?)
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn test_open_browser_request_serde_round_trip_and_unknown_field_rejected() -> TestResult {
        let request = open_request()?;
        let mut json = serde_json::to_value(&request).map_err(ctx("request serializes"))?;
        let decoded: OpenBrowserRequest =
            serde_json::from_value(json.clone()).map_err(ctx("request deserializes"))?;
        assert_eq!(decoded, request);

        json["upload_root"] = serde_json::json!("/home");
        assert!(serde_json::from_value::<OpenBrowserRequest>(json).is_err());
        Ok(())
    }

    #[test]
    fn test_profile_policy_ephemeral_serde_json_round_trip() -> TestResult {
        let policy = ProfilePolicy::Ephemeral;
        let json = serde_json::to_string(&policy).map_err(ctx("policy serializes"))?;
        let decoded: ProfilePolicy =
            serde_json::from_str(&json).map_err(ctx("policy deserializes"))?;
        assert_eq!(decoded, policy);
        Ok(())
    }
}
