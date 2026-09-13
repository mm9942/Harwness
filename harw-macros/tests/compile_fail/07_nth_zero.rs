// Diagnostic 7: #[raw(nth = 0)] — use #[raw(first)] instead.
// Expected: compile_error! "#[raw(nth = 0)] is not allowed; use #[raw(first)] instead"
use harw_macros::FromRawArgs;

#[derive(FromRawArgs)]
struct NthZero {
    #[raw(nth = 0)]
    action: Option<String>,
}

fn main() {}
