//! Semantic activity vocabulary and deterministic pattern reducer (PL-71).
//!
//! This crate is the shared, presentation-neutral layer between event
//! producers (tool calls, job runtime, agent runtime) and consumers (TUI
//! cells, status windows, later Web/Telegram views):
//!
//! ```text
//! raw subsystem event --adapter--> ActivityEvent --Reducer::apply--> ProjectionUpdate
//! ```
//!
//! - [`ActivityDescriptor`] / [`Effect`]: static meaning of a producer
//!   (`filesystem` / `read` / `path` / observe). `const`-constructible so
//!   `#[tool(activity(...))]` can emit it as an associated constant.
//! - [`ActivityEvent`]: one normalized observation, totally ordered by
//!   [`ActivityEvent::ingest_seq`], with an [`ActivityOwner`], an optional
//!   canonical [`ActivityKey`], a [`Lifecycle`] step and an [`Outcome`].
//! - [`Reducer`]: folds the stream into [`PatternInstance`]s of the four v1
//!   patterns ([`PatternId::V1`]: `file-read-burst@1`, `file-edit-read-loop@1`,
//!   `job-lifecycle@1`, `agent-role-failure-burst@1`) or standalone
//!   instances, and reports each change as a [`ProjectionUpdate`].
//!
//! # Guarantees
//! - **Deterministic**: order is `ingest_seq` only, time only enters as
//!   events ([`Boundary::Timeout`]), all state is kept in ordered maps;
//!   projection ids and revisions are `ingest_seq` values. Replaying a
//!   recorded log ([`parse_jsonl`] + [`Reducer::replay`]) yields the same
//!   instances as live reduction.
//! - **Stable identity**: a [`ProjectionId`] never changes; the rebase
//!   `Call(C)` -> `Job(W)` adds an alias to the same instance. Conflicting
//!   rebases (one key for two instances, two work ids for one job) are
//!   refused visibly, never merged.
//! - **Failures stay visible**: counters are recomputed from the
//!   constituents, so `state.counts.failed` always equals the number of
//!   failed constituents, mutating ones included, and a failure is never
//!   overwritten by a later success report. A failure reported on any
//!   lifecycle step counts (fail closed).
//! - **Unknown semantics stay standalone** (PL-71 I5), and every concrete
//!   activity remains inspectable as a [`Constituent`] (I2/I6).
//! - **No authority**: nothing here grants, implies or checks a permission
//!   (I4). The crate has no I/O and depends only on `serde`/`serde_json`.
//!
//! # Adapter contract
//! The reducer trusts the order and the identities it is given. Adapters
//! (the TUI glue of PL-97 M3, later Web/Telegram feeds) must uphold these
//! rules; where one is broken the reducer keeps the affected activity
//! visible but apart, flagged with an [`Anomaly`], and never merges it.
//!
//! 1. **Pair through the call id.** `Started`, `Progress` and `Finished` of
//!    one tool call carry the same [`ActivityOwner::call`]. Runs reported by
//!    a runtime without a call id pair through their `Job(W)` or `Agent(id)`
//!    resource. A `Started` or `Progress` with neither can never finish: it
//!    becomes an [`Anomaly::Unpaired`] standalone instance instead of
//!    holding a pattern open forever (it would stay running even after
//!    [`Boundary::SessionEnd`]).
//! 2. **Bind before use.** `job.start` is keyed `Call(C)` until its result
//!    names the work id; the rebase `Call(C)` -> `Job(W)` must precede, in
//!    `ingest_seq` order, every event keyed `Job(W)`: `job.status`, `logs`,
//!    `wait` calls *and* the runtime's own run events. A `Job(W)` event that
//!    arrives first opens a second `job-lifecycle@1` instance, and the late
//!    rebase is refused as [`Anomaly::RebaseConflict`] (two job cells for
//!    one job). So when the runtime may report the run before the tool
//!    result is processed, the adapter must emit the rebase first. A
//!    provisional call binds exactly one work id; only `Call(_)` keys rebase
//!    onto `Job(_)`.
//! 3. **The runtime ends the job.** The binding adds a running `run`
//!    constituent ([`ActivityDescriptor::JOB_RUN`]). The instance turns
//!    terminal only when an event keyed `Job(W)` *without* a call id reports
//!    `Finished`, for example derived from the job runtime's terminal state
//!    or from the `job.wait` result. Seeing only `job.*` tool calls, a job
//!    stays open forever.
//! 4. **A rebase is a pure binding.** Its descriptor and call id never form
//!    a constituent; the activity behind it is reported as its own events:
//!    `job.start` keyed `Call(C)`, and an observed rename as a `Mutate` call
//!    (`fs.move`) keyed `Path(old)` in the same session turn *before* the
//!    rebase `Path(old)` -> `Path(new)`. The mutation seals the read burst of
//!    `old` and opens the edit/read loop that the rebase then rebinds to
//!    `new`. A rebase must not carry a failure: one that does is not applied
//!    ([`Anomaly::RebaseFailed`]); a cancelled rebase is not applied either.
//!    Report the failed or cancelled call instead.
//! 5. **One sequencer.** `ingest_seq` is unique and increasing per stream;
//!    re-delivered or older events are refused and counted
//!    ([`Reducer::stale_events`]), never reordered.
//!
//! # Example
//! ```
//! use harw_activity::{
//!     ActivityDescriptor, ActivityEvent, ActivityKey, ActivityOwner, Effect, Lifecycle,
//!     Outcome, PatternId, Reducer,
//! };
//!
//! const FS_READ: ActivityDescriptor =
//!     ActivityDescriptor::new("filesystem", "read", Some("path"), Effect::Observe);
//!
//! let read = |seq: u64, call: &str, lifecycle: Lifecycle| {
//!     ActivityEvent::new(
//!         seq,
//!         ActivityOwner::session("root").with_turn(1).with_call(call),
//!         FS_READ,
//!         lifecycle,
//!     )
//!     .with_resource(ActivityKey::Path("src/lib.rs".into()))
//! };
//!
//! let mut reducer = Reducer::new();
//! reducer.apply(&read(1, "c1", Lifecycle::Started));
//! reducer.apply(&read(2, "c2", Lifecycle::Started));
//! reducer.apply(&read(3, "c1", Lifecycle::Finished));
//! let updates =
//!     reducer.apply(&read(4, "c2", Lifecycle::Finished).with_outcome(Outcome::failed("io")));
//!
//! let burst = &updates[0].instance;
//! assert_eq!(burst.pattern, PatternId::FILE_READ_BURST);
//! assert_eq!(burst.state.counts.total, 2);
//! assert_eq!(burst.state.counts.failed, 1);
//! assert_eq!(burst.revision, 4);
//! ```
#![forbid(unsafe_code)]

mod descriptor;
mod event;
mod pattern;
mod reducer;

pub use descriptor::{AGENT_DOMAIN, ActivityDescriptor, Effect, JOB_DOMAIN};
pub use event::{
    ActivityEvent, ActivityKey, ActivityOwner, Boundary, JsonlError, Lifecycle, Outcome,
    parse_jsonl, to_jsonl,
};
pub use pattern::{
    Anomaly, Constituent, ConstituentId, ConstituentStatus, Counts, PatternId, PatternInstance,
    PatternState, Phase, ProjectionId, ProjectionUpdate, Scope, VerbCount,
};
pub use reducer::{ReduceError, Reducer};
