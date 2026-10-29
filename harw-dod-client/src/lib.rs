//! Read-only client for the Harw-DoD suite (PL-97 D2).
//!
//! Harw-DoD ("Detect · Orient · Defend") runs as system services
//! (`harw-dod.target`: sentinel, probes, optional Warden). This crate is the
//! one place harw reads it from — the `harw dod` CLI, the `dod` operations
//! and model tools, the TUI and the `security_signals` context source all
//! share it, so they agree on paths, limits and classifications:
//!
//! - [`resolve_export_paths`] — where `findings.jsonl` / `health.json` live
//!   (`[dod] export_dir` → `/var/lib/harw-dod/export` if readable →
//!   `<home>/dod/export`).
//! - [`read_findings_tail`] — bounded tail of the findings export (256 KiB
//!   in total, continuing into the rotated `<path>.1`; 16 KiB lines;
//!   symlinks refused); accepts v1 lines and v2 lines with optional
//!   `schema_version` and `scope`, and reports whether older data was left
//!   unread ([`FindingsTail::truncated`]).
//! - [`summarize`] / [`FindingsTail::summarize`] — per-rule/per-severity
//!   counts inside a window ending at an injected `now`;
//!   [`FindingsSummary::complete`] is `false` when the counts are lower
//!   bounds.
//! - [`read_health`] — the sentinel health report.
//! - [`read_system_config`] — `/etc/harw-dod/config.toml` through
//!   `harw_dod_config`: valid (profile) / missing / invalid (reason) /
//!   untrusted / unreadable; symlinks never followed.
//! - [`unit_states`] — `systemctl show` for the six units; no `systemctl`
//!   ⇒ [`UnitStatus::NotAvailable`].
//! - [`status()`] — one [`DodStatus`] snapshot from a [`DodClientContext`].
//! - [`correlate()`] — jobs ↔ findings by cgroup path-component prefix (E4).
//! - [`ExecutionChainReport`] — the Installed/Configured/Running/Ready
//!   readiness of the job execution chain from plain host, cgroup-root and
//!   DoD facts (E5; pure, see [`chain`]).
//!
//! # Trust boundary
//! Everything read here is evidence produced from attacker-influenced
//! observations, never an instruction: reads are bounded, symlinks are
//! refused, free text is sanitized. The crate is strictly read-only: it
//! never writes DoD state, never talks to the Warden or any escalation
//! path, and runs exactly one external program (`systemctl show`, bounded
//! in time and output).
#![forbid(unsafe_code)]

pub mod chain;
pub mod config;
pub mod correlate;
mod error;
pub mod findings;
mod fsio;
pub mod health;
pub mod paths;
pub mod status;
pub mod units;

pub use chain::{
    ChainBinary, ChainDod, ChainHost, ChainInputs, ChainItem, ChainLevel, ChainRoot, ChainStage,
    ExecutionChainReport, ProfileCoverage, ServeWorkspace, ServiceState, profile_coverage,
    same_or_below,
};
pub use config::{ActiveProfile, ConfigReport, ProfileScope, read_system_config};
pub use correlate::{JobCgroup, JobFindings, JobScope, correlate, scope_within};
pub use error::DodClientError;
pub use findings::{
    DEFAULT_FINDINGS_WINDOW, FindingLine, FindingsSummary, FindingsTail, MAX_LINE_BYTES,
    MAX_TAIL_BYTES, RuleCount, SeverityCounts, parse_finding_line, read_findings_tail, summarize,
};
pub use health::{HealthReport, IpcCounters, SensorHealthEntry, SensorHealthState, read_health};
pub use paths::{
    ExportPaths, ExportSource, FINDINGS_FILE, HEALTH_FILE, SYSTEM_EXPORT_DIR, resolve_export_paths,
    resolve_export_paths_with,
};
pub use status::{
    DodClientContext, DodStatus, ExportState, InstallState, WardenState, install_state, status,
    warden_state,
};
pub use units::{DOD_UNITS, UnitState, UnitStatus, unit_states, unit_states_in};
