//! Gemeinsame Runtime-Montage (`RuntimeAssembly`) für alle `harw`-Einstiege.
//!
//! Gerüst aus Welle W0a. Verträge (`spec`, `error`) liefert Agent W0B-05,
//! die Montage die Wellen W2b/W2c gemäß `docs/remediation/CONTRACTS.md` §runtime.
#![forbid(unsafe_code)]

pub mod approval;
pub mod budget;
pub mod ceiling;
pub mod config;
pub mod error;
pub mod model;
pub mod sandbox;
pub mod services;
pub mod spec;
pub mod trace;

pub use error::{RuntimeError, RuntimeResult};
pub use spec::{
    AskResolution, CeilingPolicy, EntryKind, EntryProfile, OperationSurface, RightsSnapshot,
    RootBudget, RuntimeSpec, SpawnerPolicy,
};
