mod common;

use common::{TestError, TestResult};
use harw_browser::host::BrowserHost;
use harw_browser_thirtyfour::{FIREFOX_BIDI_BINDING_ID, FirefoxHost, FirefoxHostConfig};
use std::path::{Path, PathBuf};

fn assert_browser_host<T: BrowserHost + Send + Sync>() {}

#[test]
fn firefox_host_satisfies_the_browser_host_concurrency_contract() {
    assert_browser_host::<FirefoxHost>();
}

#[test]
fn browser_host_implementation_preserves_constructor_and_config_access() -> TestResult {
    let config =
        FirefoxHostConfig::new().with_firefox_binary(PathBuf::from("/opt/firefox/firefox"));
    let host = match FirefoxHost::new(config) {
        Ok(host) => host,
        Err(error) => {
            return Err(TestError::Unexpected(format!(
                "deterministic host construction failed: {error}"
            )));
        }
    };

    assert_eq!(
        host.config().firefox_binary(),
        Some(Path::new("/opt/firefox/firefox"))
    );
    assert!(host.config().geckodriver_pin().is_none());
    assert_eq!(
        FirefoxHost::binding_metadata().binding_id,
        FIREFOX_BIDI_BINDING_ID
    );
    Ok(())
}
