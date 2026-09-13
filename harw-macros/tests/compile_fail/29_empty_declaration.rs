// Diagnostic: leere Deklaration.
// Expected: compile_error! "darf keine leere Deklaration sein"
use harw_macros::warden_actions;

warden_actions! {}

fn main() {}
