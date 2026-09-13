//! Adapter-private ownership of the raw thirtyfour driver lifecycle.

use crate::capabilities::FirefoxCapabilityPlan;
use crate::error::{AdapterError, DriverOperation};
use thirtyfour::prelude::{CapabilitiesHelper, DesiredCapabilities, WebDriver};

/// Sole owner of a live raw WebDriver connection inside this adapter.
///
/// The type and all of its methods are crate-private so downstream callers
/// can receive only Harwness-owned session handles and contracts.
pub(crate) struct FirefoxDriver {
    webdriver: WebDriver,
}

impl FirefoxDriver {
    /// Builds concrete Firefox capabilities and starts a managed geckodriver
    /// session using thirtyfour's verified Firefox-first path.
    #[tracing::instrument(
        level = "info",
        skip(plan),
        fields(
            browser = plan.browser_name(),
            headless = plan.headless(),
            webdriver_bidi = plan.webdriver_bidi_enabled()
        )
    )]
    pub(crate) async fn start(plan: FirefoxCapabilityPlan<'_>) -> Result<Self, AdapterError> {
        let mut capabilities = DesiredCapabilities::firefox();

        if plan.headless() {
            capabilities
                .set_headless()
                .map_err(|error| AdapterError::Driver {
                    operation: DriverOperation::ConfigureCapabilities,
                    detail: error.to_string(),
                })?;
        }

        if let Some(binary) = plan.firefox_binary() {
            let binary = binary.to_str().ok_or_else(|| AdapterError::Driver {
                operation: DriverOperation::ConfigureCapabilities,
                detail: "configured Firefox binary path is not valid UTF-8".to_owned(),
            })?;
            capabilities
                .set_firefox_binary(binary)
                .map_err(|error| AdapterError::Driver {
                    operation: DriverOperation::ConfigureCapabilities,
                    detail: error.to_string(),
                })?;
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

        if plan.webdriver_bidi_enabled() {
            capabilities
                .enable_bidi()
                .map_err(|error| AdapterError::Driver {
                    operation: DriverOperation::ConfigureCapabilities,
                    detail: error.to_string(),
                })?;
        }

        let webdriver =
            WebDriver::managed(capabilities)
                .await
                .map_err(|error| AdapterError::Driver {
                    operation: DriverOperation::StartSession,
                    detail: error.to_string(),
                })?;
        tracing::info!("managed Firefox WebDriver session started");

        Ok(Self { webdriver })
    }

    /// Borrows the raw driver for other crate-private adapter components.
    pub(crate) fn webdriver(&self) -> &WebDriver {
        &self.webdriver
    }

    /// Explicitly shuts down the browser session through a cloned connection
    /// handle because thirtyfour's `WebDriver::quit` consumes its receiver.
    #[tracing::instrument(level = "info", skip(self))]
    pub(crate) async fn quit(&self) -> Result<(), AdapterError> {
        self.webdriver
            .clone()
            .quit()
            .await
            .map_err(|error| AdapterError::Driver {
                operation: DriverOperation::QuitSession,
                detail: error.to_string(),
            })?;
        tracing::info!("Firefox WebDriver session stopped");
        Ok(())
    }
}
