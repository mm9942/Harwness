//! # harw-browser
//!
//! ## Responsibility
//! This crate is the **pure contract layer** for browser automation inside the
//! Harwness agent tool runtime. It defines the vocabulary that every browser
//! backend and every caller (tool layer, channel layer) must agree on: strongly
//! typed identifiers ([`ids`]), navigation/session security policy ([`policy`]),
//! element selection strategy ([`selector`]), the mutation surface
//! ([`action`]), the read surface ([`observation`]), blocking-until-condition
//! primitives ([`wait`]), the async event stream ([`event`]), captured evidence
//! ([`artifact`]), structured failure evidence ([`diagnostic`]), backend
//! feature probing ([`capability`]), the crate-wide error type ([`error`]),
//! and the two async traits a concrete backend must implement
//! ([`host::BrowserHost`], [`host::BrowserRuntime`]) plus the handle a caller
//! holds ([`session::BrowserSessionHandle`]).
//!
//! This crate owns **types and trait contracts only**. It does not own any
//! concrete WebDriver/BiDi client, network I/O, or process management — those
//! belong to downstream crates such as `harw-browser-thirtyfour`, which
//! implement [`host::BrowserHost`] and [`host::BrowserRuntime`] against this
//! contract. `harw-browser` has no dependency on `thirtyfour` or any other
//! browser automation library.
//!
//! ## Key types exported
//! - [`Error`] — the crate-wide error type returned by every fallible
//!   operation defined here.
//! - [`Result`] — the crate-wide `Result<T, Error>` alias.
//! - [`host::BrowserHost`] — opens, looks up, and closes browser sessions.
//! - [`host::BrowserRuntime`] — the per-session operation surface (observe,
//!   act, wait, events, capability probing, close) that a backend implements.
//! - [`session::BrowserSessionHandle`] — the cheap, cloneable, `Send + Sync`
//!   handle callers use to interact with a session without depending on the
//!   concrete backend type.
//!
//! ## Concurrency
//! This crate defines contracts, not implementations, so it performs no I/O
//! and spawns no threads itself. The traits it declares
//! ([`host::BrowserHost`], [`host::BrowserRuntime`]) require `Send + Sync`,
//! and [`session::BrowserSessionHandle`] wraps its runtime in an `Arc<dyn
//! BrowserRuntime>` so handles can be cloned and shared across threads at the
//! cost of one atomic refcount bump per clone — no locking is introduced by
//! this crate.
//!
//! ## Errors
//! See [`error::Error`] for the full set of variants a conforming backend
//! implementation is expected to produce.
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::error::Result;
//! use harw_browser::host::BrowserHost;
//! use harw_browser::policy::{
//!     BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy, ProfilePolicy,
//! };
//!
//! async fn open_example(host: &dyn BrowserHost) -> Result<()> {
//!     let request = OpenBrowserRequest {
//!         start_url: url::Url::parse("https://example.com")?,
//!         headless: true,
//!         profile: ProfilePolicy::Ephemeral,
//!         bidi: BiDiRequirement::Preferred,
//!         allowed_origins: OriginPolicy::from_origins(["https://example.com"], true)?,
//!         authentication_origins: OriginPolicy::default(),
//!         viewport: None,
//!         limits: BrowserLimits::default(),
//!     };
//!     let _session = host.open(request).await?;
//!     Ok(())
//! }
//! ```

/// Mutation surface: the set of actions a caller can request against a
/// browser context, plus the request/outcome envelopes around them.
pub mod action;
/// Captured evidence (screenshots, bytes, text, HTML) referenced by
/// observations and diagnostics.
pub mod artifact;
/// Backend feature probing: what a concrete [`host::BrowserRuntime`]
/// implementation actually supports at runtime.
pub mod capability;
/// Structured, severity-ranked failure/warning evidence surfaced alongside
/// actions and observations.
pub mod diagnostic;
/// The crate-wide [`error::Error`] type and [`error::Result`] alias.
pub mod error;
/// The async event stream: navigation, network, console, and driver events.
pub mod event;
/// The [`host::BrowserHost`] and [`host::BrowserRuntime`] traits a concrete
/// backend implements.
pub mod host;
/// Strongly typed, UUID-backed identifiers and monotonic counters used
/// throughout the crate.
pub mod ids;
/// The read surface: page/element observation requests and their results.
pub mod observation;
/// Bounded, versioned page-bridge installation contracts and trust metadata.
pub mod page_bridge;
/// Session-opening and navigation security policy (origin allowlists,
/// profile persistence, BiDi requirements, viewport).
pub mod policy;
/// Element selection strategy with primary + fallback selectors.
pub mod selector;
/// The [`session::BrowserSessionHandle`] callers use to interact with an open
/// session, plus session state/info types.
pub mod session;
/// Blocking-until-condition primitives and their outcomes.
pub mod wait;

pub use error::{Error, Result};
