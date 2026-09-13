// Diagnostic 1: #[raw(first)] on a field whose type is `String`, not `Option<String>`.
// Expected: compile_error! "#[raw(first)] requires Option<String>"
use harw_macros::FromRawArgs;

#[derive(FromRawArgs)]
struct WrongType {
    #[raw(first)]
    name: String,
}

fn main() {}
