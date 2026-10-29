// Diagnostic: `permission_request!` mit unbekanntem Namen.
// Expected: Fehler am Namen, der die erlaubten Namen auflistet.
fn main() {
    let _request = harw_macros::permission_request!(read_workspace, root_access);
}
