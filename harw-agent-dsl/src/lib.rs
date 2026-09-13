//! Harwness Agent Definition DSL — Kern-Datentypen, Parser, Loader und Resolver.
//!
//! Dieses Crate implementiert den Vertical Slice der Agent-Definition-DSL gemäß
//! der normativen Spezifikation `agent-definition-dsl.md`.
//!
//! # Scope
//! Enthält: Kern-Datentypen, Parser, Loader, Layered-Resolver, Authority-Validator.
//! Nicht enthalten (Folge-Waves): Specialization-Registry, Family/Organization/Clan/Cell-Compiler,
//! vollständige Diagnostics-Codes, Frozen-Snapshot-Migration.
//!
//! # Modulübersicht
//! - [`error`] — [`DslError`](error::DslError) und [`DslResult`](error::DslResult)
//! - [`ids`] — [`DefinitionId`](ids::DefinitionId), [`Version`](ids::Version), [`DefinitionRef`](ids::DefinitionRef)
//! - [`roles`] — [`AgentRoleId`](roles::AgentRoleId) (geschlossenes Enum), [`can_spawn`](roles::can_spawn)
//! - [`raw`] — [`RawAgentDefinition`](raw::RawAgentDefinition) (TOML-nahe Struktur)
//! - [`parse`] — [`parse_toml`](parse::parse_toml)
//! - [`layers`] — [`DefinitionLayer`](layers::DefinitionLayer)
//! - [`merge`] — [`MergeOp`](merge::MergeOp), [`apply_merge_op`](merge::apply_merge_op)
//! - [`authority`] — [`AuthorityCeiling`](authority::AuthorityCeiling)
//! - [`resolved`] — [`ResolvedAgentDefinition`](resolved::ResolvedAgentDefinition), [`ResolutionTrace`](resolved::ResolutionTrace)
//! - [`resolve`] — [`resolve_definition`](resolve::resolve_definition)
//! - [`context_program`] — [`RawContextProgramDefinition`](context_program::RawContextProgramDefinition),
//!   [`resolve_context_program`](context_program::resolve_context_program), Deckenprüfung über
//!   [`ContextCeilingAdmission`](context_program::ContextCeilingAdmission) (Knoten AW2-01)
//!
//! # Nebenläufigkeit
//! Alle öffentlichen Typen sind `Send + Sync`.
//!
//! # Beispiele
//! ```rust,no_run
//! use harw_agent_dsl::parse::parse_toml;
//! use harw_agent_dsl::resolve::resolve_definition;
//! use harw_agent_dsl::ids::DefinitionId;
//! use harw_agent_dsl::layers::DefinitionLayer;
//!
//! let src = r#"
//! schema = "harwness.agent/v1"
//! id = "harwness.agent.focused-pure-coding@1"
//! version = "1.0.0"
//! role = "worker"
//! specialization = "focused-pure-coding"
//! "#;
//! let raw = parse_toml(src).unwrap();
//! let id = DefinitionId::parse("harwness.agent.focused-pure-coding@1").unwrap();
//! let layers = vec![(DefinitionLayer::BuiltIn, raw)];
//! let resolved = resolve_definition(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
//! assert_eq!(resolved.specialization, "focused-pure-coding");
//! ```

#![forbid(unsafe_code)]

pub mod authority;
pub mod context_program;
pub mod error;
pub mod executable;
pub mod family;
pub mod ids;
pub mod layers;
pub mod merge;
pub mod organization;
pub mod parse;
pub mod raw;
pub mod resolve;
pub mod resolved;
pub mod roles;

pub use executable::{ExecutableAgentIr, lower};
