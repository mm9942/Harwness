// Diagnostic: `#[traced(bogus = "x")]` — unbekannter Attributschlüssel.
// Expected: compile_error! "unbekannter Schlüssel `bogus`; erwartet: level, fields"

#[harw_macros::traced(bogus = "x")]
fn annotated() {}

fn main() {}
