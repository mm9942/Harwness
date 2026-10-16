//! Parser für die Ausgabe von `cargo` (Kurzformat) und `libtest`.
//!
//! Alle Funktionen sind rein (Text → Struktur), tolerant gegenüber Rauschen
//! (Fortschrittszeilen, Backtraces, ANSI-Reste) und panicfrei auf beliebiger
//! Eingabe. Gelesen wird das **Kurzformat** (`--message-format=short`):
//!
//! ```text
//! src/lib.rs:7:5: error[E0308]: mismatched types: expected `i32`, found `&str`
//! src/lib.rs:2:9: warning: unused variable: `unused`: help: …
//! warning: `demo` (lib) generated 1 warning
//! error: could not compile `demo` (lib) due to 1 previous error; 1 warning emitted
//! ```

use std::collections::HashSet;

/// Schweregrad einer Meldung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Level {
    /// Fehler.
    Error,
    /// Warnung.
    Warning,
}

impl Level {
    /// Name für JSON (`error`/`warning`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// Eine Compiler- oder Cargo-Meldung.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    /// Schweregrad.
    pub level: Level,
    /// Fehlercode (`E0308`), falls vorhanden.
    pub code: Option<String>,
    /// Meldungstext.
    pub message: String,
    /// Datei, falls die Meldung eine Position hat.
    pub file: Option<String>,
    /// Zeile (1-basiert).
    pub line: Option<u32>,
    /// Spalte (1-basiert).
    pub column: Option<u32>,
}

/// `Finished`-Zeile von Cargo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// Profilname (`dev`, `release`, `test`, …).
    pub profile: String,
    /// Dauer wie von Cargo ausgegeben (`1.23s`, `1m 02s`).
    pub took: String,
}

/// Ergebnis von [`parse_cargo_output`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoOutput {
    /// Eindeutige Meldungen in Reihenfolge des Auftretens.
    pub diagnostics: Vec<Diagnostic>,
    /// Cargo-Zusammenfassungen (`could not compile …`, `… generated N warnings`).
    pub notes: Vec<String>,
    /// `Finished`-Zeile, falls vorhanden.
    pub finished: Option<Finished>,
    /// Zahl ausgeführter Test-/Doctest-Binärdateien (`Running …`, `Doc-tests …`).
    pub binaries: usize,
    /// Pfade der von `cargo doc` erzeugten Dokumentation.
    pub generated: Vec<String>,
}

fn parse_u32(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 9 || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Zerlegt `error[E0308]: msg` oder `warning: msg` (ohne Position).
fn level_and_rest(text: &str) -> Option<(Level, Option<String>, &str)> {
    let (level, rest) = match text.strip_prefix("error") {
        Some(rest) => (Level::Error, rest),
        None => (Level::Warning, text.strip_prefix("warning")?),
    };
    if let Some(after) = rest.strip_prefix('[') {
        let (code, tail) = after.split_once(']')?;
        let tail = tail.strip_prefix(": ")?;
        if code.is_empty()
            || code.len() > 32
            || !code
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        {
            return None;
        }
        return Some((level, Some(code.to_owned()), tail));
    }
    let tail = rest.strip_prefix(": ")?;
    Some((level, None, tail))
}

fn parse_located(line: &str) -> Option<Diagnostic> {
    let idx = [": error", ": warning"]
        .iter()
        .filter_map(|m| line.find(m))
        .min()?;
    let prefix = &line[..idx];
    let rest = &line[idx + 2..];
    let mut parts = prefix.rsplitn(3, ':');
    let column = parse_u32(parts.next()?)?;
    let line_no = parse_u32(parts.next()?)?;
    let file = parts.next()?;
    if file.is_empty() || file.starts_with(' ') {
        return None;
    }
    let (level, code, message) = level_and_rest(rest)?;
    Some(Diagnostic {
        level,
        code,
        message: message.trim().to_owned(),
        file: Some(file.to_owned()),
        line: Some(line_no),
        column: Some(column),
    })
}

fn is_cargo_summary(level: Level, message: &str) -> bool {
    match level {
        Level::Warning => message.starts_with('`') && message.contains(" generated "),
        Level::Error => {
            message.starts_with("could not compile ")
                || message.starts_with("test failed, to rerun")
                || message.starts_with("build failed, waiting")
        }
    }
}

/// Liest `Finished \`dev\` profile [..] target(s) in 0.50s`.
fn parse_finished(line: &str) -> Option<Finished> {
    let rest = line.trim_start().strip_prefix("Finished ")?;
    let profile = rest.strip_prefix('`')?.split_once('`')?.0.to_owned();
    let took = rest.rsplit_once(" in ")?.1.trim().to_owned();
    if profile.is_empty() || took.is_empty() || took.len() > 24 {
        return None;
    }
    Some(Finished { profile, took })
}

/// Wertet die Cargo-Ausgabe (stderr) aus.
#[must_use]
pub fn parse_cargo_output(text: &str) -> CargoOutput {
    let mut out = CargoOutput::default();
    let mut seen: HashSet<Diagnostic> = HashSet::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        let diagnostic = parse_located(line).or_else(|| {
            let (level, code, message) = level_and_rest(line)?;
            if is_cargo_summary(level, message) {
                return None;
            }
            Some(Diagnostic {
                level,
                code,
                message: message.trim().to_owned(),
                file: None,
                line: None,
                column: None,
            })
        });
        if let Some(diagnostic) = diagnostic {
            if seen.insert(diagnostic.clone()) {
                out.diagnostics.push(diagnostic);
            }
            continue;
        }
        if let Some((level, _, message)) = level_and_rest(line) {
            if is_cargo_summary(level, message) {
                out.notes.push(line.to_owned());
            }
            continue;
        }
        if let Some(finished) = parse_finished(line) {
            out.finished = Some(finished);
        } else if line.trim_start().starts_with("Running ")
            || line.trim_start().starts_with("Doc-tests ")
        {
            out.binaries += 1;
        } else if let Some(path) = line.trim_start().strip_prefix("Generated ") {
            out.generated.push(path.trim().to_owned());
        }
    }
    out
}

/// Zähler einer `test result:`-Zeile.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TestCounts {
    /// Bestanden.
    pub passed: u64,
    /// Fehlgeschlagen.
    pub failed: u64,
    /// Ignoriert.
    pub ignored: u64,
    /// Gemessen (Benchmarks).
    pub measured: u64,
    /// Herausgefiltert.
    pub filtered_out: u64,
    /// Laufzeit laut libtest in Sekunden.
    pub seconds: f64,
}

/// Parst `test result: ok. 12 passed; 0 failed; …; finished in 0.05s`.
#[must_use]
pub fn parse_test_result_line(line: &str) -> Option<(bool, TestCounts)> {
    let rest = line.trim().strip_prefix("test result: ")?;
    let (status, numbers) = rest.split_once(". ")?;
    let ok = match status {
        "ok" => true,
        "FAILED" => false,
        _ => return None,
    };
    let mut counts = TestCounts::default();
    for part in numbers.split(';') {
        let part = part.trim();
        if let Some(time) = part.strip_prefix("finished in ") {
            counts.seconds = time.trim_end_matches('s').parse().unwrap_or(0.0);
            continue;
        }
        let mut words = part.splitn(2, ' ');
        let number: u64 = words.next()?.parse().ok()?;
        match words.next()?.trim() {
            "passed" => counts.passed = number,
            "failed" => counts.failed = number,
            "ignored" => counts.ignored = number,
            "measured" => counts.measured = number,
            "filtered out" => counts.filtered_out = number,
            _ => {}
        }
    }
    Some((ok, counts))
}

/// Ein fehlgeschlagener Test mit Ursache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Testname.
    pub name: String,
    /// Ort der Panik (`src/lib.rs:15:18`), falls erkennbar.
    pub location: Option<String>,
    /// Erste Zeilen der Panikmeldung.
    pub message: Option<String>,
}

/// Zusammenfassung der libtest-Ausgabe (stdout).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TestSummary {
    /// Summe aller `test result:`-Zeilen.
    pub totals: TestCounts,
    /// Anzahl `test result:`-Zeilen (Binärdateien/Doctest-Läufe mit Ergebnis).
    pub results: usize,
    /// Summe der `running N tests`-Zeilen.
    pub tests_run: u64,
    /// Namen fehlgeschlagener Tests (`test x ... FAILED`).
    pub failed_tests: Vec<String>,
    /// Details aus den `---- name stdout ----`-Abschnitten.
    pub failures: Vec<Failure>,
    /// `true`, wenn mindestens eine Ergebniszeile `FAILED` meldet.
    pub any_failed: bool,
}

/// Wertet die libtest-Ausgabe aus.
#[must_use]
pub fn parse_test_output(stdout: &str) -> TestSummary {
    let mut summary = TestSummary::default();
    let lines: Vec<&str> = stdout.lines().collect();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].trim_end();
        if let Some((ok, counts)) = parse_test_result_line(line) {
            summary.results += 1;
            summary.any_failed |= !ok;
            summary.totals.passed += counts.passed;
            summary.totals.failed += counts.failed;
            summary.totals.ignored += counts.ignored;
            summary.totals.measured += counts.measured;
            summary.totals.filtered_out += counts.filtered_out;
            summary.totals.seconds += counts.seconds;
        } else if let Some(rest) = line.strip_prefix("running ") {
            if let Some(count) = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
            {
                summary.tests_run += count;
            }
        } else if let Some(name) = line
            .strip_prefix("test ")
            .and_then(|r| r.strip_suffix(" ... FAILED"))
        {
            if !summary.failed_tests.iter().any(|n| n == name) {
                summary.failed_tests.push(name.to_owned());
            }
        } else if let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"))
        {
            let mut failure = Failure {
                name: name.to_owned(),
                location: None,
                message: None,
            };
            let mut cursor = index + 1;
            while cursor < lines.len()
                && !lines[cursor].starts_with("---- ")
                && lines[cursor].trim() != "failures:"
            {
                let body = lines[cursor];
                if let Some(at) = body.find("panicked at ") {
                    let location = body[at + "panicked at ".len()..]
                        .trim_end_matches(':')
                        .trim();
                    failure.location = Some(location.chars().take(200).collect());
                    let mut message: Vec<&str> = Vec::new();
                    let mut next = cursor + 1;
                    while next < lines.len() && message.len() < 3 {
                        let candidate = lines[next];
                        if candidate.trim().is_empty()
                            || candidate.starts_with("stack backtrace:")
                            || candidate.starts_with("note: ")
                            || candidate.starts_with("---- ")
                        {
                            break;
                        }
                        message.push(candidate.trim());
                        next += 1;
                    }
                    if !message.is_empty() {
                        failure.message = Some(message.join(" | ").chars().take(300).collect());
                    }
                    break;
                }
                cursor += 1;
            }
            summary.failures.push(failure);
        }
        index += 1;
    }
    summary
}

/// Ein Abschnitt, den `cargo fmt --check` ändern würde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmtDiff {
    /// Datei (relativ zur Wurzel, wenn möglich).
    pub file: String,
    /// Erste betroffene Zeile.
    pub line: u32,
}

/// Wertet `Diff in <datei>:<zeile>:` aus; `root` wird als Präfix entfernt.
#[must_use]
pub fn parse_fmt_diffs(stdout: &str, root: &str) -> Vec<FmtDiff> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(rest) = line.trim_end().strip_prefix("Diff in ") else {
            continue;
        };
        let rest = rest.trim_end_matches(':');
        let (file, line_no) = if let Some((file, number)) = rest.rsplit_once(" at line ") {
            (file, number)
        } else if let Some((file, number)) = rest.rsplit_once(':') {
            (file, number)
        } else {
            continue;
        };
        let Some(number) = parse_u32(line_no.trim()) else {
            continue;
        };
        let file = file
            .strip_prefix(root)
            .map_or(file, |f| f.trim_start_matches('/'));
        out.push(FmtDiff {
            file: file.to_owned(),
            line: number,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    const CHECK_ERR: &str = "    Checking demo v0.1.0 (/work/demo)\n\
src/lib.rs:7:5: error[E0308]: mismatched types: expected `i32`, found `&str`\n\
src/lib.rs:2:9: warning: unused variable: `unused`: help: if this is intentional, prefix it with an underscore: `_unused`\n\
warning: `demo` (lib) generated 1 warning\n\
error: could not compile `demo` (lib) due to 1 previous error; 1 warning emitted\n";

    #[test]
    fn parses_errors_warnings_and_summaries() -> TestResult {
        let out = parse_cargo_output(CHECK_ERR);
        assert_eq!(out.diagnostics.len(), 2);
        let first = &out.diagnostics[0];
        assert_eq!(first.level, Level::Error);
        assert_eq!(first.code.as_deref(), Some("E0308"));
        assert_eq!(
            (first.file.as_deref(), first.line, first.column),
            (Some("src/lib.rs"), Some(7), Some(5))
        );
        assert!(first.message.starts_with("mismatched types"));
        assert_eq!(out.diagnostics[1].level, Level::Warning);
        assert_eq!(out.diagnostics[1].code, None);
        assert_eq!(out.notes.len(), 2);
        assert!(out.notes[1].contains("could not compile"));
        Ok(())
    }

    #[test]
    fn duplicates_are_collapsed_and_finished_is_read() -> TestResult {
        let text = "src/a.rs:1:1: warning: unused\nsrc/a.rs:1:1: warning: unused\nsrc/b.rs:1:1: warning: unused\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.50s\n";
        let out = parse_cargo_output(text);
        assert_eq!(out.diagnostics.len(), 2);
        assert_eq!(
            out.finished,
            Some(Finished {
                profile: "dev".into(),
                took: "0.50s".into()
            })
        );
        let slow =
            parse_cargo_output("    Finished `release` profile [optimized] target(s) in 1m 02s\n");
        assert_eq!(slow.finished.map(|f| f.took), Some("1m 02s".to_owned()));
        Ok(())
    }

    #[test]
    fn unlocated_cargo_errors_are_diagnostics_but_summaries_are_not() -> TestResult {
        let text = "error: no matching package named `nope` found\nerror: failed to parse manifest at `/x/Cargo.toml`\nwarning: unused manifest key: package.foo\nerror: test failed, to rerun pass `--lib`\nerror: could not compile `x` (lib test) due to 2 previous errors\n";
        let out = parse_cargo_output(text);
        assert_eq!(out.diagnostics.len(), 3);
        assert!(out.diagnostics.iter().all(|d| d.file.is_none()));
        assert_eq!(out.notes.len(), 2);
        Ok(())
    }

    #[test]
    fn counts_binaries_and_doc_output() -> TestResult {
        let text = "     Running unittests src/lib.rs (target/debug/deps/demo-1)\n     Running tests/it.rs (target/debug/deps/it-2)\n   Doc-tests demo\n Documenting demo v0.1.0 (/x)\n   Generated /x/target/doc/demo/index.html\n";
        let out = parse_cargo_output(text);
        assert_eq!(out.binaries, 3);
        assert_eq!(out.generated, vec!["/x/target/doc/demo/index.html"]);
        Ok(())
    }

    #[test]
    fn noise_and_garbage_never_panic() -> TestResult {
        for text in [
            "",
            ":::: error",
            "a:b:c: error: x",
            "src/x.rs:99999999999:1: error: big",
            "error[",
            "error[]: x",
            "error[E0\u{0}]: x",
            ": warning: ",
            "\u{1b}[31merror\u{1b}[0m: colored",
            "Finished",
            "Finished `",
            "Finished `a` in",
            "    Running",
            "x:1:1: warning",
        ] {
            let _ = parse_cargo_output(text);
        }
        let huge = "src/x.rs:1:1: warning: w\n".repeat(10_000);
        assert_eq!(parse_cargo_output(&huge).diagnostics.len(), 1);
        Ok(())
    }

    const TEST_OUT: &str = "\nrunning 2 tests\ntest tests::passes ... ok\ntest tests::fails ... FAILED\n\nfailures:\n\n---- tests::fails stdout ----\n\nthread 'tests::fails' (21388) panicked at src/lib.rs:15:18:\nassertion `left == right` failed: boom\n  left: 1\n right: 2\nstack backtrace:\n   0: __rustc::rust_begin_unwind\n\nfailures:\n    tests::fails\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s\n\nrunning 3 tests\ntest result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 2 filtered out; finished in 0.50s\n";

    #[test]
    fn parses_libtest_output() -> TestResult {
        let summary = parse_test_output(TEST_OUT);
        assert_eq!(summary.results, 2);
        assert_eq!(summary.tests_run, 5);
        assert_eq!(summary.totals.passed, 4);
        assert_eq!(summary.totals.failed, 1);
        assert_eq!(summary.totals.ignored, 1);
        assert_eq!(summary.totals.filtered_out, 2);
        assert!((summary.totals.seconds - 0.57).abs() < 1e-9);
        assert!(summary.any_failed);
        assert_eq!(summary.failed_tests, vec!["tests::fails"]);
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(
            summary.failures[0].location.as_deref(),
            Some("src/lib.rs:15:18")
        );
        assert_eq!(
            summary.failures[0].message.as_deref(),
            Some("assertion `left == right` failed: boom | left: 1 | right: 2")
        );
        Ok(())
    }

    #[test]
    fn test_result_line_edge_cases() -> TestResult {
        assert!(parse_test_result_line("test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s").is_some());
        assert_eq!(
            parse_test_result_line("test result: ok. 1 passed").map(|(_, c)| c.passed),
            Some(1)
        );
        for bad in [
            "",
            "test result: maybe. 1 passed",
            "test result: ok.",
            "test result: ok. x passed",
            "result ok",
        ] {
            assert!(parse_test_result_line(bad).is_none(), "{bad:?}");
        }
        let (ok, counts) = parse_test_result_line("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.5e3s").ok_or(crate::test_support::TestError::Missing("line"))?;
        assert!(ok);
        assert_eq!(counts.seconds, 1500.0);
        Ok(())
    }

    #[test]
    fn libtest_garbage_never_panics() -> TestResult {
        for text in [
            "",
            "running",
            "running x tests",
            "---- ",
            "---- x stdout ----",
            "---- x stdout ----\nthread panicked at",
            "test x ... FAILED",
            "test  ... FAILED",
        ] {
            let _ = parse_test_output(text);
        }
        let many = "test t ... FAILED\n".repeat(5000);
        assert_eq!(parse_test_output(&many).failed_tests.len(), 1);
        Ok(())
    }

    #[test]
    fn fmt_diffs_are_relative_to_the_root() -> TestResult {
        let text = "Diff in /work/demo/src/lib.rs:10:\n fn x() {}\nDiff in /work/demo/src/b.rs at line 3:\n y\nDiff in /elsewhere/c.rs:1:\nnot a diff line\nDiff in broken\n";
        let diffs = parse_fmt_diffs(text, "/work/demo");
        assert_eq!(
            diffs,
            vec![
                FmtDiff {
                    file: "src/lib.rs".into(),
                    line: 10
                },
                FmtDiff {
                    file: "src/b.rs".into(),
                    line: 3
                },
                FmtDiff {
                    file: "/elsewhere/c.rs".into(),
                    line: 1
                },
            ]
        );
        Ok(())
    }
}
