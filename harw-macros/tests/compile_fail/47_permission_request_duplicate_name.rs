// Diagnostic: dieselbe Berechtigung zweimal in `permission_request!`.
// Expected: Fehler "is listed twice".
fn main() {
    let _request = harw_macros::permission_request!(read_workspace, read_workspace);
}
