//! Tests der `git.*`-Werkzeuge über die `ToolExecutor`-Schnittstelle.

use crate::test_support::{TestError, TestRepo, TestResult, error_of, json_of, run};
use crate::tools::{GIT_TOOL_NAMES, GitReadToolProvider};
use harw_authority::Permission;
use harw_extension_api::contributors::ToolProvider;
use harw_tools::{ToolName, ToolSpec};
use serde_json::{Value, json};

#[test]
fn exposes_six_unique_read_only_parallel_safe_tools() -> TestResult {
    let provider = GitReadToolProvider::new();
    let specs = provider.tools();
    assert_eq!(specs.len(), 6);
    let mut names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "git.blame",
            "git.branch",
            "git.diff",
            "git.log",
            "git.show",
            "git.status"
        ]
    );
    for name in GIT_TOOL_NAMES {
        assert!(provider.executor(&ToolName::new(*name)).is_some(), "{name}");
        assert!(provider.parallel_safe(&ToolName::new(*name)), "{name}");
    }
    assert!(provider.executor(&ToolName::new("git.commit")).is_none());
    assert!(
        GitReadToolProvider::TOOL_PERMISSIONS
            .iter()
            .all(|p| *p == Some(Permission::ReadWorkspace))
    );
    Ok(())
}

#[test]
fn descriptions_and_fields_are_documented() -> TestResult {
    for spec in GitReadToolProvider::new().tools() {
        let ToolSpec::Function(function) = spec;
        let name = function.name.as_str().to_owned();
        assert!(function.description.len() > 60, "{name}");
        assert!(function.description.contains(". "), "{name}");
        let properties = function.parameters.properties.clone().unwrap_or_default();
        for (field, schema) in &properties {
            assert!(
                schema.description.as_ref().is_some_and(|d| d.len() > 8),
                "{name}.{field}"
            );
        }
        assert!(
            function
                .parameters
                .clone()
                .into_strict()
                .additional_properties
                .is_some(),
            "{name}"
        );
    }
    Ok(())
}

fn minimal(name: &str) -> Value {
    if name == "git.blame" {
        json!({"path": "a.txt"})
    } else {
        json!({})
    }
}

fn repo() -> TestResult<TestRepo> {
    let repo = TestRepo::new()?;
    let base: &[(&str, u32, &str)] = &[
        ("a.txt", 0o100_644, "one\ntwo\n"),
        (".env", 0o100_644, "TOKEN=abc\n"),
    ];
    let c1 = repo.commit_files(base, &[], "init", 1_700_000_000)?;
    let next: &[(&str, u32, &str)] = &[
        ("a.txt", 0o100_644, "one\n2\n"),
        (".env", 0o100_644, "TOKEN=abc\n"),
    ];
    let tree = repo.tree_from(next)?;
    let c2 = repo.commit(tree, &[c1], "change a", 1_700_000_100)?;
    repo.set_ref("refs/heads/main", c2)?;
    repo.stage(next)?;
    repo.checkout(next)?;
    Ok(repo)
}

#[tokio::test]
async fn every_tool_rejects_unknown_options_and_missing_permission() -> TestResult {
    let repo = repo()?;
    let provider = GitReadToolProvider::new();
    let read = repo.read_ctx()?;
    let none = repo.ctx(vec![Permission::WriteWorkspace])?;
    for name in GIT_TOOL_NAMES {
        let tool = provider
            .executor(&ToolName::new(*name))
            .ok_or(TestError::Missing("executor"))?;
        let mut extra = minimal(name);
        extra["definitely_not_an_option"] = json!(1);
        match run(tool.as_ref(), &read, name, extra).await {
            Err(error) => assert!(
                error.to_string().contains("definitely_not_an_option"),
                "{name}: {error}"
            ),
            Ok(output) => assert!(
                error_of(output)?.contains("definitely_not_an_option"),
                "{name}"
            ),
        }
        match run(tool.as_ref(), &none, name, minimal(name)).await {
            Err(_) => {}
            Ok(output) => {
                error_of(output)?;
            }
        }
        let ok_run = json_of(run(tool.as_ref(), &read, name, minimal(name)).await?)?;
        assert_eq!(ok_run["tool"], *name);
    }
    Ok(())
}

async fn call(
    provider: &GitReadToolProvider,
    repo: &TestRepo,
    name: &str,
    args: Value,
) -> TestResult<Value> {
    let read = repo.read_ctx()?;
    let tool = provider
        .executor(&ToolName::new(name))
        .ok_or(TestError::Missing("executor"))?;
    json_of(run(tool.as_ref(), &read, name, args).await?)
}

#[tokio::test]
async fn tools_answer_over_the_executor_interface() -> TestResult {
    let repo = repo()?;
    let p = GitReadToolProvider::new();
    let status = call(&p, &repo, "git.status", json!({})).await?;
    assert_eq!(status["clean"], true);
    assert_eq!(status["head"]["branch"], "main");
    repo.write("a.txt", b"one\n3\n")?;
    repo.write("new.txt", b"n")?;
    let status = call(&p, &repo, "git.status", json!({})).await?;
    assert_eq!(status["counts"]["unstaged"], 1);
    assert_eq!(
        status["entries"][0],
        json!({"area": "unstaged", "status": "modified", "path": "a.txt"})
    );
    assert_eq!(
        status["entries"][1],
        json!({"area": "untracked", "path": "new.txt"})
    );
    let diff = call(&p, &repo, "git.diff", json!({"paths": ["a.txt"]})).await?;
    assert_eq!(diff["comparison"], "unstaged");
    assert!(
        diff["patch"]
            .as_str()
            .is_some_and(|text| text.contains("-2\n+3\n"))
    );
    let staged = call(&p, &repo, "git.diff", json!({"staged": true})).await?;
    assert_eq!(staged["total_files"], 0);
    let commits = call(
        &p,
        &repo,
        "git.diff",
        json!({"base": "HEAD~1", "target": "HEAD", "format": "name_status"}),
    )
    .await?;
    assert_eq!(commits["files"][0], json!({"status": "M", "path": "a.txt"}));
    let log = call(&p, &repo, "git.log", json!({"max_count": 1})).await?;
    assert_eq!(log["commits"][0]["subject"], "change a");
    assert_eq!(log["truncated"], true);
    let dated = call(
        &p,
        &repo,
        "git.log",
        json!({"since": "2023-11-14T22:14:00"}),
    )
    .await?;
    assert_eq!(dated["commits"].as_array().map(Vec::len), Some(1));
    let show = call(&p, &repo, "git.show", json!({"rev": "HEAD:a.txt"})).await?;
    assert_eq!(show["content"], "one\n2\n");
    let branch = call(&p, &repo, "git.branch", json!({})).await?;
    assert_eq!(branch["branches"][0]["name"], "main");
    let blame = call(&p, &repo, "git.blame", json!({"path": "a.txt"})).await?;
    assert_eq!(blame["lines"].as_array().map(Vec::len), Some(2));
    Ok(())
}

#[tokio::test]
async fn secrets_never_appear_in_any_output() -> TestResult {
    let repo = repo()?;
    repo.write(".env", b"TOKEN=changed-secret-value\n")?;
    let provider = GitReadToolProvider::new();
    let read = repo.read_ctx()?;
    let calls = [
        ("git.status", json!({})),
        ("git.diff", json!({})),
        ("git.diff", json!({"base": "HEAD~1"})),
        ("git.show", json!({"rev": "HEAD"})),
        ("git.show", json!({"rev": "HEAD~1"})),
        ("git.log", json!({"paths": [".env"]})),
        ("git.blame", json!({"path": ".env"})),
        ("git.show", json!({"rev": "HEAD:.env"})),
    ];
    for (name, args) in calls {
        let tool = provider
            .executor(&ToolName::new(name))
            .ok_or(TestError::Missing("executor"))?;
        let rendered =
            serde_json::to_string(&run(tool.as_ref(), &read, name, args.clone()).await?)?;
        assert!(
            !rendered.contains("changed-secret-value") && !rendered.contains("TOKEN=abc"),
            "{name} {args}: {rendered}"
        );
    }
    let status = call(&provider, &repo, "git.status", json!({})).await?;
    assert_eq!(status["entries"][0]["path"], ".env");
    let diff = call(&provider, &repo, "git.diff", json!({})).await?;
    assert_eq!(
        diff["files"][0]["omitted"],
        "secret path: contents are never shown"
    );
    Ok(())
}

#[tokio::test]
async fn invalid_arguments_and_missing_repositories_are_clear_errors() -> TestResult {
    let repo = repo()?;
    let provider = GitReadToolProvider::new();
    let read = repo.read_ctx()?;
    let cases = [
        ("git.status", json!({"untracked": "bogus"}), "untracked"),
        ("git.status", json!({"paths": ["../x"]}), "'..'"),
        ("git.status", json!({"paths": ["*.rs"]}), "pattern"),
        ("git.diff", json!({"format": "fancy"}), "format"),
        ("git.diff", json!({"target": "HEAD"}), "requires base"),
        (
            "git.diff",
            json!({"base": "HEAD", "target": "HEAD", "staged": true}),
            "staged",
        ),
        ("git.diff", json!({"base": "nope"}), "unknown revision"),
        ("git.log", json!({"since": "yesterday"}), "since"),
        ("git.log", json!({"rev": "a...b"}), "symmetric"),
        ("git.show", json!({"rev": "HEAD:../etc/passwd"}), "path"),
        ("git.blame", json!({"path": "../x"}), "invalid path"),
        ("git.blame", json!({"path": ".env"}), "secret"),
    ];
    for (name, args, needle) in cases {
        let tool = provider
            .executor(&ToolName::new(name))
            .ok_or(TestError::Missing("executor"))?;
        let message = error_of(run(tool.as_ref(), &read, name, args.clone()).await?)?;
        assert!(message.contains(needle), "{name} {args}: {message}");
    }
    let bare = TestRepo::new_empty()?;
    let read = bare.read_ctx()?;
    let tool = provider
        .executor(&ToolName::new("git.status"))
        .ok_or(TestError::Missing("executor"))?;
    assert!(
        error_of(run(tool.as_ref(), &read, "git.status", json!({})).await?)?
            .contains("not a git repository")
    );
    Ok(())
}

#[tokio::test]
async fn hostile_arguments_never_panic_or_exceed_the_output_budget() -> TestResult {
    let repo = repo()?;
    repo.write("deep/a/b/c/d/e/f.txt", b"1")?;
    let provider = GitReadToolProvider::new();
    let read = repo.read_ctx()?;
    let strings = [
        json!(""),
        json!("\u{0}"),
        json!("../".repeat(500)),
        json!("/".repeat(5000)),
        json!("é".repeat(3000)),
        json!("[[[[["),
        json!("*".repeat(600)),
        json!("."),
        json!("HEAD~99999999999999999999"),
        json!("HEAD^{"),
        json!("HEAD:"),
        json!("@{1}"),
        json!(".git/config"),
    ];
    let numbers = [
        json!(0),
        json!(1),
        json!(u64::MAX),
        json!(-1),
        json!("18446744073709551616"),
        json!(1.5),
        json!("x"),
    ];
    let mut calls = 0usize;
    for name in GIT_TOOL_NAMES {
        let tool = provider
            .executor(&ToolName::new(*name))
            .ok_or(TestError::Missing("executor"))?;
        for text in &strings {
            for number in &numbers {
                let attempts = [
                    json!({"rev": text, "base": text, "target": text, "path": text, "paths": [text], "limit": number, "max_count": number, "context": number, "max_files": number, "max_lines": number, "start_line": number, "end_line": number, "skip": number}),
                    json!({"path": text}),
                    json!({"pattern": text, "author": text, "grep": text, "since": text, "until": text, "format": text, "untracked": text}),
                ];
                for arguments in attempts {
                    calls += 1;
                    match run(tool.as_ref(), &read, name, arguments.clone()).await {
                        Ok(output) => {
                            let rendered = serde_json::to_string(&output)?;
                            assert!(
                                !rendered.contains("internal error"),
                                "{name} panicked on {arguments}"
                            );
                            assert!(
                                rendered.len() <= 70_000,
                                "{name}: {} bytes on {arguments}",
                                rendered.len()
                            );
                        }
                        Err(error) => assert!(!error.to_string().is_empty()),
                    }
                }
            }
        }
    }
    assert!(calls > 800);
    Ok(())
}

#[test]
fn sources_never_start_a_process() -> TestResult {
    let needles = [
        ["std::proc", "ess::Command"].concat(),
        ["tokio::proc", "ess"].concat(),
        ["Command::", "new("].concat(),
        [".spa", "wn("].concat(),
        ["libc::", "system"].concat(),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?.replace("spawn_blocking(", "");
        for needle in &needles {
            assert!(
                !text.contains(needle.as_str()),
                "{} contains {needle}",
                path.display()
            );
        }
    }
    Ok(())
}
