mod common;

use common::{TestError, TestResult, ctx};
use harw_dod_config::{Config, ConfigError, ObservationScope, SYSTEM_CONFIG_PATH, Sensor};

const CGROUP_PROFILE: &str = r#"
schema_version = 1
mode = "observe"
active_profile = "selected-services"

[profiles.selected-services]
scope = "cgroups"
cgroup_paths = ["/system.slice/example-a.service", "/system.slice/example-b.service"]
include_descendants = true
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = ["192.0.2.0/24", "2001:db8::/32"]
"#;

#[test]
fn resolves_an_explicit_named_multi_cgroup_profile() -> TestResult {
    let profile = Config::from_toml(CGROUP_PROFILE)
        .map_err(ctx("valid config"))?
        .resolve_active()
        .map_err(ctx("selected profile resolves"))?;

    assert_eq!(profile.profile_id().as_str(), "selected-services");
    assert_eq!(profile.sensors(), &[Sensor::Exec, Sensor::TcpConnect]);
    assert_eq!(profile.egress_allow_cidrs().len(), 2);
    assert!(
        matches!(profile.scope(), ObservationScope::Cgroups(scope) if scope.include_descendants())
    );
    Ok(())
}

#[test]
fn canonicalizes_set_valued_profile_fields_before_resolution() -> TestResult {
    let config = Config::from_toml(
        r#"
schema_version = 1
mode = "observe"
active_profile = "selected"

[profiles.selected]
scope = "cgroups"
cgroup_paths = ["/z", "/a"]
include_descendants = true
sensors = ["tcp-connect", "exec"]
egress_allow_cidrs = ["2001:db8::/32", "192.0.2.0/24"]
"#,
    )
    .map_err(ctx("valid config"))?;
    let profile = config
        .resolve_active()
        .map_err(ctx("selected profile resolves"))?;

    assert_eq!(profile.sensors(), &[Sensor::Exec, Sensor::TcpConnect]);
    assert_eq!(profile.egress_allow_cidrs()[0].to_string(), "192.0.2.0/24");
    let ObservationScope::Cgroups(scope) = profile.scope() else {
        return Err(TestError::Unexpected("expected cgroup scope".to_owned()));
    };
    assert_eq!(scope.paths()[0].as_path().to_str(), Some("/a"));
    assert_eq!(scope.paths()[1].as_path().to_str(), Some("/z"));
    Ok(())
}

#[test]
fn cgroup_v2_mount_root_requires_an_explicit_host_scope() {
    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "root"

[profiles.root]
scope = "cgroups"
cgroup_paths = ["/"]
include_descendants = true
sensors = ["exec"]
egress_allow_cidrs = []
"#,
        ),
        Err(ConfigError::CgroupMountRootRequiresHostScope { .. })
    ));
}

#[test]
fn no_active_profile_never_falls_back_to_host() -> TestResult {
    let config = Config::from_toml(
        r#"
schema_version = 1
mode = "observe"

[profiles.host]
scope = "host"
sensors = ["exec"]
egress_allow_cidrs = []
"#,
    )
    .map_err(ctx("config syntax is valid"))?;

    assert!(matches!(
        config.resolve_active(),
        Err(ConfigError::MissingActiveProfile)
    ));
    Ok(())
}

#[test]
fn rejects_an_unknown_active_profile() -> TestResult {
    let config = Config::from_toml(
        r#"
schema_version = 1
mode = "observe"
active_profile = "does-not-exist"

[profiles.host]
scope = "host"
sensors = ["exec"]
egress_allow_cidrs = []
"#,
    )
    .map_err(ctx("config syntax is valid"))?;

    assert!(matches!(
        config.resolve_active(),
        Err(ConfigError::UnknownActiveProfile { .. })
    ));
    Ok(())
}

#[test]
fn rejects_empty_cgroup_lists() {
    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "empty"

[profiles.empty]
scope = "cgroups"
cgroup_paths = []
include_descendants = false
sensors = ["exec"]
egress_allow_cidrs = []
"#,
        ),
        Err(ConfigError::EmptyCgroupPaths { .. })
    ));
}

#[test]
fn rejects_traversal_and_host_cgroup_field_mixing() {
    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "bad"

[profiles.bad]
scope = "cgroups"
cgroup_paths = ["/system.slice/../user.slice"]
include_descendants = false
sensors = ["exec"]
egress_allow_cidrs = []
"#,
        ),
        Err(ConfigError::InvalidCgroupPath { .. })
    ));

    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "host"

[profiles.host]
scope = "host"
cgroup_paths = ["/system.slice"]
sensors = ["exec"]
egress_allow_cidrs = []
"#,
        ),
        Err(ConfigError::HostProfileHasCgroups { .. })
    ));

    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "host"

[profiles.host]
scope = "host"
cgroup_paths = []
sensors = ["exec"]
egress_allow_cidrs = []
"#,
        ),
        Err(ConfigError::HostProfileHasCgroups { .. })
    ));
}

#[test]
fn rejects_unknown_sensors_invalid_cidrs_and_unknown_fields() {
    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "enforce"
active_profile = "host"

[profiles.host]
scope = "host"
sensors = ["exec"]
egress_allow_cidrs = []
"#,
        ),
        Err(ConfigError::Parse(_))
    ));

    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "bad"

[profiles.bad]
scope = "host"
sensors = ["exec", "filesystem"]
egress_allow_cidrs = []
"#,
        ),
        Err(ConfigError::Parse(_))
    ));

    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "bad"

[profiles.bad]
scope = "host"
sensors = ["exec"]
egress_allow_cidrs = ["not-a-cidr"]
"#,
        ),
        Err(ConfigError::Parse(_))
    ));

    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "host"
unknown = true

[profiles.host]
scope = "host"
sensors = ["exec"]
egress_allow_cidrs = []
"#,
        ),
        Err(ConfigError::Parse(_))
    ));
}

#[test]
fn rejects_equivalent_egress_cidrs_after_network_normalization() {
    assert!(matches!(
        Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "host"

[profiles.host]
scope = "host"
sensors = ["exec"]
egress_allow_cidrs = ["192.0.2.0/24", "192.0.2.1/24"]
"#,
        ),
        Err(ConfigError::DuplicateEgressAllowCidr { .. })
    ));
}

#[test]
fn system_configuration_has_one_fixed_production_path() {
    assert_eq!(SYSTEM_CONFIG_PATH, "/etc/harw-dod/config.toml");
}
