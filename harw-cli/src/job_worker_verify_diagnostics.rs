//! Befundzeilen aus der Ausgabe eines fehlgeschlagenen Verifikationsschritts.
//!
//! # Warum
//! Worker bauen und testen nie selbst (DEC-004); die zentrale Verifikation
//! ist ihre einzige Rückmeldung. Das Ende von stderr allein trägt diese
//! Rückmeldung nicht: bei `cargo test` stehen die Fehlschläge (Panic-Ort,
//! Assertion) auf **stdout**, und die letzten stderr-Zeilen sind
//! Zusammenfassungen wie `error: could not compile …` ohne Dateipfad — das
//! Routing von `failing_lines_for` kann sie keinem Worker zuordnen und
//! schickt sie an alle.
//!
//! [`failure_lines`] liest deshalb beide Ströme und zieht daraus
//! Compiler-Diagnosen (Kopfzeile plus `--> pfad:zeile:spalte`) und
//! Test-Fehlschläge (Testname, Panic-Ort, Meldung). Pfade bleiben so in der
//! Zeile, wie `cargo` sie im Workspace ausgibt (relativ zur Wurzel), damit
//! das Routing sie den `owned_paths` zuordnet. Findet sich nichts davon,
//! bleibt es beim bisherigen Verhalten: die letzten Zeilen von stderr
//! (bzw. stdout, wenn stderr leer ist).

use std::collections::BTreeSet;

/// Höchstzahl Befundzeilen je Schritt (begrenzt das Feedback je Worker).
pub(super) const MAX_FINDINGS: usize = 20;

/// Höchstlänge einer Befundzeile in Zeichen.
const MAX_LINE_CHARS: usize = 300;

/// Rückfall ohne erkannte Befunde: so viele letzte, nicht leere Zeilen.
const FALLBACK_TAIL_LINES: usize = 5;

/// Wie weit nach einer Diagnose-Kopfzeile nach `-->` gesucht wird.
const LOCATION_LOOKAHEAD: usize = 6;

/// Zusammenfassungen von cargo/rustc ohne eigenen Befund.
const SUMMARY_PREFIXES: &[&str] = &[
    "error: could not compile",
    "error: aborting due to",
    "error: test failed",
    "error: build failed",
    "error: failed to run",
    "error: process didn't exit successfully",
];

/// Verdichtet stdout und stderr eines fehlgeschlagenen Schritts.
///
/// # Returns
/// Höchstens [`MAX_FINDINGS`] Zeilen, dedupliziert, in Fundreihenfolge
/// (erst stderr, dann stdout); ohne erkannte Befunde die letzten Zeilen
/// von stderr bzw. stdout. Leer nur, wenn beide Ströme leer sind.
pub(super) fn failure_lines(stdout: &str, stderr: &str) -> Vec<String> {
    let stdout = strip_ansi(stdout);
    let stderr = strip_ansi(stderr);
    let mut findings = Findings::default();
    for text in [stderr.as_str(), stdout.as_str()] {
        let lines: Vec<&str> = text.lines().collect();
        compiler_diagnostics(&lines, &mut findings);
        test_failures(&lines, &mut findings);
    }
    if findings.lines.is_empty() {
        let source = if stderr.trim().is_empty() {
            &stdout
        } else {
            &stderr
        };
        return tail(source);
    }
    findings.lines
}

/// Gesammelte Befunde, dedupliziert und gedeckelt.
#[derive(Default)]
struct Findings {
    lines: Vec<String>,
    seen: BTreeSet<String>,
}

impl Findings {
    fn push(&mut self, line: String) {
        if self.lines.len() >= MAX_FINDINGS {
            return;
        }
        let line = clip(&line);
        if self.seen.insert(line.clone()) {
            self.lines.push(line);
        }
    }
}

/// `error[E…]: …` / `error: …` mit dem ersten folgenden `--> ort`.
fn compiler_diagnostics(lines: &[&str], findings: &mut Findings) {
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        let is_error = line.starts_with("error[") || line.starts_with("error:");
        if !is_error
            || SUMMARY_PREFIXES
                .iter()
                .any(|prefix| line.starts_with(prefix))
        {
            continue;
        }
        let location = lines
            .iter()
            .skip(index.saturating_add(1))
            .take(LOCATION_LOOKAHEAD)
            .map(|next| next.trim())
            .take_while(|next| !next.starts_with("error"))
            .find_map(|next| next.strip_prefix("--> "));
        match location {
            Some(location) => findings.push(format!("{line} at {}", location.trim())),
            None => findings.push(line.to_owned()),
        }
    }
}

/// Blöcke `---- <test> stdout ----` mit Panic-Ort und Meldung; ohne solche
/// Blöcke die Zeilen `test <name> ... FAILED`.
fn test_failures(lines: &[&str], findings: &mut Findings) {
    let mut found_block = false;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|rest| rest.strip_suffix(" stdout ----"))
        else {
            index = index.saturating_add(1);
            continue;
        };
        found_block = true;
        let body: Vec<&str> = lines
            .iter()
            .skip(index.saturating_add(1))
            .map(|next| next.trim())
            .take_while(|next| !next.starts_with("---- ") && *next != "failures:")
            .collect();
        index = index.saturating_add(1).saturating_add(body.len());
        findings.push(describe_test_failure(name, &body));
    }
    if found_block {
        return;
    }
    for line in lines {
        let line = line.trim();
        if let Some(name) = line
            .strip_prefix("test ")
            .and_then(|rest| rest.strip_suffix(" ... FAILED"))
        {
            findings.push(format!("test {name} failed"));
        }
    }
}

/// `test <name> failed at <ort>: <meldung>` aus dem Rumpf eines Blocks.
fn describe_test_failure(name: &str, body: &[&str]) -> String {
    let panic = body
        .iter()
        .position(|line| line.starts_with("thread '") && line.contains("panicked at"));
    let Some(position) = panic else {
        let first = body.iter().find(|line| !line.is_empty()).copied();
        return match first {
            Some(text) => format!("test {name} failed: {text}"),
            None => format!("test {name} failed"),
        };
    };
    let header = body[position];
    let after = header
        .split_once("panicked at ")
        .map_or("", |(_, rest)| rest.trim());
    // Neues Format: `… panicked at pfad:z:s:` und die Meldung in den
    // Folgezeilen; altes Format: `… panicked at 'meldung', pfad:z:s`.
    let (location, inline) = match after.strip_suffix(':') {
        Some(location) => (location.to_owned(), None),
        None => match after.rsplit_once("', ") {
            Some((message, location)) => (
                location.to_owned(),
                Some(message.trim_start_matches('\'').to_owned()),
            ),
            None => (after.to_owned(), None),
        },
    };
    let message = inline.unwrap_or_else(|| {
        body.iter()
            .skip(position.saturating_add(1))
            .take_while(|line| !line.is_empty() && !line.starts_with("note:"))
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join(" | ")
    });
    if message.is_empty() {
        format!("test {name} failed at {location}")
    } else {
        format!("test {name} failed at {location}: {message}")
    }
}

/// Die letzten [`FALLBACK_TAIL_LINES`] nicht leeren Zeilen.
fn tail(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let skip = lines.len().saturating_sub(FALLBACK_TAIL_LINES);
    lines.into_iter().skip(skip).map(clip).collect()
}

fn clip(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_owned();
    }
    let mut clipped: String = line.chars().take(MAX_LINE_CHARS).collect();
    clipped.push('…');
    clipped
}

/// Entfernt ANSI-Farbsequenzen (`ESC [ … Buchstabe`).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPILE_STDERR: &str = "\
   Compiling harw-parser v0.9.1 (/ws/harw-parser)
error[E0308]: mismatched types
  --> harw-parser/src/lexer.rs:42:13
   |
42 |     let x: u32 = \"no\";
   |            ---   ^^^^ expected `u32`, found `&str`

error: cannot find value `y` in this scope
 --> harw-parser/src/ast.rs:7:5

error: could not compile `harw-parser` (lib) due to 2 previous errors
";

    const TEST_STDOUT: &str = "\
running 3 tests
test lexer::tests::ok ... ok
test lexer::tests::splits_words ... FAILED
test ast::tests::roundtrip ... FAILED

failures:

---- lexer::tests::splits_words stdout ----

thread 'lexer::tests::splits_words' panicked at harw-parser/src/lexer.rs:88:9:
assertion `left == right` failed
  left: 2
 right: 3
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- ast::tests::roundtrip stdout ----
thread 'ast::tests::roundtrip' panicked at 'boom', harw-parser/src/ast.rs:120:5

failures:
    lexer::tests::splits_words
    ast::tests::roundtrip

test result: FAILED. 1 passed; 2 failed; 0 ignored
";

    #[test]
    fn test_compiler_errors_carry_their_location_and_skip_summaries() {
        let lines = failure_lines("", COMPILE_STDERR);
        assert_eq!(
            lines,
            vec![
                "error[E0308]: mismatched types at harw-parser/src/lexer.rs:42:13".to_owned(),
                "error: cannot find value `y` in this scope at harw-parser/src/ast.rs:7:5"
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn test_test_failures_are_read_from_stdout_with_location_and_message() {
        let stderr = "error: test failed, to rerun pass `-p harw-parser --lib`\n";
        let lines = failure_lines(TEST_STDOUT, stderr);
        assert_eq!(
            lines,
            vec![
                "test lexer::tests::splits_words failed at harw-parser/src/lexer.rs:88:9: \
                 assertion `left == right` failed | left: 2 | right: 3"
                    .to_owned(),
                "test ast::tests::roundtrip failed at harw-parser/src/ast.rs:120:5: boom"
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn test_failed_test_lines_are_used_without_detail_blocks() {
        let stdout = "test a::b ... FAILED\ntest a::c ... ok\n";
        assert_eq!(
            failure_lines(stdout, ""),
            vec!["test a::b failed".to_owned()]
        );
    }

    #[test]
    fn test_unknown_output_falls_back_to_the_stderr_tail() {
        let stderr = "1\n2\n\n3\n4\n5\n6\n";
        assert_eq!(
            failure_lines("ignored", stderr),
            vec!["2", "3", "4", "5", "6"]
        );
        assert_eq!(failure_lines("only stdout\n", "  \n"), vec!["only stdout"]);
        assert!(failure_lines("", "").is_empty());
    }

    #[test]
    fn test_findings_are_deduplicated_capped_and_stripped_of_ansi() {
        let mut stderr = String::new();
        for i in 0..40 {
            stderr.push_str(&format!("\u{1b}[1m\u{1b}[31merror\u{1b}[0m: e{}\n", i % 30));
        }
        let lines = failure_lines("", &stderr);
        assert_eq!(lines.len(), MAX_FINDINGS);
        assert_eq!(lines[0], "error: e0");
        assert_eq!(lines.iter().filter(|line| *line == "error: e0").count(), 1);
    }
}
