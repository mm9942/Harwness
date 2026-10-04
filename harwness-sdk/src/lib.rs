//! `harwness-sdk` — Harwness in eigene Programme einbetten.
//!
//! # Beschreibung
//! Die SDK ist die **semver-Grenze** vor den internen `harw-*`-Crates. Ihre
//! öffentliche Fläche besteht ausschließlich aus eigenen Typen (plus
//! `serde_json::Value` für JSON-Nutzdaten); keine Signatur nennt einen
//! internen Typ. Interne Crates dürfen sich ändern, ohne dass diese Fläche
//! bricht.
//!
//! | Baustein | Zweck |
//! |---|---|
//! | [`Harwness::builder`] → [`HarwnessBuilder`] | Home, Projekt, Modell/Provider, Modus, Werkzeuge, Kontext, Freigaben |
//! | [`Harwness::session`] / [`Harwness::resume`] → [`Session`] | eine Konversation |
//! | [`Session::send`] → [`TurnReport`] | einen Turn vollständig fahren |
//! | [`Session::events`] → [`EventStream`] von [`SdkEvent`] | Live-Ereignisse (Wurzel + Kind-Agenten) |
//! | [`Session::cancel_handle`] → [`CancelHandle`] | laufenden Turn abbrechen |
//! | [`Tool`], [`FnTool`], [`ContextSource`] | eigene Werkzeuge und Kontext |
//! | [`ApprovalHandler`], [`AutoDeny`], [`approval_fn`] | Freigaben |
//! | [`SdkError`] | ein Fehlertyp für alles |
//! | `RemoteHarwness` → `RemoteSession` (Feature `remote`) | dieselbe Sitzung in einem anderen Prozess, über die Control-Plane |
//!
//! # Voraussetzungen
//! - Eine Tokio-Runtime (`rt` oder `rt-multi-thread`, mit `time`).
//! - Ein eingerichteter Root-Space (`~/.harw` bzw. `HARW_HOME`) mit aktiver
//!   UIA und — außer mit [`HarwnessBuilder::offline_echo`] — einem
//!   konfigurierten Provider. `harw` richtet beides beim ersten Start ein.
//!
//! # Beispiel
//! ```rust,no_run
//! use harwness_sdk::prelude::*;
//!
//! # async fn demo() -> Result<(), SdkError> {
//! let harwness = Harwness::builder().cwd(".").build()?;
//! let mut session = harwness.session()?;
//! let report = session.send("Welche Tests gibt es hier?").await?;
//! println!("{:?}: {}", report.status, report.text.unwrap_or_default());
//! # Ok(()) }
//! ```
//!
//! # Stabilität
//! Öffentliche Enums und Berichtstypen sind `#[non_exhaustive]`. Das Feature
//! `unstable-internals` schaltet rohe Erweiterungspunkte frei
//! (`HarwnessBuilder::raw_*`), die interne Typen nennen und **keiner**
//! semver-Zusage unterliegen.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod adapter;
mod approval;
mod builder;
mod error;
mod event;
mod harwness;
mod ids;
mod session;
mod tool;

#[cfg(feature = "remote")]
mod remote;

use std::future::Future;
use std::pin::Pin;

/// Ein geboxtes, `Send`-fähiges Future — Rückgabetyp der objektsicheren
/// Traits ([`Tool`], [`ContextSource`], [`ApprovalHandler`]).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub use approval::{
    AUTO_DENY_REASON, ApprovalHandler, ApprovalPolicy, ApprovalRequest, AutoDeny, Decision,
    FnApproval, approval_fn,
};
pub use builder::{DEFAULT_PRINCIPAL_ID, HarwnessBuilder, Mode, ReasoningEffort};
pub use error::{Result, SdkError};
pub use event::{EventSource, EventStream, FinishStatus, SdkEvent, ToolOutput, Usage};
pub use harwness::Harwness;
pub use ids::SessionId;
pub use session::{
    CancelHandle, MAX_RESUMES_PER_TURN, Message, Role, Session, TurnReport, TurnStatus,
};
pub use tool::{ContextItem, ContextSource, FnTool, Tool, ToolContext, ToolError};

#[cfg(feature = "remote")]
pub use remote::{RemoteBuilder, RemoteHarwness, RemoteSession, RemoteSessionInfo};

/// `serde_json`, in der Fassung, die die SDK-Signaturen verwenden.
pub use serde_json;

/// Die üblichen Importe: `use harwness_sdk::prelude::*;`.
pub mod prelude {
    pub use crate::{
        ApprovalHandler, ApprovalPolicy, ApprovalRequest, AutoDeny, BoxFuture, ContextItem,
        ContextSource, Decision, EventStream, FnTool, Harwness, HarwnessBuilder, Mode, SdkError,
        SdkEvent, Session, SessionId, Tool, ToolContext, ToolError, TurnReport, TurnStatus,
        approval_fn,
    };
}
