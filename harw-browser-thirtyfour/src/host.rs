//! Firefox host construction and pre-I/O request validation.

use crate::bidi_pump::BidiPump;
use crate::capabilities::FirefoxCapabilityFactory;
use crate::config::FirefoxHostConfig;
use crate::driver::FirefoxDriver;
use crate::error::{AdapterError, DriverOperation};
use crate::journal::EventJournalPolicy;
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
    pub managed_driver_available: bool,
}

static FIREFOX_BINDING_METADATA: FirefoxBindingMetadata = FirefoxBindingMetadata {
    binding_id: FIREFOX_BIDI_BINDING_ID,
    capability_id: FIREFOX_CAPABILITY_ID,
    browser_name: "firefox",
    webdriver_bidi: true,
    managed_driver_available: true,
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
            managed_driver = config.managed_driver(),
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
        if !request.allowed_origins.is_allowed(&request.start_url) {
            let origin = request.start_url.origin().ascii_serialization();
            tracing::warn!(origin, "rejected browser start URL outside origin policy");
            return Err(BrowserError::OriginNotAllowed { origin });
        }

        if let Some(viewport) = request.viewport {
            if viewport.width == 0 {
                tracing::warn!(
                    height = viewport.height,
                    "rejected zero-width browser viewport"
                );
                return Err(BrowserError::InvalidArgument {
                    detail: "viewport width must be greater than zero".to_owned(),
                });
            }
            if viewport.height == 0 {
                tracing::warn!(
                    width = viewport.width,
                    "rejected zero-height browser viewport"
                );
                return Err(BrowserError::InvalidArgument {
                    detail: "viewport height must be greater than zero".to_owned(),
                });
            }
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
        if !self.config.managed_driver() {
            return Err(BrowserError::CapabilityUnavailable {
                detail: "the Firefox adapter currently requires managed geckodriver lifecycle"
                    .to_owned(),
            });
        }

        let plan = FirefoxCapabilityFactory::new(&self.config).plan(&request)?;
        let driver = FirefoxDriver::start(plan)
            .await
            .map_err(BrowserError::from)?;

        let bidi_status = if matches!(request.bidi, BiDiRequirement::NotRequired) {
            CapabilityStatus::Unavailable
        } else {
            match driver.webdriver().bidi().await {
                Ok(_) => CapabilityStatus::Native,
                Err(error) if matches!(request.bidi, BiDiRequirement::Required) => {
                    let detail = error.to_string();
                    let _ = driver.quit().await;
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
                let _ = driver.quit().await;
                return Err(driver_error("set initial Firefox viewport", error));
            }
        }
        if let Err(error) = driver.webdriver().goto(request.start_url.as_str()).await {
            let _ = driver.quit().await;
            return Err(driver_error("navigate to Firefox start URL", error));
        }
        let primary_window = match driver.webdriver().window().await {
            Ok(window) => window,
            Err(error) => {
                let _ = driver.quit().await;
                return Err(driver_error("read primary Firefox window", error));
            }
        };

        let session_id = BrowserSessionId::new();
        let primary_context_id = BrowserContextId::new();
        let bidi_requirement = request.bidi;
        let event_policy = EventJournalPolicy::bounded(1_024)?;
        let runtime = Arc::new(FirefoxRuntime::new(
            driver,
            session_id,
            request.allowed_origins,
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
                    let _ = <FirefoxRuntime as BrowserRuntime>::close(runtime.as_ref()).await;
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
                let _ = <FirefoxRuntime as BrowserRuntime>::close(runtime.as_ref()).await;
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

fn driver_error(operation: &'static str, error: thirtyfour::error::WebDriverError) -> BrowserError {
    BrowserError::from(AdapterError::Driver {
        operation: DriverOperation::ExecuteCommand,
        detail: format!("{operation}: {error}"),
    })
}
