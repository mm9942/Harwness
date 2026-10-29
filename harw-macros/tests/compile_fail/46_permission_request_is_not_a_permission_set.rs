// Diagnostic: `permission_request!` liefert nur eine Anfrage, nie eine
// Gewährung — dort, wo ein `PermissionSet` erwartet wird, ist sie ein
// Typfehler.
// Expected: E0308 "expected `PermissionSet`, found `PermissionRequest`".
fn main() {
    let _granted: harw_authority::PermissionSet =
        harw_macros::permission_request!(read_workspace, write_workspace);
}
