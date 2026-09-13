// Diagnostic 6: an unknown key inside #[raw(...)].
// Expected: compile_error! "unknown or invalid raw attribute: `rest`; ..."
use harw_macros::FromRawArgs;

#[derive(FromRawArgs)]
struct UnknownKey {
    #[raw(rest)]
    tail: Option<String>,
}

fn main() {}
