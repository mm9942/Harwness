use harw_browser::error::Error as BrowserError;
use harw_browser::policy::{
    BiDiRequirement, OpenBrowserRequest, OriginPolicy, ProfilePolicy, Viewport,
};
use harw_browser_thirtyfour::{
    AdapterError, DriverOperation, FIREFOX_BIDI_BINDING_ID, FIREFOX_CAPABILITY_ID, FirefoxHost,
    FirefoxHostConfig,
};
use std::path::{Path, PathBuf};
use url::Url;

fn url(value: &str) -> Url {
    match Url::parse(value) {
        Ok(url) => url,
        Err(error) => panic!("test URL must be valid: {error}"),
    }
}

fn open_request(start_url: &str, allowed_host: &str) -> OpenBrowserRequest {
    OpenBrowserRequest {
        start_url: url(start_url),
        headless: true,
        profile: ProfilePolicy::Ephemeral,
        bidi: BiDiRequirement::Required,
        allowed_origins: OriginPolicy::new(vec![allowed_host.to_owned()], true),
        viewport: Some(Viewport {
            width: 1_280,
            height: 720,
        }),
    }
}

#[test]
fn firefox_host_preserves_explicit_process_configuration() {
    let binary = PathBuf::from("/opt/firefox/firefox");
    let config = FirefoxHostConfig::new()
        .with_firefox_binary(binary)
        .with_managed_driver(false);

    let host = match FirefoxHost::new(config) {
        Ok(host) => host,
        Err(error) => panic!("deterministic host construction failed: {error}"),
    };

    assert_eq!(
        host.config().firefox_binary(),
        Some(Path::new("/opt/firefox/firefox"))
    );
    assert!(!host.config().managed_driver());
}

#[test]
fn open_validation_rejects_a_disallowed_origin_before_driver_startup() {
    let host = match FirefoxHost::new(FirefoxHostConfig::new()) {
        Ok(host) => host,
        Err(error) => panic!("default host construction failed: {error}"),
    };
    let request = open_request("https://outside.example/login", "erp.example");

    let error = match host.validate_open_request(&request) {
        Ok(()) => panic!("a start URL outside the allowlist must be rejected"),
        Err(error) => error,
    };

    match error {
        BrowserError::OriginNotAllowed { origin } => {
            assert_eq!(origin, "https://outside.example");
        }
        other => panic!("unexpected validation error: {other}"),
    }
}

#[test]
fn open_validation_rejects_a_zero_sized_viewport_without_a_driver() {
    let host = match FirefoxHost::new(FirefoxHostConfig::new()) {
        Ok(host) => host,
        Err(error) => panic!("default host construction failed: {error}"),
    };
    let mut request = open_request("https://erp.example/login", "erp.example");
    request.viewport = Some(Viewport {
        width: 0,
        height: 720,
    });

    let error = match host.validate_open_request(&request) {
        Ok(()) => panic!("a zero-width viewport must be rejected"),
        Err(error) => error,
    };

    match error {
        BrowserError::InvalidArgument { detail } => {
            assert!(detail.contains("viewport"));
            assert!(detail.contains("width"));
        }
        other => panic!("unexpected validation error: {other}"),
    }
}

#[test]
fn typed_driver_timeout_maps_to_the_browser_timeout_boundary() {
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
        other => panic!("unexpected mapped error: {other}"),
    }
}

#[test]
fn typed_missing_bidi_maps_to_capability_unavailable() {
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
        other => panic!("unexpected mapped error: {other}"),
    }
}

#[test]
fn firefox_binding_metadata_is_stable_and_explicit() {
    let metadata = FirefoxHost::binding_metadata();

    assert_eq!(FIREFOX_BIDI_BINDING_ID, "thirtyfour.firefox-bidi@1");
    assert_eq!(metadata.binding_id, FIREFOX_BIDI_BINDING_ID);
    assert_eq!(metadata.capability_id, FIREFOX_CAPABILITY_ID);
    assert_eq!(metadata.browser_name, "firefox");
    assert!(metadata.webdriver_bidi);
    assert!(metadata.managed_driver_available);
}
