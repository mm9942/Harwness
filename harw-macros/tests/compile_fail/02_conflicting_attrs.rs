// Diagnostic 2: a single field carries both #[raw(first)] and #[raw(join)].
// Expected: compile_error! "conflicting #[raw(...)] attributes on field 'text': first, join"
use harw_macros::FromRawArgs;

#[derive(FromRawArgs)]
struct Conflicting {
    #[raw(first)]
    #[raw(join)]
    text: Option<String>,
}

fn main() {}
