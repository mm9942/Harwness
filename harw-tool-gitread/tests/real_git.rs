//! Kreuzprüfung gegen das echte `git` (nur Tests).
//!
//! Die `git.*`-Werkzeuge sind reines Rust und starten nie einen Prozess. Diese
//! Tests bauen mit dem installierten `git` echte Repositories (lose Objekte,
//! `gc`-Packs, Deltas, Merges, Tags, Index-Version 4, Racy-Einträge, Intent-
//! to-add, Skip-Worktree, geteilter Index) und vergleichen Hashes, Status,
//! Diff-Zähler, Blame und Zweige mit der Ausgabe von `git`. Ist `git` nicht
//! vorhanden, melden die Tests `skipped: git` auf stderr und enden grün
//! (kein stilles Grün ohne Meldung).

use harw_tool_gitread::tools::{run_blame, run_branch, run_diff, run_log, run_show, run_status};
use harw_tools::ToolOutput;
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O error",
    Json(serde_json::Error) => "JSON error"
);

impl From<String> for TestError {
    fn from(message: String) -> Self {
        Self::Unexpected(message)
    }
}

fn have_git() -> bool {
    let present = Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !present {
        eprintln!("skipped: git");
    }
    present
}

struct Real {
    _dir: tempfile::TempDir,
    root: PathBuf,
    clock: Cell<i64>,
}

impl Real {
    fn new() -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?.join("ws");
        std::fs::create_dir_all(&root)?;
        let real = Self {
            _dir: dir,
            root,
            clock: Cell::new(1_700_000_000),
        };
        real.git(&["init", "-q", "-b", "main"])?;
        Ok(real)
    }

    fn git(&self, args: &[&str]) -> TestResult<String> {
        self.git_as(args, "Ada", "ada@example.com")
    }

    fn git_as(&self, args: &[&str], name: &str, email: &str) -> TestResult<String> {
        self.clock.set(self.clock.get() + 60);
        let date = format!("{} +0000", self.clock.get());
        let output = Command::new("git")
            .args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.safecrlf=false",
                "-c",
                "gc.auto=0",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", name)
            .env("GIT_AUTHOR_EMAIL", email)
            .env("GIT_COMMITTER_NAME", name)
            .env("GIT_COMMITTER_EMAIL", email)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("LC_ALL", "C")
            .output()?;
        if !output.status.success() {
            return Err(TestError::Unexpected(format!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> TestResult {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    fn commit(&self, message: &str, name: &str) -> TestResult {
        self.git(&["add", "-A"])?;
        let email = format!("{}@example.com", name.to_lowercase());
        self.git_as(&["commit", "-q", "-m", message], name, &email)?;
        Ok(())
    }
}

fn out(output: ToolOutput) -> TestResult<Value> {
    match output {
        ToolOutput::Json { content } => Ok(content),
        ToolOutput::Error { message } => {
            Err(TestError::Unexpected(format!("tool error: {message}")))
        }
        other => Err(TestError::Unexpected(format!("not JSON: {other:?}"))),
    }
}

fn err_of(output: ToolOutput) -> TestResult<String> {
    match output {
        ToolOutput::Error { message } => Ok(message),
        other => Err(TestError::Unexpected(format!(
            "expected an error: {other:?}"
        ))),
    }
}

fn status(real: &Real, args: Value) -> TestResult<Value> {
    out(run_status(&real.root, &serde_json::from_value(args)?))
}
fn diff(real: &Real, args: Value) -> TestResult<Value> {
    out(run_diff(&real.root, &serde_json::from_value(args)?))
}
fn log(real: &Real, args: Value) -> TestResult<Value> {
    out(run_log(&real.root, &serde_json::from_value(args)?))
}
fn show(real: &Real, args: Value) -> TestResult<Value> {
    out(run_show(&real.root, &serde_json::from_value(args)?))
}
fn branch(real: &Real, args: Value) -> TestResult<Value> {
    out(run_branch(&real.root, &serde_json::from_value(args)?))
}
fn blame(real: &Real, args: Value) -> TestResult<Value> {
    out(run_blame(&real.root, &serde_json::from_value(args)?))
}

/// Verlauf mit Merge, Tags, Zweigen und einem Upstream.
fn scenario() -> TestResult<Real> {
    let real = Real::new()?;
    let a_lines = (1..=12).map(|i| format!("l{i}\n")).collect::<String>();
    real.write("a.txt", a_lines.as_bytes())?;
    real.write("src/lib.rs", b"fn one() {}\nfn two() {}\nfn three() {}\n")?;
    real.write("docs/readme.md", b"# Title\n\ntext\n")?;
    real.write("bin.dat", &[0, 1, 2, 3, 255, 0, 7])?;
    real.write("run.sh", b"#!/bin/sh\necho hi\n")?;
    real.write(".gitignore", b"*.log\ntarget/\n")?;
    real.write(".env", b"TOKEN=original\n")?;
    std::fs::set_permissions(
        real.root.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )?;
    std::os::unix::fs::symlink("a.txt", real.root.join("link"))?;
    real.commit("initial import", "Ada")?;
    real.write("a.txt", a_lines.replace("l2\n", "L2\n").as_bytes())?;
    real.write("src/more.rs", b"pub fn more() {}\n")?;
    real.commit("second: tweak a\n\nWith a body\nover two lines.", "Bob")?;
    real.git(&["checkout", "-q", "-b", "feature"])?;
    real.write(
        "a.txt",
        a_lines
            .replace("l2\n", "L2\n")
            .replace("l10\n", "L10\n")
            .as_bytes(),
    )?;
    real.write("docs/readme.md", b"# Title\n\ntext\nmore feature text\n")?;
    real.commit("feature work", "Cy")?;
    real.write(
        "src/lib.rs",
        b"fn one() {}\nfn two() { feature }\nfn three() {}\n",
    )?;
    real.commit("feature two", "Cy")?;
    real.git(&["checkout", "-q", "main"])?;
    real.write(
        "src/lib.rs",
        b"fn zero() {}\nfn one() {}\nfn two() {}\nfn three() {}\n",
    )?;
    real.commit("main moves on", "Ada")?;
    real.git(&["merge", "--no-ff", "-q", "-m", "merge feature", "feature"])?;
    real.git(&["tag", "v1", "HEAD~1"])?;
    real.git(&["tag", "-a", "v2", "-m", "annotated release", "HEAD"])?;
    real.git(&["branch", "topic", "HEAD~2"])?;
    real.git(&["update-ref", "refs/remotes/origin/main", "HEAD~1"])?;
    real.git(&["config", "branch.main.remote", "origin"])?;
    real.git(&["config", "branch.main.merge", "refs/heads/main"])?;
    Ok(real)
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

fn names(value: &Value, key: &str, field: &str) -> Vec<String> {
    value[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e[field].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn assert_history_matches(real: &Real, layout: &str) -> TestResult {
    // Hashes und Reihenfolge von `git log`
    let expected = lines(&real.git(&["log", "--format=%H"])?);
    let ours = names(&log(real, json!({"max_count": 500}))?, "commits", "commit");
    assert_eq!(ours, expected, "{layout}: log order");
    let first_parent = lines(&real.git(&["log", "--first-parent", "--format=%H"])?);
    assert_eq!(
        names(
            &log(real, json!({"first_parent": true}))?,
            "commits",
            "commit"
        ),
        first_parent,
        "{layout}: first-parent"
    );
    let no_merges = lines(&real.git(&["log", "--no-merges", "--format=%H"])?);
    assert_eq!(
        names(&log(real, json!({"no_merges": true}))?, "commits", "commit"),
        no_merges,
        "{layout}: no-merges"
    );
    let range = lines(&real.git(&["log", "v1..HEAD", "--format=%H"])?);
    assert_eq!(
        names(&log(real, json!({"rev": "v1..HEAD"}))?, "commits", "commit"),
        range,
        "{layout}: range"
    );
    let by_author = lines(&real.git(&["log", "--author=Cy", "--format=%H"])?);
    assert_eq!(
        names(&log(real, json!({"author": "cy"}))?, "commits", "commit"),
        by_author,
        "{layout}: author"
    );
    let by_grep = lines(&real.git(&["log", "-i", "--grep=FEATURE", "--format=%H"])?);
    assert_eq!(
        names(&log(real, json!({"grep": "FEATURE"}))?, "commits", "commit"),
        by_grep,
        "{layout}: grep"
    );
    let path = lines(&real.git(&["log", "--full-history", "--format=%H", "--", "docs"])?);
    assert_eq!(
        names(&log(real, json!({"paths": ["docs"]}))?, "commits", "commit"),
        path,
        "{layout}: path filter"
    );
    // Felder eines Commits
    let head = log(real, json!({"max_count": 1}))?;
    let fmt = real.git(&["log", "-1", "--format=%an|%ae|%aI|%cI|%s|%P"])?;
    let parts: Vec<&str> = fmt.trim().split('|').collect();
    let first = &head["commits"][0];
    assert_eq!(first["author"]["name"], parts[0]);
    assert_eq!(first["author"]["email"], parts[1]);
    assert_eq!(first["author"]["date"], parts[2].replace('Z', "+00:00"));
    assert_eq!(first["subject"], parts[4]);
    let parents: Vec<String> = parts[5].split(' ').map(str::to_owned).collect();
    assert_eq!(
        first["parents"].as_array().map(|a| a
            .iter()
            .filter_map(|p| p.as_str().map(str::to_owned))
            .collect::<Vec<_>>()),
        Some(parents)
    );

    // Revisionsausdrücke
    for spec in [
        "HEAD",
        "HEAD~1",
        "HEAD~3",
        "v2^2",
        "v2^2^1",
        "v2^2~1",
        "v2~2",
        "v1",
        "v2",
        "v2^{commit}",
        "v2^{tree}",
        "main",
        "topic",
        "origin/main",
        "feature",
        "HEAD^{tree}",
        "refs/tags/v1",
    ] {
        let expected = real.git(&["rev-parse", spec])?.trim().to_owned();
        let shown = show(real, json!({"rev": spec, "diff": false}))?;
        let got = shown["commit"]
            .as_str()
            .or(shown["oid"].as_str())
            .unwrap_or("");
        assert_eq!(got, expected, "{layout}: rev {spec}");
    }
    let abbreviated = real
        .git(&["rev-parse", "--short=10", "HEAD~1"])?
        .trim()
        .to_owned();
    assert_eq!(
        show(real, json!({"rev": abbreviated, "diff": false}))?["commit"]
            .as_str()
            .map(str::to_owned),
        Some(real.git(&["rev-parse", "HEAD~1"])?.trim().to_owned())
    );

    // Bäume und Blobs
    let tree = real.git(&["ls-tree", "HEAD"])?;
    let listing = show(real, json!({"rev": "HEAD^{tree}"}))?;
    let ours: Vec<String> = listing["entries"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| {
                    format!(
                        "{} {} {}\t{}",
                        e["mode"].as_str().unwrap_or(""),
                        e["type"].as_str().unwrap_or(""),
                        e["oid"].as_str().unwrap_or(""),
                        e["name"].as_str().unwrap_or("").trim_end_matches('/')
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(ours, lines(&tree), "{layout}: ls-tree");
    for path in [
        "a.txt",
        "src/lib.rs",
        "docs/readme.md",
        "src/more.rs",
        "run.sh",
    ] {
        for rev in ["HEAD", "HEAD~2", "v1"] {
            let spec = format!("{rev}:{path}");
            let content = real.git(&["show", &spec])?;
            let shown = show(real, json!({"rev": spec}))?;
            assert_eq!(
                shown["content"].as_str(),
                Some(content.as_str()),
                "{layout}: {spec}"
            );
        }
    }
    let binary = show(real, json!({"rev": "HEAD:bin.dat"}))?;
    assert_eq!(binary["binary"], true);
    assert_eq!(binary["size"], 7);
    let tag = show(real, json!({"rev": "v2", "diff": false}))?;
    assert_eq!(tag["kind"], "tag");
    assert_eq!(tag["message"], "annotated release");

    // Diff zwischen Commits (Zähler)
    for (a, b) in [("v1", "v2"), ("HEAD~3", "HEAD"), ("topic", "feature")] {
        let numstat = real.git(&["diff", "--no-renames", "--numstat", a, b])?;
        let tool = diff(real, json!({"base": a, "target": b, "format": "stat"}))?;
        assert_eq!(
            numstat_rows(&numstat),
            tool_rows(&tool),
            "{layout}: diff {a} {b}"
        );
    }

    // Blame
    for path in ["a.txt", "src/lib.rs", "docs/readme.md"] {
        let porcelain = real.git(&["blame", "--porcelain", path])?;
        let mut expected: BTreeMap<u64, String> = BTreeMap::new();
        for line in porcelain.lines() {
            let mut parts = line.split(' ');
            if let (Some(sha), Some(_orig), Some(fin)) = (parts.next(), parts.next(), parts.next())
            {
                if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                    if let Ok(n) = fin.parse::<u64>() {
                        expected.insert(n, sha.to_owned());
                    }
                }
            }
        }
        let ours = blame(real, json!({"path": path, "max_lines": 2000}))?;
        let mut got: BTreeMap<u64, String> = BTreeMap::new();
        for l in ours["lines"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let short = l["commit"].as_str().unwrap_or("");
            got.insert(
                l["line"].as_u64().unwrap_or(0),
                ours["commits"][short]["commit"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned(),
            );
        }
        assert_eq!(got, expected, "{layout}: blame {path}");
        assert_eq!(ours["approximate"], false);
    }

    // Zweige
    let refs = real.git(&[
        "for-each-ref",
        "--format=%(refname:short) %(objectname)",
        "refs/heads",
    ])?;
    let listing = branch(real, json!({}))?;
    let ours: Vec<String> = listing["branches"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|b| {
                    format!(
                        "{} {}",
                        b["name"].as_str().unwrap_or(""),
                        b["oid"].as_str().unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(ours, lines(&refs), "{layout}: branches");
    let counts = real.git(&["rev-list", "--left-right", "--count", "main...origin/main"])?;
    let mut it = counts.split_whitespace();
    let (ahead, behind) = (it.next().unwrap_or("?"), it.next().unwrap_or("?"));
    let main = listing["branches"]
        .as_array()
        .and_then(|a| a.iter().find(|b| b["name"] == "main"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(main["ahead"].to_string(), ahead, "{layout}");
    assert_eq!(main["behind"].to_string(), behind, "{layout}");
    assert_eq!(main["upstream"], "origin/main");
    let all = branch(real, json!({"remotes": true, "tags": true}))?;
    let tags = real.git(&["for-each-ref", "--format=%(refname:short)", "refs/tags"])?;
    assert_eq!(
        names(&all, "branches", "name")
            .iter()
            .filter(|n| n.starts_with('v'))
            .cloned()
            .collect::<Vec<_>>(),
        lines(&tags)
    );
    Ok(())
}

/// Zeilen von `git diff --numstat`: `(pfad, plus, minus)`; Binär als `-`.
fn numstat_rows(text: &str) -> Vec<(String, String, String)> {
    let mut rows: Vec<(String, String, String)> = text
        .lines()
        .filter(|l| !l.ends_with("\t.env"))
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            Some((
                p.next()?.to_owned(),
                p.next()?.to_owned(),
                p.next()?.to_owned(),
            ))
        })
        .map(|(a, d, path)| (path, a, d))
        .collect();
    rows.sort();
    rows
}

fn tool_rows(tool: &Value) -> Vec<(String, String, String)> {
    let mut rows: Vec<(String, String, String)> = tool["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        // Geheimnis-Dateien: Zähler werden absichtlich nicht berechnet.
        .filter(|f| f.get("omitted").is_none())
        .map(|f| {
            let path = f["path"].as_str().unwrap_or("").to_owned();
            if f["binary"] == true {
                (path, "-".to_owned(), "-".to_owned())
            } else {
                (path, f["additions"].to_string(), f["deletions"].to_string())
            }
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn history_matches_git_for_loose_objects() -> TestResult {
    if !have_git() {
        return Ok(());
    }
    let real = scenario()?;
    assert_history_matches(&real, "loose")
}

#[test]
fn history_matches_git_for_gc_packs_and_deltas() -> TestResult {
    if !have_git() {
        return Ok(());
    }
    let real = scenario()?;
    real.git(&["gc", "-q", "--prune=now"])?;
    assert!(real.git(&["count-objects", "-v"])?.contains("count: 0"));
    assert_history_matches(&real, "gc")?;
    // Mehr Commits als lose Objekte über den Packs, danach harte Delta-Packs.
    real.write(
        "a.txt",
        (1..=12)
            .map(|i| format!("l{i} v3\n"))
            .collect::<String>()
            .as_bytes(),
    )?;
    real.commit("after gc", "Ada")?;
    assert_history_matches(&real, "gc+loose")?;
    real.git(&[
        "repack",
        "-a",
        "-d",
        "-f",
        "-q",
        "--depth=50",
        "--window=50",
    ])?;
    assert_history_matches(&real, "repack-deltas")?;
    let verify = real.git(&["verify-pack", "-v", &first_pack(&real.root)?])?;
    eprintln!(
        "delta objects in the test pack: {}",
        verify
            .lines()
            .filter(|l| l.split_whitespace().count() >= 7)
            .count()
    );
    Ok(())
}

fn first_pack(root: &Path) -> TestResult<String> {
    for entry in std::fs::read_dir(root.join(".git/objects/pack"))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("idx") {
            return Ok(path.to_string_lossy().into_owned());
        }
    }
    Err(TestError::Missing("pack"))
}

/// Statuszeilen von `git status --porcelain=v1` als `pfad → XY`.
fn porcelain(real: &Real) -> TestResult<BTreeMap<String, String>> {
    let text = real.git(&[
        "status",
        "--porcelain=v1",
        "--no-renames",
        "--ignored",
        "--untracked-files=normal",
    ])?;
    Ok(text
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| (l[3..].to_owned(), l[..2].to_owned()))
        .collect())
}

fn ours_porcelain(real: &Real) -> TestResult<BTreeMap<String, String>> {
    let status = status(real, json!({"ignored": true, "limit": 5000}))?;
    let mut map: BTreeMap<String, [char; 2]> = BTreeMap::new();
    let letter = |s: &str| match s {
        "added" => 'A',
        "modified" => 'M',
        "deleted" => 'D',
        "typechange" => 'T',
        _ => '?',
    };
    for entry in status["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let path = entry["path"].as_str().unwrap_or("").to_owned();
        let slot = map.entry(path).or_insert([' ', ' ']);
        match entry["area"].as_str().unwrap_or("") {
            "staged" => slot[0] = letter(entry["status"].as_str().unwrap_or("")),
            "unstaged" => slot[1] = letter(entry["status"].as_str().unwrap_or("")),
            "untracked" => *slot = ['?', '?'],
            "ignored" => *slot = ['!', '!'],
            "conflict" => *slot = ['U', 'U'],
            _ => {}
        }
    }
    Ok(map
        .into_iter()
        .map(|(k, v)| (k, v.iter().collect()))
        .collect())
}

fn dirty(real: &Real) -> TestResult {
    real.write(
        "a.txt",
        (1..=12)
            .map(|i| {
                if i == 3 {
                    "CHANGED\n".to_owned()
                } else {
                    format!("l{i}\n")
                }
            })
            .collect::<String>()
            .as_bytes(),
    )?;
    real.write("src/more.rs", b"pub fn more() {}\npub fn staged() {}\n")?;
    real.git(&["add", "src/more.rs"])?;
    real.write(
        "src/more.rs",
        b"pub fn more() {}\npub fn staged() {}\npub fn unstaged() {}\n",
    )?;
    std::fs::remove_file(real.root.join("docs/readme.md"))?;
    std::fs::set_permissions(
        real.root.join("src/lib.rs"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )?;
    std::fs::remove_file(real.root.join("link"))?;
    real.write("link", b"now a regular file\n")?;
    real.write("bin.dat", &[0, 1, 2, 9, 255, 0, 7])?;
    real.write("fresh.txt", b"fresh\n")?;
    real.write("newdir/deep/file.txt", b"deep\n")?;
    real.write("debug.log", b"ignored\n")?;
    real.write("target/out/x.o", b"ignored\n")?;
    real.write("vendor/fake/.git/HEAD", b"ref: refs/heads/x\n")?;
    real.write("vendor/fake/x.rs", b"not a real nested repo\n")?;
    real.git(&["init", "-q", "vendor/real"])?;
    real.write("vendor/real/y.rs", b"nested repo\n")?;
    real.write("intent.txt", b"intent to add\n")?;
    real.git(&["add", "-N", "intent.txt"])?;
    real.write(".env", b"TOKEN=changed-secret\n")?;
    Ok(())
}

#[test]
fn status_and_diff_match_git_on_a_dirty_tree() -> TestResult {
    if !have_git() {
        return Ok(());
    }
    let real = scenario()?;
    real.git(&["gc", "-q"])?;
    dirty(&real)?;
    assert_eq!(
        ours_porcelain(&real)?,
        porcelain(&real)?,
        "status porcelain"
    );
    let untracked_all = real.git(&[
        "status",
        "--porcelain=v1",
        "--no-renames",
        "--untracked-files=all",
    ])?;
    let ours = status(&real, json!({"untracked": "all"}))?;
    let expected: Vec<String> = untracked_all
        .lines()
        .filter(|l| l.starts_with("??"))
        .map(|l| l[3..].to_owned())
        .collect();
    let got: Vec<String> = ours["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|e| e["area"] == "untracked")
        .filter_map(|e| e["path"].as_str().map(str::to_owned))
        .collect();
    // Git zeigt ein verschachteltes Repository auch im Modus `all` als `dir/`.
    assert_eq!(got, expected, "untracked=all");

    // Zähler und Patch: Arbeitsverzeichnis gegen Index und Index gegen HEAD
    for (args, tool_args) in [
        (
            vec!["diff", "--no-renames", "--numstat"],
            json!({"format": "stat"}),
        ),
        (
            vec!["diff", "--cached", "--no-renames", "--numstat"],
            json!({"format": "stat", "staged": true}),
        ),
        (
            vec!["diff", "--no-renames", "--numstat", "HEAD~2"],
            json!({"format": "stat", "base": "HEAD~2"}),
        ),
    ] {
        let expected = numstat_rows(&real.git(&args)?);
        let tool = diff(&real, tool_args.clone())?;
        assert_eq!(tool_rows(&tool), expected, "{args:?}");
    }
    let names_expected = real.git(&["diff", "--no-renames", "--name-status"])?;
    let names_ours = diff(&real, json!({"format": "name_status"}))?;
    let ours: Vec<String> = names_ours["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|f| {
            format!(
                "{}\t{}",
                f["status"].as_str().unwrap_or(""),
                f["path"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(ours, lines(&names_expected));

    // Hunk-Texte Zeile für Zeile (ohne `index`-Zeile und Funktionskontext hinter `@@`).
    let normalize = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| !l.starts_with("index "))
            .map(|l| {
                if l.starts_with("@@ ") {
                    l.splitn(3, "@@").take(2).collect::<Vec<_>>().join("@@") + "@@"
                } else {
                    l.to_owned()
                }
            })
            .collect()
    };
    for path in [
        "a.txt",
        "src/more.rs",
        "bin.dat",
        "docs/readme.md",
        "src/lib.rs",
    ] {
        let expected = real.git(&["diff", "--no-renames", "--", path])?;
        let tool = diff(&real, json!({"paths": [path]}))?;
        assert_eq!(
            normalize(tool["patch"].as_str().unwrap_or("")),
            normalize(&expected),
            "patch of {path}"
        );
    }
    let secret = diff(&real, json!({"paths": [".env"]}))?;
    assert!(!secret.to_string().contains("changed-secret"));
    assert_eq!(
        secret["files"][0]["omitted"],
        "secret path: contents are never shown"
    );
    // Der Typwechsel `link` erscheint wie bei Git als Löschung plus Neuanlage.
    let type_change = real.git(&["diff", "--no-renames", "--", "link"])?;
    let tool = diff(&real, json!({"paths": ["link"]}))?;
    assert_eq!(
        normalize(tool["patch"].as_str().unwrap_or("")),
        normalize(&type_change),
        "typechange patch"
    );
    Ok(())
}

#[test]
fn index_versions_racy_entries_and_flags_match_git() -> TestResult {
    if !have_git() {
        return Ok(());
    }
    for version in ["2", "3", "4"] {
        let real = scenario()?;
        real.git(&["update-index", &format!("--index-version={version}")])?;
        dirty(&real)?;
        assert_eq!(
            ours_porcelain(&real)?,
            porcelain(&real)?,
            "index v{version}"
        );
    }
    // Racy: gleiche Größe, gleiche Sekunde wie das `git add`.
    let real = scenario()?;
    real.write("racy.txt", b"aaaa\n")?;
    real.git(&["add", "racy.txt"])?;
    real.write("racy.txt", b"bbbb\n")?;
    assert_eq!(ours_porcelain(&real)?, porcelain(&real)?, "racy");
    assert_eq!(
        porcelain(&real)?.get("racy.txt").map(String::as_str),
        Some("AM")
    );
    // Skip-Worktree und assume-unchanged: Änderungen werden wie bei Git nicht gemeldet.
    real.git(&["update-index", "--skip-worktree", "src/more.rs"])?;
    real.write("src/more.rs", b"edited but hidden\n")?;
    real.git(&["update-index", "--assume-unchanged", "run.sh"])?;
    real.write("run.sh", b"edited but assumed\n")?;
    assert_eq!(
        ours_porcelain(&real)?,
        porcelain(&real)?,
        "skip-worktree/assume-unchanged"
    );
    Ok(())
}

#[test]
fn conflicts_and_operations_match_git() -> TestResult {
    if !have_git() {
        return Ok(());
    }
    let real = scenario()?;
    real.git(&["checkout", "-q", "-b", "left", "v1"])?;
    real.write("a.txt", b"left side\n")?;
    real.commit("left", "Ada")?;
    real.git(&["checkout", "-q", "-b", "right", "v1"])?;
    real.write("a.txt", b"right side\n")?;
    real.commit("right", "Bob")?;
    let merged = Command::new("git")
        .args(["merge", "left"])
        .current_dir(&real.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "x")
        .env("GIT_AUTHOR_EMAIL", "x@x")
        .env("GIT_COMMITTER_NAME", "x")
        .env("GIT_COMMITTER_EMAIL", "x@x")
        .output()?;
    assert!(
        !merged.status.success(),
        "the merge is expected to conflict"
    );
    assert_eq!(ours_porcelain(&real)?, porcelain(&real)?, "conflict status");
    let ours = status(&real, json!({}))?;
    assert_eq!(ours["operation"], "merge");
    assert_eq!(ours["counts"]["conflicts"], 1);
    assert_eq!(ours["clean"], false);
    Ok(())
}

#[test]
fn split_index_is_refused_with_a_clear_message() -> TestResult {
    if !have_git() {
        return Ok(());
    }
    let real = scenario()?;
    real.git(&["update-index", "--split-index"])?;
    let message = err_of(run_status(&real.root, &serde_json::from_value(json!({}))?))?;
    assert!(message.contains("extension"), "{message}");
    Ok(())
}

#[test]
fn hostile_repositories_are_refused_not_followed() -> TestResult {
    if !have_git() {
        return Ok(());
    }
    // .git als Symlink nach außen
    let outside = tempfile::tempdir()?;
    let other = Real::new()?;
    other.write("x", b"1")?;
    other.commit("x", "Ada")?;
    let ws = tempfile::tempdir()?;
    let root = ws.path().canonicalize()?;
    std::os::unix::fs::symlink(other.root.join(".git"), root.join(".git"))?;
    assert!(
        err_of(run_status(&root, &serde_json::from_value(json!({}))?))?
            .contains("unsupported .git entry")
    );
    let message = err_of(run_log(&root, &serde_json::from_value(json!({}))?))?;
    assert!(!message.contains("initial"), "{message}");
    // gitdir-Datei, die nach außen zeigt (verknüpfter Worktree)
    let root2 = outside.path().canonicalize()?;
    std::fs::write(
        root2.join(".git"),
        format!("gitdir: {}\n", other.root.join(".git").display()),
    )?;
    let message = err_of(run_status(&root2, &serde_json::from_value(json!({}))?))?;
    assert!(message.contains("outside the workspace"), "{message}");
    // Pfad-Ausbrüche
    let real = scenario()?;
    for path in ["../x", "/etc/passwd", ".git/config", "a/../../x"] {
        assert!(
            err_of(run_blame(
                &real.root,
                &serde_json::from_value(json!({"path": path}))?
            ))
            .is_ok(),
            "{path}"
        );
        assert!(
            err_of(run_status(
                &real.root,
                &serde_json::from_value(json!({"paths": [path]}))?
            ))
            .is_ok(),
            "{path}"
        );
    }
    Ok(())
}
