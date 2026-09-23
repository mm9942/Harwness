//! `harw-tools` — Tool-Vokabular-Boundary des Harness.
//!
//! Definiert Schema, Spezifikation, Invokation, Ausgabe und den
//! Ausführungs-Trait für Tools. Reine Daten plus eine schlanke
//! `ToolExecutor`-Boundary; keine konkreten Tool-Implementierungen.
//!
//! # Makro-Oberfläche
//!
//! Dieses Crate ist zugleich die Pfad-Wurzel für den von Makros erzeugten
//! Code. `#[harw_macros::tool]` und [`macro@tool_provider`] emittieren
//! ausschließlich `::harw_tools::…`-Pfade; die Re-Exports unten (insbesondere
//! [`Permission`], [`require_permission`], [`require_host_access`] und
//! [`host_from_url`]) sind deshalb Teil des öffentlichen Kontrakts und dürfen
//! nicht entfernt werden, ohne die generierten Prologe zu brechen. Ein Crate,
//! das diese Makros verwendet, braucht dadurch keine direkte
//! `harw-sandbox`-Dependency.

#![forbid(unsafe_code)]

/// Re-exported for generated tool schema code that constructs JSON values.
pub use serde_json;

pub mod call;
pub mod context_load;
pub mod error;
pub mod executor;
pub mod output;
pub mod provider_macro;
pub mod sandbox_guard;
pub mod schema;
pub mod spec;

pub use call::ToolCall;
pub use context_load::{
    ContextLoadExecutor, ContextLoadOutput, InMemoryReferenceStore, ReferenceStore,
    CONTEXT_LOAD_TOOL_NAME, DEFAULT_MAX_LOADS_PER_TURN,
};
pub use error::{ToolsError, ToolsResult};
pub use executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture, TracedToolExecutor};
pub use output::ToolOutput;
pub use sandbox_guard::{host_from_url, require_host_access, require_permission};
pub use schema::{AdditionalProperties, JsonSchema, JsonSchemaType};
pub use spec::{FunctionToolSpec, ToolName, ToolSpec};

/// Re-export der Sandbox-Berechtigungen.
///
/// Von `#[harw_macros::tool]` als `::harw_tools::Permission` referenziert, damit
/// der generierte Sicherheits-Prolog keine direkte `harw-authority`-Dependency
/// im aufrufenden Crate voraussetzt.
pub use harw_authority::Permission;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
