// Diagnostic: `#[traced(fields(missing))]` — `missing` ist kein Parameter der
// annotierten Funktion.
// Expected: compile_error! "`missing` ist kein Argument dieser Funktion; ..."

#[harw_macros::traced(fields(missing))]
fn annotated(present: i32) -> i32 {
    present
}

fn main() {}
