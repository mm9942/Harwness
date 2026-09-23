//! Firefox host construction, pre-I/O request validation and session start (B-ADAPT).
//!
//! # Description
//! `open` validates the request (`OpenBrowserRequest::validate`), requires a
//! pinned geckodriver and a sandbox launcher from host configuration, starts
//! the driver through [`crate::launcher::launch_pinned_driver`], navigates to
//! the start URL and verifies the observed location before the session is
//! handed out. There is no managed driver fallback.

use crate::bidi_pump::BidiPump;
use crate::capabilities::FirefoxCapabilityFactory;
use crate::config::FirefoxHostConfig;
use crate::driver::FirefoxDriver;
use crate::error::{AdapterError, DriverOperation};
use crate::launcher::launch_pinned_driver;
use crate::runtime::FirefoxRuntime;
use async_trait::async_trait;
use harw_browser::capability::CapabilityStatus;
use harw_browser::error::Error as BrowserError;
use harw_browser::host::{BrowserHost, BrowserRuntime};
use harw_browser::ids::{BrowserContextId, BrowserSessionId};
use harw_browser::policy::{BiDiRequirement, OpenBrowserRequest};
use harw_browser::session::BrowserSessionHandle;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Stable identity of the concrete thirtyfour Firefox/BiDi binding.
pub const FIREFOX_BIDI_BINDING_ID: &str = "thirtyfour.firefox-bidi@1";

/// Stable host capability implemented by the Firefox/BiDi binding.
pub const FIREFOX_CAPABILITY_ID: &str = "browser.firefox-bidi.v1";

/// Static public metadata for the concrete adapter binding.
///
/// This deliberately contains only Harwness-owned scalar values, keeping
/// thirtyfour capability types behind the adapter boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirefoxBindingMetadata {
    pub binding_id: &'static str,
    pub capability_id: &'static str,
    pub browser_name: &'static str,
    pub webdriver_bidi: bool,
    /// Always `true`: geckodriver must be pinned by path and SHA-256 and runs sandboxed.
    pub pinned_driver_required: bool,
}

static FIREFOX_BINDING_METADATA: FirefoxBindingMetadata = FirefoxBindingMetadata {
    binding_id: FIREFOX_BIDI_BINDING_ID,
    capability_id: FIREFOX_CAPABILITY_ID,
    browser_name: "firefox",
    webdriver_bidi: true,
    pinned_driver_required: true,
};

/// Owns configuration for Firefox sessions provided by this adapter.
pub struct FirefoxHost {
    config: FirefoxHostConfig,
    sessions: RwLock<HashMap<BrowserSessionId, SessionEntry>>,
}

struct SessionEntry {
    runtime: Arc<FirefoxRuntime>,
    _pump: Option<BidiPump>,
}

impl fmt::Debug for FirefoxHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FirefoxHost")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl FirefoxHost {
    /// Constructs a host without starting Firefox or geckodriver.
    pub fn new(config: FirefoxHostConfig) -> Result<Self, AdapterError> {
        tracing::debug!(
            binding_id = FIREFOX_BIDI_BINDING_ID,
            geckodriver_pinned = config.geckodriver_pin().is_some(),
            launcher_configured = config.launcher().is_some(),
            explicit_firefox_binary = config.firefox_binary().is_some(),
            "constructed Firefox browser host"
        );
        Ok(Self {
            config,
            sessions: RwLock::new(HashMap::new()),
        })
    }

    /// Returns the process configuration borrowed from this host.
    pub fn config(&self) -> &FirefoxHostConfig {
        &self.config
    }

    /// Returns stable compile-time metadata for this concrete binding.
    pub fn binding_metadata() -> &'static FirefoxBindingMetadata {
        &FIREFOX_BINDING_METADATA
    }

    /// Checks request invariants that must fail before any driver is started.
    #[tracing::instrument(
        level = "debug",
        skip(self, request),
        fields(binding_id = FIREFOX_BIDI_BINDING_ID, start_url = %request.start_url)
    )]
    pub fn validate_open_request(&self, request: &OpenBrowserRequest) -> harw_browser::Result<()> {
        if let Err(error) = request.validate() {
            tracing::warn!(%error, "rejected browser open request before driver start");
            return Err(error);
        }
        tracing::debug!("browser open request passed pre-driver validation");
        Ok(())
    }
}

#[async_trait]
impl BrowserHost for FirefoxHost {
    async fn open(
        &self,
        request: OpenBrowserRequest,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        self.validate_open_request(&request)?;
        let pin = self.config.geckodriver_pin().ok_or_else(|| {
            BrowserError::CapabilityUnavailable {
                detail:
                    "no pinned geckodriver (path + SHA-256) is configured for the Firefox adapter"
                        .to_owned(),
            }
        })?;
        let launcher =
            self.config
                .launcher()
                .ok_or_else(|| BrowserError::CapabilityUnavailable {
                    detail: "no sandbox launcher is configured for the Firefox adapter".to_owned(),
                })?;

        let plan = FirefoxCapabilityFactory::new(&self.config).plan(&request)?;
        let event_policy = self.config.journal_policy()?;
        let launched = launch_pinned_driver(launcher.as_ref(), pin)
            .await
            .map_err(BrowserError::from)?;
        let mut driver = FirefoxDriver::start(plan, launched)
            .await
            .map_err(BrowserError::from)?;

        let bidi_status = if matches!(request.bidi, BiDiRequirement::NotRequired) {
            CapabilityStatus::Unavailable
        } else {
            match driver.webdriver().bidi().await {
                Ok(_) => CapabilityStatus::Native,
                Err(error) if matches!(request.bidi, BiDiRequirement::Required) => {
                    let detail = error.to_string();
                    quit_after_failed_open(&mut driver).await;
                    return Err(BrowserError::CapabilityUnavailable {
                        detail: format!(
                            "Firefox WebDriver BiDi was required but connection failed: {detail}"
                        ),
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        detail = %error,
                        "preferred Firefox WebDriver BiDi unavailable; continuing degraded"
                    );
                    CapabilityStatus::Unavailable
                }
            }
        };

        if let Some(viewport) = request.viewport {
            if let Err(error) = driver
                .webdriver()
                .set_window_rect(0, 0, viewport.width, viewport.height)
                .await
            {
                quit_after_failed_open(&mut driver).await;
                return Err(driver_error("set initial Firefox viewport", error));
            }
        }
        if let Err(error) = driver.webdriver().goto(request.start_url.as_str()).await {
            quit_after_failed_open(&mut driver).await;
            return Err(driver_error("navigate to Firefox start URL", error));
        }
        // The start URL may redirect; the landed location must satisfy the policy (F-009).
        match driver.webdriver().current_url().await {
            Ok(landed) => {
                if let Err(error) = request.check_observed_location(&landed) {
                    tracing::error!(
                        origin = %landed.origin().ascii_serialization(),
                        "Firefox start URL landed outside the origin policy"
                    );
                    quit_after_failed_open(&mut driver).await;
                    return Err(error);
                }
            }
            Err(error) => {
                quit_after_failed_open(&mut driver).await;
                return Err(driver_error("read Firefox start location", error));
            }
        }
        let primary_window = match driver.webdriver().window().await {
            Ok(window) => window,
            Err(error) => {
                quit_after_failed_open(&mut driver).await;
                return Err(driver_error("read primary Firefox window", error));
            }
        };

        let session_id = BrowserSessionId::new();
        let primary_context_id = BrowserContextId::new();
        let bidi_requirement = request.bidi;
        let runtime = Arc::new(FirefoxRuntime::new(
            driver,
            session_id,
            request,
            bidi_status,
            primary_context_id,
            primary_window,
            event_policy,
        ));

        let bidi_mapping_ready = if bidi_status == CapabilityStatus::Native {
            let primary_bidi_context = {
                let driver = runtime.driver().await;
                match driver.webdriver().bidi().await {
                    Ok(bidi) => bidi
                        .browsing_context()
                        .top_level()
                        .await
                        .map(|context| context.as_str().to_owned())
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                }
            };

            let mapping = match primary_bidi_context {
                Ok(primary_bidi_context) => runtime
                    .register_bidi_context(primary_bidi_context, primary_context_id)
                    .await
                    .map_err(|error| error.to_string()),
                Err(detail) => Err(detail),
            };
            if let Err(detail) = mapping {
                if matches!(bidi_requirement, BiDiRequirement::Required) {
                    if let Err(error) =
                        <FirefoxRuntime as BrowserRuntime>::close(runtime.as_ref()).await
                    {
                        tracing::warn!(%error, "Firefox runtime close failed after an aborted open");
                    }
                    return Err(BrowserError::CapabilityUnavailable {
                        detail: format!(
                            "required Firefox BiDi primary-context mapping failed: {detail}"
                        ),
                    });
                }
                tracing::warn!(
                    detail,
                    "preferred Firefox BiDi connected without a trustworthy primary-context mapping; continuing degraded"
                );
                false
            } else {
                true
            }
        } else {
            false
        };

        let pump = if bidi_mapping_ready {
            let outcome = BidiPump::start(Arc::clone(&runtime)).await;
            tracing::info!(
                browsing_context = ?outcome.browsing_context,
                network = ?outcome.network,
                log = ?outcome.log,
                script = ?outcome.script,
                "Firefox BiDi event-domain capability result"
            );
            runtime
                .set_event_capabilities(
                    outcome.browsing_context,
                    outcome.network,
                    outcome.log,
                    outcome.script,
                )
                .await;
            let any_native = [
                outcome.browsing_context,
                outcome.network,
                outcome.log,
                outcome.script,
            ]
            .into_iter()
            .any(|status| status == CapabilityStatus::Native);
            if matches!(bidi_requirement, BiDiRequirement::Required) && !any_native {
                if let Err(error) =
                    <FirefoxRuntime as BrowserRuntime>::close(runtime.as_ref()).await
                {
                    tracing::warn!(%error, "Firefox runtime close failed after an aborted open");
                }
                return Err(BrowserError::CapabilityUnavailable {
                    detail: "required Firefox BiDi connected but no event domain subscription became available"
                        .to_owned(),
                });
            }
            outcome.pump
        } else {
            None
        };
        self.sessions.write().await.insert(
            session_id,
            SessionEntry {
                runtime: Arc::clone(&runtime),
                _pump: pump,
            },
        );

        Ok(BrowserSessionHandle::new(runtime))
    }

    async fn session(&self, id: &BrowserSessionId) -> harw_browser::Result<BrowserSessionHandle> {
        let runtime = self
            .sessions
            .read()
            .await
            .get(id)
            .filter(|entry| !entry.runtime.is_closed())
            .map(|entry| Arc::clone(&entry.runtime))
            .ok_or(BrowserError::SessionNotFound { session_id: *id })?;
        Ok(BrowserSessionHandle::new(runtime))
    }

    async fn close(&self, id: &BrowserSessionId) -> harw_browser::Result<()> {
        let entry = self
            .sessions
            .write()
            .await
            .remove(id)
            .ok_or(BrowserError::SessionNotFound { session_id: *id })?;
        <FirefoxRuntime as BrowserRuntime>::close(entry.runtime.as_ref()).await
    }
}

async fn quit_after_failed_open(driver: &mut FirefoxDriver) {
    if let Err(error) = driver.quit().await {
        tracing::warn!(%error, "Firefox driver quit failed after an aborted open");
    }
}

fn driver_error(operation: &'static str, error: thirtyfour::error::WebDriverError) -> BrowserError {
    BrowserError::from(AdapterError::Driver {
        operation: DriverOperation::ExecuteCommand,
        detail: format!("{operation}: {error}"),
    })
}
