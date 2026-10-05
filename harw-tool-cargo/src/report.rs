//! Zusammenfassung der Cargo-Ausgabe zu begrenztem JSON.
//!
//! Aus dem Rohlog (stdout/stderr des `shell.exec`-Laufs) wird:
//! Status, Exit-Code, Dauer, Fehler-/Warnungszahlen, eine Meldungsliste
//! mit `datei:zeile:spalte`, bei Tests die Zähler und Fehlschläge, dazu ein
//! **gekürzter** Rohlog (letzte Bytes, ohne ANSI). Die Gesamtantwort bleibt
//! unter [`harw_tool_fsread::budget::MAX_OUTPUT_BYTES`].

use crate::parse::{Diagnostic, Level, parse_cargo_output, parse_fmt_diffs, parse_test_output};
use crate::shell::ShellRun;
use harw_tool_fsread::budget::{Collector, MAX_TEXT_BYTES, clip_text, ok};
use harw_tools::ToolOutput;
use serde_json::{Value, json};

/// Länge der Log-Ausschnitte bei Erfolg.
pub const TAIL_OK_BYTES: usize = 2 * 1024;

/// Länge der Log-Ausschnitte bei Misserfolg.
pub const TAIL_FAILED_BYTES: usize = 6 * 1024;

/// Höchstlänge einer Meldung in Zeichen.
pub const MESSAGE_CHARS: usize = 300;

/// Entfernt ANSI-Escape-Folgen (`ESC [ … Buchstabe`).
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// Die letzten höchstens `max` Bytes von `text`, an einer Zeichengrenze.
#[must_use]
pub fn tail(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut start = text.len() - max;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), true)
}

fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// Gemeinsame Kopfdaten aller Berichte.
fn base(command: &str, run: &ShellRun, status: &str) -> Value {
    let mut data = json!({
        "command": command,
        "status": status,
        "exit_code": run.exit_code,
        "duration_ms": u64::try_from(run.wall_ms).unwrap_or(u64::MAX),
        "output_truncated": run.truncated,
    });
    if let Some(place) = &run.executed_on {
        data["executed_on"] = json!(place);
    }
    data
}

fn log_value(run: &ShellRun, failed: bool) -> Value {
    let max = if failed {
        TAIL_FAILED_BYTES
    } else {
        TAIL_OK_BYTES
    };
    let (stdout, out_cut) = tail(&strip_ansi(&run.stdout), max);
    let (stderr, err_cut) = tail(&strip_ansi(&run.stderr), max);
    json!({
        "stdout_tail": stdout,
        "stderr_tail": stderr,
        "stdout_bytes": run.stdout.len(),
        "stderr_bytes": run.stderr.len(),
        "cut": out_cut || err_cut,
    })
}

fn diagnostic_json(d: &Diagnostic) -> Value {
    let mut value = json!({
        "level": d.level.as_str(),
        "message": clip_chars(&d.message, MESSAGE_CHARS),
    });
    if let Some(code) = &d.code {
        value["code"] = json!(code);
    }
    if let Some(file) = &d.file {
        value["file"] = json!(clip_chars(file, 200));
    }
    if let Some(line) = d.line {
        value["line"] = json!(line);
    }
    if let Some(column) = d.column {
        value["column"] = json!(column);
    }
    value
}

/// Byte-Budget der Meldungsliste.
pub const DIAGNOSTICS_BYTES: usize = 24 * 1024;

/// Fügt Meldungen, Zähler und Cargo-Hinweise ein; liefert `(errors, warnings)`.
fn add_diagnostics(
    data: &mut Value,
    diagnostics: &[Diagnostic],
    notes: &[String],
    max: usize,
) -> (usize, usize) {
    let errors = diagnostics
        .iter()
        .filter(|d| d.level == Level::Error)
        .count();
    let warnings = diagnostics.len() - errors;
    // Fehler zuerst, dann Warnungen; innerhalb der Gruppe in Ausgabereihenfolge.
    let mut ordered: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.level == Level::Error)
        .collect();
    ordered.extend(diagnostics.iter().filter(|d| d.level == Level::Warning));
    let mut listed = Collector::with_budget(max, DIAGNOSTICS_BYTES);
    for diagnostic in &ordered {
        if !listed.push(diagnostic_json(diagnostic)) {
            break;
        }
    }
    data["errors"] = json!(errors);
    data["warnings"] = json!(warnings);
    data["diagnostics_truncated"] = json!(diagnostics.len() > listed.len());
    data["diagnostics"] = Value::Array(listed.into_items());
    let mut kept = Collector::with_budget(10, 3 * 1024);
    for note in notes {
        if !kept.push(json!(clip_chars(note, MESSAGE_CHARS))) {
            break;
        }
    }
    data["cargo_notes"] = Value::Array(kept.into_items());
    (errors, warnings)
}

/// Bericht für `check`, `build`, `clippy` und `doc`.
#[must_use]
pub fn build_report(
    tool: &str,
    command: &str,
    run: &ShellRun,
    max_diagnostics: usize,
) -> ToolOutput {
    let parsed = parse_cargo_output(&run.stderr);
    let success = run.exit_code == 0;
    let mut data = base(command, run, if success { "ok" } else { "failed" });
    let (errors, warnings) = add_diagnostics(
        &mut data,
        &parsed.diagnostics,
        &parsed.notes,
        max_diagnostics,
    );
    if let Some(finished) = &parsed.finished {
        data["profile"] = json!(finished.profile);
        data["cargo_took"] = json!(finished.took);
    }
    if !parsed.generated.is_empty() {
        data["generated"] = json!(parsed.generated.iter().take(20).collect::<Vec<_>>());
    }
    data["log"] = log_value(run, !success);
    data["truncated"] = json!(data["diagnostics_truncated"] == true || run.truncated);
    let summary = format!(
        "{tool}: {}, {errors} errors, {warnings} warnings in {:.1}s",
        if success { "ok" } else { "failed" },
        run.wall_ms as f64 / 1000.0
    );
    ok(tool, summary, data)
}

/// Bericht für `test`.
#[must_use]
pub fn test_report(
    tool: &str,
    command: &str,
    run: &ShellRun,
    max_diagnostics: usize,
    no_run: bool,
) -> ToolOutput {
    let cargo = parse_cargo_output(&run.stderr);
    let tests = parse_test_output(&run.stdout);
    let success = run.exit_code == 0;
    let status = if success {
        "ok"
    } else if tests.any_failed || !tests.failed_tests.is_empty() {
        "tests_failed"
    } else {
        "failed"
    };
    let mut data = base(command, run, status);
    add_diagnostics(&mut data, &cargo.diagnostics, &cargo.notes, max_diagnostics);
    if let Some(finished) = &cargo.finished {
        data["profile"] = json!(finished.profile);
        data["cargo_took"] = json!(finished.took);
    }
    let mut failures = Collector::with_budget(max_diagnostics, 12 * 1024);
    for failure in &tests.failures {
        let entry = json!({
            "name": clip_chars(&failure.name, 200),
            "location": failure.location,
            "message": failure.message,
        });
        if !failures.push(entry) {
            break;
        }
    }
    let mut names = Collector::with_budget(max_diagnostics, 4 * 1024);
    for name in &tests.failed_tests {
        if !names.push(json!(clip_chars(name, 200))) {
            break;
        }
    }
    let lists_cut = failures.truncated() || names.truncated();
    data["tests"] = json!({
        "ran": !no_run,
        "passed": tests.totals.passed,
        "failed": tests.totals.failed,
        "ignored": tests.totals.ignored,
        "measured": tests.totals.measured,
        "filtered_out": tests.totals.filtered_out,
        "reported_secs": (tests.totals.seconds * 100.0).round() / 100.0,
        "result_lines": tests.results,
        "binaries": cargo.binaries,
        "failed_tests": names.into_items(),
        "failures": failures.into_items(),
    });
    data["log"] = log_value(run, !success);
    data["truncated"] = json!(run.truncated || data["diagnostics_truncated"] == true || lists_cut);
    let summary = if no_run {
        format!(
            "{tool}: {status}, tests compiled (not run) in {:.1}s",
            run.wall_ms as f64 / 1000.0
        )
    } else {
        format!(
            "{tool}: {status}, {} passed, {} failed, {} ignored, {} filtered out in {:.1}s",
            tests.totals.passed,
            tests.totals.failed,
            tests.totals.ignored,
            tests.totals.filtered_out,
            run.wall_ms as f64 / 1000.0
        )
    };
    ok(tool, summary, data)
}

/// Bericht für `fmt --check`.
#[must_use]
pub fn fmt_report(
    tool: &str,
    command: &str,
    run: &ShellRun,
    root: &str,
    max_diagnostics: usize,
) -> ToolOutput {
    let diffs = parse_fmt_diffs(&run.stdout, root);
    let status = match (run.exit_code, diffs.is_empty()) {
        (0, _) => "ok",
        (_, false) => "needs_formatting",
        _ => "failed",
    };
    let mut data = base(command, run, status);
    let mut files: Vec<&str> = diffs.iter().map(|d| d.file.as_str()).collect();
    files.sort_unstable();
    files.dedup();
    data["diffs"] = json!(diffs.len());
    data["files_with_diffs"] = json!(files.len());
    let mut listed = Collector::with_budget(max_diagnostics, 24 * 1024);
    for diff in &diffs {
        if !listed.push(json!({"file": clip_chars(&diff.file, 200), "line": diff.line})) {
            break;
        }
    }
    data["truncated"] = json!(diffs.len() > listed.len() || run.truncated);
    data["listed"] = Value::Array(listed.into_items());
    data["log"] = log_value(run, status == "failed");
    let summary = match status {
        "ok" => format!("{tool}: formatting is clean"),
        "needs_formatting" => format!(
            "{tool}: {} files need formatting ({} hunks)",
            files.len(),
            diffs.len()
        ),
        _ => format!("{tool}: failed (exit code {})", run.exit_code),
    };
    ok(tool, summary, data)
}

/// Bericht für `tree`.
#[must_use]
pub fn tree_report(tool: &str, command: &str, run: &ShellRun) -> ToolOutput {
    let success = run.exit_code == 0;
    let mut data = base(command, run, if success { "ok" } else { "failed" });
    let text = strip_ansi(&run.stdout);
    let lines = text.lines().count();
    let (clipped, was_clipped) = clip_text(&text, MAX_TEXT_BYTES);
    data["tree"] = json!(clipped);
    data["lines"] = json!(lines);
    data["truncated"] = json!(was_clipped || run.truncated);
    if !success {
        let parsed = parse_cargo_output(&run.stderr);
        let mut sink = json!({});
        add_diagnostics(&mut sink, &parsed.diagnostics, &parsed.notes, 20);
        data["diagnostics"] = sink["diagnostics"].clone();
        data["log"] = log_value(run, true);
    }
    let summary = if success {
        format!("{tool}: {lines} lines")
    } else {
        format!("{tool}: failed (exit code {})", run.exit_code)
    };
    ok(tool, summary, data)
}

/// Fehlerbericht für `metadata`, wenn der Lauf scheiterte.
#[must_use]
pub fn failed_report(
    tool: &str,
    command: &str,
    run: &ShellRun,
    max_diagnostics: usize,
) -> ToolOutput {
    build_report(tool, command, run, max_diagnostics)
}

/// Fasst `cargo metadata` zusammen.
#[must_use]
pub fn metadata_report(
    tool: &str,
    command: &str,
    run: &ShellRun,
    metadata: &Value,
    limit: usize,
) -> ToolOutput {
    let mut data = base(command, run, "ok");
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let members: Vec<&str> = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .map(|m| m.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let root = metadata
        .get("workspace_root")
        .and_then(Value::as_str)
        .unwrap_or("");
    let is_member = |id: &str| members.contains(&id);
    let mut workspace = Collector::with_budget(limit, 28 * 1024);
    let mut member_total = 0usize;
    let mut external = 0usize;
    let mut external_list = Collector::with_budget(limit, 8 * 1024);
    let mut sorted: Vec<&Value> = packages.iter().collect();
    sorted.sort_by_key(|p| {
        p.get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    });
    for package in sorted {
        let id = package.get("id").and_then(Value::as_str).unwrap_or("");
        let name = package.get("name").and_then(Value::as_str).unwrap_or("");
        let version = package.get("version").and_then(Value::as_str).unwrap_or("");
        if is_member(id) {
            let manifest = package
                .get("manifest_path")
                .and_then(Value::as_str)
                .unwrap_or("");
            let relative = manifest
                .strip_prefix(root)
                .map_or(manifest, |m| m.trim_start_matches('/'));
            let targets: Vec<Value> = package
                .get("targets")
                .and_then(Value::as_array)
                .map(|t| {
                    t.iter()
                        .map(|target| {
                            json!({
                                "name": target.get("name"),
                                "kind": target.get("kind"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut features: Vec<&str> = package
                .get("features")
                .and_then(Value::as_object)
                .map(|f| f.keys().map(String::as_str).collect())
                .unwrap_or_default();
            features.sort_unstable();
            member_total += 1;
            workspace.push(json!({
                "name": name,
                "version": version,
                "manifest": relative,
                "edition": package.get("edition"),
                "targets": targets,
                "features": features,
                "dependencies": package.get("dependencies").and_then(Value::as_array).map_or(0, Vec::len),
            }));
        } else {
            external += 1;
            external_list.push(json!({"name": name, "version": version}));
        }
    }
    let truncated_members = member_total > workspace.len();
    let listed_externals = external_list.len();
    data["workspace_root"] = json!(".");
    data["workspace_members"] = Value::Array(workspace.into_items());
    data["workspace_member_count"] = json!(member_total);
    data["external_package_count"] = json!(external);
    data["external_packages"] = Value::Array(external_list.into_items());
    data["truncated"] = json!(truncated_members || external > listed_externals || run.truncated);
    data["log"] = log_value(run, false);
    let summary =
        format!("{tool}: {member_total} workspace packages, {external} external packages");
    ok(tool, summary, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, json_of};

    fn run(exit_code: i64, stdout: &str, stderr: &str) -> ShellRun {
        ShellRun {
            exit_code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            truncated: false,
            executed_on: None,
            wall_ms: 1500,
        }
    }

    #[test]
    fn strips_ansi_and_tails_on_char_boundaries() -> TestResult {
        assert_eq!(
            strip_ansi("\u{1b}[31mred\u{1b}[0m plain \u{1b}"),
            "red plain "
        );
        assert_eq!(strip_ansi("no escapes ü"), "no escapes ü");
        let (text, cut) = tail("aäbcd", 3);
        assert!(cut);
        assert_eq!(text, "bcd");
        let (text, cut) = tail("ä", 1);
        assert!(cut && text.is_empty());
        assert_eq!(tail("abc", 10), ("abc".to_owned(), false));
        Ok(())
    }

    #[test]
    fn build_report_lists_errors_first_with_positions() -> TestResult {
        let stderr = "src/a.rs:2:9: warning: unused variable: `x`\nsrc/lib.rs:7:5: error[E0308]: mismatched types\nwarning: `demo` (lib) generated 1 warning\nerror: could not compile `demo` (lib) due to 1 previous error\n";
        let value = json_of(build_report(
            "cargo.check",
            "cargo check",
            &run(101, "", stderr),
            50,
        ))?;
        assert_eq!(value["status"], "failed");
        assert_eq!(value["exit_code"], 101);
        assert_eq!(value["errors"], 1);
        assert_eq!(value["warnings"], 1);
        assert_eq!(value["diagnostics"][0]["level"], "error");
        assert_eq!(value["diagnostics"][0]["file"], "src/lib.rs");
        assert_eq!(value["diagnostics"][0]["line"], 7);
        assert_eq!(value["diagnostics"][0]["column"], 5);
        assert_eq!(value["diagnostics"][0]["code"], "E0308");
        assert_eq!(value["cargo_notes"].as_array().map(Vec::len), Some(2));
        assert_eq!(value["duration_ms"], 1500);
        assert!(
            value["summary"]
                .as_str()
                .is_some_and(|s| s.contains("1 errors, 1 warnings"))
        );
        Ok(())
    }

    #[test]
    fn diagnostics_are_capped_but_counted() -> TestResult {
        let stderr: String = (0..200)
            .map(|i| format!("src/a.rs:{}:1: warning: w{i}\n", i + 1))
            .collect();
        let value = json_of(build_report(
            "cargo.check",
            "cargo check",
            &run(0, "", &stderr),
            5,
        ))?;
        assert_eq!(value["warnings"], 200);
        assert_eq!(value["diagnostics"].as_array().map(Vec::len), Some(5));
        assert_eq!(value["diagnostics_truncated"], true);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["status"], "ok");
        Ok(())
    }

    #[test]
    fn messages_and_log_tails_are_clipped() -> TestResult {
        let long = "m".repeat(1000);
        let stderr = format!("src/a.rs:1:1: error[E0001]: {long}\n");
        let stdout = "o".repeat(50_000);
        let value = json_of(build_report(
            "cargo.check",
            "cargo check",
            &run(0, &stdout, &stderr),
            5,
        ))?;
        let message = value["diagnostics"][0]["message"].as_str().unwrap_or("");
        assert!(
            message.chars().count() <= MESSAGE_CHARS,
            "{}",
            message.chars().count()
        );
        assert!(message.ends_with('…'));
        let tail = value["log"]["stdout_tail"].as_str().unwrap_or("");
        assert_eq!(tail.len(), TAIL_OK_BYTES);
        assert_eq!(value["log"]["cut"], true);
        assert_eq!(value["log"]["stdout_bytes"], 50_000);
        let failed = json_of(build_report(
            "cargo.check",
            "cargo check",
            &run(1, &stdout, &stderr),
            5,
        ))?;
        assert_eq!(
            failed["log"]["stdout_tail"].as_str().map(str::len),
            Some(TAIL_FAILED_BYTES)
        );
        Ok(())
    }

    #[test]
    fn hostile_output_stays_within_the_budget() -> TestResult {
        let long = "x".repeat(5000);
        let stderr: String = (0..3000)
            .map(|i| format!("src/{long}.rs:{i}:1: error[E{i:04}]: {long}\n"))
            .collect();
        let stdout = format!(
            "{}test t ... FAILED\n---- t stdout ----\nthread 't' panicked at {long}:1:1:\n{long}\n",
            "line\n".repeat(50_000)
        );
        let value = build_report("cargo.test", "cargo test", &run(101, &stdout, &stderr), 500);
        let rendered = serde_json::to_string(&value)?;
        assert!(
            rendered.len() <= harw_tool_fsread::budget::MAX_OUTPUT_BYTES,
            "{} bytes",
            rendered.len()
        );
        let value = test_report(
            "cargo.test",
            "cargo test",
            &run(101, &stdout, &stderr),
            500,
            false,
        );
        let rendered = serde_json::to_string(&value)?;
        assert!(
            rendered.len() <= harw_tool_fsread::budget::MAX_OUTPUT_BYTES,
            "{} bytes",
            rendered.len()
        );
        Ok(())
    }

    #[test]
    fn test_report_has_counters_failures_and_status() -> TestResult {
        let stdout = "running 2 tests\ntest a ... ok\ntest b ... FAILED\n\n---- b stdout ----\nthread 'b' panicked at src/lib.rs:15:18:\nboom\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 4 filtered out; finished in 0.07s\n";
        let stderr = "src/lib.rs:2:9: warning: unused\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.36s\n     Running unittests src/lib.rs (target/debug/deps/demo-1)\nerror: test failed, to rerun pass `--lib`\n";
        let value = json_of(test_report(
            "cargo.test",
            "cargo test",
            &run(101, stdout, stderr),
            50,
            false,
        ))?;
        assert_eq!(value["status"], "tests_failed");
        let tests = &value["tests"];
        assert_eq!(
            (
                tests["passed"].as_u64(),
                tests["failed"].as_u64(),
                tests["ignored"].as_u64(),
                tests["filtered_out"].as_u64()
            ),
            (Some(1), Some(1), Some(1), Some(4))
        );
        assert_eq!(tests["binaries"], 1);
        assert_eq!(tests["failed_tests"], json!(["b"]));
        assert_eq!(tests["failures"][0]["location"], "src/lib.rs:15:18");
        assert_eq!(tests["failures"][0]["message"], "boom");
        assert_eq!(value["profile"], "test");
        assert_eq!(value["cargo_took"], "2.36s");
        let ok_value = json_of(test_report(
            "cargo.test",
            "cargo test",
            &run(
                0,
                "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
                "",
            ),
            50,
            false,
        ))?;
        assert_eq!(ok_value["status"], "ok");
        assert!(
            ok_value["summary"]
                .as_str()
                .is_some_and(|s| s.contains("3 passed"))
        );
        let compile_fail = json_of(test_report(
            "cargo.test",
            "cargo test",
            &run(101, "", "src/lib.rs:1:1: error[E0432]: unresolved import\n"),
            50,
            false,
        ))?;
        assert_eq!(compile_fail["status"], "failed");
        let no_run = json_of(test_report(
            "cargo.test",
            "cargo test --no-run",
            &run(0, "", ""),
            50,
            true,
        ))?;
        assert_eq!(no_run["tests"]["ran"], false);
        Ok(())
    }

    #[test]
    fn fmt_report_distinguishes_dirty_and_failed() -> TestResult {
        let dirty = run(
            1,
            "Diff in /w/ws/src/a.rs:3:\n x\nDiff in /w/ws/src/a.rs:9:\n y\nDiff in /w/ws/src/b.rs:1:\n",
            "",
        );
        let value = json_of(fmt_report(
            "cargo.fmt_check",
            "cargo fmt --check",
            &dirty,
            "/w/ws",
            50,
        ))?;
        assert_eq!(value["status"], "needs_formatting");
        assert_eq!(value["diffs"], 3);
        assert_eq!(value["files_with_diffs"], 2);
        assert_eq!(value["listed"][0], json!({"file": "src/a.rs", "line": 3}));
        let clean = json_of(fmt_report(
            "cargo.fmt_check",
            "cargo fmt --check",
            &run(0, "", ""),
            "/w/ws",
            50,
        ))?;
        assert_eq!(clean["status"], "ok");
        let broken = json_of(fmt_report(
            "cargo.fmt_check",
            "cargo fmt --check",
            &run(1, "", "error: no such subcommand"),
            "/w/ws",
            50,
        ))?;
        assert_eq!(broken["status"], "failed");
        Ok(())
    }

    #[test]
    fn tree_report_counts_lines_and_clips() -> TestResult {
        let value = json_of(tree_report(
            "cargo.tree",
            "cargo tree",
            &run(0, "demo v0.1.0\n├── a v1\n└── b v2\n", ""),
        ))?;
        assert_eq!(value["lines"], 3);
        assert_eq!(value["truncated"], false);
        let huge = "x".repeat(200_000);
        let value = json_of(tree_report("cargo.tree", "cargo tree", &run(0, &huge, "")))?;
        assert_eq!(value["truncated"], true);
        let failed = json_of(tree_report(
            "cargo.tree",
            "cargo tree",
            &run(101, "", "error: package `x` not found\n"),
        ))?;
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["diagnostics"][0]["level"], "error");
        Ok(())
    }

    #[test]
    fn metadata_report_summarizes_members_and_externals() -> TestResult {
        let metadata = json!({
            "workspace_root": "/w/ws",
            "workspace_members": ["path+file:///w/ws/a#a@0.1.0"],
            "packages": [
                {"name": "a", "version": "0.1.0", "id": "path+file:///w/ws/a#a@0.1.0", "manifest_path": "/w/ws/a/Cargo.toml", "edition": "2021",
                 "targets": [{"name": "a", "kind": ["lib"]}], "features": {"fancy": [], "default": []}, "dependencies": [{}, {}]},
                {"name": "serde", "version": "1.0.0", "id": "registry+x#serde@1.0.0", "manifest_path": "/cargo/serde/Cargo.toml", "targets": [], "features": {}, "dependencies": []},
                {"name": "anyhow", "version": "1.0.1", "id": "registry+x#anyhow@1.0.1", "manifest_path": "/cargo/anyhow/Cargo.toml", "targets": [], "features": {}, "dependencies": []}
            ]
        });
        let value = json_of(metadata_report(
            "cargo.metadata",
            "cargo metadata",
            &run(0, "", ""),
            &metadata,
            1,
        ))?;
        assert_eq!(value["workspace_member_count"], 1);
        assert_eq!(value["workspace_members"][0]["manifest"], "a/Cargo.toml");
        assert_eq!(
            value["workspace_members"][0]["features"],
            json!(["default", "fancy"])
        );
        assert_eq!(value["workspace_members"][0]["dependencies"], 2);
        assert_eq!(value["external_package_count"], 2);
        assert_eq!(value["external_packages"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["external_packages"][0]["name"], "anyhow");
        assert_eq!(value["truncated"], true);
        assert_eq!(value["workspace_root"], ".");
        assert!(
            !serde_json::to_string(&value)?.contains("/w/ws"),
            "absolute paths must not leak"
        );
        Ok(())
    }
}
