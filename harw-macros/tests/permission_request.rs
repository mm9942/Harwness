//! PL-97 M2 / authority doc §6.3: `permission_request!` expands to a
//! `harw_authority::PermissionRequest` built through the public request API
//! and nothing else.

use harw_authority::{Permission, PermissionRequest};

#[test]
fn listed_names_become_a_request() {
    let request: PermissionRequest =
        harw_macros::permission_request!(read_workspace, write_workspace);
    assert_eq!(
        request,
        PermissionRequest::from_permissions([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace
        ])
    );
    assert!(request.contains(Permission::ReadWorkspace));
    assert!(!request.contains(Permission::ExecuteProcess));
    // Eine Anfrage trägt keinen Netz-Scope und damit keine Netz-Obergrenze.
    assert_eq!(
        request.network_scope(),
        PermissionRequest::empty().network_scope()
    );
}

/// Every spelling of the table resolves, in `Permission::ALL` order,
/// `manage_containers` included.
#[test]
fn every_permission_name_resolves() {
    let request = harw_macros::permission_request!(
        read_workspace,
        write_workspace,
        execute_process,
        network_access,
        read_secrets,
        manage_plugins,
        read_cargo_registry,
        manage_containers,
    );
    assert_eq!(request.iter().collect::<Vec<_>>(), Permission::ALL.to_vec());
}

#[test]
fn empty_list_is_the_empty_request() {
    let request = harw_macros::permission_request!();
    assert_eq!(request, PermissionRequest::empty());
    assert_eq!(request.iter().count(), 0);
}

/// The macro is an ordinary expression; the order of the list does not
/// matter for the resulting request.
#[test]
fn order_does_not_matter() {
    assert_eq!(
        harw_macros::permission_request!(network_access, read_workspace),
        harw_macros::permission_request!(read_workspace, network_access)
    );
}
