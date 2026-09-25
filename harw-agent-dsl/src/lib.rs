//! Harwness Agent Definition DSL — Kern-Datentypen, Parser, Loader und Resolver.
//!
//! Dieses Crate implementiert den Vertical Slice der Agent-Definition-DSL gemäß
//! der normativen Spezifikation `docs/design/agent-definition-dsl.md`.
//!
//! # Scope
//! Enthält: Kern-Datentypen, Parser, Layered-Resolver, Authority-Validator,
//! Skill-Bindung (`skills = [...]`, [`skills`]), Lowering zur
//! [`ExecutableAgentIr`] samt Snapshot-Digest, Kontextprogramme
//! ([`context_program`]), Familien inkl. Mitgliedschaften ([`family`]) sowie
//! Organisationen mit Clan-/Cell-Deklaration und Strukturprüfung
//! ([`organization`]).
//!
//! Seit #22 Welle 1 zusätzlich: die typisierte, serialisierbare Agent-IR v2
//! ([`ir_v2::AgentIr`], Schema `harwness.agent-ir/v2`, Snapshot-Hash v7),
//! ihr Lowering ([`lower_v2::lower_v2`], [`lower_v2::compile_agent`]), stabile
//! Diagnose-Codes `HARW-<AREA>-NNN` mit Quellspannen ([`diagnostics`]) und die
//! Kontextprogramm-Bindung ([`bind`]).
//!
//! Nicht enthalten (Folge-Waves): Specialization-Registry (`specialization`
//! bleibt ein freies Label), Ausführung von Clans/Cells (hier nur Deklaration
//! und Validierung — das Laufzeitverhalten liegt außerhalb dieses Crates),
//! Migration eingefrorener Snapshots über
//! Hash-Domänen hinweg (ein Verweis aus einer älteren Domäne wird nur
//! abgelehnt, nicht übersetzt) sowie die Übersetzung von `reasoning_effort`
//! in Runtime-Typen (Aufgabe der Konsumenten).
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
//! - [`skills`] — Namensregeln und Vererbung der Skill-Liste einer Definition
//! - [`ir_v2`](mod@ir_v2) — [`AgentIr`](ir_v2::AgentIr) (IR v2, serde, Snapshot v7)
//! - [`lower_v2`](mod@lower_v2) — [`lower_v2`](lower_v2::lower_v2), [`compile_agent`](lower_v2::compile_agent), [`LowerSources`](lower_v2::LowerSources)
//! - [`diagnostics`](mod@diagnostics) — [`Diagnostic`](diagnostics::Diagnostic), Code-Katalog [`CATALOG`](diagnostics::CATALOG)
//! - [`bind`](mod@bind) — [`ContextProgramLibrary`](bind::ContextProgramLibrary), [`bind_context_program`](bind::bind_context_program)
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
pub mod bind;
pub mod context_program;
pub mod diagnostics;
pub mod error;
pub mod executable;
pub mod family;
pub mod ids;
pub mod ir_v2;
pub mod layers;
pub mod lower_v2;
pub mod merge;
pub mod organization;
pub mod parse;
pub mod raw;
pub mod resolve;
pub mod resolved;
pub mod roles;
pub mod skills;
#[cfg(test)]
mod test_support;

pub use diagnostics::{Diagnostic, Diagnostics};
pub use executable::{ExecutableAgentIr, lower};
pub use ir_v2::{AGENT_IR_SCHEMA, AGENT_IR_SNAPSHOT_DOMAIN, AgentIr};
pub use lower_v2::{LowerSources, compile_agent, lower_v2};
