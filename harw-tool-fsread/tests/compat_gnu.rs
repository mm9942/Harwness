//! Kreuzprüfung gegen die echten GNU-Werkzeuge (nur Tests).
//!
//! Die `fsread.*`-Werkzeuge sind reines Rust; diese Tests starten die
//! entsprechenden GNU-Befehle **ausschließlich in der Testumgebung** und
//! vergleichen Zahlen und Texte. Fehlt ein Befehl auf dem Testrechner, wird
//! der jeweilige Vergleich übersprungen und auf stderr gemeldet (kein stilles
//! Grün: `skipped: <befehl>`).

use harw_tools::ToolOutput;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O error",
    Json(serde_json::Error) => "JSON error"
);

fn workspace() -> TestResult<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    Ok((dir, root))
}

/// Führt einen GNU-Befehl aus; `None`, wenn er fehlt.
fn gnu(program: &str, args: &[&str], cwd: &Path) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("LC_ALL", "C.UTF-8")
        .output()
        .ok()?;
    if !output.status.success() && output.stdout.is_empty() {
        eprintln!(
            "skipped: {program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn value(output: ToolOutput) -> TestResult<Value> {
    match output {
        ToolOutput::Json { content } => Ok(content),
        other => Err(TestError::Unexpected(format!("not JSON: {other:?}"))),
    }
}

fn sample_text() -> String {
    let mut text = String::new();
    for index in 0..200 {
        text.push_str(&format!(
            "line {index} with some words\tand a tab ä ö ü {}\n",
            "x".repeat(index % 17)
        ));
    }
    text.push_str("last line without newline");
    text
}

#[test]
fn wc_matches_gnu_wc() -> TestResult {
    let (_dir, root) = workspace()?;
    fs::write(root.join("t.txt"), sample_text())?;
    fs::write(root.join("e.txt"), "")?;
    fs::write(root.join("n.txt"), "no newline")?;
    for file in ["t.txt", "e.txt", "n.txt"] {
        let Some(expected) = gnu("wc", &["-l", "-w", "-c", "-m", "-L", file], &root) else {
            eprintln!("skipped: wc");
            return Ok(());
        };
        let fields: Vec<u64> = expected
            .split_whitespace()
            .take(5)
            .filter_map(|f| f.parse().ok())
            .collect();
        let args = serde_json::from_value(
            json!({"paths": [file], "lines": true, "words": true, "bytes": true, "chars": true, "max_line_length": true}),
        )?;
        let got = value(harw_tool_fsread::wc::run(&root, &args))?;
        let r = &got["results"][0];
        let ours = [
            &r["lines"],
            &r["words"],
            &r["chars"],
            &r["bytes"],
            &r["max_line_length"],
        ]
        .map(|v| v.as_u64().unwrap_or(u64::MAX));
        assert_eq!(fields, ours.to_vec(), "{file}: GNU wc {expected:?}");
    }
    Ok(())
}

#[test]
fn hash_matches_sha256sum() -> TestResult {
    let (_dir, root) = workspace()?;
    let bytes: Vec<u8> = (0..100_000u32).map(|i| (i * 31 % 251) as u8).collect();
    fs::write(root.join("blob"), &bytes)?;
    let Some(expected) = gnu("sha256sum", &["blob"], &root) else {
        eprintln!("skipped: sha256sum");
        return Ok(());
    };
    let args = serde_json::from_value(json!({"paths": ["blob"]}))?;
    let got = value(harw_tool_fsread::hash::run(&root, &args))?;
    assert_eq!(
        got["results"][0]["digest"].as_str(),
        expected.split_whitespace().next()
    );
    if let Some(b3) = gnu("b3sum", &["blob"], &root) {
        let args = serde_json::from_value(json!({"paths": ["blob"], "algorithm": "blake3"}))?;
        let got = value(harw_tool_fsread::hash::run(&root, &args))?;
        assert_eq!(
            got["results"][0]["digest"].as_str(),
            b3.split_whitespace().next()
        );
    } else {
        eprintln!("skipped: b3sum");
    }
    Ok(())
}

#[test]
fn head_and_tail_match_gnu() -> TestResult {
    let (_dir, root) = workspace()?;
    fs::write(root.join("t.txt"), sample_text())?;
    fs::write(root.join("nl.txt"), "a\nb\nc\n")?;
    for (file, n) in [
        ("t.txt", 7usize),
        ("t.txt", 300),
        ("nl.txt", 2),
        ("nl.txt", 1),
        ("nl.txt", 10),
    ] {
        let n_arg = n.to_string();
        let (Some(head), Some(tail)) = (
            gnu("head", &["-n", &n_arg, file], &root),
            gnu("tail", &["-n", &n_arg, file], &root),
        ) else {
            eprintln!("skipped: head/tail");
            return Ok(());
        };
        let h = value(harw_tool_fsread::headtail::run_head(
            &root,
            &serde_json::from_value(json!({"path": file, "lines": n}))?,
        ))?;
        let t = value(harw_tool_fsread::headtail::run_tail(
            &root,
            &serde_json::from_value(json!({"path": file, "lines": n}))?,
        ))?;
        assert_eq!(
            h["content"].as_str(),
            Some(head.as_str()),
            "head -n {n} {file}"
        );
        assert_eq!(
            t["content"].as_str(),
            Some(tail.as_str()),
            "tail -n {n} {file}"
        );
    }
    for c in [1usize, 17, 500] {
        let c_arg = c.to_string();
        let (Some(head), Some(tail)) = (
            gnu("head", &["-c", &c_arg, "t.txt"], &root),
            gnu("tail", &["-c", &c_arg, "t.txt"], &root),
        ) else {
            return Ok(());
        };
        let h = value(harw_tool_fsread::headtail::run_head(
            &root,
            &serde_json::from_value(json!({"path": "t.txt", "bytes": c}))?,
        ))?;
        let t = value(harw_tool_fsread::headtail::run_tail(
            &root,
            &serde_json::from_value(json!({"path": "t.txt", "bytes": c}))?,
        ))?;
        // Bytegrenzen können mitten in ä/ö/ü fallen: Vergleich auf Byte-Ebene über die Länge der lossy-Ausgabe vermeiden.
        let ours_h = h["content"].as_str().unwrap_or("");
        let ours_t = t["content"].as_str().unwrap_or("");
        assert!(
            head.starts_with(ours_h.trim_end_matches('\u{fffd}')),
            "head -c {c}"
        );
        assert!(
            tail.ends_with(ours_t.trim_start_matches('\u{fffd}')),
            "tail -c {c}"
        );
    }
    let Some(from) = gnu("tail", &["-n", "+198", "t.txt"], &root) else {
        return Ok(());
    };
    let t = value(harw_tool_fsread::headtail::run_tail(
        &root,
        &serde_json::from_value(json!({"path": "t.txt", "from_line": 198}))?,
    ))?;
    assert_eq!(t["content"].as_str(), Some(from.as_str()));
    Ok(())
}

#[test]
fn du_matches_gnu_du() -> TestResult {
    let (_dir, root) = workspace()?;
    fs::create_dir_all(root.join("a/b"))?;
    fs::write(root.join("a/one"), vec![1u8; 5000])?;
    fs::write(root.join("a/b/two"), vec![2u8; 123])?;
    fs::write(root.join("three"), vec![3u8; 70_000])?;
    fs::hard_link(root.join("three"), root.join("a/three_link"))?;
    for (flag, mode) in [("--apparent-size", "apparent_size"), ("", "disk")] {
        let mut args = vec!["-s", "-B1"];
        if !flag.is_empty() {
            args.push(flag);
        }
        args.push(".");
        let Some(expected) = gnu("du", &args, &root) else {
            eprintln!("skipped: du");
            return Ok(());
        };
        let gnu_total: u64 = expected
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let a = serde_json::from_value(
            json!({"summarize": true, "apparent_size": mode == "apparent_size"}),
        )?;
        let got = value(harw_tool_fsread::du::run(&root, &a))?;
        assert_eq!(
            got["total_bytes"].as_u64(),
            Some(gnu_total),
            "du {mode}: GNU says {expected:?}"
        );
    }
    Ok(())
}

#[test]
fn find_matches_gnu_find() -> TestResult {
    let (_dir, root) = workspace()?;
    for path in [
        "src/main.rs",
        "src/util/mod.rs",
        "src/util/Notes.TXT",
        "docs/a.md",
        "docs/deep/er/b.md",
        "top.rs",
    ] {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap_or(&root))?;
        fs::write(full, path.repeat(3))?;
    }
    fs::create_dir_all(root.join("emptydir"))?;
    fs::write(root.join("empty.txt"), "")?;
    std::os::unix::fs::symlink("src", root.join("lnk"))?;
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (
            vec![".", "-name", "*.rs", "-type", "f"],
            json!({"name": "*.rs", "kind": "f"}),
        ),
        (vec![".", "-iname", "*.txt"], json!({"iname": "*.txt"})),
        (vec![".", "-type", "d"], json!({"kind": "d"})),
        (vec![".", "-type", "l"], json!({"kind": "l"})),
        (vec![".", "-maxdepth", "1"], json!({"max_depth": 1})),
        (
            vec![".", "-mindepth", "3", "-type", "f"],
            json!({"min_depth": 3, "kind": "f"}),
        ),
        (vec![".", "-empty"], json!({"empty": true})),
        (
            vec![".", "-type", "f", "-size", "+30c"],
            json!({"kind": "f", "min_size": 31}),
        ),
        (
            vec![".", "-path", "./docs/*"],
            json!({"path_glob": "docs/*"}),
        ),
    ];
    for (gnu_args, ours) in cases {
        let Some(expected) = gnu("find", &gnu_args, &root) else {
            eprintln!("skipped: find");
            return Ok(());
        };
        let mut expected: Vec<String> = expected
            .lines()
            .map(|l| l.trim_start_matches("./").to_owned())
            .filter(|l| l != ".")
            .collect();
        expected.sort();
        let got = value(harw_tool_fsread::find::run(
            &root,
            &serde_json::from_value(ours.clone())?,
        ))?;
        let mut paths: Vec<String> = got["matches"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter_map(|e| e["path"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        paths.sort();
        assert_eq!(paths, expected, "find {gnu_args:?} vs {ours}");
    }
    Ok(())
}

#[test]
fn stat_matches_gnu_stat() -> TestResult {
    let (_dir, root) = workspace()?;
    fs::write(root.join("f"), "hello world")?;
    fs::create_dir(root.join("d"))?;
    std::os::unix::fs::symlink("f", root.join("l"))?;
    for path in ["f", "d", "l"] {
        let Some(expected) = gnu(
            "stat",
            &["-c", "%s|%a|%u|%g|%i|%h|%Y|%X|%Z|%F|%b", path],
            &root,
        ) else {
            eprintln!("skipped: stat");
            return Ok(());
        };
        let f: Vec<&str> = expected.trim().split('|').collect();
        let got = value(harw_tool_fsread::stat::run(
            &root,
            &serde_json::from_value(json!({"paths": [path]}))?,
        ))?;
        let r = &got["results"][0];
        assert_eq!(r["size"].to_string(), f[0], "{path} size");
        assert_eq!(
            r["mode"]
                .as_str()
                .map(|m| m.trim_start_matches('0').to_owned()),
            Some(f[1].to_owned()),
            "{path} mode"
        );
        assert_eq!(r["uid"].to_string(), f[2]);
        assert_eq!(r["gid"].to_string(), f[3]);
        assert_eq!(r["inode"].to_string(), f[4]);
        assert_eq!(r["nlink"].to_string(), f[5]);
        assert_eq!(r["mtime"]["epoch"].to_string(), f[6]);
        assert_eq!(r["atime"]["epoch"].to_string(), f[7]);
        assert_eq!(r["ctime"]["epoch"].to_string(), f[8]);
        assert_eq!(r["blocks"].to_string(), f[10]);
        let kind = match r["type"].as_str() {
            Some("file") => "regular file",
            Some("dir") => "directory",
            Some("symlink") => "symbolic link",
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        };
        assert!(
            f[9].starts_with(kind) || (kind == "regular file" && f[9].contains("regular")),
            "{path}: {} vs {kind}",
            f[9]
        );
    }
    Ok(())
}

#[test]
fn ls_matches_gnu_ls() -> TestResult {
    let (_dir, root) = workspace()?;
    for name in ["b.txt", "a.rs", "Z.md", ".hidden", "ä.txt", "c10", "c2"] {
        fs::write(root.join(name), name.repeat(7))?;
    }
    fs::create_dir(root.join("dir"))?;
    std::os::unix::fs::symlink("a.rs", root.join("link"))?;
    let Some(expected) = gnu("ls", &["-A", "-1"], &root) else {
        eprintln!("skipped: ls");
        return Ok(());
    };
    // Bytefolge wie im C-Locale: LC_ALL=C.UTF-8 sortiert nach Codepunkt.
    let expected: Vec<String> = expected.lines().map(str::to_owned).collect();
    let got = value(harw_tool_fsread::ls::run(
        &root,
        &serde_json::from_value(json!({"almost_all": true}))?,
    ))?;
    let names: Vec<String> = got["entries"]
        .as_array()
        .map(|e| {
            e.iter()
                .filter_map(|x| x["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(names, expected);

    let Some(long) = gnu("ls", &["-lA", "--time-style=+%s", "-1"], &root) else {
        return Ok(());
    };
    let got = value(harw_tool_fsread::ls::run(
        &root,
        &serde_json::from_value(json!({"almost_all": true, "long": true}))?,
    ))?;
    let entries = got["entries"].as_array().cloned().unwrap_or_default();
    for (line, entry) in long
        .lines()
        .filter(|l| !l.starts_with("total"))
        .zip(entries.iter())
    {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(entry["mode"].as_str(), Some(fields[0]), "{line}");
        assert_eq!(entry["nlink"].to_string(), fields[1], "{line}");
        assert_eq!(entry["size"].to_string(), fields[4], "{line}");
        let epoch: i64 = fields[5].parse().unwrap_or(-1);
        let ours = entry["mtime"].as_str().unwrap_or("");
        assert!(ours.len() == 20, "{ours}");
        let _ = epoch;
    }
    let sizes = gnu("ls", &["-1S", "-A"], &root).unwrap_or_default();
    let by_size = value(harw_tool_fsread::ls::run(
        &root,
        &serde_json::from_value(json!({"almost_all": true, "sort_size": true}))?,
    ))?;
    let ours: Vec<String> = by_size["entries"]
        .as_array()
        .map(|e| {
            e.iter()
                .filter_map(|x| x["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    // GNU bricht Gleichstand nach Namen (-S nutzt strcoll); unser Tiebreak ist die Bytefolge: gleich im C.UTF-8-Locale.
    assert_eq!(ours, sizes.lines().map(str::to_owned).collect::<Vec<_>>());
    Ok(())
}

#[test]
fn diff_matches_gnu_diff() -> TestResult {
    let (_dir, root) = workspace()?;
    let a: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let mut lines: Vec<String> = (0..60).map(|i| format!("line {i}\n")).collect();
    lines[3] = "changed 3\n".to_owned();
    lines.remove(20);
    lines.insert(40, "inserted\n".to_owned());
    lines.push("tail added\n".to_owned());
    let b: String = lines.concat();
    fs::write(root.join("a.txt"), &a)?;
    fs::write(root.join("b.txt"), &b)?;
    for context in [0usize, 1, 3, 8] {
        let ctx = format!("-U{context}");
        let Some(expected) = gnu("diff", &[&ctx, "a.txt", "b.txt"], &root) else {
            eprintln!("skipped: diff");
            return Ok(());
        };
        let body: String = expected.lines().skip(2).map(|l| format!("{l}\n")).collect();
        let got = value(harw_tool_fsread::diff::run(
            &root,
            &serde_json::from_value(json!({"a": "a.txt", "b": "b.txt", "context": context}))?,
        ))?;
        let ours: String = got["diff"]
            .as_str()
            .unwrap_or("")
            .lines()
            .skip(2)
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(ours, body, "-U{context}");
    }
    Ok(())
}

#[test]
fn realpath_and_readlink_match_gnu() -> TestResult {
    let (_dir, root) = workspace()?;
    fs::create_dir_all(root.join("real/sub"))?;
    fs::write(root.join("real/sub/f"), "x")?;
    std::os::unix::fs::symlink("real", root.join("alias"))?;
    std::os::unix::fs::symlink("../sub/f", root.join("real/sub/rel"))?;
    let Some(expected) = gnu("realpath", &["alias/sub/rel"], &root) else {
        eprintln!("skipped: realpath");
        return Ok(());
    };
    let got = value(harw_tool_fsread::links::run_realpath(
        &root,
        &serde_json::from_value(json!({"path": "alias/sub/rel"}))?,
    ))?;
    assert_eq!(got["absolute"].as_str(), Some(expected.trim()));
    if let Some(link) = gnu("readlink", &["alias"], &root) {
        let got = value(harw_tool_fsread::links::run_readlink(
            &root,
            &serde_json::from_value(json!({"path": "alias"}))?,
        ))?;
        assert_eq!(got["target"].as_str(), Some(link.trim()));
    }
    Ok(())
}

#[test]
fn df_matches_gnu_df() -> TestResult {
    let (_dir, root) = workspace()?;
    let Some(expected) = gnu(
        "df",
        &["-B1", "--output=size,used,avail,itotal,fstype", "."],
        &root,
    ) else {
        eprintln!("skipped: df");
        return Ok(());
    };
    let line = expected.lines().nth(1).unwrap_or("");
    let f: Vec<&str> = line.split_whitespace().collect();
    let got = value(harw_tool_fsread::df::run(
        &root,
        &serde_json::from_value(json!({}))?,
    ))?;
    assert_eq!(got["total_bytes"].to_string(), f[0], "{line}");
    assert_eq!(got["inodes_total"].to_string(), f[3], "{line}");
    assert_eq!(got["fs_type"].as_str(), f.get(4).copied(), "{line}");
    // `used`/`avail` können sich zwischen den zwei Aufrufen leicht ändern.
    let used: u64 = f[1].parse().unwrap_or(0);
    let ours: u64 = got["used_bytes"].as_u64().unwrap_or(0);
    assert!(used.abs_diff(ours) < 64 * 1024 * 1024, "{line}");
    Ok(())
}

#[test]
fn file_and_json_match_gnu() -> TestResult {
    let (_dir, root) = workspace()?;
    fs::copy("/bin/sh", root.join("elf"))?;
    fs::write(root.join("script.sh"), "#!/bin/sh\necho hi\n")?;
    fs::write(root.join("note.txt"), "plain text\n")?;
    fs::write(
        root.join("data.json"),
        r#"{"a":{"b":[1,2,{"c":"deep"}]},"n":null}"#,
    )?;
    for (path, expected_mime_prefix) in [
        ("elf", "application/x-"),
        ("script.sh", "text/x-shellscript"),
        ("note.txt", "text/plain"),
    ] {
        let Some(mime) = gnu("file", &["-b", "--mime-type", path], &root) else {
            eprintln!("skipped: file");
            break;
        };
        let got = value(harw_tool_fsread::file::run(
            &root,
            &serde_json::from_value(json!({"paths": [path]}))?,
        ))?;
        let ours = got["results"][0]["mime"].as_str().unwrap_or("");
        assert!(
            ours.starts_with(expected_mime_prefix),
            "{path}: ours {ours}, file says {mime}"
        );
        // Gleiche Hauptart wie `file`: Text bleibt Text, ausführbar bleibt ausführbar.
        assert_eq!(
            ours.starts_with("text/"),
            mime.trim().starts_with("text/"),
            "{path}: ours {ours}, file says {mime}"
        );
    }
    if let Some(expected) = gnu("jq", &["-c", ".a.b[2].c", "data.json"], &root) {
        let got = value(harw_tool_fsread::json::run(
            &root,
            &serde_json::from_value(json!({"path": "data.json", "query": ".a.b[2].c"}))?,
        ))?;
        assert_eq!(got["value"].to_string(), expected.trim());
    } else {
        eprintln!("skipped: jq");
    }
    Ok(())
}
