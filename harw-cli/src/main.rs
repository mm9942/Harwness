//! The `harw` binary: everything lives in the `harw_cli` library
//! ([`harw_cli::main_entry`]), which a native, personalized harw build
//! (`harw agent build <uia> --native`) links as well.

#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    harw_cli::main_entry()
}
