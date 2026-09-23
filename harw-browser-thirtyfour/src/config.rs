//! Host-level Firefox process configuration (B-ADAPT).
//!
//! # Description
//! Owns everything that is decided by the trusted host, never by a model
//! request: the Firefox executable, the pinned geckodriver
//! ([`GeckodriverPin`], from `harw_config::BrowserSection::{geckodriver_path,
//! geckodriver_sha256}`), the sandbox [`BrowserLauncher`], the event-journal
//! policy and persistent-profile bindings. There is no managed/automatic
//! driver mode: without both a pin and a launcher, sessions cannot open.
//!
//! # Concurrency
//! `Clone + Send + Sync`; the launcher is shared through `Arc`.
//!
//! # Errors
//! Builders return `harw_browser::Error::InvalidArgument` for empty bindings.

use crate::journal::{DEFAULT_JOURNAL_CAPACITY, EventJournalPolicy};
use crate::launcher::{BrowserLauncher, GeckodriverPin};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Configuration shared by sessions opened through one Firefox host.
///
/// Request-specific policy such as headless mode, profile selection, origins,
/// limits and viewport remains in `OpenBrowserRequest`; this type only owns
/// adapter process configuration.
#[derive(Clone)]
pub struct FirefoxHostConfig {
    firefox_binary: Option<PathBuf>,
    geckodriver: Option<GeckodriverPin>,
    launcher: Option<Arc<dyn BrowserLauncher>>,
    journal_policy: Option<EventJournalPolicy>,
    profile_bindings: BTreeMap<String, PathBuf>,
}

impl fmt::Debug for FirefoxHostConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FirefoxHostConfig")
            .field("firefox_binary", &self.firefox_binary)
            .field("geckodriver", &self.geckodriver)
            .field("launcher", &self.launcher)
            .field("journal_policy", &self.journal_policy)
            .field("profile_bindings", &self.profile_bindings)
            .finish()
    }
}

impl FirefoxHostConfig {
    /// Creates a fail-closed default: no pinned geckodriver and no launcher,
    /// so `open` reports `CapabilityUnavailable` until both are configured.
    pub fn new() -> Self {
        Self {
            firefox_binary: None,
            geckodriver: None,
            launcher: None,
            journal_policy: None,
            profile_bindings: BTreeMap::new(),
        }
    }

    /// Selects an explicit Firefox executable for sessions opened by the host.
    #[must_use]
    pub fn with_firefox_binary(mut self, firefox_binary: PathBuf) -> Self {
        self.firefox_binary = Some(firefox_binary);
        self
    }

    /// Sets the pinned geckodriver (path + SHA-256) verified before every start.
    #[must_use]
    pub fn with_geckodriver_pin(mut self, pin: GeckodriverPin) -> Self {
        self.geckodriver = Some(pin);
        self
    }

    /// Sets the sandbox launcher that starts geckodriver with `NetworkMode::ProxyOnly`.
    #[must_use]
    pub fn with_launcher(mut self, launcher: Arc<dyn BrowserLauncher>) -> Self {
        self.launcher = Some(launcher);
        self
    }

    /// Overrides the per-session event-journal bounds.
    #[must_use]
    pub fn with_journal_policy(mut self, policy: EventJournalPolicy) -> Self {
        self.journal_policy = Some(policy);
        self
    }

    /// Binds an explicit persistent-profile name to an adapter-owned directory.
    ///
    /// This records configuration only. It never creates, reads, or validates
    /// the directory on the filesystem; lifecycle checks belong to driver start.
    pub fn with_profile_binding(
        mut self,
        binding: impl Into<String>,
        directory: PathBuf,
    ) -> harw_browser::Result<Self> {
        let binding = binding.into();
        if binding.trim().is_empty() {
            return Err(harw_browser::Error::InvalidArgument {
                detail: "persistent Firefox profile binding must not be empty".to_owned(),
            });
        }
        self.profile_bindings.insert(binding, directory);
        Ok(self)
    }

    /// Returns the configured Firefox executable without allocating.
    pub fn firefox_binary(&self) -> Option<&Path> {
        self.firefox_binary.as_deref()
    }

    /// Returns the pinned geckodriver, if configured.
    pub fn geckodriver_pin(&self) -> Option<&GeckodriverPin> {
        self.geckodriver.as_ref()
    }

    /// Returns a shared handle to the configured launcher, if any.
    pub fn launcher(&self) -> Option<Arc<dyn BrowserLauncher>> {
        self.launcher.as_ref().map(Arc::clone)
    }

    /// Returns the effective event-journal policy.
    ///
    /// # Errors
    /// - `InvalidArgument`: only if the built-in default were invalid.
    pub fn journal_policy(&self) -> harw_browser::Result<EventJournalPolicy> {
        match self.journal_policy {
            Some(policy) => Ok(policy),
            None => EventJournalPolicy::bounded(DEFAULT_JOURNAL_CAPACITY),
        }
    }

    /// Returns the directory bound to `binding` without allocating or exposing
    /// ownership of host configuration.
    pub fn profile_directory(&self, binding: &str) -> Option<&Path> {
        self.profile_bindings.get(binding).map(PathBuf::as_path)
    }
}

impl Default for FirefoxHostConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_firefox_host_config_new_is_fail_closed() -> TestResult {
        let config = FirefoxHostConfig::new();
        assert!(config.geckodriver_pin().is_none());
        assert!(config.launcher().is_none());
        assert_eq!(
            config
                .journal_policy()
                .map_err(ctx("default policy"))?
                .capacity(),
            DEFAULT_JOURNAL_CAPACITY
        );
        Ok(())
    }

    #[test]
    fn test_firefox_host_config_with_geckodriver_pin_is_retained() -> TestResult {
        let pin = GeckodriverPin::new(PathBuf::from("/opt/geckodriver"), &"b".repeat(64))
            .map_err(ctx("valid pin"))?;
        let config = FirefoxHostConfig::new().with_geckodriver_pin(pin.clone());
        assert_eq!(config.geckodriver_pin(), Some(&pin));
        Ok(())
    }
}
