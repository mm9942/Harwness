//! Post-effect origin enforcement for every browsing context (F-009).
//!
//! # Description
//! The origin boundary is not only checked for model-supplied navigation
//! targets: after every action, navigation, wait, find and observation the
//! runtime reads the location of **all** open top-level windows (redirects,
//! clicks, form submission, script navigation and `window.open` included) and
//! validates each with `OpenBrowserRequest::check_observed_location`. Any
//! violation, and any failure to read the locations, aborts the whole session
//! (fail closed) and surfaces `OriginNotAllowed` (or the read error) instead of
//! page content.
//!
//! # Concurrency
//! [`LocationProbe`] implementations must be `Send + Sync`; the runtime's
//! implementation acquires the driver mutex internally, so callers must not
//! hold it.
//!
//! # Errors
//! `harw_browser::Error::OriginNotAllowed` on a policy breach, otherwise the
//! probe's own error.

use async_trait::async_trait;
use harw_browser::policy::OpenBrowserRequest;

/// Source of observed window locations and the session kill switch.
// `async_trait` setzt auf jede erzeugte Methode ein `#[must_use]`, obwohl der
// erzeugte Rückgabetyp (`Pin<Box<dyn Future>>`) ohnehin schon als `must_use`
// gilt. Die Doppelung entsteht im Makro, nicht in diesem Code — deshalb hier
// eine benannte Ausnahme statt einer Änderung an den Methodensignaturen.
#[allow(clippy::double_must_use)]
#[async_trait]
pub(crate) trait LocationProbe: Send + Sync {
    /// Returns the current URL of every open top-level window of the session.
    async fn open_locations(&self) -> harw_browser::Result<Vec<url::Url>>;

    /// Terminates the session after a policy breach; must be idempotent.
    async fn abort_session(&self, reason: &str);
}

/// Validates all observed locations and aborts the session on any breach.
pub(crate) async fn enforce_location_policy<P>(
    probe: &P,
    request: &OpenBrowserRequest,
) -> harw_browser::Result<()>
where
    P: LocationProbe + ?Sized,
{
    let locations = match probe.open_locations().await {
        Ok(locations) => locations,
        Err(error) => {
            tracing::error!(%error, "could not read browser locations; aborting session");
            probe
                .abort_session("browser locations could not be verified")
                .await;
            return Err(error);
        }
    };
    for location in &locations {
        if let Err(error) = request.check_observed_location(location) {
            tracing::error!(
                origin = %location.origin().ascii_serialization(),
                "observed browser location left the origin policy; aborting session"
            );
            probe
                .abort_session("observed browser location left the origin policy")
                .await;
            return Err(error);
        }
    }
    tracing::trace!(
        windows = locations.len(),
        "browser locations within origin policy"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_browser::error::Error;
    use harw_browser::policy::{BiDiRequirement, BrowserLimits, OriginPolicy, ProfilePolicy};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeProbe {
        locations: harw_browser::Result<Vec<url::Url>>,
        aborts: AtomicUsize,
    }

    impl FakeProbe {
        fn with(locations: &[&str]) -> TestResult<Self> {
            let parsed = locations
                .iter()
                .map(|value| url::Url::parse(value).map_err(ctx("valid test url")))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Self {
                locations: Ok(parsed),
                aborts: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl LocationProbe for FakeProbe {
        async fn open_locations(&self) -> harw_browser::Result<Vec<url::Url>> {
            match &self.locations {
                Ok(locations) => Ok(locations.clone()),
                Err(_) => Err(Error::CapabilityUnavailable {
                    detail: "window vanished".to_owned(),
                }),
            }
        }

        async fn abort_session(&self, _reason: &str) {
            self.aborts.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn request() -> TestResult<OpenBrowserRequest> {
        Ok(OpenBrowserRequest {
            start_url: url::Url::parse("https://erp.example.com/")
                .map_err(ctx("valid test url"))?,
            headless: true,
            profile: ProfilePolicy::Ephemeral,
            bidi: BiDiRequirement::NotRequired,
            allowed_origins: OriginPolicy::from_origins(["https://erp.example.com"], true)
                .map_err(ctx("valid test policy"))?,
            authentication_origins: OriginPolicy::from_origins(["https://sso.example.com"], true)
                .map_err(ctx("valid test policy"))?,
            viewport: None,
            limits: BrowserLimits::default(),
        })
    }

    #[tokio::test]
    async fn test_enforce_location_policy_allows_policy_and_auth_origins() -> TestResult {
        let probe = FakeProbe::with(&[
            "https://erp.example.com/inbox",
            "https://sso.example.com/login",
        ])?;
        enforce_location_policy(&probe, &request()?)
            .await
            .map_err(ctx("allowed locations pass"))?;
        assert_eq!(probe.aborts.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_enforce_location_policy_redirect_outside_policy_aborts_session() -> TestResult {
        let probe =
            FakeProbe::with(&["https://erp.example.com/", "https://evil.example.net/phish"])?;
        match enforce_location_policy(&probe, &request()?).await {
            Err(Error::OriginNotAllowed { origin }) => {
                assert_eq!(origin, "https://evil.example.net")
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected origin violation, got {other:?}"
                )));
            }
        }
        assert_eq!(probe.aborts.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_enforce_location_policy_file_and_blank_locations_abort_session() -> TestResult {
        for location in [
            "file:///etc/passwd",
            "about:blank",
            "http://127.0.0.1:4444/session",
        ] {
            let probe = FakeProbe::with(&[location])?;
            assert!(
                enforce_location_policy(&probe, &request()?).await.is_err(),
                "{location} must be rejected"
            );
            assert_eq!(probe.aborts.load(Ordering::SeqCst), 1, "{location}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_enforce_location_policy_unreadable_locations_fail_closed() -> TestResult {
        let probe = FakeProbe {
            locations: Err(Error::Timeout {
                detail: "unused".to_owned(),
            }),
            aborts: AtomicUsize::new(0),
        };
        assert!(matches!(
            enforce_location_policy(&probe, &request()?).await,
            Err(Error::CapabilityUnavailable { .. })
        ));
        assert_eq!(probe.aborts.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
