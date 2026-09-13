use harw_browser::policy::{BiDiRequirement, OpenBrowserRequest, OriginPolicy, ProfilePolicy};
use harw_browser_thirtyfour::{
    BackpressureDisposition, BidiEventClass, BidiEventDomain, BidiSubscriptionPlan,
    EventJournalPolicy, FirefoxCapabilityFactory, FirefoxHostConfig,
};
use std::path::{Path, PathBuf};
use url::Url;

fn firefox_request() -> OpenBrowserRequest {
    let start_url = match Url::parse("https://erp.example/login") {
        Ok(url) => url,
        Err(error) => panic!("test URL must be valid: {error}"),
    };
    OpenBrowserRequest {
        start_url,
        headless: true,
        profile: ProfilePolicy::Ephemeral,
        bidi: BiDiRequirement::Required,
        allowed_origins: OriginPolicy::new(vec!["erp.example".to_owned()], true),
        viewport: None,
    }
}

#[test]
fn firefox_capability_factory_builds_a_harwness_owned_bidi_plan() {
    let config =
        FirefoxHostConfig::new().with_firefox_binary(PathBuf::from("/opt/firefox/firefox"));
    let factory = FirefoxCapabilityFactory::new(&config);

    let plan = match factory.plan(&firefox_request()) {
        Ok(plan) => plan,
        Err(error) => panic!("valid Firefox capability plan failed: {error}"),
    };

    assert_eq!(plan.browser_name(), "firefox");
    assert!(plan.headless());
    assert!(plan.webdriver_bidi_enabled());
    assert_eq!(
        plan.firefox_binary(),
        Some(Path::new("/opt/firefox/firefox"))
    );
}

#[test]
fn standard_bidi_subscription_plan_names_all_initial_event_domains() {
    let plan = BidiSubscriptionPlan::standard();

    assert_eq!(
        plan.domains(),
        &[
            BidiEventDomain::BrowsingContext,
            BidiEventDomain::Network,
            BidiEventDomain::Log,
            BidiEventDomain::Script,
        ]
    );
    assert_eq!(
        BidiEventDomain::BrowsingContext.wire_name(),
        "browsingContext"
    );
    assert_eq!(BidiEventDomain::Network.wire_name(), "network");
    assert_eq!(BidiEventDomain::Log.wire_name(), "log");
    assert_eq!(BidiEventDomain::Script.wire_name(), "script");
}

#[test]
fn bounded_event_journal_policy_never_silently_drops_control_events() {
    let policy = match EventJournalPolicy::bounded(256) {
        Ok(policy) => policy,
        Err(error) => panic!("nonzero event-journal capacity failed: {error}"),
    };

    assert_eq!(policy.capacity(), 256);
    assert_eq!(
        policy.disposition(BidiEventClass::CriticalControl),
        BackpressureDisposition::Retain
    );
    assert_eq!(
        policy.disposition(BidiEventClass::RequestLifecycle),
        BackpressureDisposition::AggregateWhenFull
    );
    assert_eq!(
        policy.disposition(BidiEventClass::Console),
        BackpressureDisposition::Deduplicate
    );
    assert_eq!(
        policy.disposition(BidiEventClass::StaticAsset),
        BackpressureDisposition::Sample
    );
    assert_eq!(
        policy.disposition(BidiEventClass::ArtifactPayload),
        BackpressureDisposition::StoreExternally
    );
}

#[test]
fn event_journal_rejects_an_unbounded_zero_capacity_configuration() {
    let error = match EventJournalPolicy::bounded(0) {
        Ok(_) => panic!("zero-capacity journal must not be accepted"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("capacity"));
}
