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

/// Wie weit nach einer Diagnose-Kopfzeile nach `-->` und Notizen gesucht
/// wird.
const DIAGNOSTIC_LOOKAHEAD: usize = 8;

/// Höchstzahl `note:`/`help:`-Zeilen je Diagnose ohne Ort.
const MAX_NOTES: usize = 3;

/// Zusammenfassungen von cargo/rustc ohne eigenen Befund.
const SUMMARY_PREFIXES: &[&str] = &[
    "error: could not compile",
    "error: aborting due to",
    "error: test failed",
    "error: build failed",
    "error: failed to run",
    "error: process didn't exit successfully",
];

/// Ergebnis von [`failure_lines`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FailureLines {
    /// Befundzeilen, bzw. ohne erkannte Befunde das Ende eines Stroms.
    pub(super) lines: Vec<String>,
    /// `true`, wenn `lines` erkannte Befunde sind (nicht der Rückfall).
    /// Dann trägt eine pfadlose Sammelzeile („Exit-Code passt nicht")
    /// nichts bei und würde nur jeden Worker wecken.
    pub(super) recognized: bool,
}

/// Verdichtet stdout und stderr eines fehlgeschlagenen Schritts.
///
/// # Returns
/// Höchstens [`MAX_FINDINGS`] Zeilen, dedupliziert, in Fundreihenfolge
/// (erst stderr, dann stdout). Jede Zeile mit Ort beginnt mit
/// `pfad:zeile:spalte: `, damit die Kappung auf [`MAX_LINE_CHARS`] den Ort
/// nie abschneidet. Ohne erkannte Befunde die letzten Zeilen von stderr
/// bzw. stdout (`recognized = false`). Leer nur, wenn beide Ströme leer
/// sind.
pub(super) fn failure_lines(stdout: &str, stderr: &str) -> FailureLines {
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
        return FailureLines {
            lines: tail(source),
            recognized: false,
        };
    }
    FailureLines {
        lines: findings.lines,
        recognized: true,
    }
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

/// `error[E…]: …` / `error: …` mit dem ersten folgenden `--> ort` als
/// `ort: kopfzeile`; ohne Ort (z. B. Linker) die Kopfzeile plus bis zu
/// [`MAX_NOTES`] folgende `note:`/`help:`-Zeilen, die den eigentlichen
/// Grund tragen (`cannot find -lssl`).
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
        let body: Vec<&str> = lines
            .iter()
            .skip(index.saturating_add(1))
            .take(DIAGNOSTIC_LOOKAHEAD)
            .map(|next| next.trim())
            .take_while(|next| !next.starts_with("error") && !next.starts_with("warning"))
            .collect();
        match body.iter().find_map(|next| next.strip_prefix("--> ")) {
            Some(location) => findings.push(format!("{}: {line}", location.trim())),
            None => {
                findings.push(line.to_owned());
                for note in body
                    .iter()
                    .filter_map(|next| note_text(next))
                    .take(MAX_NOTES)
                {
                    findings.push(format!("  {note}"));
                }
            }
        }
    }
}

/// `= note: …`, `note: …`, `= help: …`, `help: …` ohne das führende `=`.
fn note_text(line: &str) -> Option<&str> {
    let text = line.trim_start_matches('=').trim_start();
    (text.starts_with("note:") || text.starts_with("help:")).then_some(text)
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

/// `<ort>: test <name> failed: <meldung>` aus dem Rumpf eines Blocks.
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
        format!("{location}: test {name} failed")
    } else {
        format!("{location}: test {name} failed: {message}")
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

    const LINKER_STDERR: &str = "\
error: linking with `cc` failed: exit status: 1
  |
  = note: LC_ALL=\"C\" PATH=\"/usr/bin\" \"cc\" \"-m64\" \"/tmp/rustc/symbols.o\"
  = note: /usr/bin/ld: cannot find -lssl: No such file or directory
          collect2: error: ld returned 1 exit status

error: could not compile `harw-net` (bin \"harw-net\") due to 1 previous error
";

    #[test]
    fn test_compiler_errors_lead_with_their_location_and_skip_summaries() {
        let found = failure_lines("", COMPILE_STDERR);
        assert!(found.recognized);
        assert_eq!(
            found.lines,
            vec![
                "harw-parser/src/lexer.rs:42:13: error[E0308]: mismatched types".to_owned(),
                "harw-parser/src/ast.rs:7:5: error: cannot find value `y` in this scope".to_owned(),
            ]
        );
    }

    #[test]
    fn test_test_failures_are_read_from_stdout_with_location_and_message() {
        let stderr = "error: test failed, to rerun pass `-p harw-parser --lib`\n";
        let found = failure_lines(TEST_STDOUT, stderr);
        assert!(found.recognized);
        assert_eq!(
            found.lines,
            vec![
                "harw-parser/src/lexer.rs:88:9: test lexer::tests::splits_words failed: \
                 assertion `left == right` failed | left: 2 | right: 3"
                    .to_owned(),
                "harw-parser/src/ast.rs:120:5: test ast::tests::roundtrip failed: boom".to_owned(),
            ]
        );
    }

    /// Review #132: an over-long header must not clip away the location.
    #[test]
    fn test_the_location_survives_the_line_cap() {
        let header = format!("error[E0277]: {}", "x".repeat(290));
        let stderr = format!("{header}\n --> src/a.rs:1:1\n");
        let found = failure_lines("", &stderr);
        assert_eq!(found.lines.len(), 1);
        assert!(found.lines[0].starts_with("src/a.rs:1:1: error[E0277]: "));
        assert!(found.lines[0].chars().count() <= MAX_LINE_CHARS + 1);
    }

    /// Review #132: a locationless diagnostic keeps the notes with the cause.
    #[test]
    fn test_a_linker_failure_keeps_its_notes() {
        let found = failure_lines("", LINKER_STDERR);
        assert!(found.recognized);
        assert_eq!(
            found.lines[0],
            "error: linking with `cc` failed: exit status: 1"
        );
        assert!(
            found
                .lines
                .iter()
                .any(|line| line.contains("cannot find -lssl")),
            "{:?}",
            found.lines
        );
        assert!(
            !found
                .lines
                .iter()
                .any(|line| line.contains("could not compile"))
        );
    }

    #[test]
    fn test_failed_test_lines_are_used_without_detail_blocks() {
        let stdout = "test a::b ... FAILED\ntest a::c ... ok\n";
        assert_eq!(
            failure_lines(stdout, "").lines,
            vec!["test a::b failed".to_owned()]
        );
    }

    #[test]
    fn test_unknown_output_falls_back_to_the_stderr_tail() {
        let stderr = "1\n2\n\n3\n4\n5\n6\n";
        let found = failure_lines("ignored", stderr);
        assert!(!found.recognized);
        assert_eq!(found.lines, vec!["2", "3", "4", "5", "6"]);
        assert_eq!(
            failure_lines("only stdout\n", "  \n").lines,
            vec!["only stdout"]
        );
        assert!(failure_lines("", "").lines.is_empty());
    }

    #[test]
    fn test_findings_are_deduplicated_capped_and_stripped_of_ansi() {
        let mut stderr = String::new();
        for i in 0..40 {
            stderr.push_str(&format!("\u{1b}[1m\u{1b}[31merror\u{1b}[0m: e{}\n", i % 30));
        }
        let lines = failure_lines("", &stderr).lines;
        assert_eq!(lines.len(), MAX_FINDINGS);
        assert_eq!(lines[0], "error: e0");
        assert_eq!(lines.iter().filter(|line| *line == "error: e0").count(), 1);
    }
}
