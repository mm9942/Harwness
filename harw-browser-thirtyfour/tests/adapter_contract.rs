mod common;

use common::{TestError, TestResult, ctx};
use harw_browser::error::Error as BrowserError;
use harw_browser::host::BrowserHost;
use harw_browser::policy::{
    BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy, ProfilePolicy, Viewport,
};
use harw_browser_thirtyfour::{
    AdapterError, DriverOperation, FIREFOX_BIDI_BINDING_ID, FIREFOX_CAPABILITY_ID, FirefoxHost,
    FirefoxHostConfig,
};
use std::path::{Path, PathBuf};
use url::Url;

fn url(value: &str) -> TestResult<Url> {
    Url::parse(value).map_err(ctx("test URL must be valid"))
}

fn open_request(start_url: &str, allowed_origin: &str) -> TestResult<OpenBrowserRequest> {
    let allowed_origins = OriginPolicy::from_origins([allowed_origin], true)
        .map_err(ctx("test origin policy must be valid"))?;
    Ok(OpenBrowserRequest {
        start_url: url(start_url)?,
        headless: true,
        profile: ProfilePolicy::Ephemeral,
        bidi: BiDiRequirement::Required,
        allowed_origins,
        authentication_origins: OriginPolicy::default(),
        viewport: Some(Viewport {
            width: 1_280,
            height: 720,
        }),
        limits: BrowserLimits::default(),
    })
}

#[test]
fn firefox_host_preserves_explicit_process_configuration() -> TestResult {
    let binary = PathBuf::from("/opt/firefox/firefox");
    let config = FirefoxHostConfig::new().with_firefox_binary(binary);

    let host = FirefoxHost::new(config).map_err(ctx("deterministic host construction failed"))?;

    assert_eq!(
        host.config().firefox_binary(),
        Some(Path::new("/opt/firefox/firefox"))
    );
    assert!(host.config().geckodriver_pin().is_none());
    assert!(host.config().launcher().is_none());
    Ok(())
}

#[tokio::test]
async fn test_open_without_pinned_geckodriver_fails_closed_before_any_process_start() -> TestResult
{
    let host = FirefoxHost::new(FirefoxHostConfig::new())
        .map_err(ctx("default host construction failed"))?;
    let request = open_request("https://erp.example/login", "https://erp.example")?;

    match host.open(request).await {
        Err(BrowserError::CapabilityUnavailable { detail }) => {
            assert!(detail.contains("pinned geckodriver"));
        }
        Err(other) => {
            return Err(TestError::Unexpected(format!(
                "unexpected open error: {other}"
            )));
        }
        Ok(_) => {
            return Err(TestError::Unexpected(
                "open must fail without a pinned geckodriver".into(),
            ));
        }
    }
    Ok(())
}

#[test]
fn open_validation_rejects_a_disallowed_origin_before_driver_startup() -> TestResult {
    let host = FirefoxHost::new(FirefoxHostConfig::new())
        .map_err(ctx("default host construction failed"))?;
    let request = open_request("https://outside.example/login", "https://erp.example")?;

    let Err(error) = host.validate_open_request(&request) else {
        return Err(TestError::Unexpected(
            "a start URL outside the allowlist must be rejected".into(),
        ));
    };

    match error {
        BrowserError::OriginNotAllowed { origin } => {
            assert_eq!(origin, "https://outside.example");
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "unexpected validation error: {other}"
            )));
        }
    }
    Ok(())
}

#[test]
fn open_validation_rejects_a_zero_sized_viewport_without_a_driver() -> TestResult {
    let host = FirefoxHost::new(FirefoxHostConfig::new())
        .map_err(ctx("default host construction failed"))?;
    let mut request = open_request("https://erp.example/login", "https://erp.example")?;
    request.viewport = Some(Viewport {
        width: 0,
        height: 720,
    });

    let Err(error) = host.validate_open_request(&request) else {
        return Err(TestError::Unexpected(
            "a zero-width viewport must be rejected".into(),
        ));
    };

    match error {
        BrowserError::InvalidArgument { detail } => {
            assert!(detail.contains("viewport"));
            assert!(detail.contains("0x720"));
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "unexpected validation error: {other}"
            )));
        }
    }
    Ok(())
}

#[test]
fn typed_driver_timeout_maps_to_the_browser_timeout_boundary() -> TestResult {
    let adapter_error = AdapterError::DriverTimeout {
        operation: DriverOperation::StartSession,
        detail: "geckodriver did not answer before the startup deadline".to_owned(),
    };

    let browser_error = BrowserError::from(adapter_error);

    match browser_error {
        BrowserError::Timeout { detail } => {
            assert!(detail.contains("start session"));
            assert!(detail.contains("startup deadline"));
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "unexpected mapped error: {other}"
            )));
        }
    }
    Ok(())
}

#[test]
fn typed_missing_bidi_maps_to_capability_unavailable() -> TestResult {
    let adapter_error = AdapterError::CapabilityUnavailable {
        capability: FIREFOX_CAPABILITY_ID,
        detail: "WebDriver did not return a BiDi websocket URL".to_owned(),
    };

    let browser_error = BrowserError::from(adapter_error);

    match browser_error {
        BrowserError::CapabilityUnavailable { detail } => {
            assert!(detail.contains(FIREFOX_CAPABILITY_ID));
            assert!(detail.contains("BiDi websocket URL"));
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "unexpected mapped error: {other}"
            )));
        }
    }
    Ok(())
}

#[test]
fn firefox_binding_metadata_is_stable_and_explicit() {
    let metadata = FirefoxHost::binding_metadata();

    assert_eq!(FIREFOX_BIDI_BINDING_ID, "thirtyfour.firefox-bidi@1");
    assert_eq!(metadata.binding_id, FIREFOX_BIDI_BINDING_ID);
    assert_eq!(metadata.capability_id, FIREFOX_CAPABILITY_ID);
    assert_eq!(metadata.browser_name, "firefox");
    assert!(metadata.webdriver_bidi);
    assert!(metadata.pinned_driver_required);
}
