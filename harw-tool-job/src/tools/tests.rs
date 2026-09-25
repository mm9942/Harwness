//! Tests der Modell-Werkzeuge: Argumente, Rechteweg, Besitz, Ablauf.

use super::*;
use crate::launcher::{DirectLauncher, ShellJobLauncher};
use crate::manager::Caller;
use crate::model::JobState;
use crate::test_support::{Env, TestError, TestResult, context, ctx, eventually, sandbox};
use harw_tool_shell::ShellToolProvider;
use harw_types::ToolCallId;
use std::fs;

fn call(tool: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(tool),
        arguments,
    }
}

fn provider(env: &Env, lineage: Arc<dyn JobLineage>) -> JobToolProvider {
    job_tools(Arc::clone(&env.manager), Arc::new(DirectLauncher), lineage)
}

async fn run(
    provider: &JobToolProvider,
    context: &ToolExecutionContext,
    tool: &str,
    arguments: Value,
) -> Result<ToolOutput, ToolsError> {
    let executor = provider
        .executor(&ToolName::new(tool))
        .ok_or_else(|| ToolsError::NotFound {
            name: tool.to_owned(),
        })?;
    executor.execute(context, &call(tool, arguments)).await
}

fn json_of(output: ToolOutput) -> TestResult<Value> {
    match output {
        ToolOutput::Json { content } => Ok(content),
        other => Err(TestError::Unexpected(format!(
            "expected JSON, got {other:?}"
        ))),
    }
}

fn error_of(output: ToolOutput) -> TestResult<String> {
    match output {
        ToolOutput::Error { message } => Ok(message),
        other => Err(TestError::Unexpected(format!(
            "expected error, got {other:?}"
        ))),
    }
}

fn start_args(command: &str, name: &str) -> Value {
    json!({
        "command": command,
        "argv": null,
        "cwd": null,
        "name": name,
        "env": null,
        "notify_every_secs": null,
    })
}

#[test]
fn test_tool_specs_and_parallel_safety() -> TestResult {
    let env = Env::new()?;
    let provider = provider(&env, Arc::new(NoLineage));
    let names: Vec<String> = provider
        .tools()
        .iter()
        .map(|spec| spec.name().to_owned())
        .collect();
    assert_eq!(names, JOB_TOOL_NAMES.map(str::to_owned).to_vec());
    for name in JOB_TOOL_NAMES {
        assert!(provider.executor(&ToolName::new(name)).is_some());
    }
    assert!(provider.executor(&ToolName::new("job.other")).is_none());
    assert!(!provider.parallel_safe(&ToolName::new(JOB_START_TOOL)));
    assert!(!provider.parallel_safe(&ToolName::new(JOB_STOP_TOOL)));
    assert!(provider.parallel_safe(&ToolName::new(JOB_STATUS_TOOL)));
    assert!(provider.parallel_safe(&ToolName::new(JOB_WAIT_TOOL)));
    Ok(())
}

/// Plan-Modus/lesendes Profil: kein `ExecuteProcess` → `job.start` lehnt ab,
/// bevor irgendetwas startet — über den echten `shell.exec`-Weg.
#[tokio::test]
async fn test_job_start_refused_without_execute_process() -> TestResult {
    let env = Env::new()?;
    let shell_launcher = Arc::new(ShellJobLauncher::new(ShellToolProvider::new()));
    let provider = JobToolProvider::new(Arc::clone(&env.manager), shell_launcher);
    let read_only = context(
        "planner",
        sandbox(&env.dir.path().join("ws"), vec![Permission::ReadWorkspace])?,
    );
    let output = run(
        &provider,
        &read_only,
        JOB_START_TOOL,
        start_args("echo hi", "x"),
    )
    .await
    .map_err(ctx("job.start"))?;
    let message = error_of(output)?;
    assert!(message.contains("ExecuteProcess"), "{message}");
    assert!(env.manager.list(Caller::Operator).is_empty());
    Ok(())
}

/// Der produktive Startweg selbst (`ShellToolProvider::prepare_background_launch`)
/// lehnt ohne `ExecuteProcess` und bei sudo ab — wie `shell.exec`.
#[tokio::test]
async fn test_shell_launcher_applies_shell_exec_rules() -> TestResult {
    let env = Env::new()?;
    let launcher = ShellJobLauncher::new(ShellToolProvider::new());
    let read_only = context(
        "planner",
        sandbox(&env.dir.path().join("ws"), vec![Permission::ReadWorkspace])?,
    );
    let denied = launcher
        .prepare(&read_only, "echo hi", 60)
        .await
        .err()
        .ok_or(TestError::Missing("refusal without ExecuteProcess"))?;
    assert!(error_of(denied)?.contains("ExecuteProcess"));

    let exec = env.exec_context("worker")?;
    let denied = launcher
        .prepare(&exec, "sudo apt-get install foo", 60)
        .await
        .err()
        .ok_or(TestError::Missing("refusal for sudo"))?;
    assert!(error_of(denied)?.contains("host.sudo_exec"));

    let blank = launcher
        .prepare(&exec, "   ", 60)
        .await
        .err()
        .ok_or(TestError::Missing("refusal for blank command"))?;
    assert!(error_of(blank)?.contains("blank"));
    Ok(())
}

#[tokio::test]
async fn test_tool_flow_start_status_logs_wait_list_stop() -> TestResult {
    let env = Env::new()?;
    let lineage: Arc<dyn JobLineage> = Arc::new(FnLineage(|session: &SessionId| {
        if session.as_str() == "worker" {
            vec!["orchestrator".to_owned()]
        } else {
            Vec::new()
        }
    }));
    let provider = provider(&env, lineage);
    let worker = env.exec_context("worker")?;
    let orchestrator = env.exec_context("orchestrator")?;
    let stranger = env.exec_context("stranger")?;

    let mut args = start_args("echo \"$GREETING\"; echo second; sleep 30", "greeter");
    args["env"] = json!(["GREETING=hi there"]);
    args["notify_every_secs"] = json!(0);
    let started = json_of(
        run(&provider, &worker, JOB_START_TOOL, args)
            .await
            .map_err(ctx("start"))?,
    )?;
    let job_id = started["job_id"]
        .as_str()
        .ok_or(TestError::Missing("job_id"))?
        .to_owned();
    assert_eq!(started["state"], json!("running"));
    assert_eq!(started["notify_every_secs"], json!(0));
    let id = JobId::parse(&job_id).ok_or(TestError::Missing("valid job id"))?;

    let seen = eventually(Duration::from_secs(10), || {
        env.manager
            .status(&id, Caller::Agent("worker"))
            .is_ok_and(|status| status.stdout_lines >= 2)
    })
    .await;
    assert!(seen);

    let status = json_of(
        run(
            &provider,
            &orchestrator,
            JOB_STATUS_TOOL,
            json!({ "job_id": job_id }),
        )
        .await
        .map_err(ctx("status"))?,
    )?;
    assert_eq!(status["state"], json!("running"));

    let logs = json_of(
        run(
            &provider,
            &worker,
            JOB_LOGS_TOOL,
            json!({ "job_id": job_id, "stream": "stdout", "tail": null, "since_line": 2, "grep": null }),
        )
        .await
        .map_err(ctx("logs"))?,
    )?;
    assert_eq!(logs["stdout"]["lines"], json!(["2: second"]));
    assert!(logs.get("stderr").is_none());

    let logs = json_of(
        run(
            &provider,
            &worker,
            JOB_LOGS_TOOL,
            json!({ "job_id": job_id, "grep": "hi" }),
        )
        .await
        .map_err(ctx("logs grep"))?,
    )?;
    assert_eq!(logs["stdout"]["lines"], json!(["1: hi there"]));
    assert_eq!(logs["stderr"]["total_lines"], json!(0));

    let waited = json_of(
        run(
            &provider,
            &worker,
            JOB_WAIT_TOOL,
            json!({ "job_id": job_id, "timeout_secs": 1 }),
        )
        .await
        .map_err(ctx("wait"))?,
    )?;
    assert_eq!(waited["outcome"], json!("timeout"));

    let listed = json_of(
        run(&provider, &orchestrator, JOB_LIST_TOOL, json!({}))
            .await
            .map_err(ctx("list"))?,
    )?;
    assert_eq!(listed["count"], json!(1));

    // Fremde Sitzung: dieselbe Meldung wie für unbekannte Jobs.
    let foreign = error_of(
        run(
            &provider,
            &stranger,
            JOB_STATUS_TOOL,
            json!({ "job_id": job_id }),
        )
        .await
        .map_err(ctx("foreign status"))?,
    )?;
    assert!(foreign.contains("unknown job"), "{foreign}");
    let listed = json_of(
        run(&provider, &stranger, JOB_LIST_TOOL, json!({}))
            .await
            .map_err(ctx("foreign list"))?,
    )?;
    assert_eq!(listed["count"], json!(0));
    let foreign_stop = error_of(
        run(
            &provider,
            &stranger,
            JOB_STOP_TOOL,
            json!({ "job_id": job_id, "signal": "KILL" }),
        )
        .await
        .map_err(ctx("foreign stop"))?,
    )?;
    assert!(foreign_stop.contains("unknown job"));

    let stopped = json_of(
        run(
            &provider,
            &orchestrator,
            JOB_STOP_TOOL,
            json!({ "job_id": job_id, "signal": null }),
        )
        .await
        .map_err(ctx("stop"))?,
    )?;
    assert_eq!(stopped["state"], json!("stopped"));
    assert_eq!(stopped["signal_sent"], json!("SIGTERM"));

    let meta = env
        .manager
        .status(&id, Caller::Operator)
        .map_err(ctx("operator status"))?
        .meta;
    assert_eq!(meta.state, JobState::Stopped);
    assert_eq!(meta.env_keys, vec!["GREETING".to_owned()]);
    assert_eq!(meta.owner.ancestors, vec!["orchestrator".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn test_job_start_with_argv_and_cwd() -> TestResult {
    let env = Env::new()?;
    let provider = provider(&env, Arc::new(NoLineage));
    let worker = env.exec_context("worker")?;
    let root = worker.sandbox().workspace().canonical_root().to_path_buf();
    fs::create_dir_all(root.join("sub dir")).map_err(ctx("create sub dir"))?;

    let started = json_of(
        run(
            &provider,
            &worker,
            JOB_START_TOOL,
            json!({
                "command": null,
                "argv": ["sh", "-c", "pwd; exit 7"],
                "cwd": "sub dir",
                "name": "where",
                "env": null,
                "notify_every_secs": "30",
            }),
        )
        .await
        .map_err(ctx("start"))?,
    )?;
    let job_id = started["job_id"]
        .as_str()
        .ok_or(TestError::Missing("job_id"))?
        .to_owned();
    assert_eq!(started["command"], json!("sh -c 'pwd; exit 7'"));

    let waited = json_of(
        run(
            &provider,
            &worker,
            JOB_WAIT_TOOL,
            json!({ "job_id": job_id, "timeout_secs": 10 }),
        )
        .await
        .map_err(ctx("wait"))?,
    )?;
    assert_eq!(waited["outcome"], json!("finished"));
    assert_eq!(waited["status"]["state"], json!("failed"));
    assert_eq!(waited["status"]["exit_code"], json!(7));

    let logs = json_of(
        run(
            &provider,
            &worker,
            JOB_LOGS_TOOL,
            json!({ "job_id": job_id, "stream": "stdout" }),
        )
        .await
        .map_err(ctx("logs"))?,
    )?;
    let expected = format!("1: {}", root.join("sub dir").display());
    assert_eq!(logs["stdout"]["lines"], json!([expected]));
    Ok(())
}

#[tokio::test]
async fn test_invalid_arguments_are_rejected() -> TestResult {
    let env = Env::new()?;
    let provider = provider(&env, Arc::new(NoLineage));
    let worker = env.exec_context("worker")?;

    let cases = vec![
        (JOB_START_TOOL, json!({ "name": "x" })),
        (
            JOB_START_TOOL,
            json!({ "command": "echo", "argv": ["echo"], "name": "x" }),
        ),
        (
            JOB_START_TOOL,
            json!({ "command": "echo", "name": "x", "bogus": 1 }),
        ),
        (JOB_START_TOOL, json!({ "command": "echo", "name": "  " })),
        (
            JOB_START_TOOL,
            json!({ "command": "echo", "name": "x", "cwd": "../.." }),
        ),
        (
            JOB_START_TOOL,
            json!({ "command": "echo", "name": "x", "env": ["1BAD=x"] }),
        ),
        (
            JOB_START_TOOL,
            json!({ "command": "echo", "name": "x", "env": ["NOEQUALS"] }),
        ),
        (JOB_STATUS_TOOL, json!({ "job_id": "../../etc" })),
        (
            JOB_LOGS_TOOL,
            json!({ "job_id": "job-1", "stream": "both-ish" }),
        ),
        (
            JOB_STOP_TOOL,
            json!({ "job_id": "job-1", "signal": "STOP" }),
        ),
        (JOB_LIST_TOOL, json!({ "all": true })),
        (
            JOB_WAIT_TOOL,
            json!({ "job_id": "job-1", "timeout_secs": 0 }),
        ),
        (JOB_WAIT_TOOL, json!({ "job_id": "job-1" })),
    ];
    for (tool, arguments) in cases {
        let result = run(&provider, &worker, tool, arguments.clone()).await;
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "{tool} {arguments}: {result:?}"
        );
    }
    assert!(env.manager.list(Caller::Operator).is_empty());

    // Unbekannte, aber gültige Kennung: Tool-Fehler, kein Argumentfehler.
    let unknown = error_of(
        run(
            &provider,
            &worker,
            JOB_STATUS_TOOL,
            json!({ "job_id": "job-404" }),
        )
        .await
        .map_err(ctx("unknown status"))?,
    )?;
    assert!(unknown.contains("unknown job"));
    Ok(())
}

#[test]
fn test_compose_quotes_env_and_cwd() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let root = dir.path().canonicalize().map_err(ctx("canonical root"))?;
    fs::create_dir_all(root.join("a")).map_err(ctx("mkdir"))?;
    let args = StartArgs {
        command: None,
        argv: Some(vec!["echo".into(), "it's".into(), "$HOME".into()]),
        cwd: Some("a".into()),
        name: " build ".into(),
        env: Some(vec!["X=1 2".into(), "Y=".into()]),
        notify_every_secs: None,
    };
    let composed = compose(&args, &root).map_err(TestError::Unexpected)?;
    assert_eq!(composed.name, "build");
    assert_eq!(composed.display, "echo 'it'\\''s' '$HOME'");
    let cwd = shell_quote(&root.join("a").display().to_string());
    assert_eq!(
        composed.shell,
        format!("export X='1 2' Y='' && cd {cwd} && echo 'it'\\''s' '$HOME'")
    );
    assert_eq!(composed.env_keys, vec!["X".to_owned(), "Y".to_owned()]);
    assert_eq!(composed.cwd, Some(root.join("a")));
    Ok(())
}

#[test]
fn test_shell_quote_and_env_names() {
    assert_eq!(shell_quote("a-b_c.d/e:f"), "a-b_c.d/e:f");
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("a b"), "'a b'");
    assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    assert!(valid_env_name("RUST_LOG"));
    assert!(valid_env_name("_x1"));
    assert!(!valid_env_name("1X"));
    assert!(!valid_env_name("A-B"));
    assert!(!valid_env_name(""));
}

/// Freigabetext von `job.start` (Plan R9, Teil F): `command` oder gequotetes
/// `argv` — und wortgleich zur Auswertung der Allow-Regeln in
/// `harw-extension-api` (dort dupliziert, weil dieses Crate davon abhängt).
#[test]
fn test_job_start_command_text_matches_allow_rules() {
    use harw_extension_api::allow_rules::{AllowRuleSet, ApprovalRule, RuleDecision, RuleScope};

    let argv = json!({"argv": ["cargo", "test", "it's", "a b"], "command": null, "env": null});
    let text = job_start_command_text(&argv);
    assert_eq!(text.as_deref(), Some(r"cargo test 'it'\''s' 'a b'"));
    assert_eq!(
        job_start_command_text(&start_args("ninja -C build", "b")).as_deref(),
        Some("ninja -C build")
    );
    assert_eq!(
        job_start_command_text(&json!({"command": "x", "argv": ["y"]})),
        None
    );
    assert_eq!(job_start_command_text(&json!({"command": "  "})), None);

    // Eine Regel mit dem vollständigen Text trifft genau diesen Aufruf.
    let rules = AllowRuleSet::new();
    rules.add(ApprovalRule {
        tool: JOB_START_TOOL.to_owned(),
        pattern: text.clone(),
        decision: RuleDecision::Allow,
        scope: RuleScope::Session,
    });
    assert_eq!(
        rules.evaluate(JOB_START_TOOL, &argv),
        Some(RuleDecision::Allow)
    );
}
