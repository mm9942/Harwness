//! Host-level Firefox process configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Configuration shared by sessions opened through one Firefox host.
///
/// Request-specific policy such as headless mode, profile selection, origins,
/// and viewport remains in `OpenBrowserRequest`; this type only owns adapter
/// process configuration.
#[derive(Debug, Clone)]
pub struct FirefoxHostConfig {
    firefox_binary: Option<PathBuf>,
    managed_driver: bool,
    profile_bindings: BTreeMap<String, PathBuf>,
}

impl FirefoxHostConfig {
    /// Creates the production default: discover Firefox and let thirtyfour
    /// manage the geckodriver lifecycle.
    pub fn new() -> Self {
        Self {
            firefox_binary: None,
            managed_driver: true,
            profile_bindings: BTreeMap::new(),
        }
    }

    /// Selects an explicit Firefox executable for sessions opened by the host.
    #[must_use]
    pub fn with_firefox_binary(mut self, firefox_binary: PathBuf) -> Self {
        self.firefox_binary = Some(firefox_binary);
        self
    }

    /// Enables or disables thirtyfour's managed geckodriver lifecycle.
    #[must_use]
    pub fn with_managed_driver(mut self, managed_driver: bool) -> Self {
        self.managed_driver = managed_driver;
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

    /// Reports whether the host should use the managed driver lifecycle.
    pub fn managed_driver(&self) -> bool {
        self.managed_driver
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
