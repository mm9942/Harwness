//! Adapter-private ownership of the raw thirtyfour driver lifecycle (B-ADAPT).
//!
//! # Description
//! Connects to a pinned, sandboxed geckodriver that was started through
//! [`crate::launcher::launch_pinned_driver`]; `WebDriver::managed` and the
//! thirtyfour `manager` feature are not used. The WebDriver HTTP client ignores
//! proxy environment variables and never follows redirects. The sandbox process
//! handle is owned here and dropped (killing the sandbox) when the session ends.
//!
//! # Concurrency
//! Crate-private; guarded by the runtime's Tokio mutex.
//!
//! # Errors
//! [`AdapterError::Driver`] / [`AdapterError::DriverTimeout`].

use crate::capabilities::FirefoxCapabilityPlan;
use crate::error::{AdapterError, DriverOperation};
use crate::firefox_prefs::{PrefValue, hardened_preferences};
use crate::launcher::{DriverProcess, LaunchedDriver};
use std::time::Duration;
use thirtyfour::common::capabilities::firefox::FirefoxPreferences;
use thirtyfour::prelude::{CapabilitiesHelper, DesiredCapabilities, WebDriver};

// Deadline for the WebDriver "New Session" handshake (includes Firefox start).
const SESSION_START_TIMEOUT: Duration = Duration::from_secs(60);
// Per-request HTTP timeout towards geckodriver.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Sole owner of a live raw WebDriver connection inside this adapter.
///
/// The type and all of its methods are crate-private so downstream callers
/// can receive only Harwness-owned session handles and contracts.
pub(crate) struct FirefoxDriver {
    webdriver: WebDriver,
    process: Option<Box<dyn DriverProcess>>,
}

impl FirefoxDriver {
    /// Builds hardened Firefox capabilities and opens a session on the
    /// sandboxed geckodriver described by `launched`.
    #[tracing::instrument(
        level = "info",
        skip(plan, launched),
        fields(
            browser = plan.browser_name(),
            headless = plan.headless(),
            webdriver_bidi = plan.webdriver_bidi_enabled()
        )
    )]
    pub(crate) async fn start(
        plan: FirefoxCapabilityPlan<'_>,
        launched: LaunchedDriver,
    ) -> Result<Self, AdapterError> {
        let proxy_port = launched.proxy_port();
        let webdriver_url = launched.webdriver_url().as_str().to_owned();
        let (_prepared, process) = launched.into_parts();

        let mut capabilities = DesiredCapabilities::firefox();
        if plan.headless() {
            capabilities.set_headless().map_err(configure_error)?;
        }

        if let Some(binary) = plan.firefox_binary() {
            let binary = binary.to_str().ok_or_else(|| AdapterError::Driver {
                operation: DriverOperation::ConfigureCapabilities,
                detail: "configured Firefox binary path is not valid UTF-8".to_owned(),
            })?;
            capabilities
                .set_firefox_binary(binary)
                .map_err(configure_error)?;
        }

        if let Some(profile_directory) = plan.profile_directory() {
            let encoded_profile =
                crate::profile_archive::encode_firefox_profile(profile_directory)?;
            capabilities
                .set_encoded_profile(&encoded_profile)
                .map_err(|error| AdapterError::Driver {
                    operation: DriverOperation::ConfigureCapabilities,
                    detail: format!(
                        "could not apply encoded Firefox profile from '{}': {error}",
                        profile_directory.display()
                    ),
                })?;
        }

        let mut preferences = FirefoxPreferences::new();
        for (name, value) in hardened_preferences(proxy_port) {
            let applied = match value {
                PrefValue::Bool(value) => preferences.set(name, value),
                PrefValue::Int(value) => preferences.set(name, value),
                PrefValue::Str(value) => preferences.set(name, value),
            };
            applied.map_err(configure_error)?;
        }
        capabilities
            .set_preferences(preferences)
            .map_err(configure_error)?;

        if plan.webdriver_bidi_enabled() {
            capabilities.enable_bidi().map_err(configure_error)?;
        }

        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| AdapterError::Driver {
                operation: DriverOperation::StartSession,
                detail: format!("could not build the WebDriver HTTP client: {error}"),
            })?;

        let connect = WebDriver::builder(webdriver_url, capabilities)
            .client(client)
            .connect();
        let webdriver = match tokio::time::timeout(SESSION_START_TIMEOUT, connect).await {
            Ok(Ok(webdriver)) => webdriver,
            Ok(Err(error)) => {
                return Err(AdapterError::Driver {
                    operation: DriverOperation::StartSession,
                    detail: error.to_string(),
                });
            }
            Err(_elapsed) => {
                return Err(AdapterError::DriverTimeout {
                    operation: DriverOperation::StartSession,
                    detail: format!(
                        "geckodriver did not open a session within {} s",
                        SESSION_START_TIMEOUT.as_secs()
                    ),
                });
            }
        };
        tracing::info!(proxy_port, "sandboxed Firefox WebDriver session started");

        Ok(Self {
            webdriver,
            process: Some(process),
        })
    }

    /// Borrows the raw driver for other crate-private adapter components.
    pub(crate) fn webdriver(&self) -> &WebDriver {
        &self.webdriver
    }

    /// Ends the browser session and then kills the sandbox process tree.
    ///
    /// The process handle is released even when the WebDriver quit fails.
    #[tracing::instrument(level = "info", skip(self))]
    pub(crate) async fn quit(&mut self) -> Result<(), AdapterError> {
        let result = self
            .webdriver
            .clone()
            .quit()
            .await
            .map_err(|error| AdapterError::Driver {
                operation: DriverOperation::QuitSession,
                detail: error.to_string(),
            });
        if let Some(process) = self.process.take() {
            tracing::debug!(pid = ?process.id(), "releasing sandboxed geckodriver process");
            drop(process);
        }
        result?;
        tracing::info!("Firefox WebDriver session stopped");
        Ok(())
    }
}

fn configure_error(error: thirtyfour::error::WebDriverError) -> AdapterError {
    AdapterError::Driver {
        operation: DriverOperation::ConfigureCapabilities,
        detail: error.to_string(),
    }
}
