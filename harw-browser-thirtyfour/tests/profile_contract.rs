use std::path::{Path, PathBuf};

use harw_browser::policy::{
    BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy, ProfilePolicy,
};
use harw_browser_thirtyfour::{FirefoxCapabilityFactory, FirefoxHostConfig};

mod common;
use common::{TestError, TestResult, ctx};

const PROFILE_BINDING: &str = "support-operator";
const PROFILE_DIRECTORY: &str = "/configured/firefox/profiles/support-operator";

fn persistent_request(binding: &str) -> TestResult<OpenBrowserRequest> {
    Ok(OpenBrowserRequest {
        start_url: url::Url::parse("https://erp.example.com/inbox")
            .map_err(ctx("fixture URL is valid"))?,
        headless: true,
        profile: ProfilePolicy::Persistent {
            binding: binding.to_owned(),
        },
        bidi: BiDiRequirement::Required,
        allowed_origins: OriginPolicy::from_origins(["https://erp.example.com"], true)
            .map_err(ctx("fixture origin policy is valid"))?,
        authentication_origins: OriginPolicy::default(),
        viewport: None,
        limits: BrowserLimits::default(),
    })
}

#[test]
fn config_binds_nonempty_profile_name_and_returns_directory_borrowed() -> TestResult {
    let directory = PathBuf::from(PROFILE_DIRECTORY);
    let config = FirefoxHostConfig::new()
        .with_profile_binding(PROFILE_BINDING, directory)
        .map_err(ctx("nonempty profile binding is valid"))?;

    assert_eq!(
        config.profile_directory(PROFILE_BINDING),
        Some(Path::new(PROFILE_DIRECTORY))
    );
    assert!(config.profile_directory("unknown-binding").is_none());

    let empty = FirefoxHostConfig::new()
        .with_profile_binding("  ", PathBuf::from("/configured/firefox/profiles/rejected"));
    assert!(empty.is_err());
    Ok(())
}

#[test]
fn persistent_profile_plan_rejects_unbound_binding_before_driver_start() -> TestResult {
    let config = FirefoxHostConfig::new();
    let request = persistent_request(PROFILE_BINDING)?;
    let result = FirefoxCapabilityFactory::new(&config).plan(&request);
    let Err(error) = result else {
        return Err(TestError::Unexpected(
            "unbound persistent profile must not produce a capability plan".into(),
        ));
    };

    let message = error.to_string();
    assert!(message.contains(PROFILE_BINDING));
    let normalized = message.to_ascii_lowercase();
    assert!(normalized.contains("configur") || normalized.contains("bound"));
    Ok(())
}

#[test]
fn persistent_profile_plan_preserves_bound_directory_identity() -> TestResult {
    let config = FirefoxHostConfig::new()
        .with_profile_binding(PROFILE_BINDING, PathBuf::from(PROFILE_DIRECTORY))
        .map_err(ctx("nonempty profile binding is valid"))?;
    let configured_directory = config
        .profile_directory(PROFILE_BINDING)
        .ok_or(TestError::Missing("bound directory is available"))?;

    let request = persistent_request(PROFILE_BINDING)?;
    let plan = FirefoxCapabilityFactory::new(&config)
        .plan(&request)
        .map_err(ctx(
            "bound persistent profile plans without starting Firefox",
        ))?;
    let planned_directory = plan.profile_directory().ok_or(TestError::Missing(
        "persistent plan retains its profile directory",
    ))?;

    assert_eq!(planned_directory, Path::new(PROFILE_DIRECTORY));
    assert!(std::ptr::eq(planned_directory, configured_directory));
    Ok(())
}
