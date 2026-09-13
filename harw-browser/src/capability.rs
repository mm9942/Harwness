//! # capability
//!
//! ## Responsibility
//! This module owns **backend feature probing**: the vocabulary for
//! describing what a concrete [`crate::host::BrowserRuntime`] implementation
//! actually supports at runtime ([`BrowserCapabilityProbe`]) and the
//! per-feature support level for each probed feature ([`CapabilityStatus`]).
//! It does not own the probing logic itself (that lives in the backend crate
//! that implements [`crate::host::BrowserRuntime::capability_probe`]) or any
//! policy decision about how a caller should react to a given capability
//! level.
//!
//! ## Key types exported
//! - [`CapabilityStatus`] — whether a feature is natively supported, only
//!   available via a fallback strategy, or unavailable.
//! - [`BrowserCapabilityProbe`] — the full snapshot of browser/driver
//!   identity and per-feature support levels for one session.
//!
//! ## Concurrency
//! Single-threaded, pure data types; `Send + Sync` follows automatically from
//! the fields. No locking or shared state.
//!
//! ## Errors
//! This module defines no fallible operations itself; probing failures are
//! reported by the backend via [`crate::error::Error`] from
//! [`crate::host::BrowserRuntime::capability_probe`].
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::capability::{BrowserCapabilityProbe, CapabilityStatus};
//!
//! let probe = BrowserCapabilityProbe::new(
//!     "chrome".to_owned(),
//!     Some("120.0".to_owned()),
//!     Some("120.0.1".to_owned()),
//!     true,
//!     CapabilityStatus::Native,
//!     CapabilityStatus::Native,
//!     CapabilityStatus::Native,
//!     CapabilityStatus::Fallback,
//!     CapabilityStatus::Native,
//! );
//! assert!(probe.bidi_connected);
//! ```

/// The support level a backend reports for one probed feature.
///
/// # Concurrency
/// `Copy`, plain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CapabilityStatus {
    Native,
    Fallback,
    Unavailable,
}

/// A full snapshot of a session's browser/driver identity and per-feature
/// capability levels.
///
/// # Description
/// Produced by [`crate::host::BrowserRuntime::capability_probe`] so callers
/// (and diagnostics) can reason about what is actually available on the live
/// session rather than assuming based on browser name alone — a browser may
/// report a version but still lack a BiDi connection, for example.
///
/// # Concurrency
/// Plain data, `Send + Sync`, no interior mutability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BrowserCapabilityProbe {
    pub browser_name: String,
    pub browser_version: Option<String>,
    pub driver_version: Option<String>,
    pub bidi_connected: bool,
    pub network_events: CapabilityStatus,
    pub console_events: CapabilityStatus,
    pub navigation_events: CapabilityStatus,
    pub script_messages: CapabilityStatus,
    pub driver_logs: CapabilityStatus,
}

impl BrowserCapabilityProbe {
    /// Builds a capability probe from explicitly determined field values.
    ///
    /// # Description
    /// Every field must be explicitly probed by the caller rather than
    /// defaulted (per the crate's capability-probing contract), so this
    /// constructor intentionally takes all nine fields instead of exposing a
    /// builder with implicit defaults — a backend cannot accidentally report
    /// `Unavailable` for a feature it never actually checked.
    ///
    /// # Arguments
    /// - `browser_name` (`String`): the browser's reported name, e.g.
    ///   `"chrome"`. Owned.
    /// - `browser_version` (`Option<String>`): the browser's reported
    ///   version, if determinable. Owned.
    /// - `driver_version` (`Option<String>`): the driver's reported version,
    ///   if determinable. Owned.
    /// - `bidi_connected` (`bool`): whether a live BiDi connection is
    ///   established for this session.
    /// - `network_events` (`CapabilityStatus`): support level for network
    ///   event delivery.
    /// - `console_events` (`CapabilityStatus`): support level for console
    ///   event delivery.
    /// - `navigation_events` (`CapabilityStatus`): support level for
    ///   navigation event delivery.
    /// - `script_messages` (`CapabilityStatus`): support level for
    ///   script-message delivery.
    /// - `driver_logs` (`CapabilityStatus`): support level for driver log
    ///   delivery.
    ///
    /// # Returns
    /// `BrowserCapabilityProbe` — with every field set to the given value.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::capability::{BrowserCapabilityProbe, CapabilityStatus};
    ///
    /// let probe = BrowserCapabilityProbe::new(
    ///     "firefox".to_owned(),
    ///     None,
    ///     None,
    ///     false,
    ///     CapabilityStatus::Unavailable,
    ///     CapabilityStatus::Unavailable,
    ///     CapabilityStatus::Native,
    ///     CapabilityStatus::Fallback,
    ///     CapabilityStatus::Native,
    /// );
    /// assert_eq!(probe.browser_name, "firefox");
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        browser_name: String,
        browser_version: Option<String>,
        driver_version: Option<String>,
        bidi_connected: bool,
        network_events: CapabilityStatus,
        console_events: CapabilityStatus,
        navigation_events: CapabilityStatus,
        script_messages: CapabilityStatus,
        driver_logs: CapabilityStatus,
    ) -> Self {
        Self {
            browser_name,
            browser_version,
            driver_version,
            bidi_connected,
            network_events,
            console_events,
            navigation_events,
            script_messages,
            driver_logs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_constructs_probe_with_matching_fields() {
        let probe = BrowserCapabilityProbe::new(
            "chrome".to_owned(),
            Some("120.0".to_owned()),
            Some("120.0.1".to_owned()),
            true,
            CapabilityStatus::Native,
            CapabilityStatus::Fallback,
            CapabilityStatus::Unavailable,
            CapabilityStatus::Native,
            CapabilityStatus::Fallback,
        );

        assert_eq!(probe.browser_name, "chrome");
        assert_eq!(probe.browser_version, Some("120.0".to_owned()));
        assert_eq!(probe.driver_version, Some("120.0.1".to_owned()));
        assert!(probe.bidi_connected);
        assert_eq!(probe.network_events, CapabilityStatus::Native);
        assert_eq!(probe.console_events, CapabilityStatus::Fallback);
        assert_eq!(probe.navigation_events, CapabilityStatus::Unavailable);
        assert_eq!(probe.script_messages, CapabilityStatus::Native);
        assert_eq!(probe.driver_logs, CapabilityStatus::Fallback);
    }

    #[test]
    fn test_browser_capability_probe_serde_json_round_trip() {
        let probe = BrowserCapabilityProbe::new(
            "firefox".to_owned(),
            None,
            None,
            false,
            CapabilityStatus::Unavailable,
            CapabilityStatus::Unavailable,
            CapabilityStatus::Native,
            CapabilityStatus::Fallback,
            CapabilityStatus::Native,
        );

        let json = serde_json::to_string(&probe).expect("probe serializes");
        let decoded: BrowserCapabilityProbe =
            serde_json::from_str(&json).expect("probe deserializes");
        assert_eq!(decoded, probe);
    }
}
