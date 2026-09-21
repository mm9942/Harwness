//! `harw-operations` — Kern-Contract für ausführbare Harness-Operationen.
//!
//! Diese Crate definiert die gemeinsame Abstraktion (`Operation`-Trait und zugehörige
//! Typen), über die alle konkreten Operationen im Harness implementiert und von
//! Adaptern (TUI-Command, Modell-Tool, Channel-Command) konsumiert werden.
//!
//! # Verantwortungsbereich
//! - Definiert [`operation::Operation`], den Kern-Trait aller Operationen.
//! - Definiert [`operation::OperationMeta`], [`operation::OpInput`],
//!   [`operation::OpOutput`] sowie Hilfstypen (Domäne, Berechtigung, Flächen).
//! - Definiert [`error::OpError`], den Fehler-Enum dieser Crate.
//! - Delegiert konkrete Implementierungen an eigenständige Crates
//!   (z. B. `harw-ops-session`, `harw-ops-agents`).
//! - Delegiert Adapter-Logik an `harw-tui`, `harw-channel` etc.
//!
//! # Schlüsseltypen
//! - [`Operation`] — Kern-Trait
//! - [`OperationMeta`] — statische Metadaten
//! - [`OperationDomain`] — thematische Gruppierung
//! - [`PermissionTier`] — Berechtigungsstufen (Observer → Operator → Maintainer → Owner)
//! - [`Surface`] — deklarierte Expositionsfläche (Command oder ModelTool)
//! - [`OpInput`] / [`OpOutput`] — fläche-neutrale Ein-/Ausgabe
//! - [`OpContext`] — unveränderlicher Ausführungs-Kontext (Session, Turn, Sandbox, Services)
//! - [`ServiceMap`] — getypter Service-Container
//! - [`OpError`] — Fehler dieser Crate
//!
//! # Makros
//! Das interne Modul `service_macro` definiert drei `#[macro_export]`-Makros
//! gegen wiederkehrendes Boilerplate in Operation-Implementierungen (am
//! Crate-Root nutzbar, kein `#[macro_use]` nötig):
//! - `require_service!(ctx, Ty, "Label")` — holt einen Service aus [`OpContext::service`]
//!   oder liefert früh `Err(`[`OpError::NotAvailable`]`(...))`.
//! - `operations![registry; OpA, OpB, ...]` — registriert mehrere Operationen via
//!   [`registry::OperationRegistry::register`] (infallible, panikt bei Kollision).
//! - `try_operations![registry; OpA, OpB, ...]` — wie `operations!`, aber via
//!   [`registry::OperationRegistry::try_register`] mit `?`-Propagation von
//!   [`registry::RegistryError`].
//!
//! # Abhängigkeiten
//! - `harw-types`: ID-Newtypes und Basisvokabular
//! - `harw-tools`: Tool-Vokabular (ToolSpec, ToolExecutor …)
//! - `harw-sandbox`: `SandboxSpec` (Authority-Boundary)
//! - `serde` / `serde_json`: Serialisierung von JSON-Argumenten in [`OpInput`]
//!
//! # Nebenläufigkeit
//! [`Operation`] ist `Send + Sync`. [`operation::OpFuture`] ist `Send`.
//! Die Crate enthält selbst keine Threads und kein Async-Runtime-Setup.
//!
//! # Fehlertypen
//! [`error::OpError`] — drei Varianten: `InvalidArguments`, `Execution`, `NotAvailable`.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_operations::{Operation, OperationMeta, OperationDomain, OperationCategory, PermissionTier, OpInput, OpOutput, OpContext};
//!
//! struct Noop;
//!
//! impl Operation for Noop {
//!     fn meta(&self) -> &OperationMeta {
//!         static META: std::sync::OnceLock<OperationMeta> = std::sync::OnceLock::new();
//!         META.get_or_init(|| OperationMeta {
//!             name: "noop",
//!             summary: "Tut nichts.",
//!             domain: OperationDomain::Misc,
//!             permission: PermissionTier::Observer,
//!             surfaces: vec![],
//!             aliases: &[],
//!             category: OperationCategory::Misc,
//!             args_schema: None,
//!             output_schema: None,
//!             busy: Default::default(),
//!         })
//!     }
//!
//!     fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> harw_operations::operation::OpFuture<'a> {
//!         Box::pin(async { Ok(OpOutput { text: String::new(), data: None }) })
//!     }
//! }
//! ```

#![forbid(unsafe_code)]

pub mod adapter;
pub mod args;
pub mod context;
pub mod error;
pub mod op_schema;
pub mod operation;
pub mod registry;
mod service_macro;
pub mod session_control;

// ── Re-Exporte der Kern-Typen ─────────────────────────────────────────────────

pub use args::{
    first_optional, join_all_optional, join_from, nth_optional, require_empty, require_first,
    split_subcommand,
};
pub use context::{OpContext, ServiceMap};
pub use error::OpError;
pub use op_schema::OpArgsSchema;
pub use operation::{
    ApprovalPolicy, CommandVisibility, FromRawArgs, OpFuture, OpInput, OpInvocation, OpOutput,
    Operation, OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface,
    WebMethod,
};
pub use session_control::{
    NullSessionController, SessionControlError, SessionControlSnapshot, SessionController,
    SharedSessionController,
};
