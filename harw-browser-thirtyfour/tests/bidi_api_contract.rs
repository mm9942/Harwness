mod common;

use common::{TestError, TestResult, ctx};
use harw_browser::policy::{
    BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy, ProfilePolicy,
};
use harw_browser_thirtyfour::{
    BackpressureDisposition, BidiEventClass, BidiEventDomain, BidiSubscriptionPlan,
    EventJournalPolicy, FirefoxCapabilityFactory, FirefoxHostConfig,
};
use std::path::{Path, PathBuf};
use url::Url;

fn firefox_request() -> TestResult<OpenBrowserRequest> {
    let start_url = Url::parse("https://erp.example/login").map_err(ctx("Test-URL parsen"))?;
    Ok(OpenBrowserRequest {
        start_url,
        headless: true,
        profile: ProfilePolicy::Ephemeral,
        bidi: BiDiRequirement::Required,
        allowed_origins: OriginPolicy::from_origins(["https://erp.example"], true)
            .map_err(ctx("Fixture-Origin-Policy ist ungültig"))?,
        authentication_origins: OriginPolicy::default(),
        viewport: None,
        limits: BrowserLimits::default(),
    })
}

#[test]
fn firefox_capability_factory_builds_a_harwness_owned_bidi_plan() -> TestResult {
    let config =
        FirefoxHostConfig::new().with_firefox_binary(PathBuf::from("/opt/firefox/firefox"));
    let factory = FirefoxCapabilityFactory::new(&config);

    let plan = factory
        .plan(&firefox_request()?)
        .map_err(ctx("valider Firefox-Capability-Plan ist fehlgeschlagen"))?;

    assert_eq!(plan.browser_name(), "firefox");
    assert!(plan.headless());
    assert!(plan.webdriver_bidi_enabled());
    assert_eq!(
        plan.firefox_binary(),
        Some(Path::new("/opt/firefox/firefox"))
    );
    Ok(())
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
fn bounded_event_journal_policy_never_silently_drops_control_events() -> TestResult {
    let policy = EventJournalPolicy::bounded(256)
        .map_err(ctx("nonzero event-journal capacity ist fehlgeschlagen"))?;

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
    Ok(())
}

#[test]
fn event_journal_rejects_an_unbounded_zero_capacity_configuration() -> TestResult {
    let Err(error) = EventJournalPolicy::bounded(0) else {
        return Err(TestError::Unexpected(
            "zero-capacity journal darf nicht akzeptiert werden".to_owned(),
        ));
    };

    assert!(error.to_string().contains("capacity"));
    Ok(())
}
