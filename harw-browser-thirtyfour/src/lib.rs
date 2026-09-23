//! Firefox/WebDriver BiDi adapter for the Harwness browser contracts.
//!
//! This crate owns Firefox and geckodriver process configuration, concrete
//! driver lifecycle state, and translation from backend failures into the
//! stable `harw-browser` vocabulary. Callers receive Harwness contracts and
//! session handles rather than raw thirtyfour capabilities, drivers, or BiDi
//! handles; driver authority therefore remains inside this adapter boundary.
//!
//! Security model (B-ADAPT, F-009): geckodriver is never downloaded or managed
//! automatically; it is pinned by path and SHA-256 ([`launcher::GeckodriverPin`])
//! and started through a [`launcher::BrowserLauncher`] inside a network-isolated
//! sandbox whose only egress is the `harw-netns-relay` SOCKS relay
//! ([`firefox_prefs`]). Every action, navigation, wait, find and observation is
//! followed by an origin check of all open windows; a breach aborts the session.
//! The event journal is bounded by entries and bytes ([`journal`]). File upload
//! and model-supplied script waits are not implemented.

mod element_actions;
mod wait_navigation;

mod bidi_pump;
pub mod capabilities;
pub mod config;
mod driver;
pub mod error;
pub mod firefox_prefs;
mod gesture_actions;
pub mod host;
pub mod journal;
pub mod launcher;
mod location_guard;
mod navigation;
mod observe;
mod preload_bridge;
mod profile_archive;
mod runtime;
mod runtime_impl;
mod submit_action;
pub mod telemetry;
mod wait_elements;
mod waits;

pub use capabilities::{
    BidiEventDomain, BidiSubscriptionPlan, FirefoxCapabilityFactory, FirefoxCapabilityPlan,
};
pub use config::FirefoxHostConfig;
pub use error::{AdapterError, DriverOperation, ProfileArchiveOperation};
pub use host::{
    FIREFOX_BIDI_BINDING_ID, FIREFOX_CAPABILITY_ID, FirefoxBindingMetadata, FirefoxHost,
};
pub use journal::{BackpressureDisposition, BidiEventClass, EventJournalPolicy};
pub use launcher::{
    BrowserLauncher, DriverPorts, DriverProcess, GeckodriverCommand, GeckodriverPin,
    PreparedLaunch, RelayEndpoint, VerifiedGeckodriver,
};
pub use telemetry::build_subscriber;
mod selector;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
