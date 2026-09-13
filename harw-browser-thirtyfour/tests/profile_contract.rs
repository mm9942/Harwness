use std::path::{Path, PathBuf};

use harw_browser::policy::{BiDiRequirement, OpenBrowserRequest, OriginPolicy, ProfilePolicy};
use harw_browser_thirtyfour::{FirefoxCapabilityFactory, FirefoxHostConfig};

const PROFILE_BINDING: &str = "support-operator";
const PROFILE_DIRECTORY: &str = "/configured/firefox/profiles/support-operator";

fn persistent_request(binding: &str) -> OpenBrowserRequest {
    OpenBrowserRequest {
        start_url: url::Url::parse("https://erp.example.com/inbox").expect("fixture URL is valid"),
        headless: true,
        profile: ProfilePolicy::Persistent {
            binding: binding.to_owned(),
        },
        bidi: BiDiRequirement::Required,
        allowed_origins: OriginPolicy::new(vec!["erp.example.com".to_owned()], true),
        viewport: None,
    }
}

#[test]
fn config_binds_nonempty_profile_name_and_returns_directory_borrowed() {
    let directory = PathBuf::from(PROFILE_DIRECTORY);
    let config = FirefoxHostConfig::new()
        .with_profile_binding(PROFILE_BINDING, directory)
        .expect("nonempty profile binding is valid");

    assert_eq!(
        config.profile_directory(PROFILE_BINDING),
        Some(Path::new(PROFILE_DIRECTORY))
    );
    assert!(config.profile_directory("unknown-binding").is_none());

    let empty = FirefoxHostConfig::new()
        .with_profile_binding("  ", PathBuf::from("/configured/firefox/profiles/rejected"));
    assert!(empty.is_err());
}

#[test]
fn persistent_profile_plan_rejects_unbound_binding_before_driver_start() {
    let config = FirefoxHostConfig::new();
    let error = FirefoxCapabilityFactory::new(&config)
        .plan(&persistent_request(PROFILE_BINDING))
        .expect_err("unbound persistent profile must not produce a capability plan");

    let message = error.to_string();
    assert!(message.contains(PROFILE_BINDING));
    let normalized = message.to_ascii_lowercase();
    assert!(normalized.contains("configur") || normalized.contains("bound"));
}

#[test]
fn persistent_profile_plan_preserves_bound_directory_identity() {
    let config = FirefoxHostConfig::new()
        .with_profile_binding(PROFILE_BINDING, PathBuf::from(PROFILE_DIRECTORY))
        .expect("nonempty profile binding is valid");
    let configured_directory = config
        .profile_directory(PROFILE_BINDING)
        .expect("bound directory is available");

    let plan = FirefoxCapabilityFactory::new(&config)
        .plan(&persistent_request(PROFILE_BINDING))
        .expect("bound persistent profile plans without starting Firefox");
    let planned_directory = plan
        .profile_directory()
        .expect("persistent plan retains its profile directory");

    assert_eq!(planned_directory, Path::new(PROFILE_DIRECTORY));
    assert!(std::ptr::eq(planned_directory, configured_directory));
}
