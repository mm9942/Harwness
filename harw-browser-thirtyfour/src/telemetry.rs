//! Opt-in tracing subscriber construction for embedding binaries.
//!
//! This module never installs a global subscriber. The harness that owns the
//! process decides whether and where to install the returned value.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;

const DEFAULT_FILTER: &str = "warn,harw_browser=info,harw_browser_thirtyfour=info";

/// Builds the adapter's conservative structured-text tracing subscriber.
///
/// A syntactically valid `RUST_LOG` value takes precedence. Missing or invalid
/// environment configuration falls back to warnings globally and informational
/// events only from the Harwness browser contract and Firefox adapter targets.
/// Event fields are emitted by call sites; this layer never derives or adds
/// request payloads, URLs, credentials, or other potentially secret values.
pub fn build_subscriber() -> impl tracing::Subscriber + Send + Sync {
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::new(DEFAULT_FILTER),
    };

    tracing_subscriber::registry().with(filter).with(
        tracing_subscriber::fmt::layer()
            .compact()
            .with_ansi(false)
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false),
    )
}
