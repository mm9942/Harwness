//! Firefox/WebDriver BiDi adapter for the Harwness browser contracts.
//!
//! This crate owns Firefox and geckodriver process configuration, concrete
//! driver lifecycle state, and translation from backend failures into the
//! stable `harw-browser` vocabulary. Callers receive Harwness contracts and
//! session handles rather than raw thirtyfour capabilities, drivers, or BiDi
//! handles; driver authority therefore remains inside this adapter boundary.

mod element_actions;
mod wait_navigation;

mod bidi_pump;
pub mod capabilities;
pub mod config;
mod driver;
pub mod error;
mod gesture_actions;
pub mod host;
pub mod journal;
mod navigation;
mod observe;
mod preload_bridge;
mod profile_archive;
mod runtime;
mod runtime_impl;
mod submit_action;
pub mod telemetry;
mod wait_elements;
mod wait_script;
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
pub use telemetry::build_subscriber;
mod selector;
