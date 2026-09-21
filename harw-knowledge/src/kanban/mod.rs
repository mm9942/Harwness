//! Kanban projection of governed work.
//!
//! Spec: `docs/design/knowledge-surfaces.md` §6. [`board`] owns the typed
//! `Board`/`Lane`/`Card` model plus its durable persistence (§1.2, §6.1);
//! [`lifecycle`] owns the §6.3 state-transition functions and the §6.4
//! approval/structural gates. Neither submodule calls `harw-job-runtime`'s
//! claim/lease/retry machinery directly — [`board::Card::work_id`] carries
//! the `WorkId` that ties a card into that machinery, but driving the actual
//! governed-work lifecycle stays with the caller (§6.2).

pub mod board;
pub mod lifecycle;
