// Diagnostic: field!("Child.Admitted") — Großbuchstaben sind nicht erlaubt.
// Expected: compile_error! ".. enthaelt ungueltiges Zeichen 'C'; erlaubt sind nur [a-z0-9_.]"
fn main() {
    let _ = harw_macros::field!("Child.Admitted");
}
