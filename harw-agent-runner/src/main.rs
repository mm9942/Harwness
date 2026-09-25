//! The `harw-agent-runner` binary: everything lives in the
//! `harw_agent_runner` library ([`harw_agent_runner::run_from_current_exe`]).
//!
//! Built plain (no artifact appended yet), this reads its own file, finds
//! no embedded artifact and exits with a clear error — it becomes a
//! compiled agent only once `harw_agent_artifact::append_to_executable`
//! (or `--capabilities`, from `harw-agent-compiler`) puts one at its end.

#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    harw_agent_runner::run_from_current_exe()
}
