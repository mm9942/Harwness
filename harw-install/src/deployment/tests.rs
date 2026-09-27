//! Parity and consistency tests of the embedded deployment assets
//! (Crypto Masterplan v2 §40, H10).
//!
//! The file-system side is read at test time from
//! `CARGO_MANIFEST_DIR/..` (the repository root), so these tests compare the
//! compiled-in text with the tree that `dod/scripts/install.sh` installs from.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{
    AssetKind, DEPLOYMENT_ASSETS, DOD_PACKAGE_ASSETS, EmbeddedDeploymentAsset, PLACEHOLDERS,
    RenderPaths, asset, print_systemd, render, systemd_unit, systemd_units,
    unresolved_placeholders,
};
use crate::dod_units::{ParsedUnit, capability_set, parse_unit};
use crate::test_support::{TestError, TestResult, ctx};

/// Binaries that a unit may start although the root workspace may not list
/// their crate yet: the infrastructure daemons of Masterplan H3/H7/H8, which
/// land in parallel with H10. Remove an entry once its crate is a root
/// member (a member's binary is found by discovery anyway).
const PLANNED_BINARIES: &[&str] = &["harw-auth-hub", "harw-netsec", "harw-security-hub"];

/// Units outside this repository that `WantedBy=`/`After=`/… may name.
const SYSTEM_UNITS: &[&str] = &[
    "multi-user.target",
    "sockets.target",
    "local-fs.target",
    "network.target",
    "network-online.target",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Collects every regular file below `dir`, as `/`-separated paths relative
/// to `base`.
fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<String>) -> TestResult {
    for entry in fs::read_dir(dir).map_err(ctx("read deploy directory"))? {
        let entry = entry.map_err(ctx("read deploy directory entry"))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(ctx("stat deploy entry"))?;
        if file_type.is_dir() {
            walk(base, &path, out)?;
        } else {
            let relative = path
                .strip_prefix(base)
                .map_err(ctx("strip repository prefix"))?;
            let parts: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.insert(parts.join("/"));
        }
    }
    Ok(())
}

fn rendered(asset: &EmbeddedDeploymentAsset) -> String {
    render(asset.contents, &RenderPaths::default())
}

fn parsed(asset: &EmbeddedDeploymentAsset) -> ParsedUnit {
    parse_unit(&rendered(asset))
}

fn embedded_unit(name: &str) -> TestResult<&'static EmbeddedDeploymentAsset> {
    systemd_unit(name).ok_or_else(|| TestError::Unexpected(format!("unit {name} is not embedded")))
}

/// Names of all embedded systemd units.
fn unit_names() -> BTreeSet<&'static str> {
    systemd_units()
        .map(EmbeddedDeploymentAsset::file_name)
        .collect()
}

/// Splits a space-separated unit list directive into names.
fn words(values: &[&str]) -> Vec<String> {
    values
        .iter()
        .flat_map(|v| v.split_whitespace())
        .map(str::to_owned)
        .collect()
}

/// The program name of an `ExecStart=` value: first word, systemd prefix
/// characters stripped, directories dropped.
fn exec_binary(exec_start: &str) -> Option<&str> {
    let program = exec_start.split_whitespace().next()?;
    let program = program.trim_start_matches(['@', '-', ':', '+', '!']);
    program.rsplit('/').next()
}

/// The value following `flag` in an `ExecStart=` line.
fn exec_flag<'a>(exec_start: &'a str, flag: &str) -> Option<&'a str> {
    let mut words = exec_start.split_whitespace();
    words.find(|w| *w == flag)?;
    words.next()
}

/// Every binary the root workspace builds: `[[bin]]` names, the package name
/// for an implicit `src/main.rs`, and `src/bin/*.rs` stems.
fn workspace_binaries(root: &Path) -> TestResult<BTreeSet<String>> {
    let text = fs::read_to_string(root.join("Cargo.toml")).map_err(ctx("read root Cargo.toml"))?;
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(ctx("parse root Cargo.toml"))?;
    let members = doc
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml_edit::Item::as_array)
        .ok_or(TestError::Missing("[workspace] members"))?;

    let mut binaries = BTreeSet::new();
    for member in members.iter().filter_map(toml_edit::Value::as_str) {
        let dir = root.join(member);
        let Ok(text) = fs::read_to_string(dir.join("Cargo.toml")) else {
            // A member another change is still creating; cargo itself would
            // reject it, this test only needs the binaries that exist.
            continue;
        };
        let doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(ctx("parse member Cargo.toml"))?;
        let explicit: Vec<String> = doc
            .get("bin")
            .and_then(toml_edit::Item::as_array_of_tables)
            .map(|tables| {
                tables
                    .iter()
                    .filter_map(|t| t.get("name").and_then(toml_edit::Item::as_str))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if explicit.is_empty() && dir.join("src/main.rs").is_file() {
            if let Some(name) = doc
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(toml_edit::Item::as_str)
            {
                binaries.insert(name.to_owned());
            }
        }
        binaries.extend(explicit);
        if let Ok(entries) = fs::read_dir(dir.join("src/bin")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "rs") {
                    if let Some(stem) = path.file_stem() {
                        binaries.insert(stem.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }
    Ok(binaries)
}

/// Structural INI problems of a unit text: lines that are neither a comment,
/// blank, a `[Section]` header nor `Key=Value`; keys before the first
/// section; sections outside `allowed`.
fn ini_violations(text: &str, allowed: &[&str]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut in_section = false;
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if raw.ends_with('\\') {
            violations.push(format!("line {line_no}: continuation lines are not used"));
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if !allowed.contains(&name) {
                violations.push(format!("line {line_no}: unexpected section [{name}]"));
            }
            in_section = true;
            continue;
        }
        match line.split_once('=') {
            Some((key, _))
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
            {
                if !in_section {
                    violations.push(format!("line {line_no}: key before first section"));
                }
            }
            _ => violations.push(format!("line {line_no}: not Key=Value: {line}")),
        }
    }
    violations
}

/// `u`/`g` declarations of the embedded sysusers snippet: (users, groups).
fn sysusers_declarations() -> TestResult<(BTreeSet<String>, BTreeSet<String>)> {
    let sysusers = asset("deploy/sysusers.d/harw.conf").ok_or(TestError::Missing("sysusers"))?;
    let mut users = BTreeSet::from(["root".to_owned()]);
    let mut groups = BTreeSet::from(["root".to_owned()]);
    for line in sysusers.contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next()) {
            (Some("u"), Some(name)) => {
                users.insert(name.to_owned());
                // sysusers creates a same-named primary group for `u`.
                groups.insert(name.to_owned());
            }
            (Some("g"), Some(name)) => {
                groups.insert(name.to_owned());
            }
            (Some("m"), Some(_)) => {}
            _ => {
                return Err(TestError::Unexpected(format!(
                    "unknown sysusers line: {line}"
                )));
            }
        }
    }
    Ok((users, groups))
}

// ---------------------------------------------------------------------------
// Parity: deploy/ <-> embedded <-> DoD manifest/installer
// ---------------------------------------------------------------------------

#[test]
fn test_every_deploy_file_is_embedded_byte_identically() -> TestResult {
    let root = repo_root();
    let mut on_disk = BTreeSet::new();
    walk(&root, &root.join("deploy"), &mut on_disk)?;

    let embedded: BTreeSet<String> = DEPLOYMENT_ASSETS
        .iter()
        .map(|a| a.path.to_owned())
        .collect();
    let missing: Vec<&String> = on_disk.difference(&embedded).collect();
    let stale: Vec<&String> = embedded.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "files under deploy/ that DEPLOYMENT_ASSETS does not embed: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "DEPLOYMENT_ASSETS entries without a file under deploy/: {stale:?}"
    );

    for asset in DEPLOYMENT_ASSETS {
        let bytes = fs::read(root.join(asset.path)).map_err(ctx("read deploy file"))?;
        assert!(
            bytes == asset.contents.as_bytes(),
            "{} differs from its embedded copy",
            asset.path
        );
    }
    Ok(())
}

#[test]
fn test_assets_are_sorted_unique_and_kinded_by_extension() {
    let paths: Vec<&str> = DEPLOYMENT_ASSETS.iter().map(|a| a.path).collect();
    let mut sorted = paths.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(paths, sorted, "DEPLOYMENT_ASSETS must be sorted and unique");

    for asset in DEPLOYMENT_ASSETS {
        let expected = if asset.path.starts_with("deploy/sysusers.d/") {
            AssetKind::Sysusers
        } else if asset.path.starts_with("deploy/tmpfiles.d/") {
            AssetKind::Tmpfiles
        } else if asset.path.ends_with(".service") {
            AssetKind::SystemdService
        } else if asset.path.ends_with(".socket") {
            AssetKind::SystemdSocket
        } else {
            AssetKind::SystemdTarget
        };
        assert_eq!(asset.kind, expected, "{}: wrong AssetKind", asset.path);
        if asset.kind.is_systemd_unit() {
            assert!(
                asset.path.starts_with("deploy/systemd/"),
                "{}: units live under deploy/systemd/",
                asset.path
            );
        }
    }
}

#[test]
fn test_dod_manifest_matches_the_dod_package_assets() -> TestResult {
    let manifest = fs::read_to_string(repo_root().join("dod/packaging/manifest"))
        .map_err(ctx("read dod/packaging/manifest"))?;
    let mut manifested = BTreeSet::new();
    for line in manifest.lines().map(str::trim) {
        let mapped = if let Some(name) = line.strip_prefix("systemd/") {
            format!("deploy/systemd/{name}")
        } else if let Some(name) = line.strip_prefix("sysusers/") {
            format!("deploy/sysusers.d/{name}")
        } else if let Some(name) = line.strip_prefix("tmpfiles/") {
            format!("deploy/tmpfiles.d/{name}")
        } else {
            continue;
        };
        manifested.insert(mapped);
    }
    let package: BTreeSet<String> = DOD_PACKAGE_ASSETS.iter().map(|p| (*p).to_owned()).collect();
    assert_eq!(
        manifested, package,
        "dod/packaging/manifest and DOD_PACKAGE_ASSETS disagree"
    );
    for path in DOD_PACKAGE_ASSETS {
        assert!(
            asset(path).is_some(),
            "{path} is installed but not embedded"
        );
    }
    Ok(())
}

#[test]
fn test_dod_installer_installs_exactly_the_package_units_from_deploy() -> TestResult {
    let script = fs::read_to_string(repo_root().join("dod/scripts/install.sh"))
        .map_err(ctx("read dod/scripts/install.sh"))?;
    let line = script
        .lines()
        .find_map(|l| l.strip_prefix("dod_units='"))
        .and_then(|rest| rest.strip_suffix('\''))
        .ok_or(TestError::Missing("dod_units='…' in install.sh"))?;
    let installed: BTreeSet<String> = line.split_whitespace().map(str::to_owned).collect();
    let expected: BTreeSet<String> = DOD_PACKAGE_ASSETS
        .iter()
        .filter_map(|p| p.strip_prefix("deploy/systemd/"))
        .map(str::to_owned)
        .collect();
    assert_eq!(installed, expected, "install.sh dod_units differs");

    assert!(
        script.contains("/../../deploy"),
        "install.sh must read units from the repository's deploy/ tree"
    );
    for snippet in [
        "$deploy_dir/sysusers.d/harw.conf",
        "$deploy_dir/tmpfiles.d/harw.conf",
    ] {
        assert!(
            script.contains(snippet),
            "install.sh must install {snippet}"
        );
    }
    assert!(
        !script.contains("packaging_dir/systemd") && !script.contains("packaging/systemd"),
        "install.sh must not read the deleted dod/packaging/systemd tree"
    );
    Ok(())
}

#[test]
fn test_no_duplicate_unit_tree_remains_under_dod_packaging() {
    let packaging = repo_root().join("dod/packaging");
    for stale in ["systemd", "sysusers.d", "tmpfiles.d"] {
        assert!(
            !packaging.join(stale).exists(),
            "dod/packaging/{stale} must not exist: deploy/ is the only source"
        );
    }
}

// ---------------------------------------------------------------------------
// Placeholders and rendering
// ---------------------------------------------------------------------------

#[test]
fn test_assets_use_only_known_placeholders_and_render_completely() {
    for asset in DEPLOYMENT_ASSETS {
        for token in unresolved_placeholders(asset.contents) {
            assert!(
                PLACEHOLDERS.contains(&token.as_str()),
                "{}: unknown placeholder {token}",
                asset.path
            );
        }
        let leftover = unresolved_placeholders(&rendered(asset));
        assert!(
            leftover.is_empty(),
            "{}: unresolved after rendering: {leftover:?}",
            asset.path
        );
    }
}

#[test]
fn test_placeholders_never_appear_in_comments() {
    // install.sh substitutes every occurrence; a placeholder in a comment
    // would render into a misleading path fragment.
    for asset in DEPLOYMENT_ASSETS {
        for line in asset.contents.lines() {
            let line = line.trim_start();
            if line.starts_with('#') {
                assert!(
                    unresolved_placeholders(line).is_empty(),
                    "{}: placeholder in comment: {line}",
                    asset.path
                );
            }
        }
    }
}

#[test]
fn test_render_substitutes_every_placeholder() {
    let paths = RenderPaths {
        libexecdir: "/L".to_owned(),
        bpfdir: "/B".to_owned(),
        sysconfdir: "/S".to_owned(),
        statedir: "/T".to_owned(),
        logdir: "/G".to_owned(),
    };
    let text = PLACEHOLDERS.join(" ");
    assert_eq!(render(&text, &paths), "/L /B /S /T /G");
}

#[test]
fn test_unresolved_placeholders_finds_only_well_formed_tokens() {
    assert_eq!(
        unresolved_placeholders("a @FOO@ b @x@ c @BAR_BAZ@@ d user@host"),
        vec!["@FOO@".to_owned(), "@BAR_BAZ@".to_owned()]
    );
    assert!(unresolved_placeholders("no tokens here").is_empty());
}

// ---------------------------------------------------------------------------
// Unit syntax and references
// ---------------------------------------------------------------------------

#[test]
fn test_every_unit_is_well_formed_ini_with_its_expected_sections() -> TestResult {
    for asset in systemd_units() {
        let allowed: &[&str] = match asset.kind {
            AssetKind::SystemdService => &["Unit", "Service", "Install"],
            AssetKind::SystemdSocket => &["Unit", "Socket", "Install"],
            AssetKind::SystemdTarget => &["Unit", "Install"],
            AssetKind::Sysusers | AssetKind::Tmpfiles => {
                return Err(TestError::Unexpected(format!(
                    "{} is not a unit",
                    asset.path
                )));
            }
        };
        let violations = ini_violations(asset.contents, allowed);
        assert!(violations.is_empty(), "{}: {violations:?}", asset.path);

        let unit = parsed(asset);
        assert!(unit.has_section("Unit"), "{}: missing [Unit]", asset.path);
        assert!(
            unit.last_value("Unit", "Description").is_some(),
            "{}: missing Description=",
            asset.path
        );
        match asset.kind {
            AssetKind::SystemdService => assert!(
                unit.last_value("Service", "ExecStart").is_some(),
                "{}: missing ExecStart=",
                asset.path
            ),
            AssetKind::SystemdSocket => assert_eq!(
                unit.count_keys_with_prefix("Socket", "Listen"),
                1,
                "{}: exactly one Listen*= line (LISTEN_FDS=1)",
                asset.path
            ),
            _ => {}
        }
    }
    Ok(())
}

#[test]
fn test_every_exec_start_names_a_known_workspace_binary() -> TestResult {
    let known = workspace_binaries(&repo_root())?;
    assert!(
        known.contains("harw") && known.contains("harw-sentinel"),
        "binary discovery is broken: {known:?}"
    );
    for asset in systemd_units().filter(|a| a.kind == AssetKind::SystemdService) {
        let unit = parsed(asset);
        for exec in unit.values("Service", "ExecStart") {
            let binary = exec_binary(exec)
                .ok_or_else(|| TestError::Unexpected(format!("{}: empty ExecStart", asset.path)))?;
            assert!(
                known.contains(binary) || PLANNED_BINARIES.contains(&binary),
                "{}: ExecStart runs {binary}, which no workspace crate builds",
                asset.path
            );
        }
    }
    Ok(())
}

#[test]
fn test_every_referenced_unit_is_embedded_or_a_system_unit() {
    let names = unit_names();
    let known = |name: &str| names.contains(name) || SYSTEM_UNITS.contains(&name);
    for asset in systemd_units() {
        let unit = parsed(asset);
        let mut referenced = Vec::new();
        for key in ["Wants", "Requires", "After", "Before", "PartOf", "BindsTo"] {
            referenced.extend(words(&unit.values("Unit", key)));
        }
        for key in ["WantedBy", "RequiredBy"] {
            referenced.extend(words(&unit.values("Install", key)));
        }
        referenced.extend(words(&unit.values("Service", "Sockets")));
        referenced.extend(words(&unit.values("Socket", "Service")));
        for name in referenced {
            assert!(
                known(&name),
                "{}: references {name}, which is neither embedded nor a system unit",
                asset.path
            );
        }
    }
}

#[test]
fn test_sockets_and_services_reference_each_other() -> TestResult {
    let names = unit_names();
    for asset in systemd_units().filter(|a| a.kind == AssetKind::SystemdSocket) {
        let unit = parsed(asset);
        let service = unit.last_value("Socket", "Service").map_or_else(
            || asset.file_name().replace(".socket", ".service"),
            str::to_owned,
        );
        assert!(
            names.contains(service.as_str()),
            "{}: activates {service}, which is not embedded",
            asset.path
        );
        let service_unit = parsed(embedded_unit(&service)?);
        assert!(
            words(&service_unit.values("Service", "Sockets"))
                .iter()
                .any(|s| s == asset.file_name()),
            "{service}: must name {} in Sockets=",
            asset.file_name()
        );
    }
    for asset in systemd_units().filter(|a| a.kind == AssetKind::SystemdService) {
        let unit = parsed(asset);
        for socket in words(&unit.values("Service", "Sockets")) {
            assert!(
                names.contains(socket.as_str()),
                "{}: Sockets={socket} is not embedded",
                asset.path
            );
            assert!(
                words(&unit.values("Unit", "Requires")).contains(&socket),
                "{}: a socket-activated service must Requires= its socket {socket}",
                asset.path
            );
        }
    }
    Ok(())
}

#[test]
fn test_every_account_and_group_is_declared_in_sysusers() -> TestResult {
    let (users, groups) = sysusers_declarations()?;
    for asset in systemd_units() {
        let unit = parsed(asset);
        for user in unit
            .values("Service", "User")
            .into_iter()
            .chain(unit.values("Socket", "SocketUser"))
        {
            assert!(
                users.contains(user),
                "{}: undeclared user {user}",
                asset.path
            );
        }
        let mut unit_groups = words(&unit.values("Service", "SupplementaryGroups"));
        unit_groups.extend(words(&unit.values("Service", "Group")));
        unit_groups.extend(words(&unit.values("Socket", "SocketGroup")));
        for group in unit_groups {
            assert!(
                groups.contains(&group),
                "{}: undeclared group {group}",
                asset.path
            );
        }
    }
    let tmpfiles = asset("deploy/tmpfiles.d/harw.conf").ok_or(TestError::Missing("tmpfiles"))?;
    for line in tmpfiles.contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(user), Some(group)) = (fields.get(3), fields.get(4)) else {
            return Err(TestError::Unexpected(format!(
                "short tmpfiles line: {line}"
            )));
        };
        assert!(users.contains(*user), "tmpfiles: undeclared user {user}");
        assert!(
            groups.contains(*group),
            "tmpfiles: undeclared group {group}"
        );
    }
    Ok(())
}

#[test]
fn test_every_socket_directory_is_created_by_tmpfiles() -> TestResult {
    let tmpfiles = asset("deploy/tmpfiles.d/harw.conf").ok_or(TestError::Missing("tmpfiles"))?;
    let rendered_tmpfiles = rendered(tmpfiles);
    let declared: BTreeSet<&str> = rendered_tmpfiles
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("d "))
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();

    let mut socket_paths: Vec<(String, String)> = Vec::new();
    for asset in systemd_units() {
        let unit = parsed(asset);
        for key in ["ListenStream", "ListenSequentialPacket", "ListenDatagram"] {
            for path in unit.values("Socket", key) {
                socket_paths.push((asset.path.to_owned(), path.to_owned()));
            }
        }
        for exec in unit.values("Service", "ExecStart") {
            for flag in ["--socket", "--sentinel-socket"] {
                if let Some(path) = exec_flag(exec, flag) {
                    socket_paths.push((asset.path.to_owned(), path.to_owned()));
                }
            }
        }
    }
    assert!(!socket_paths.is_empty());
    for (unit_path, socket) in socket_paths {
        let parent = socket
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .ok_or_else(|| TestError::Unexpected(format!("{unit_path}: relative socket")))?;
        assert!(
            declared.contains(parent),
            "{unit_path}: socket {socket} lives in {parent}, which tmpfiles.d does not create"
        );
    }
    Ok(())
}

#[test]
fn test_no_service_uses_a_shared_runtime_directory() {
    // /run/harw and /run/harw/infra are shared; a RuntimeDirectory= of one
    // unit would delete every other socket in them when that unit stops.
    for asset in systemd_units() {
        let unit = parsed(asset);
        for dir in unit.values("Service", "RuntimeDirectory") {
            assert!(
                !dir.split_whitespace()
                    .any(|d| d == "harw" || d == "harw/infra"),
                "{}: RuntimeDirectory={dir} would own a shared runtime directory",
                asset.path
            );
        }
    }
}

// ---------------------------------------------------------------------------
// DoD and infrastructure contracts
// ---------------------------------------------------------------------------

#[test]
fn test_probes_and_sentinel_agree_on_the_sentinel_socket() -> TestResult {
    let sentinel = parsed(embedded_unit("harw-sentinel.service")?);
    let exec = sentinel
        .last_value("Service", "ExecStart")
        .ok_or(TestError::Missing("sentinel ExecStart"))?;
    let socket = exec_flag(exec, "--socket").ok_or(TestError::Missing("sentinel --socket"))?;
    assert_eq!(socket, "/run/harw/sentinel.sock");
    for probe in ["harw-probe-bpf.service", "harw-probe-fs.service"] {
        let unit = parsed(embedded_unit(probe)?);
        let exec = unit
            .last_value("Service", "ExecStart")
            .ok_or(TestError::Missing("probe ExecStart"))?;
        assert_eq!(
            exec_flag(exec, "--sentinel-socket"),
            Some(socket),
            "{probe}: pushes to a different socket than the sentinel binds"
        );
    }
    Ok(())
}

#[test]
fn test_dod_target_pulls_in_observation_only() -> TestResult {
    let target = parsed(embedded_unit("harw-dod.target")?);
    let wants = words(&target.values("Unit", "Wants"));
    assert_eq!(
        wants,
        vec![
            "harw-sentinel.service".to_owned(),
            "harw-probe-bpf.service".to_owned()
        ]
    );
    for asset in systemd_units() {
        if asset.file_name().starts_with("harw-warden") {
            let unit = parsed(asset);
            assert!(
                !unit.has_section("Install"),
                "{}: the Warden must ship without [Install]",
                asset.path
            );
            assert_eq!(
                unit.last_value("Unit", "ConditionPathExists"),
                Some("/etc/harw-dod/warden.enable"),
                "{}: missing the explicit opt-in guard",
                asset.path
            );
        }
    }
    Ok(())
}

#[test]
fn test_infra_sockets_listen_under_run_harw_infra() -> TestResult {
    let expected = BTreeMap::from([
        ("harw-auth-hub.socket", "/run/harw/infra/secure.sock"),
        ("harw-control.socket", "/run/harw/infra/control.sock"),
        ("harw-netsec.socket", "/run/harw/infra/network.sock"),
        ("harw-security-hub.socket", "/run/harw/infra/security.sock"),
    ]);
    for (socket, path) in expected {
        let unit = parsed(embedded_unit(socket)?);
        assert_eq!(
            unit.last_value("Socket", "ListenStream"),
            Some(path),
            "{socket}"
        );
        assert_eq!(
            unit.last_value("Socket", "SocketMode"),
            Some("0660"),
            "{socket}"
        );
        assert_eq!(unit.last_value("Socket", "Accept"), Some("no"), "{socket}");
    }
    // Socket-activated daemons must be told to take the inherited descriptor
    // and must not bind a path of their own (it would collide with the
    // socket unit's listener).
    for service in [
        "harw-auth-hub.service",
        "harw-control.service",
        "harw-netsec.service",
        "harw-security-hub.service",
    ] {
        let unit = parsed(embedded_unit(service)?);
        let exec = unit
            .last_value("Service", "ExecStart")
            .ok_or(TestError::Missing("infra ExecStart"))?;
        assert!(
            exec.split_whitespace().any(|w| w == "--systemd-socket"),
            "{service}: socket-activated daemons need --systemd-socket"
        );
        for flag in ["--socket", "--socket-group"] {
            assert!(
                !exec
                    .split_whitespace()
                    .any(|w| w == flag || w.strip_prefix(flag).is_some_and(|r| r.starts_with('='))),
                "{service}: {flag} conflicts with socket activation"
            );
        }
    }
    Ok(())
}

#[test]
fn test_control_plane_is_socket_activated_for_harw_control_clients() -> TestResult {
    let socket = parsed(embedded_unit("harw-control.socket")?);
    for (key, value) in [
        ("SocketUser", "harw-control"),
        ("SocketGroup", "harw-control-clients"),
        ("SocketMode", "0660"),
        ("Service", "harw-control.service"),
        ("RemoveOnStop", "yes"),
    ] {
        assert_eq!(
            socket.last_value("Socket", key),
            Some(value),
            "harw-control.socket: {key}"
        );
    }
    assert_eq!(
        words(&socket.values("Install", "WantedBy")),
        ["harw-infra.target"]
    );
    let (_, groups) = sysusers_declarations()?;
    assert!(
        groups.contains("harw-control-clients"),
        "sysusers.d must declare harw-control-clients"
    );

    let service = parsed(embedded_unit("harw-control.service")?);
    for key in ["Requires", "After"] {
        assert!(
            words(&service.values("Unit", key)).contains(&"harw-control.socket".to_owned()),
            "harw-control.service: {key}=harw-control.socket"
        );
    }
    assert_eq!(
        words(&service.values("Service", "Sockets")),
        ["harw-control.socket"]
    );
    assert_eq!(
        service.last_value("Service", "ExecStart"),
        Some("/usr/bin/harw --home /var/lib/harw-control web --systemd-socket")
    );
    // Enabled through the socket, never directly.
    assert!(!service.has_section("Install"));
    let target = parsed(embedded_unit("harw-infra.target")?);
    for key in ["Wants", "After"] {
        let listed = words(&target.values("Unit", key));
        assert!(
            listed.contains(&"harw-control.socket".to_owned()),
            "harw-infra.target: {key}=harw-control.socket"
        );
        assert!(
            !listed.contains(&"harw-control.service".to_owned()),
            "harw-infra.target: {key}= must name the socket, not the service"
        );
    }
    Ok(())
}

#[test]
fn test_run_harw_infra_is_root_owned_because_every_daemon_is_activated() -> TestResult {
    let tmpfiles = asset("deploy/tmpfiles.d/harw.conf").ok_or(TestError::Missing("tmpfiles"))?;
    let infra: Vec<Vec<&str>> = tmpfiles
        .contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|f| f.get(1) == Some(&"/run/harw/infra"))
        .collect();
    assert_eq!(
        infra,
        [["d", "/run/harw/infra", "0755", "root", "root", "-"]],
        "/run/harw/infra must be 0755 root:root"
    );
    // Nothing binds there except systemd: no service needs write access, and
    // the former self-binding group harw-infra is gone.
    for asset in systemd_units().filter(|a| a.kind == AssetKind::SystemdService) {
        let unit = parsed(asset);
        for paths in unit.values("Service", "ReadWritePaths") {
            assert!(
                !paths
                    .split_whitespace()
                    .map(|p| p.trim_start_matches(['-', '+']))
                    .any(|p| p == "/run/harw/infra" || p.starts_with("/run/harw/infra/")),
                "{}: ReadWritePaths={paths} claims /run/harw/infra",
                asset.path
            );
        }
        assert!(
            !words(&unit.values("Service", "SupplementaryGroups"))
                .contains(&"harw-infra".to_owned()),
            "{}: harw-infra no longer exists",
            asset.path
        );
    }
    let (_, groups) = sysusers_declarations()?;
    assert!(
        !groups.contains("harw-infra"),
        "sysusers.d: harw-infra is unused and must not be declared"
    );
    Ok(())
}

#[test]
fn test_security_hub_is_socket_activated_for_harw_security() -> TestResult {
    let socket = parsed(embedded_unit("harw-security-hub.socket")?);
    assert_eq!(
        socket.last_value("Socket", "SocketGroup"),
        Some("harw-security")
    );
    assert_eq!(
        socket.last_value("Socket", "SocketUser"),
        Some("harw-security-hub")
    );
    assert_eq!(
        socket.last_value("Socket", "Service"),
        Some("harw-security-hub.service")
    );
    let service = parsed(embedded_unit("harw-security-hub.service")?);
    for key in ["Requires", "After"] {
        assert!(
            words(&service.values("Unit", key)).contains(&"harw-security-hub.socket".to_owned()),
            "harw-security-hub.service: {key}=harw-security-hub.socket"
        );
    }
    // Activated, not self-binding: no write access to /run/harw/infra.
    assert!(
        service.values("Service", "ReadWritePaths").is_empty(),
        "harw-security-hub.service: ReadWritePaths= is obsolete"
    );
    // Enabled through the socket, never directly.
    assert!(!service.has_section("Install"));
    let target = parsed(embedded_unit("harw-infra.target")?);
    let wants = words(&target.values("Unit", "Wants"));
    assert!(wants.contains(&"harw-security-hub.socket".to_owned()));
    assert!(!wants.contains(&"harw-security-hub.service".to_owned()));
    Ok(())
}

#[test]
fn test_infra_services_are_hardened() -> TestResult {
    let families = BTreeMap::from([
        ("harw-auth-hub.service", "AF_UNIX"),
        ("harw-security-hub.service", "AF_UNIX"),
        ("harw-netsec.service", "AF_UNIX AF_INET AF_INET6"),
        ("harw-control.service", "AF_UNIX AF_INET AF_INET6"),
    ]);
    for (service, expected_families) in families {
        let unit = parsed(embedded_unit(service)?);
        for (key, value) in [
            ("NoNewPrivileges", "yes"),
            ("ProtectSystem", "strict"),
            ("ProtectHome", "yes"),
            ("PrivateTmp", "yes"),
        ] {
            assert_eq!(
                unit.last_value("Service", key),
                Some(value),
                "{service}: {key}"
            );
        }
        for key in ["CapabilityBoundingSet", "AmbientCapabilities"] {
            assert!(
                unit.last_value("Service", key)
                    .is_some_and(|v| capability_set(v).is_empty()),
                "{service}: {key}= must be present and empty"
            );
        }
        assert!(
            unit.values("Service", "SystemCallFilter")
                .iter()
                .any(|v| v.split_whitespace().any(|s| s == "@system-service")),
            "{service}: SystemCallFilter=@system-service"
        );
        assert_eq!(
            unit.last_value("Service", "RestrictAddressFamilies")
                .map(capability_set),
            Some(capability_set(expected_families)),
            "{service}: RestrictAddressFamilies"
        );
        assert_ne!(
            unit.last_value("Service", "User"),
            Some("root"),
            "{service}: never root"
        );
    }
    Ok(())
}

#[test]
fn test_every_dod_and_infra_service_sets_the_common_hardening() -> TestResult {
    for asset in systemd_units().filter(|a| a.kind == AssetKind::SystemdService) {
        let unit = parsed(asset);
        for (key, value) in [
            ("NoNewPrivileges", "yes"),
            ("ProtectSystem", "strict"),
            ("PrivateTmp", "yes"),
            ("ProtectHome", "yes"),
        ] {
            assert_eq!(
                unit.last_value("Service", key),
                Some(value),
                "{}: {key}",
                asset.path
            );
        }
        assert!(
            unit.last_value("Service", "User")
                .is_some_and(|u| u != "root"),
            "{}: must run as a dedicated non-root user",
            asset.path
        );
        assert!(
            unit.last_value("Service", "CapabilityBoundingSet")
                .is_some(),
            "{}: must state its bounding set explicitly",
            asset.path
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// print_systemd
// ---------------------------------------------------------------------------

#[test]
fn test_print_systemd_single_unit_is_the_rendered_asset() -> TestResult {
    let paths = RenderPaths::default();
    let text =
        print_systemd(Some("harw-sentinel.service"), &paths).map_err(ctx("print sentinel"))?;
    let expected = rendered(embedded_unit("harw-sentinel.service")?);
    assert_eq!(text, expected);
    assert!(text.contains("/usr/local/libexec/harw-dod/harw-sentinel"));

    let without_suffix =
        print_systemd(Some("harw-sentinel"), &paths).map_err(ctx("print by stem"))?;
    assert_eq!(without_suffix, expected);
    Ok(())
}

#[test]
fn test_print_systemd_all_lists_every_unit_once() -> TestResult {
    let text = print_systemd(None, &RenderPaths::default()).map_err(ctx("print all"))?;
    for asset in systemd_units() {
        let header = format!("# ---- {} ----", asset.path);
        assert_eq!(text.matches(&header).count(), 1, "{}", asset.path);
    }
    assert!(unresolved_placeholders(&text).is_empty());
    assert!(
        !text.contains("deploy/sysusers.d"),
        "only units are printed"
    );
    Ok(())
}

#[test]
fn test_print_systemd_unknown_unit_lists_the_available_ones() -> TestResult {
    let error = match print_systemd(Some("harw-nope.service"), &RenderPaths::default()) {
        Err(error) => error,
        Ok(_) => {
            return Err(TestError::Unexpected(
                "an unknown unit must be an error".to_owned(),
            ));
        }
    };
    assert_eq!(error.requested, "harw-nope.service");
    let message = error.to_string();
    assert!(message.contains("harw-nope.service"));
    assert!(message.contains("harw-warden.socket"));
    Ok(())
}

#[test]
fn test_exec_binary_strips_prefixes_and_directories() {
    assert_eq!(exec_binary("/usr/bin/harw web"), Some("harw"));
    assert_eq!(exec_binary("-/usr/local/bin/harw-x --a"), Some("harw-x"));
    assert_eq!(exec_binary("harw-y"), Some("harw-y"));
    assert_eq!(exec_binary("   "), None);
}

#[test]
fn test_ini_violations_reports_malformed_lines() {
    let bad = "Key=before\n[Unit]\nnot a pair\n[Bogus]\nA=b\n";
    let violations = ini_violations(bad, &["Unit"]);
    assert_eq!(violations.len(), 3, "{violations:?}");
    assert!(ini_violations("# c\n[Unit]\nDescription=x\n", &["Unit"]).is_empty());
}
