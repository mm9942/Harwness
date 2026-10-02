//! Host alias store tests (S07). Panic-free style: tests return `Result`.

use std::error::Error;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use harw_session_remote::{AliasStore, HostAlias, RemoteError};

type TestResult = Result<(), Box<dyn Error>>;

fn node(alias: &str) -> HostAlias {
    HostAlias {
        alias: alias.to_owned(),
        endpoint: "node:host.example:7443".to_owned(),
        expected_node: Some("node-abc_1".to_owned()),
    }
}

fn unix(alias: &str) -> HostAlias {
    HostAlias {
        alias: alias.to_owned(),
        endpoint: "unix:/run/user/1000/harw.sock".to_owned(),
        expected_node: None,
    }
}

fn is_state(r: &Result<(), RemoteError>) -> bool {
    matches!(r, Err(RemoteError::State(_)))
}

#[test]
fn roundtrip_get_list_remove() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    assert!(store.list()?.is_empty());
    assert_eq!(store.get("prod")?, None);
    store.put(&node("prod"))?;
    store.put(&unix("local"))?;
    assert_eq!(store.get("prod")?, Some(node("prod")));
    let all = store.list()?;
    assert_eq!(all, vec![unix("local"), node("prod")]);
    assert!(store.remove("prod")?);
    assert!(!store.remove("prod")?);
    assert_eq!(store.get("prod")?, None);
    Ok(())
}

#[test]
fn put_replaces_explicitly_and_persists_across_instances() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    store.put(&node("prod"))?;
    let mut changed = node("prod");
    changed.expected_node = Some("other-node".to_owned());
    store.put(&changed)?;
    assert_eq!(AliasStore::new(tmp.path()).get("prod")?, Some(changed));
    Ok(())
}

#[test]
fn files_are_private_atomic_and_leave_no_temp() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    store.put(&node("prod"))?;
    let dir = tmp.path().join("hosts");
    assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
    let file = dir.join("prod.json");
    assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o777, 0o600);
    let names: Vec<String> = fs::read_dir(&dir)?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names, vec!["prod.json".to_owned()]);
    Ok(())
}

#[test]
fn rejects_traversal_and_invalid_aliases() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    let long = "a".repeat(65);
    let bad = [
        "",
        ".",
        "..",
        "../evil",
        "a/b",
        "a\\b",
        "/abs",
        "a.json",
        ".hidden",
        "-lead",
        "_lead",
        "Upper",
        "sp ace",
        "nul\0",
        "tab\t",
        "uni\u{e9}",
        "a:b",
        long.as_str(),
    ];
    for alias in bad {
        assert!(is_state(&store.put(&node(alias))), "put {alias:?}");
        assert!(store.get(alias).is_err(), "get {alias:?}");
        assert!(store.remove(alias).is_err(), "remove {alias:?}");
    }
    // Nothing escaped or got created.
    assert!(!tmp.path().join("evil.json").exists());
    assert!(store.list()?.is_empty());
    assert!(
        AliasStore::new(tmp.path())
            .put(&node(&"a".repeat(64)))
            .is_ok()
    );
    Ok(())
}

#[test]
fn encoding_is_injective_over_valid_alphabet() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    let aliases = ["a", "a-b", "a_b", "ab", "a0", "0a", "a--b", "a-", "z9_-"];
    for a in aliases {
        store.put(&node(a))?;
    }
    assert_eq!(store.list()?.len(), aliases.len());
    for a in aliases {
        assert_eq!(store.get(a)?.map(|r| r.alias), Some(a.to_owned()));
    }
    Ok(())
}

#[test]
fn rejects_bad_endpoints_and_secrets() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    let cases: Vec<(&str, Option<&str>)> = vec![
        ("", None),
        ("http://x:1", Some("n")),
        ("node:host", Some("n")),
        ("node:host:0", Some("n")),
        ("node:host:99999", Some("n")),
        ("node::80", Some("n")),
        ("node:user:pw@host:80", Some("n")),
        ("node:host:80?token=abc", Some("n")),
        ("node:host:80#frag", Some("n")),
        ("node:ho st:80", Some("n")),
        ("node:host:80", None),
        ("node:host:80", Some("")),
        ("node:host:80", Some("bad id")),
        ("unix:relative.sock", None),
        ("unix:/run/../etc/x", None),
        ("unix:/run/x?y", None),
        ("unix:/run/x.sock", Some("n")),
        ("unix:/run/\nx", None),
    ];
    for (endpoint, expected) in cases {
        let rec = HostAlias {
            alias: "h".to_owned(),
            endpoint: endpoint.to_owned(),
            expected_node: expected.map(str::to_owned),
        };
        assert!(is_state(&store.put(&rec)), "{endpoint:?} {expected:?}");
    }
    assert!(!tmp.path().join("hosts").join("h.json").exists());
    let ipv6 = HostAlias {
        alias: "h".to_owned(),
        endpoint: "node:[::1]:7443".to_owned(),
        expected_node: Some("n".to_owned()),
    };
    store.put(&ipv6)?;
    Ok(())
}

#[test]
fn stored_file_holds_only_the_three_fields() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    store.put(&node("prod"))?;
    let text = fs::read_to_string(tmp.path().join("hosts/prod.json"))?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let mut keys: Vec<&str> = v
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(keys, vec!["alias", "endpoint", "expected_node"]);
    Ok(())
}

#[test]
fn tampered_records_fail_closed() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    store.put(&node("prod"))?;
    let path = tmp.path().join("hosts/prod.json");

    // Loosened permissions are refused.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    assert!(store.get("prod").is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    assert!(store.get("prod")?.is_some());

    // Alias/file-name mismatch, unknown fields, garbage, oversize.
    let write = |body: &str| -> Result<(), Box<dyn Error>> {
        fs::write(&path, body)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    };
    write(r#"{"alias":"other","endpoint":"unix:/a","expected_node":null}"#)?;
    assert!(store.get("prod").is_err());
    write(r#"{"alias":"prod","endpoint":"unix:/a","expected_node":null,"token":"x"}"#)?;
    assert!(store.get("prod").is_err());
    write("not json")?;
    assert!(store.get("prod").is_err());
    write(&"x".repeat(100_000))?;
    assert!(store.get("prod").is_err());
    assert!(store.list().is_err());
    Ok(())
}

#[test]
fn symlinked_record_is_not_followed() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    store.put(&unix("a"))?;
    let outside = tmp.path().join("outside.json");
    fs::write(&outside, "{}")?;
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))?;
    let link = tmp.path().join("hosts/b.json");
    symlink(&outside, &link)?;
    assert!(store.get("b").is_err());
    // put over a symlink replaces the link itself, never the target.
    store.put(&unix("b"))?;
    assert_eq!(fs::read_to_string(&outside)?, "{}");
    assert_eq!(store.get("b")?, Some(unix("b")));
    Ok(())
}

#[test]
fn list_ignores_leftover_temp_files_but_flags_strays() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(tmp.path());
    store.put(&unix("a"))?;
    let dir = tmp.path().join("hosts");
    fs::write(dir.join(".a.json.123.0.deadbeef.tmp"), "partial")?;
    assert_eq!(store.list()?, vec![unix("a")]);
    fs::write(dir.join("notes.txt"), "x")?;
    assert!(store.list().is_err());
    Ok(())
}

#[test]
fn missing_state_dir_is_empty_not_error() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = AliasStore::new(&tmp.path().join("nope/deeper"));
    assert!(store.list()?.is_empty());
    assert_eq!(store.get("x")?, None);
    assert!(!store.remove("x")?);
    store.put(&unix("x"))?;
    assert!(store.get("x")?.is_some());
    Ok(())
}
