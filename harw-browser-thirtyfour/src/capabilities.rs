//! Harwness-owned planning types for Firefox capabilities and BiDi domains.

use crate::config::FirefoxHostConfig;
use harw_browser::error::Error as BrowserError;
use harw_browser::policy::{BiDiRequirement, OpenBrowserRequest, ProfilePolicy};
use std::path::Path;

/// Builds an immutable capability plan from host configuration and a request.
///
/// The factory deliberately returns adapter-owned metadata rather than a
/// thirtyfour capability object. Conversion to the backend type remains an
/// internal driver-start concern.
pub struct FirefoxCapabilityFactory<'config> {
    config: &'config FirefoxHostConfig,
}

impl<'config> FirefoxCapabilityFactory<'config> {
    pub fn new(config: &'config FirefoxHostConfig) -> Self {
        Self { config }
    }

    pub fn plan(
        &self,
        request: &OpenBrowserRequest,
    ) -> harw_browser::Result<FirefoxCapabilityPlan<'config>> {
        let profile_directory = match &request.profile {
            ProfilePolicy::Ephemeral => None,
            ProfilePolicy::Persistent { binding } => {
                if binding.trim().is_empty() {
                    return Err(BrowserError::InvalidArgument {
                        detail: "persistent Firefox profile binding must not be empty".to_owned(),
                    });
                }
                Some(self.config.profile_directory(binding).ok_or_else(|| {
                    BrowserError::InvalidArgument {
                        detail: format!(
                            "persistent Firefox profile binding '{binding}' is not configured"
                        ),
                    }
                })?)
            }
        };

        Ok(FirefoxCapabilityPlan {
            headless: request.headless,
            webdriver_bidi_enabled: !matches!(request.bidi, BiDiRequirement::NotRequired),
            firefox_binary: self.config.firefox_binary(),
            profile_directory,
        })
    }
}

/// Backend-neutral description of the Firefox capabilities to construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirefoxCapabilityPlan<'config> {
    headless: bool,
    webdriver_bidi_enabled: bool,
    firefox_binary: Option<&'config Path>,
    profile_directory: Option<&'config Path>,
}

impl FirefoxCapabilityPlan<'_> {
    pub fn browser_name(&self) -> &'static str {
        "firefox"
    }

    pub fn headless(&self) -> bool {
        self.headless
    }

    pub fn webdriver_bidi_enabled(&self) -> bool {
        self.webdriver_bidi_enabled
    }

    pub fn firefox_binary(&self) -> Option<&Path> {
        self.firefox_binary
    }

    pub fn profile_directory(&self) -> Option<&Path> {
        self.profile_directory
    }
}

/// Initial WebDriver BiDi event domains consumed by Harwness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiEventDomain {
    BrowsingContext,
    Network,
    Log,
    Script,
}

impl BidiEventDomain {
    /// Returns the WebDriver BiDi module name used on the wire.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::BrowsingContext => "browsingContext",
            Self::Network => "network",
            Self::Log => "log",
            Self::Script => "script",
        }
    }
}

const STANDARD_BIDI_DOMAINS: [BidiEventDomain; 4] = [
    BidiEventDomain::BrowsingContext,
    BidiEventDomain::Network,
    BidiEventDomain::Log,
    BidiEventDomain::Script,
];

/// Immutable subscription-domain metadata for a Firefox BiDi session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BidiSubscriptionPlan;

impl BidiSubscriptionPlan {
    pub fn standard() -> Self {
        Self
    }

    pub fn domains(&self) -> &'static [BidiEventDomain] {
        &STANDARD_BIDI_DOMAINS
    }
}
