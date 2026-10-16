//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use crate::{CargoProfileError, SandboxError, TmuxProfileError};
use harw_authority::AuthorityError;

harw_test_support::define_test_error!(
    pub(crate),
    Sandbox(SandboxError) => "Sandbox-Fehler",
    Io(std::io::Error) => "I/O-Fehler",
    Authority(AuthorityError) => "Authority-Fehler",
    CargoProfile(CargoProfileError) => "Cargo-Profil-Fehler",
    TmuxProfile(TmuxProfileError) => "Tmux-Profil-Fehler",
);
