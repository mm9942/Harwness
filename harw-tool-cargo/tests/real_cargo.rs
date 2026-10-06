//! Ende-zu-Ende-Prüfung gegen das **echte** `cargo` (nur Tests).
//!
//! `LocalShell` ersetzt hier den `shell.exec`-Ausführer des Harness: es
//! führt das von den Werkzeugen gebaute Kommando mit `/bin/sh -c` im
//! Workspace aus und liefert die Antwort in der Form von `shell.exec`
//! (inkl. 64-KiB-Kappung). Geprüft wird damit, dass die erzeugten Kommandos
//! von echtem Cargo akzeptiert werden und dass die Parser die echte Ausgabe
//! (Kurzformat, libtest, rustfmt) verstehen. **Nicht** geprüft wird hier der
//! bwrap-Sandbox-Weg von `shell.exec` selbst.
//!
//! Ohne `cargo`/`clippy`/`rustfmt` im `PATH` werden die betroffenen Tests
//! mit `skipped: …` auf stderr übersprungen.

use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_extension_api::contributors::ToolProvider;
use harw_tool_cargo::{CargoToolProvider, ShellDelegate};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O error",
    Json(serde_json::Error) => "JSON error",
    Tools(harw_tools::ToolsError) => "tool error"
);

/// Ersatz für `shell.exec`: echtes `/bin/sh -c` im Workspace.
struct LocalShell {
    root: PathBuf,
    commands: std::sync::Mutex<Vec<String>>,
}

const SHELL_OUTPUT_LIMIT: usize = 64 * 1024;

fn clip(bytes: &[u8], limit: usize) -> (String, bool) {
    if bytes.len() <= limit {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let mut end = limit;
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    (String::from_utf8_lossy(&bytes[..end]).into_owned(), true)
}

impl ToolExecutor for LocalShell {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let command = call
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            self.commands
                .lock()
                .map(|mut c| c.push(command.clone()))
                .ok();
            let output = Command::new("/bin/sh")
                .arg("-c")
                .arg(&command)
                .current_dir(&self.root)
                .env_remove("CARGO_TARGET_DIR")
                .env_remove("RUSTC_WRAPPER")
                .output();
            Ok(match output {
                Err(error) => ToolOutput::error(format!("spawn failed: {error}")),
                Ok(out) => {
                    let stdout_limit = out.stdout.len().min(SHELL_OUTPUT_LIMIT);
                    let (stdout, cut_out) = clip(&out.stdout, stdout_limit);
                    let (stderr, cut_err) = clip(&out.stderr, SHELL_OUTPUT_LIMIT - stdout_limit);
                    ToolOutput::json(json!({
                        "exit_code": out.status.code().unwrap_or(-1),
                        "stdout": stdout,
                        "stderr": stderr,
                        "truncated": cut_out || cut_err,
                        "killed_by_output_limit": false,
                    }))
                }
            })
        })
    }
}

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    ctx: ToolExecutionContext,
    shell: Arc<LocalShell>,
    provider: CargoToolProvider,
}

fn have(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn write(root: &Path, relative: &str, text: &str) -> TestResult {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

impl Project {
    fn new() -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let base = dir.path().canonicalize()?;
        let root = base.join("ws");
        write(
            &root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"demo\", \"util\"]\nresolver = \"2\"\n",
        )?;
        write(
            &root,
            "demo/Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        write(
            &root,
            "demo/src/lib.rs",
            "/// Adds.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn passes() {\n        println!(\"visible-output\");\n        assert_eq!(super::add(1, 2), 3);\n    }\n\n    #[test]\n    fn slow_one() {\n        assert_eq!(super::add(2, 2), 4);\n    }\n}\n",
        )?;
        write(
            &root,
            "util/Cargo.toml",
            "[package]\nname = \"util\"\nversion = \"0.2.0\"\nedition = \"2021\"\n\n[features]\nfancy = []\n\n[[bin]]\nname = \"util-cli\"\npath = \"src/main.rs\"\n",
        )?;
        write(
            &root,
            "util/src/lib.rs",
            "pub fn id(x: i32) -> i32 {\n    x\n}\n",
        )?;
        write(
            &root,
            "util/src/main.rs",
            "fn main() {\n    println!(\"{}\", util::id(1));\n}\n",
        )?;
        let registry = WorkspaceRegistry::build(
            &base,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(vec![Permission::ExecuteProcess]),
        );
        let ctx = ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox);
        let shell = Arc::new(LocalShell {
            root: root.clone(),
            commands: std::sync::Mutex::new(Vec::new()),
        });
        let provider = CargoToolProvider::new(ShellDelegate::new(shell.clone()));
        Ok(Self {
            _dir: dir,
            root,
            ctx,
            shell,
            provider,
        })
    }

    async fn call(&self, tool: &str, arguments: Value) -> TestResult<Value> {
        let executor = self
            .provider
            .executor(&ToolName::new(tool))
            .ok_or(TestError::Missing("executor"))?;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(tool),
            arguments,
        };
        match executor.execute(&self.ctx, &call).await? {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("{tool}: {other:?}"))),
        }
    }

    fn last_command(&self) -> String {
        self.shell
            .commands
            .lock()
            .ok()
            .and_then(|c| c.last().cloned())
            .unwrap_or_default()
    }
}

fn skip_without_cargo() -> bool {
    if have("cargo", &["--version"]) {
        return false;
    }
    eprintln!("skipped: cargo is not available");
    true
}

#[tokio::test]
async fn check_scopes_packages_and_reports_errors_with_positions() -> TestResult {
    if skip_without_cargo() {
        return Ok(());
    }
    let p = Project::new()?;
    let clean = p.call("cargo.check", json!({"workspace": true})).await?;
    assert_eq!(clean["status"], "ok", "{clean}");
    assert_eq!(clean["errors"], 0);
    assert_eq!(clean["profile"], "dev");
    assert!(clean["cargo_took"].is_string());
    write(
        &p.root,
        "util/src/lib.rs",
        "pub fn id(x: i32) -> i32 {\n    let unused = 1;\n    \"oops\"\n}\n",
    )?;
    let broken = p.call("cargo.check", json!({"package": ["util"]})).await?;
    assert_eq!(broken["status"], "failed", "{broken}");
    assert!(broken["errors"].as_u64().unwrap_or(0) >= 1);
    let first = &broken["diagnostics"][0];
    assert_eq!(first["level"], "error");
    assert_eq!(first["code"], "E0308");
    assert_eq!(first["file"], "util/src/lib.rs");
    assert_eq!(first["line"], 3);
    // Lint-Warnungen gibt es bei Typfehlern nicht (rustc bricht vorher ab): nur der Fehler.
    assert_eq!(broken["warnings"], 0, "{broken}");
    assert_ne!(broken["exit_code"], 0);
    // Das andere Paket ist von dem Fehler nicht betroffen.
    let other = p
        .call(
            "cargo.check",
            json!({"package": ["demo"], "locked": true, "offline": true}),
        )
        .await?;
    assert_eq!(other["status"], "ok", "{other}");
    assert!(
        p.last_command()
            .contains("-p demo --locked --offline --message-format=short --color never"),
        "{}",
        p.last_command()
    );
    let excluded = p
        .call(
            "cargo.check",
            json!({"workspace": true, "exclude": ["util"]}),
        )
        .await?;
    assert_eq!(excluded["status"], "ok", "{excluded}");
    Ok(())
}

#[tokio::test]
async fn build_release_and_bin_selection() -> TestResult {
    if skip_without_cargo() {
        return Ok(());
    }
    let p = Project::new()?;
    let built = p
        .call(
            "cargo.build",
            json!({"package": ["util"], "bin": ["util-cli"], "release": true, "jobs": 2}),
        )
        .await?;
    assert_eq!(built["status"], "ok", "{built}");
    assert_eq!(built["profile"], "release");
    assert!(p.root.join("target/release/util-cli").exists());
    let features = p
        .call(
            "cargo.build",
            json!({"package": ["util"], "features": ["fancy"], "lib": true}),
        )
        .await?;
    assert_eq!(features["status"], "ok", "{features}");
    let unknown = p
        .call(
            "cargo.build",
            json!({"package": ["util"], "features": ["nope"]}),
        )
        .await?;
    assert_eq!(unknown["status"], "failed");
    assert!(
        unknown["diagnostics"]
            .as_array()
            .is_some_and(|d| !d.is_empty()),
        "{unknown}"
    );
    Ok(())
}

#[tokio::test]
async fn test_counts_failures_filters_and_allowlisted_harness_args() -> TestResult {
    if skip_without_cargo() {
        return Ok(());
    }
    let p = Project::new()?;
    let all = p
        .call("cargo.test", json!({"package": ["demo"], "lib": true}))
        .await?;
    assert_eq!(all["status"], "ok", "{all}");
    assert_eq!(all["tests"]["passed"], 2);
    assert_eq!(all["tests"]["failed"], 0);
    assert_eq!(all["tests"]["binaries"], 1);
    let filtered = p
        .call(
            "cargo.test",
            json!({"package": ["demo"], "lib": true, "test_filter": "passes"}),
        )
        .await?;
    assert_eq!(filtered["tests"]["passed"], 1);
    assert_eq!(filtered["tests"]["filtered_out"], 1);
    let loud = p.call("cargo.test", json!({"package": ["demo"], "lib": true, "harness_args": ["--nocapture", "--test-threads=1"]})).await?;
    assert!(
        loud["log"]["stdout_tail"]
            .as_str()
            .is_some_and(|t| t.contains("visible-output")),
        "{loud}"
    );
    let skipped = p
        .call(
            "cargo.test",
            json!({"package": ["demo"], "lib": true, "harness_args": ["--skip=slow_one"]}),
        )
        .await?;
    assert_eq!(skipped["tests"]["passed"], 1);
    assert_eq!(skipped["tests"]["filtered_out"], 1);
    let no_run = p
        .call("cargo.test", json!({"package": ["demo"], "no_run": true}))
        .await?;
    assert_eq!(no_run["status"], "ok", "{no_run}");
    assert_eq!(no_run["tests"]["ran"], false);

    // Ein fehlschlagender Test: Status, Zähler, Ort und Meldung.
    write(
        &p.root,
        "demo/tests/it.rs",
        "#[test]\nfn broken() {\n    assert_eq!(1 + 1, 3, \"math is hard\");\n}\n",
    )?;
    let failing = p
        .call(
            "cargo.test",
            json!({"package": ["demo"], "tests": true, "no_fail_fast": true}),
        )
        .await?;
    assert_eq!(failing["status"], "tests_failed", "{failing}");
    assert_eq!(failing["tests"]["failed"], 1);
    assert_eq!(failing["tests"]["failed_tests"], json!(["broken"]));
    assert_eq!(
        failing["tests"]["failures"][0]["location"],
        "demo/tests/it.rs:3:5"
    );
    assert!(
        failing["tests"]["failures"][0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("math is hard"))
    );
    assert!(failing["log"]["stdout_tail"].is_string());

    // Ein Kompilierfehler im Test ist ein „failed“, kein „tests_failed“.
    write(
        &p.root,
        "demo/tests/it.rs",
        "#[test]\nfn broken() {\n    let x: i32 = \"s\";\n}\n",
    )?;
    let compile = p
        .call("cargo.test", json!({"package": ["demo"], "tests": true}))
        .await?;
    assert_eq!(compile["status"], "failed", "{compile}");
    assert_eq!(compile["diagnostics"][0]["file"], "demo/tests/it.rs");
    Ok(())
}

#[tokio::test]
async fn doc_tests_run_with_doc_flag() -> TestResult {
    if skip_without_cargo() {
        return Ok(());
    }
    let p = Project::new()?;
    write(
        &p.root,
        "demo/src/lib.rs",
        "/// ```\n/// assert_eq!(demo::add(1, 1), 2);\n/// ```\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )?;
    let value = p
        .call("cargo.test", json!({"package": ["demo"], "doc": true}))
        .await?;
    assert_eq!(value["status"], "ok", "{value}");
    assert_eq!(value["tests"]["passed"], 1);
    Ok(())
}

#[tokio::test]
async fn clippy_reports_lints_and_typed_levels() -> TestResult {
    if skip_without_cargo() || !have("cargo", &["clippy", "--version"]) {
        eprintln!("skipped: clippy");
        return Ok(());
    }
    let p = Project::new()?;
    write(
        &p.root,
        "util/src/lib.rs",
        "pub fn id(x: i32) -> i32 {\n    return x;\n}\n",
    )?;
    let warned = p
        .call(
            "cargo.clippy",
            json!({"package": ["util"], "no_deps": true}),
        )
        .await?;
    assert_eq!(warned["status"], "ok", "{warned}");
    assert_eq!(warned["warnings"], 1);
    assert_eq!(warned["diagnostics"][0]["file"], "util/src/lib.rs");
    assert_eq!(warned["diagnostics"][0]["line"], 2);
    let denied = p
        .call(
            "cargo.clippy",
            json!({"package": ["util"], "no_deps": true, "deny": ["clippy::needless_return"]}),
        )
        .await?;
    assert_eq!(denied["status"], "failed", "{denied}");
    assert!(denied["errors"].as_u64().unwrap_or(0) >= 1);
    assert!(
        p.last_command().ends_with("-- -D clippy::needless_return"),
        "{}",
        p.last_command()
    );
    let allowed = p
        .call(
            "cargo.clippy",
            json!({"package": ["util"], "no_deps": true, "allow": ["clippy::needless_return"]}),
        )
        .await?;
    assert_eq!(allowed["warnings"], 0, "{allowed}");
    Ok(())
}

#[tokio::test]
async fn fmt_check_finds_hunks_and_reports_clean_after_fixing() -> TestResult {
    if skip_without_cargo() || !have("cargo", &["fmt", "--version"]) {
        eprintln!("skipped: rustfmt");
        return Ok(());
    }
    let p = Project::new()?;
    write(&p.root, "util/src/lib.rs", "pub fn   id( x:i32 )->i32{x}\n")?;
    let dirty = p.call("cargo.fmt_check", json!({"all": true})).await?;
    assert_eq!(dirty["status"], "needs_formatting", "{dirty}");
    assert_eq!(dirty["listed"][0]["file"], "util/src/lib.rs");
    assert!(dirty["diffs"].as_u64().unwrap_or(0) >= 1);
    // Es wurde nichts geschrieben.
    assert_eq!(
        std::fs::read_to_string(p.root.join("util/src/lib.rs"))?,
        "pub fn   id( x:i32 )->i32{x}\n"
    );
    let scoped = p
        .call("cargo.fmt_check", json!({"package": ["demo"]}))
        .await?;
    assert_eq!(scoped["status"], "ok", "{scoped}");
    let fixed = Command::new("cargo")
        .arg("fmt")
        .arg("--all")
        .current_dir(&p.root)
        .output()?;
    assert!(fixed.status.success());
    let clean = p.call("cargo.fmt_check", json!({"all": true})).await?;
    assert_eq!(clean["status"], "ok", "{clean}");
    Ok(())
}

#[tokio::test]
async fn metadata_summarizes_the_workspace_without_absolute_paths() -> TestResult {
    if skip_without_cargo() {
        return Ok(());
    }
    let p = Project::new()?;
    let value = p.call("cargo.metadata", json!({"no_deps": true})).await?;
    assert_eq!(value["status"], "ok", "{value}");
    assert_eq!(value["workspace_member_count"], 2);
    let names: Vec<&str> = value["workspace_members"]
        .as_array()
        .map(|m| m.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(names, vec!["demo", "util"]);
    let util = &value["workspace_members"][1];
    assert_eq!(util["version"], "0.2.0");
    assert_eq!(util["manifest"], "util/Cargo.toml");
    assert_eq!(util["features"], json!(["fancy"]));
    assert!(
        util["targets"]
            .as_array()
            .is_some_and(|t| t.iter().any(|x| x["name"] == "util-cli"))
    );
    assert_eq!(value["external_package_count"], 0);
    let rendered = serde_json::to_string(&value)?;
    assert!(
        !rendered.contains(&p.root.to_string_lossy().to_string()),
        "absolute path leaked: {rendered}"
    );
    let leftovers = std::fs::read_dir(p.root.join("target/harw-tool-cargo"))?.count();
    assert_eq!(leftovers, 0, "scratch file must be removed");
    // Eine kaputte Manifestdatei ergibt einen Fehlerbericht, kein Parser-Chaos.
    write(&p.root, "util/Cargo.toml", "[package\nname = ")?;
    let broken = p.call("cargo.metadata", json!({"no_deps": true})).await?;
    assert_eq!(broken["status"], "failed", "{broken}");
    assert!(
        broken["diagnostics"]
            .as_array()
            .is_some_and(|d| !d.is_empty())
    );
    Ok(())
}

#[tokio::test]
async fn tree_shows_packages_and_honours_depth_and_edges() -> TestResult {
    if skip_without_cargo() {
        return Ok(());
    }
    let p = Project::new()?;
    write(
        &p.root,
        "util/Cargo.toml",
        "[package]\nname = \"util\"\nversion = \"0.2.0\"\nedition = \"2021\"\n\n[features]\nfancy = []\n\n[dependencies]\ndemo = { path = \"../demo\" }\n",
    )?;
    let tree = p.call("cargo.tree", json!({"package": ["util"]})).await?;
    assert_eq!(tree["status"], "ok", "{tree}");
    let text = tree["tree"].as_str().unwrap_or("");
    assert!(
        text.contains("util v0.2.0") && text.contains("demo v0.1.0"),
        "{text}"
    );
    let flat = p
        .call("cargo.tree", json!({"package": ["util"], "depth": 0}))
        .await?;
    assert_eq!(flat["lines"], 1);
    let inverted = p
        .call(
            "cargo.tree",
            json!({"invert": "demo", "workspace": true, "edges": ["normal"], "prefix": "none"}),
        )
        .await?;
    assert!(
        inverted["tree"]
            .as_str()
            .is_some_and(|t| t.starts_with("demo v0.1.0")),
        "{inverted}"
    );
    let missing = p
        .call("cargo.tree", json!({"package": ["does-not-exist"]}))
        .await?;
    assert_eq!(missing["status"], "failed");
    // `--locked` scheitert, wenn Cargo.lock aktualisiert werden müsste.
    write(
        &p.root,
        "demo/Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.1\"\nedition = \"2021\"\n",
    )?;
    let locked = p
        .call("cargo.check", json!({"workspace": true, "locked": true}))
        .await?;
    assert_eq!(locked["status"], "failed", "{locked}");
    Ok(())
}

#[tokio::test]
async fn doc_reports_generated_path_and_warnings() -> TestResult {
    if skip_without_cargo() {
        return Ok(());
    }
    let p = Project::new()?;
    write(
        &p.root,
        "demo/src/lib.rs",
        "/// See [`missing_item`].\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )?;
    let value = p
        .call(
            "cargo.doc",
            json!({"package": ["demo"], "no_deps": true, "document_private_items": true}),
        )
        .await?;
    assert_eq!(value["status"], "ok", "{value}");
    assert!(
        value["generated"]
            .as_array()
            .is_some_and(|g| g.iter().any(|x| x
                .as_str()
                .is_some_and(|s| s.ends_with("target/doc/demo/index.html")))),
        "{value}"
    );
    assert!(
        value["warnings"].as_u64().unwrap_or(0) >= 1,
        "unresolved link must warn: {value}"
    );
    assert!(p.root.join("target/doc/demo/index.html").exists());
    Ok(())
}

#[tokio::test]
async fn huge_output_is_bounded_even_through_the_64k_shell_cap() -> TestResult {
    if skip_without_cargo() {
        return Ok(());
    }
    let p = Project::new()?;
    // 400 Warnungen erzeugen weit mehr als 64 KiB Rohausgabe.
    let body: String = (0..400)
        .map(|i| format!("pub fn f{i}() {{\n    let unused_{i} = {i};\n}}\n"))
        .collect();
    write(&p.root, "demo/src/lib.rs", &body)?;
    let value = p
        .call(
            "cargo.check",
            json!({"package": ["demo"], "max_diagnostics": 500}),
        )
        .await?;
    assert_eq!(value["status"], "ok", "{}", value["status"]);
    assert!(
        value["warnings"].as_u64().unwrap_or(0) >= 100,
        "{}",
        value["warnings"]
    );
    let rendered = serde_json::to_string(&value)?;
    assert!(rendered.len() <= 64 * 1024, "{} bytes", rendered.len());
    Ok(())
}
