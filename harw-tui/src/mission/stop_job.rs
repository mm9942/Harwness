//! Stop a managed job from the Jobs window (PL-97 C4, PL-68 W20-E).
//!
//! `x` on the selected running job of the focused Jobs window opens a
//! confirmation ([`StopJobDialog`], a reused [`ChoiceDialog`]):
//! `Stop job "<name>" (<id>)? TERM, then the existing job stop policy.
//! [Enter] stop [Esc] cancel`.
//!
//! - `Enter` yields the literal line `/jobs stop <id>`
//!   ([`DispatchLine::jobs_stop`]). The caller hands it to the regular command
//!   path (`HarwEvent::Command`), so it gets the same admission, busy class
//!   and `OpContext` as a typed command. Nothing here, and nothing in the key
//!   handler, calls `JobManager::stop` or any other job API; a rejected
//!   admission is the end, there is no second path.
//! - `Esc` closes the dialog and dispatches nothing.
//! - After the next jobs refresh the TUI reports the state it then sees,
//!   neutrally (`app::jobs_glue`), never an optimistic "stopped".
//!
//! # Scope
//! Managed (`JobManager`) jobs only. Durable `harw-job-store` work items are
//! not in the Jobs window (PL-97 §8.3); they keep their own lease and claim
//! path (`JobMutationScope`), which this key never bypasses.

use crossterm::event::KeyEvent;
use harw_tool_job::{JobId, JobState};

use crate::choice_dialog::{ChoiceAction, ChoiceDialog};
use crate::command_exec::DispatchLine;
use crate::jobs_panel::JobRow;

/// Title of the confirmation.
const TITLE: &str = "Stop job";

/// The one option: `Enter` (or `1`) stops.
const STOP_OPTION: &str = "Stop job (TERM)";

/// Footer of the confirmation.
const FOOTER: &str = "Enter stop · Esc cancel";

/// Confirmation text for the job `name` (`id`).
#[must_use]
pub(crate) fn prompt(name: &str, id: &JobId) -> String {
    format!(
        "Stop job \"{name}\" ({id})? TERM, then the existing job stop policy. \
         [Enter] stop [Esc] cancel"
    )
}

/// The job a confirmation is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StopTarget {
    /// Job id.
    pub(crate) job: JobId,
    /// Display name (terminal-safe).
    pub(crate) name: String,
    /// State when the operator asked (the neutral report compares to it).
    pub(crate) state: JobState,
}

/// What a key in the confirmation means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StopDecision {
    /// The dialog stays open.
    Stay,
    /// `Esc`: close, dispatch nothing.
    Cancel,
    /// `Enter`: close and dispatch `line` through the regular command path.
    Confirm {
        /// `/jobs stop <id>`.
        line: DispatchLine,
        /// The job it stops.
        target: StopTarget,
    },
}

/// Confirmation of a job stop (field of `Overlay::StopJob`).
#[derive(Debug, Clone)]
pub(crate) struct StopJobDialog {
    /// The reused choice dialog (drawn like every other choice overlay).
    pub(crate) dialog: ChoiceDialog,
    /// The job it is about.
    target: StopTarget,
}

impl StopJobDialog {
    /// Confirmation for `row`; `None` unless the job is still running and
    /// its id is a valid job id (fail closed: no dialog, nothing to stop).
    #[must_use]
    pub(crate) fn for_row(row: &JobRow) -> Option<Self> {
        if !row.active {
            return None;
        }
        let job = JobId::parse(&row.id)?;
        let dialog = ChoiceDialog::new(
            TITLE,
            Some(prompt(&row.name, &job)),
            vec![STOP_OPTION.to_owned()],
        )
        .with_footer_hint(FOOTER);
        Some(Self {
            dialog,
            target: StopTarget {
                job,
                name: row.name.clone(),
                state: row.state,
            },
        })
    }

    /// Handles one key (see the module docs).
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> StopDecision {
        match self.dialog.handle_key(key) {
            ChoiceAction::Stay => StopDecision::Stay,
            ChoiceAction::Cancel => StopDecision::Cancel,
            ChoiceAction::Chosen(_) => StopDecision::Confirm {
                line: DispatchLine::jobs_stop(&self.target.job),
                target: self.target.clone(),
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::mission::projection::tests::job_in;
    use crate::style::Theme;
    use crate::test_support::{TestError, TestResult, ctx};
    use crossterm::event::{KeyCode, KeyModifiers};
    use harw_operations::adapter::CommandAdapter;
    use harw_operations::{
        BusyAvailability, CommandVisibility, OpContext, OpFuture, OpInput, OpInvocation, OpOutput,
        Operation, OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface,
    };

    /// A stand-in for the `jobs` operation that only records the argument
    /// tokens of every dispatch that reached it (same name, path, tier and
    /// busy class as `harw-ops`' `/jobs`).
    pub(crate) struct RecordingJobs {
        meta: OperationMeta,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl RecordingJobs {
        pub(crate) fn new() -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                meta: OperationMeta {
                    name: "jobs",
                    summary: "Test-only recording jobs command.",
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Operator,
                    surfaces: vec![Surface::Command {
                        path: "/jobs",
                        visibility: CommandVisibility::TuiOnly,
                    }],
                    aliases: &[],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    output_schema: None,
                    busy: BusyAvailability::Immediate,
                },
                calls: Mutex::new(Vec::new()),
            })
        }

        /// Argument tokens of every dispatch so far.
        pub(crate) fn calls(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .map(|calls| calls.clone())
                .unwrap_or_default()
        }

        /// The adapters of this operation (one `/jobs` command).
        pub(crate) fn adapters(self: &std::sync::Arc<Self>) -> Vec<CommandAdapter> {
            let op: std::sync::Arc<dyn Operation> = self.clone();
            CommandAdapter::from_operation(op)
        }
    }

    impl Operation for RecordingJobs {
        fn meta(&self) -> &OperationMeta {
            &self.meta
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
            if let OpInvocation::Command { args, .. } = input.invocation
                && let Ok(mut calls) = self.calls.lock()
            {
                calls.push(args);
            }
            Box::pin(async {
                Ok(OpOutput {
                    text: "recorded".to_owned(),
                    data: None,
                })
            })
        }
    }

    /// A sandbox over a fresh workspace below `root`.
    pub(crate) fn sandbox_at(root: &std::path::Path) -> TestResult<harw_authority::SandboxSpec> {
        use harw_authority::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
        use harw_types::{TenantId, WorkspaceId};
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("workspace"))?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-stop-job-test"),
                workspace: WorkspaceId::from_str("workspace"),
                root: std::path::PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-stop-job-test"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve"))?;
        Ok(harw_authority::SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        ))
    }

    fn running(id: &str) -> JobRow {
        JobRow {
            name: "ladybird build".to_owned(),
            ..job_in(id, JobState::Running)
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The dialog text is the specified one, drawn unwrapped at a wide size.
    #[test]
    fn stop_job_dialog_names_the_job_and_the_stop_policy() -> TestResult {
        let row = running("job-7");
        let stop = StopJobDialog::for_row(&row).ok_or(TestError::Missing("dialog"))?;
        assert_eq!(stop.target.job.as_str(), "job-7");
        assert_eq!(stop.target.state, JobState::Running);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 12))
            .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                let area = frame.area();
                stop.dialog.render(area, frame.buffer_mut(), Theme::Dark);
            })
            .map_err(ctx("draw"))?;
        let buffer = terminal.backend().buffer();
        let shown: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            shown.contains(
                "Stop job \"ladybird build\" (job-7)? TERM, then the existing job stop policy. \
                 [Enter] stop [Esc] cancel"
            ),
            "{shown}"
        );
        assert!(shown.contains("Stop job (TERM)"), "{shown}");
        Ok(())
    }

    /// Only a running job with a valid id gets a confirmation.
    #[test]
    fn stop_job_dialog_needs_a_running_job_with_a_valid_id() {
        assert!(StopJobDialog::for_row(&job_in("job-1", JobState::Succeeded)).is_none());
        assert!(StopJobDialog::for_row(&running("job 1; rm")).is_none());
        assert!(StopJobDialog::for_row(&running("job-1")).is_some());
    }

    /// Enter confirms with the literal line, Esc cancels, other keys keep
    /// the dialog open.
    #[test]
    fn stop_job_keys_confirm_cancel_or_stay() -> TestResult {
        let mut stop =
            StopJobDialog::for_row(&running("job-7")).ok_or(TestError::Missing("dialog"))?;
        assert_eq!(stop.handle_key(key(KeyCode::Char('x'))), StopDecision::Stay);
        assert_eq!(stop.handle_key(key(KeyCode::Esc)), StopDecision::Cancel);
        match stop.handle_key(key(KeyCode::Enter)) {
            StopDecision::Confirm { line, target } => {
                assert_eq!(line.as_str(), "/jobs stop job-7");
                assert_eq!(target.name, "ladybird build");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Confirm, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// The confirmed line reaches the `jobs` command exactly once through
    /// `execute_command_as` (same admission as a typed line); a rejected
    /// admission (caller below the command's tier) reaches nothing and
    /// offers no second path.
    #[tokio::test]
    async fn stop_job_line_reaches_the_jobs_command_once_or_not_at_all() -> TestResult {
        let recorder = RecordingJobs::new();
        let adapters = recorder.adapters();
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let sandbox = sandbox_at(dir.path())?;
        let session = harw_types::SessionId::new();
        let line = DispatchLine::jobs_stop(&JobId::parse("job-7").ok_or(TestError::Missing("id"))?);

        let rejected = crate::command_exec::execute_command_as(
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Observer,
            line.as_str(),
            harw_operations::ServiceMap::new,
        )
        .await;
        assert!(recorder.calls().is_empty(), "rejected: {rejected}");
        assert_ne!(rejected, "recorded");

        let output = crate::command_exec::execute_command_as(
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Operator,
            line.as_str(),
            harw_operations::ServiceMap::new,
        )
        .await;
        assert_eq!(output, "recorded");
        assert_eq!(
            recorder.calls(),
            vec![vec!["stop".to_owned(), "job-7".to_owned()]]
        );
        Ok(())
    }
}
