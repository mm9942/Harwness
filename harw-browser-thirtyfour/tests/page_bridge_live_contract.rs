//! Explicit opt-in, end-to-end Firefox coverage for the typed page bridge.
//!
//! This test is ignored by default because it starts a real Firefox and may
//! cause thirtyfour to manage a local geckodriver.  It is additionally gated
//! on `HARW_RUN_FIREFOX_LIVE=1` so even `cargo test -- --ignored` cannot
//! accidentally start browser infrastructure.

use harw_browser::host::BrowserHost;
use harw_browser::page_bridge::{PageBridgeInstallRequest, PageBridgePolicy};
use harw_browser::policy::{BiDiRequirement, OpenBrowserRequest, OriginPolicy, ProfilePolicy};
use harw_browser_thirtyfour::{FirefoxHost, FirefoxHostConfig};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::time::Duration;

const LIVE_GATE: &str = "HARW_RUN_FIREFOX_LIVE";
const BRIDGE_ID: &str = "live-page-bridge-contract";
const BRIDGE_VERSION: &str = "v1";
const BRIDGE_SOURCE: &str = r#"
const payload = { kind: "harwness-live-contract", value: 1 };
if (!__harwBridge.emit(payload)) {
    throw new Error("Harwness page bridge rejected its bounded live-contract payload");
}
"#;

#[tokio::test]
#[ignore = "requires a local Firefox/geckodriver and HARW_RUN_FIREFOX_LIVE=1"]
async fn firefox_bidi_installs_a_bounded_page_bridge_with_a_typed_receipt() -> Result<(), String> {
    if std::env::var_os(LIVE_GATE).as_deref() != Some(OsStr::new("1")) {
        eprintln!("skipping live Firefox page-bridge contract; set {LIVE_GATE}=1 to opt in");
        return Ok(());
    }

    let start_url = url::Url::parse("https://example.com/")
        .map_err(|error| format!("live test start URL must be valid: {error}"))?;
    let request = OpenBrowserRequest {
        start_url,
        headless: true,
        profile: ProfilePolicy::Ephemeral,
        bidi: BiDiRequirement::Required,
        allowed_origins: OriginPolicy::new(vec!["example.com".to_owned()], true),
        viewport: None,
    };
    let policy = PageBridgePolicy::new(1_024, 2, Duration::from_secs(5))
        .map_err(|error| format!("live bridge policy must be valid: {error}"))?;

    let host = FirefoxHost::new(FirefoxHostConfig::new())
        .map_err(|error| format!("Firefox host construction failed: {error}"))?;
    let session = host
        .open(request)
        .await
        .map_err(|error| format!("live Firefox/BiDi open failed: {error}"))?;
    let session_id = session.id();
    let context_id = session.primary_context_id();
    let bridge =
        PageBridgeInstallRequest::new(context_id, BRIDGE_ID, BRIDGE_VERSION, BRIDGE_SOURCE, policy)
            .map_err(|error| format!("live bridge request must be valid: {error}"))?;

    let installation = session.install_page_bridge(bridge).await;
    let close = host.close(&session_id).await;
    let receipt =
        installation.map_err(|error| format!("live page-bridge install failed: {error}"))?;
    close.map_err(|error| format!("live Firefox session close failed: {error}"))?;

    assert!(!receipt.installation_id().to_string().is_empty());
    assert_eq!(receipt.context_id(), context_id);
    assert_eq!(receipt.bridge_id(), BRIDGE_ID);
    assert_eq!(receipt.version(), BRIDGE_VERSION);
    assert_eq!(
        receipt.script_sha256(),
        sha256_hex(BRIDGE_SOURCE.as_bytes())
    );
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
