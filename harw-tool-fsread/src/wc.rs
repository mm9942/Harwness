//! `fsread.wc` — `wc` in reinem Rust.
//!
//! Zählt Zeilen (`-l`), Wörter (`-w`), Bytes (`-c`), Zeichen (`-m`) und die
//! längste Zeile (`-L`) streamend, ohne die Datei in den Speicher zu laden.
//!
//! # Genauigkeit bei kaputtem UTF-8
//! `chars` zählt Bytes, die keine UTF-8-Folgebytes sind (`0b10xxxxxx`); gültiges
//! UTF-8 wird damit exakt, ungültige Folgen näherungsweise gezählt, ohne zu
//! scheitern. Wörter sind durch ASCII-Leerraum getrennt. `max_line_length`
//! zählt Anzeigespalten wie GNU `wc -L` (Tabs bis zum nächsten Vielfachen von
//! 8, Steuerzeichen ohne Breite, jedes Zeichen Breite 1).
//!
//! # Grenzen
//! Höchstens [`crate::io::MAX_STREAM_BYTES`] (64 MiB) je Datei; darüber ist
//! das Ergebnis der Präfix und `truncated` gesetzt.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{flag, ok};
use crate::io::{MAX_STREAM_BYTES, open_text};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.wc";

/// Höchstzahl Pfade je Aufruf.
pub const MAX_PATHS: usize = 32;

/// Argumente für `fsread.wc`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct WcArgs {
    /// Files relative to the workspace root (1-32).
    pub paths: Vec<String>,
    /// -l: count lines (newline characters).
    #[serde(default)]
    pub lines: Option<bool>,
    /// -w: count words (separated by ASCII whitespace).
    #[serde(default)]
    pub words: Option<bool>,
    /// -c: count bytes.
    #[serde(default)]
    pub bytes: Option<bool>,
    /// -m: count characters (UTF-8 aware; invalid sequences approximated).
    #[serde(default)]
    pub chars: Option<bool>,
    /// -L: length of the longest line in display columns.
    #[serde(default)]
    pub max_line_length: Option<bool>,
}

/// Zählerstände.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Zeilenumbrüche.
    pub lines: u64,
    /// Wörter.
    pub words: u64,
    /// Bytes.
    pub bytes: u64,
    /// Zeichen.
    pub chars: u64,
    /// Längste Zeile (Spalten).
    pub max_line: u64,
}

/// Streamender Zähler.
#[derive(Debug, Default)]
pub struct Counter {
    counts: Counts,
    in_word: bool,
    column: u64,
}

impl Counter {
    /// Verarbeitet einen Block.
    pub fn feed(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            self.counts.bytes += 1;
            if byte & 0xC0 != 0x80 {
                self.counts.chars += 1;
            }
            match byte {
                b'\n' => {
                    self.counts.lines += 1;
                    self.counts.max_line = self.counts.max_line.max(self.column);
                    self.column = 0;
                }
                b'\t' => self.column = (self.column / 8 + 1) * 8,
                0x00..=0x1f | 0x7f => {}
                _ if byte & 0xC0 == 0x80 => {}
                _ => self.column += 1,
            }
            let space = matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
            if space {
                self.in_word = false;
            } else if !self.in_word {
                self.in_word = true;
                self.counts.words += 1;
            }
        }
    }

    /// Beendet den Zähler.
    #[must_use]
    pub fn finish(mut self) -> Counts {
        self.counts.max_line = self.counts.max_line.max(self.column);
        self.counts
    }
}

/// Zählt `reader` bis höchstens `cap` Bytes; `true`, wenn mehr folgte.
///
/// # Errors
/// I/O-Fehler.
pub fn count_reader<R: Read>(reader: &mut R, cap: u64) -> std::io::Result<(Counts, bool)> {
    let mut counter = Counter::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let remaining = cap.saturating_sub(total);
        if remaining == 0 {
            // Prüfen, ob noch etwas folgt.
            let mut probe = [0u8; 1];
            let more = reader.read(&mut probe)? > 0;
            return Ok((counter.finish(), more));
        }
        let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
        let read = reader.read(&mut buf[..want])?;
        if read == 0 {
            return Ok((counter.finish(), false));
        }
        counter.feed(&buf[..read]);
        total += read as u64;
    }
}

/// Führt `fsread.wc` aus.
#[must_use]
pub fn run(root: &Path, args: &WcArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        if args.paths.is_empty() {
            return Err("paths must contain at least one file".to_owned());
        }
        if args.paths.len() > MAX_PATHS {
            return Err(format!("too many paths (max {MAX_PATHS})"));
        }
        let mut selected = [
            flag(args.lines),
            flag(args.words),
            flag(args.bytes),
            flag(args.chars),
            flag(args.max_line_length),
        ];
        if selected.iter().all(|s| !*s) {
            selected = [true, true, true, false, false];
        }
        let [want_lines, want_words, want_bytes, want_chars, want_max] = selected;
        let render = |path: &str, counts: &Counts, truncated: bool| -> Value {
            let mut value = json!({"path": path, "ok": true, "truncated": truncated});
            if want_lines {
                value["lines"] = json!(counts.lines);
            }
            if want_words {
                value["words"] = json!(counts.words);
            }
            if want_bytes {
                value["bytes"] = json!(counts.bytes);
            }
            if want_chars {
                value["chars"] = json!(counts.chars);
            }
            if want_max {
                value["max_line_length"] = json!(counts.max_line);
            }
            value
        };
        let mut results = Vec::new();
        let mut total = Counts::default();
        let mut any_truncated = false;
        let mut counted = 0usize;
        for input in &args.paths {
            match open_text(scope, input) {
                Err(message) => results.push(json!({"path": input, "ok": false, "error": message})),
                Ok((rel, mut file)) => match count_reader(&mut file, MAX_STREAM_BYTES) {
                    Err(error) => results.push(json!({
                        "path": rel.display(),
                        "ok": false,
                        "error": crate::scope::io_message(&error),
                    })),
                    Ok((counts, truncated)) => {
                        total.lines += counts.lines;
                        total.words += counts.words;
                        total.bytes += counts.bytes;
                        total.chars += counts.chars;
                        total.max_line = total.max_line.max(counts.max_line);
                        any_truncated |= truncated;
                        counted += 1;
                        results.push(render(&rel.display(), &counts, truncated));
                    }
                },
            }
        }
        let failed = results.iter().filter(|r| r["ok"] == false).count();
        let mut data = json!({
            "results": results,
            "failed": failed,
            "truncated": any_truncated,
        });
        if counted > 1 {
            data["total"] = render("total", &total, any_truncated);
        }
        Ok(ok(
            TOOL,
            format!(
                "{counted} files counted, {failed} failed{}",
                if any_truncated { ", truncated" } else { "" }
            ),
            data,
        ))
    })
}

/// Zählt Zeilen, Wörter, Bytes wie `wc`.
#[harw_macros::tool(
    name = "fsread.wc",
    description = "Counts lines (-l), words (-w), bytes (-c), characters (-m) and the longest line (-L) of up to 32 workspace files like wc, streaming and bounded to 64 MiB per file. Use when you need sizes or line counts instead of running wc. Returns JSON {results:[{path, lines, words, bytes,...}], total, failed, truncated}. Default counts are lines, words and bytes.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_wc(context: &ToolExecutionContext, args: WcArgs) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};

    fn wc(fx: &Fixture, args: Value) -> TestResult<Value> {
        json_of(run(&fx.ws, &serde_json::from_value(args)?))
    }

    #[test]
    fn counts_match_gnu_wc_for_plain_text() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a", b"one two\nthree\n\nfour five six")?;
        let value = wc(&fx, json!({"paths": ["a"]}))?;
        let r = &value["results"][0];
        assert_eq!(r["lines"], 3);
        assert_eq!(r["words"], 6);
        assert_eq!(r["bytes"], 28);
        assert!(r["chars"].is_null());
        Ok(())
    }

    #[test]
    fn chars_and_max_line_with_utf8_and_tabs() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("u", "grüße\n\tab\n".as_bytes())?;
        let value = wc(
            &fx,
            json!({"paths": ["u"], "chars": true, "max_line_length": true, "bytes": true}),
        )?;
        let r = &value["results"][0];
        assert_eq!(r["bytes"], 12);
        assert_eq!(r["chars"], 10);
        assert_eq!(r["max_line_length"], 10);
        Ok(())
    }

    #[test]
    fn broken_utf8_and_binary_do_not_fail() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("bad", &[0xff, 0xfe, b'a', b'\n', 0x80, 0x00])?;
        let value = wc(
            &fx,
            json!({"paths": ["bad"], "bytes": true, "chars": true, "lines": true}),
        )?;
        let r = &value["results"][0];
        assert_eq!(r["ok"], true);
        assert_eq!(r["bytes"], 6);
        assert_eq!(r["lines"], 1);
        Ok(())
    }

    #[test]
    fn totals_and_per_file_errors() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a", b"x\n")?;
        fx.write("b", b"y\nz\n")?;
        fx.write(".env", b"SECRET=1")?;
        fx.plant_escapes()?;
        let value = wc(
            &fx,
            json!({"paths": ["a", "b", "missing", "../outside/secret.txt", ".env", "link_file"], "lines": true}),
        )?;
        assert_eq!(value["total"]["lines"], 3);
        assert_eq!(value["failed"], 4);
        for index in 2..6 {
            assert_eq!(value["results"][index]["ok"], false);
        }
        Ok(())
    }

    #[test]
    fn cap_marks_truncation() -> TestResult {
        let mut data: &[u8] = b"abcdefghij";
        let (counts, more) = count_reader(&mut data, 4)?;
        assert_eq!(counts.bytes, 4);
        assert!(more);
        let mut data: &[u8] = b"abcd";
        let (counts, more) = count_reader(&mut data, 4)?;
        assert_eq!(counts.bytes, 4);
        assert!(!more);
        Ok(())
    }

    #[test]
    fn bad_arguments_rejected() -> TestResult {
        let fx = Fixture::new()?;
        error_of(run(&fx.ws, &serde_json::from_value(json!({"paths": []}))?))?;
        let many: Vec<String> = (0..=MAX_PATHS).map(|i| format!("f{i}")).collect();
        error_of(run(
            &fx.ws,
            &serde_json::from_value(json!({"paths": many}))?,
        ))?;
        assert!(
            serde_json::from_value::<WcArgs>(json!({"paths": ["a"], "chars_only": true})).is_err()
        );
        Ok(())
    }
}
