// Diagnostic: `#[traced(level = "verbose")]` — ungültiger Level.
// Expected: compile_error! "unbekannter level `verbose`; erwartet: trace, debug, info, warn, error"

#[harw_macros::traced(level = "verbose")]
fn annotated() {}

fn main() {}
