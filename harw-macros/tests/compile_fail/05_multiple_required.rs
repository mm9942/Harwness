// Diagnostic 5: more than one field carries #[raw(required)].
// Expected: compile_error! "at most one field may be #[raw(required)]; found: first_name, last_name"
use harw_macros::FromRawArgs;

#[derive(FromRawArgs)]
struct DualRequired {
    #[raw(required)]
    first_name: Option<String>,
    #[raw(required)]
    last_name: Option<String>,
}

fn main() {}
