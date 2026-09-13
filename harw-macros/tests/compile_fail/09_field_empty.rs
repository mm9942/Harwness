// Diagnostic: field!("") — leerer Feldname.
// Expected: compile_error! "Feldname darf nicht leer sein"
fn main() {
    let _ = harw_macros::field!("");
}
