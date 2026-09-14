//! Hardened Firefox preferences applied to every adapter session (B-ADAPT).
//!
//! # Description
//! Produces the `moz:firefoxOptions.prefs` map. Firefox runs inside a network
//! namespace whose only egress is the `harw-netns-relay` listener on
//! `127.0.0.1:<relay port>`, which forwards to the harness SOCKS5 egress proxy.
//! The preferences therefore:
//!
//! - route **all** traffic, including `localhost`, through SOCKS5 with remote
//!   DNS (`socks5h` semantics) and forbid direct failover;
//! - disable DNS-over-HTTPS, prefetching, speculative connections and WebRTC;
//! - disable telemetry, studies, crash-report submission and health reports;
//! - disable application, extension and search-engine updates plus background
//!   service connections;
//! - neutralise downloads (target directory that does not exist inside the
//!   sandbox, no "open with", no automatic handling);
//! - keep `file://` documents isolated (unique origin, strict origin policy).
//!   `file://` navigation is additionally rejected by the origin policy after
//!   every action, and the sandbox does not expose host files.
//!
//! Preferences are a defence-in-depth layer; the network namespace and the
//! egress proxy remain the enforcement boundary.
//!
//! # Concurrency
//! Pure functions returning owned data; thread-safe.
//!
//! # Errors
//! None; [`hardened_preferences`] is infallible.
//!
//! # Examples
//! ```rust,no_run
//! use harw_browser_thirtyfour::firefox_prefs::{PrefValue, hardened_preferences};
//!
//! let prefs = hardened_preferences(1080);
//! assert!(prefs.contains(&("network.proxy.socks_port", PrefValue::Int(1080))));
//! ```

/// A single Firefox preference value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefValue {
    /// Boolean preference.
    Bool(bool),
    /// Integer preference.
    Int(i64),
    /// String preference.
    Str(&'static str),
}

/// Host the relay listens on inside the sandbox netns.
pub const RELAY_PROXY_HOST: &str = "127.0.0.1";

/// Directory Firefox is told to download into; it does not exist in the sandbox.
pub const DISABLED_DOWNLOAD_DIR: &str = "/nonexistent/harw-downloads-disabled";

/// Returns the complete hardened preference list for a relay listening on `proxy_port`.
///
/// # Arguments
/// - `proxy_port` (`u16`): TCP port of `harw-netns-relay` inside the sandbox.
///
/// # Returns
/// Ordered `(name, value)` pairs; names are unique.
pub fn hardened_preferences(proxy_port: u16) -> Vec<(&'static str, PrefValue)> {
    use PrefValue::{Bool, Int, Str};
    vec![
        // Proxy: manual SOCKS5 with remote DNS, nothing bypasses it.
        ("network.proxy.type", Int(1)),
        ("network.proxy.socks", Str(RELAY_PROXY_HOST)),
        ("network.proxy.socks_port", Int(i64::from(proxy_port))),
        ("network.proxy.socks_version", Int(5)),
        ("network.proxy.socks_remote_dns", Bool(true)),
        ("network.proxy.no_proxies_on", Str("")),
        ("network.proxy.allow_hijacking_localhost", Bool(true)),
        ("network.proxy.failover_direct", Bool(false)),
        ("network.proxy.share_proxy_settings", Bool(false)),
        ("network.http.proxy.respect-be-conservative", Bool(true)),
        ("network.trr.mode", Int(5)),
        ("network.dns.disablePrefetch", Bool(true)),
        ("network.prefetch-next", Bool(false)),
        ("network.predictor.enabled", Bool(false)),
        ("network.http.speculative-parallel-limit", Int(0)),
        ("media.peerconnection.enabled", Bool(false)),
        ("network.captive-portal-service.enabled", Bool(false)),
        ("network.connectivity-service.enabled", Bool(false)),
        // Telemetry and data collection.
        ("toolkit.telemetry.enabled", Bool(false)),
        ("toolkit.telemetry.unified", Bool(false)),
        ("toolkit.telemetry.archive.enabled", Bool(false)),
        ("toolkit.telemetry.server", Str("")),
        ("datareporting.healthreport.uploadEnabled", Bool(false)),
        ("datareporting.policy.dataSubmissionEnabled", Bool(false)),
        ("app.shield.optoutstudies.enabled", Bool(false)),
        ("app.normandy.enabled", Bool(false)),
        ("browser.ping-centre.telemetry", Bool(false)),
        ("browser.newtabpage.activity-stream.feeds.telemetry", Bool(false)),
        ("browser.crashReports.unsubmittedCheck.autoSubmit2", Bool(false)),
        ("breakpad.reportURL", Str("")),
        // Updates and background services.
        ("app.update.auto", Bool(false)),
        ("app.update.enabled", Bool(false)),
        ("app.update.disabledForTesting", Bool(true)),
        ("extensions.update.enabled", Bool(false)),
        ("extensions.update.autoUpdateDefault", Bool(false)),
        ("extensions.getAddons.cache.enabled", Bool(false)),
        ("browser.search.update", Bool(false)),
        ("browser.safebrowsing.malware.enabled", Bool(false)),
        ("browser.safebrowsing.phishing.enabled", Bool(false)),
        ("browser.safebrowsing.downloads.enabled", Bool(false)),
        ("browser.safebrowsing.downloads.remote.enabled", Bool(false)),
        // Downloads.
        ("browser.download.folderList", Int(2)),
        ("browser.download.dir", Str(DISABLED_DOWNLOAD_DIR)),
        ("browser.download.useDownloadDir", Bool(true)),
        ("browser.download.forbid_open_with", Bool(true)),
        ("browser.download.always_ask_before_handling_new_types", Bool(false)),
        ("browser.download.manager.showWhenStarting", Bool(false)),
        ("browser.helperApps.neverAsk.saveToDisk", Str("")),
        ("browser.helperApps.neverAsk.openFile", Str("")),
        ("pdfjs.disabled", Bool(false)),
        // file:// isolation.
        ("security.fileuri.strict_origin_policy", Bool(true)),
        ("privacy.file_unique_origin", Bool(true)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn value_of(prefs: &[(&'static str, PrefValue)], name: &str) -> Option<PrefValue> {
        prefs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.clone())
    }

    #[test]
    fn test_hardened_preferences_routes_everything_through_socks5h_relay() {
        let prefs = hardened_preferences(31_080);
        assert_eq!(value_of(&prefs, "network.proxy.type"), Some(PrefValue::Int(1)));
        assert_eq!(value_of(&prefs, "network.proxy.socks"), Some(PrefValue::Str("127.0.0.1")));
        assert_eq!(value_of(&prefs, "network.proxy.socks_port"), Some(PrefValue::Int(31_080)));
        assert_eq!(value_of(&prefs, "network.proxy.socks_version"), Some(PrefValue::Int(5)));
        assert_eq!(value_of(&prefs, "network.proxy.socks_remote_dns"), Some(PrefValue::Bool(true)));
        assert_eq!(value_of(&prefs, "network.proxy.no_proxies_on"), Some(PrefValue::Str("")));
        assert_eq!(
            value_of(&prefs, "network.proxy.allow_hijacking_localhost"),
            Some(PrefValue::Bool(true))
        );
        assert_eq!(value_of(&prefs, "network.proxy.failover_direct"), Some(PrefValue::Bool(false)));
        assert_eq!(value_of(&prefs, "network.trr.mode"), Some(PrefValue::Int(5)));
        assert_eq!(value_of(&prefs, "media.peerconnection.enabled"), Some(PrefValue::Bool(false)));
    }

    #[test]
    fn test_hardened_preferences_disables_telemetry_updates_downloads_and_file_access() {
        let prefs = hardened_preferences(1080);
        for name in [
            "toolkit.telemetry.enabled",
            "datareporting.healthreport.uploadEnabled",
            "datareporting.policy.dataSubmissionEnabled",
            "app.normandy.enabled",
            "app.update.auto",
            "app.update.enabled",
            "extensions.update.enabled",
            "browser.search.update",
        ] {
            assert_eq!(value_of(&prefs, name), Some(PrefValue::Bool(false)), "{name}");
        }
        assert_eq!(value_of(&prefs, "app.update.disabledForTesting"), Some(PrefValue::Bool(true)));
        assert_eq!(
            value_of(&prefs, "browser.download.dir"),
            Some(PrefValue::Str(DISABLED_DOWNLOAD_DIR))
        );
        assert_eq!(value_of(&prefs, "browser.download.folderList"), Some(PrefValue::Int(2)));
        assert_eq!(
            value_of(&prefs, "browser.download.forbid_open_with"),
            Some(PrefValue::Bool(true))
        );
        assert_eq!(
            value_of(&prefs, "security.fileuri.strict_origin_policy"),
            Some(PrefValue::Bool(true))
        );
        assert_eq!(value_of(&prefs, "privacy.file_unique_origin"), Some(PrefValue::Bool(true)));
    }

    #[test]
    fn test_hardened_preferences_names_are_unique() {
        let prefs = hardened_preferences(1080);
        let names: HashSet<&str> = prefs.iter().map(|(name, _)| *name).collect();
        assert_eq!(names.len(), prefs.len());
    }
}
