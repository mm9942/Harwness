use super::*;
use crate::test_support::{TestResult, ctx};

fn exact(paths: &[&str]) -> TestResult<FileWriteScope> {
    FileWriteScope::exact_files(paths.iter().copied()).map_err(ctx("exact scope"))
}

fn binding() -> WorkspaceBinding {
    WorkspaceBinding {
        tenant: TenantId::from_str("scope-tenant"),
        workspace: WorkspaceId::from_str("scope-workspace"),
        canonical_root: std::env::temp_dir(),
    }
}

#[test]
fn exact_scope_denies_invalid_paths_and_does_not_grant_prefixes() -> TestResult {
    for path in ["", ".", "..", "a/../b", "a/./b", "/tmp/a", "a\\b", "a\0b", "a\nb", "a//b", "a/"] {
        assert!(FileWriteScope::exact_files([path]).is_err(), "{path:?}");
    }
    let scope = exact(&["src/a.rs", "src/b.rs"])?;
    assert!(scope.allows_path(Path::new("src/a.rs")));
    for path in ["src", "src/a.rs/child", "src/other.rs", "src/../src/a.rs", "src//a.rs"] {
        assert!(!scope.allows_path(Path::new(path)), "{path}");
    }
    Ok(())
}

#[test]
fn write_scope_intersection_is_non_widening_and_commutative() -> TestResult {
    let scopes = [FileWriteScope::workspace(), exact(&[])?, exact(&["a"])? , exact(&["a", "b"])?];
    for left in &scopes {
        for right in &scopes {
            let result = left.intersection(right);
            assert!(result.is_subset_of(left));
            assert!(result.is_subset_of(right));
            assert_eq!(result, right.intersection(left));
        }
    }
    assert!(!FileWriteScope::workspace().is_subset_of(&exact(&["a"])?));
    Ok(())
}

#[test]
fn scope_restriction_removes_escape_permissions_and_cannot_be_dropped() -> TestResult {
    let parent = SandboxSpec::from_resolved(binding(), PermissionSet::from_policy(Permission::ALL));
    let scoped = parent.restrict(&PermissionRequest::from_permissions(Permission::ALL)
        .with_write_scope(exact(&["src/a.rs"])?));
    assert!(scoped.permissions().contains(Permission::WriteWorkspace));
    for denied in [Permission::ExecuteProcess, Permission::ManageContainers, Permission::ManagePlugins, Permission::NetworkAccess] {
        assert!(!scoped.permissions().contains(denied));
    }
    scoped.ensure_child_of(&parent)?;
    assert!(parent.ensure_child_of(&scoped).is_err());
    let child = scoped.restrict(&PermissionRequest::from_permissions(Permission::ALL));
    assert_eq!(child.write_scope(), scoped.write_scope());
    child.ensure_child_of(&scoped)?;
    Ok(())
}

#[test]
fn scoped_snapshot_roundtrip_and_reissue_preserve_exact_files() -> TestResult {
    let bootstrap = PolicyBootstrap {
        source: PolicySourceKind::System,
        permissions: PermissionSet::from_policy(Permission::ALL),
        network_scope: NetworkScope::empty(),
        policy_digest: "scope-test".to_owned(),
    };
    let issued = bootstrap.issue(binding(), &PermissionRequest::from_permissions(Permission::ALL)
        .with_write_scope(exact(&["src/a.rs"])?))?;
    assert!(!issued.permissions().contains(Permission::ExecuteProcess));
    let encoded = toml::to_string(&issued.snapshot()).map_err(ctx("snapshot encode"))?;
    let snapshot: AuthoritySnapshot = toml::from_str(&encoded).map_err(ctx("snapshot decode"))?;
    let reissued = bootstrap.reissue(binding(), &snapshot)?;
    assert_eq!(reissued.write_scope(), issued.write_scope());
    assert!(reissued.is_subset_of(&issued));
    assert!(!reissued.write_scope().allows_path(Path::new("src/b.rs")));
    Ok(())
}

#[test]
fn scope_wire_rejects_invalid_exact_paths_and_unknown_fields() -> TestResult {
    for text in [
        "kind = 'exact_files'\npaths = ['../escape']",
        "kind = 'exact_files'\npaths = ['src/./a']",
        "kind = 'workspace'\npaths = ['a']",
        "kind = 'exact_files'\npaths = ['a']\npermission = 'full'",
    ] {
        assert!(toml::from_str::<FileWriteScope>(text).is_err(), "{text}");
    }
    let request: PermissionRequest = toml::from_str("requested = ['read_workspace']")
        .map_err(ctx("legacy request"))?;
    assert!(request.write_scope().is_workspace());
    Ok(())
}

#[test]
fn scoped_request_wire_fails_closed_in_legacy_readers() -> TestResult {
    #[derive(Deserialize)]
    struct LegacyRequest {
        requested: BTreeSet<Permission>,
        #[serde(default)]
        network_scope: NetworkScope,
    }
    let scoped = PermissionRequest::from_permissions([Permission::WriteWorkspace])
        .with_write_scope(exact(&["src/a.rs"])?);
    let encoded = toml::to_string(&scoped).map_err(ctx("scoped request encode"))?;
    assert!(toml::from_str::<LegacyRequest>(&encoded).is_err());
    let restored: PermissionRequest = toml::from_str(&encoded).map_err(ctx("scoped request decode"))?;
    assert_eq!(restored, scoped);
    let legacy = PermissionRequest::from_permissions([Permission::ReadWorkspace]);
    let encoded = toml::to_string(&legacy).map_err(ctx("legacy encode"))?;
    let decoded: LegacyRequest = toml::from_str(&encoded).map_err(ctx("legacy decode"))?;
    assert!(decoded.requested.contains(&Permission::ReadWorkspace));
    assert!(decoded.network_scope.is_empty());
    assert_eq!(toml::from_str::<PermissionRequest>(&encoded).map_err(ctx("new legacy decode"))?, legacy);
    Ok(())
}

#[test]
fn scoped_request_rejects_version_drift_and_missing_scope() {
    for text in [
        "[requested]\nschema_version=3\npermissions=['write_workspace']\n[requested.write_scope]\nkind='exact_files'\npaths=['a']",
        "[requested]\nschema_version=2\npermissions=['write_workspace']",
        "[requested]\nschema_version=2\npermissions=['write_workspace']\n[requested.write_scope]\nkind='workspace'",
        "requested=['write_workspace']\n[write_scope]\nkind='exact_files'\npaths=['a']",
    ] {
        assert!(toml::from_str::<PermissionRequest>(text).is_err(), "{text}");
    }
}

#[test]
fn exact_scope_bounds_constructor_and_wire_collection() {
    assert!(FileWriteScope::exact_files((0..257).map(|index| format!("file-{index}"))).is_err());
    let paths = (0..257).map(|index| format!("'file-{index}'")).collect::<Vec<_>>().join(",");
    let wire = format!("kind='exact_files'\npaths=[{paths}]");
    assert!(toml::from_str::<FileWriteScope>(&wire).is_err());
    assert!(FileWriteScope::exact_files(["x".repeat(4097)]).is_err());
}
