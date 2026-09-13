// Diagnostic 4: #[derive(FromRawArgs)] on a tuple struct.
// Expected: compile_error! "FromRawArgs requires a struct with named fields"
use harw_macros::FromRawArgs;

#[derive(FromRawArgs)]
struct TupleArgs(Option<String>);

fn main() {}
