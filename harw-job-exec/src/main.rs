//! `harw-job-exec --plan <path> [--report <path>]` — see the library docs.
//!
//! Exit status: the job's own status on success (the process image is
//! replaced), otherwise `125` (setup failed), `126` (sandbox refused) or
//! `127` (exec failed), with a typed message on stderr.

use std::process::ExitCode;

fn main() -> ExitCode {
    match harw_job_exec::run_trampoline(std::env::args_os().skip(1)) {
        Ok(never) => match never {},
        Err(error) => {
            eprintln!("harw-job-exec: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
