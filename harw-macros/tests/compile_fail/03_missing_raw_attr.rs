// Diagnostic 3: a named field has no #[raw(...)] attribute at all.
// Expected: compile_error! "field 'value' has no #[raw(...)] attribute; ..."
use harw_macros::FromRawArgs;

#[derive(FromRawArgs)]
struct MissingAttr {
    #[raw(first)]
    action: Option<String>,
    // `value` has no #[raw(...)] — this must be rejected.
    value: Option<String>,
}

fn main() {}
