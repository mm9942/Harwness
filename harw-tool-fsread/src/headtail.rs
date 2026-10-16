//! `fsread.head` und `fsread.tail` — `head`/`tail` in reinem Rust.
//!
//! `-n`/`--lines` und `-c`/`--bytes` schließen sich aus; ohne Angabe gelten
//! 10 Zeilen. `tail` kennt zusätzlich die `+N`-Form (`from_line`: ab Zeile N).
//!
//! # Grenzen
//! - Ausgabe höchstens [`crate::budget::MAX_TEXT_BYTES`] (48 KiB); darüber
//!   wird gekürzt (bei `tail` bleibt das **Ende**) und `truncated` gesetzt.
//! - `tail -n` sucht in den letzten [`TAIL_WINDOW`] (1 MiB) der Datei. Liegen
//!   weniger als N Zeilen im Fenster, ist das Ergebnis partiell und
//!   `truncated` gesetzt; `from_line` scannt höchstens [`TAIL_WINDOW`] ab Anfang.
//! - Binärdateien (NUL in den ersten 8 KiB) werden abgelehnt.
//! - Ungültiges UTF-8 wird beim Ausgeben ersetzt (`U+FFFD`).

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{MAX_TEXT_BYTES, clip_lossy, limit_or, ok};
use crate::io::{looks_binary, open_text, read_prefix};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Name von `fsread.head`.
pub const HEAD_TOOL: &str = "fsread.head";

/// Name von `fsread.tail`.
pub const TAIL_TOOL: &str = "fsread.tail";

/// Standard-Zeilenzahl.
pub const DEFAULT_LINES: usize = 10;

/// Höchstzahl Zeilen.
pub const MAX_LINES: usize = 5_000;

/// Suchfenster von `tail`.
pub const TAIL_WINDOW: u64 = 1024 * 1024;

/// Argumente für `fsread.head`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct HeadArgs {
    /// File relative to the workspace root.
    pub path: String,
    /// -n / --lines: number of lines (default 10, maximum 5000). Excludes bytes.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub lines: Option<usize>,
    /// -c / --bytes: number of bytes (maximum 49152). Excludes lines.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub bytes: Option<usize>,
}

/// Argumente für `fsread.tail`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct TailArgs {
    /// File relative to the workspace root.
    pub path: String,
    /// -n / --lines: number of last lines (default 10, maximum 5000). Excludes bytes and from_line.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub lines: Option<usize>,
    /// -c / --bytes: number of last bytes (maximum 49152). Excludes lines and from_line.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub bytes: Option<usize>,
    /// -n +N form: start output at this 1-based line (scans at most the first 1 MiB). Excludes lines and bytes.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub from_line: Option<usize>,
}

fn count_lines(bytes: &[u8]) -> usize {
    let newlines = bytes.iter().filter(|b| **b == b'\n').count();
    if bytes.last().is_some_and(|b| *b != b'\n') {
        newlines + 1
    } else {
        newlines
    }
}

/// Byte-Länge der ersten `n` Zeilen von `data` (inkl. Zeilenumbruch).
fn head_lines_len(data: &[u8], n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let mut seen = 0usize;
    for (index, byte) in data.iter().enumerate() {
        if *byte == b'\n' {
            seen += 1;
            if seen == n {
                return index + 1;
            }
        }
    }
    data.len()
}

/// Startindex der letzten `n` Zeilen in `data` (ein abschließendes `\n` zählt
/// nicht als eigene Zeile). `None`, wenn `data` weniger als `n` Zeilen hat.
fn tail_lines_start(data: &[u8], n: usize) -> Option<usize> {
    if n == 0 {
        return Some(data.len());
    }
    let mut count = 0usize;
    let last = data.len().checked_sub(1)?;
    for index in (0..=last).rev() {
        if data[index] == b'\n' && index != last {
            count += 1;
            if count == n {
                return Some(index + 1);
            }
        }
    }
    None
}

fn response(
    tool: &str,
    rel: &str,
    bytes: &[u8],
    size: u64,
    truncated: bool,
    more_available: bool,
    extra: Value,
) -> ToolOutput {
    let (content, clipped) = clip_lossy(bytes, MAX_TEXT_BYTES);
    let mut data = json!({
        "path": rel,
        "content": content,
        "bytes_returned": bytes.len().min(MAX_TEXT_BYTES),
        "lines_returned": count_lines(content.as_bytes()),
        "file_size": size,
        "truncated": truncated || clipped,
        "more_available": more_available || clipped,
    });
    if let (Some(map), Some(extra)) = (data.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            map.insert(key.clone(), value.clone());
        }
    }
    let summary = format!(
        "{} lines, {} bytes of {rel} ({size} bytes total){}",
        count_lines(content.as_bytes()),
        bytes.len().min(MAX_TEXT_BYTES),
        if truncated || clipped {
            ", truncated"
        } else {
            ""
        }
    );
    ok(tool, summary, data)
}

fn check_text(tool: &str, rel: &str, sample: &[u8]) -> Result<(), String> {
    if looks_binary(sample) {
        return Err(format!(
            "'{rel}' looks like a binary file; use fsread.file to identify it or fsread.hash to fingerprint it ({tool} only returns text)"
        ));
    }
    Ok(())
}

/// Führt `fsread.head` aus.
#[must_use]
pub fn run_head(root: &Path, args: &HeadArgs) -> ToolOutput {
    scoped(HEAD_TOOL, root, |scope| {
        if args.lines.is_some() && args.bytes.is_some() {
            return Err("lines and bytes are mutually exclusive".to_owned());
        }
        let (rel, mut file) = open_text(scope, &args.path)?;
        let size = file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| e.to_string())?;
        let shown = rel.display();
        // Ohne Zeilen-/Byte-Angabe: 10 Zeilen.
        let (data, more) =
            read_prefix(&mut file, MAX_TEXT_BYTES as u64).map_err(|e| e.to_string())?;
        check_text(HEAD_TOOL, &shown, &data)?;
        if let Some(bytes) = args.bytes {
            let want = bytes.clamp(1, MAX_TEXT_BYTES);
            let take = want.min(data.len());
            let cap_hit = bytes > MAX_TEXT_BYTES && data.len() >= MAX_TEXT_BYTES && more;
            let remaining = (take as u64) < size;
            return Ok(response(
                HEAD_TOOL,
                &shown,
                &data[..take],
                size,
                cap_hit,
                remaining,
                json!({"mode": "bytes", "requested": bytes}),
            ));
        }
        let n = limit_or(args.lines, DEFAULT_LINES, MAX_LINES);
        let len = head_lines_len(&data, n);
        let got_lines = count_lines(&data[..len]);
        // Gekürzt, wenn weniger Zeilen geliefert wurden als verlangt, obwohl die Datei weitergeht.
        let truncated = more && got_lines < n;
        let remaining = (len as u64) < size;
        Ok(response(
            HEAD_TOOL,
            &shown,
            &data[..len],
            size,
            truncated,
            remaining,
            json!({"mode": "lines", "requested": n}),
        ))
    })
}

fn read_window(file: &mut File, size: u64, window: u64) -> Result<(Vec<u8>, u64), String> {
    let start = size.saturating_sub(window);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.by_ref()
        .take(window)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok((buf, start))
}

/// Führt `fsread.tail` aus.
#[must_use]
pub fn run_tail(root: &Path, args: &TailArgs) -> ToolOutput {
    scoped(TAIL_TOOL, root, |scope| {
        let given = [
            args.lines.is_some(),
            args.bytes.is_some(),
            args.from_line.is_some(),
        ];
        if given.iter().filter(|g| **g).count() > 1 {
            return Err("lines, bytes and from_line are mutually exclusive".to_owned());
        }
        let (rel, mut file) = open_text(scope, &args.path)?;
        let size = file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| e.to_string())?;
        let shown = rel.display();

        if let Some(from_line) = args.from_line {
            if from_line == 0 {
                return Err("from_line is 1-based and must be at least 1".to_owned());
            }
            let (data, more) = read_prefix(&mut file, TAIL_WINDOW).map_err(|e| e.to_string())?;
            check_text(TAIL_TOOL, &shown, &data)?;
            let mut start = 0usize;
            let mut seen = 1usize;
            while seen < from_line {
                match data[start..].iter().position(|b| *b == b'\n') {
                    Some(offset) => {
                        start += offset + 1;
                        seen += 1;
                    }
                    None => {
                        return if more {
                            Err(format!(
                                "line {from_line} is beyond the scan window ({TAIL_WINDOW} bytes)"
                            ))
                        } else {
                            Ok(response(
                                TAIL_TOOL,
                                &shown,
                                b"",
                                size,
                                false,
                                false,
                                json!({"mode": "from_line", "requested": from_line, "note": "file has fewer lines"}),
                            ))
                        };
                    }
                }
            }
            let slice = &data[start..];
            let remaining = more;
            return Ok(response(
                TAIL_TOOL,
                &shown,
                slice,
                size,
                more && slice.len() >= MAX_TEXT_BYTES,
                remaining,
                json!({"mode": "from_line", "requested": from_line}),
            ));
        }

        if let Some(bytes) = args.bytes {
            let want = u64::try_from(bytes.clamp(1, MAX_TEXT_BYTES)).unwrap_or(0);
            let (data, start) = read_window(&mut file, size, want)?;
            check_text(TAIL_TOOL, &shown, &data)?;
            return Ok(response(
                TAIL_TOOL,
                &shown,
                &data,
                size,
                bytes > MAX_TEXT_BYTES,
                start > 0,
                json!({"mode": "bytes", "requested": bytes}),
            ));
        }

        let n = limit_or(args.lines, DEFAULT_LINES, MAX_LINES);
        let (data, window_start) = read_window(&mut file, size, TAIL_WINDOW)?;
        check_text(TAIL_TOOL, &shown, &data)?;
        let (start, partial) = match tail_lines_start(&data, n) {
            Some(start) => (start, false),
            // Weniger als n Zeilen im Fenster: alles, partiell nur wenn das Fenster nicht am Dateianfang beginnt.
            None => (0, window_start > 0),
        };
        let slice = &data[start..];
        let more_before = window_start > 0 || start > 0;
        // Bei Überlänge bleibt das Ende: von hinten kürzen.
        if slice.len() > MAX_TEXT_BYTES {
            let mut cut = slice.len() - MAX_TEXT_BYTES;
            while cut < slice.len() && (slice[cut] & 0xC0) == 0x80 {
                cut += 1;
            }
            return Ok(response(
                TAIL_TOOL,
                &shown,
                &slice[cut..],
                size,
                true,
                true,
                json!({"mode": "lines", "requested": n}),
            ));
        }
        Ok(response(
            TAIL_TOOL,
            &shown,
            slice,
            size,
            partial,
            more_before,
            json!({"mode": "lines", "requested": n}),
        ))
    })
}

/// Zeigt den Dateianfang wie `head`.
#[harw_macros::tool(
    name = "fsread.head",
    description = "Returns the first lines (-n, default 10, max 5000) or bytes (-c) of a workspace text file like head. Use when you need only the beginning of a file instead of running head or reading it whole. Returns JSON {content, lines_returned, bytes_returned, file_size, truncated, more_available}; output is capped at 48 KiB and binary files are refused.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_head(
    context: &ToolExecutionContext,
    args: HeadArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(HEAD_TOOL, move || run_head(&root, &args)).await
}

/// Zeigt das Dateiende wie `tail`.
#[harw_macros::tool(
    name = "fsread.tail",
    description = "Returns the last lines (-n, default 10, max 5000) or bytes (-c) of a workspace text file like tail, or everything from a given line (from_line, the -n +N form). Use when you need the end of a log or file instead of running tail. Returns JSON {content, lines_returned, file_size, truncated, more_available}; searches the last 1 MiB, output capped at 48 KiB keeping the end, binary files refused.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_tail(
    context: &ToolExecutionContext,
    args: TailArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TAIL_TOOL, move || run_tail(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};

    fn numbered(count: usize) -> Vec<u8> {
        (1..=count)
            .map(|n| format!("line{n}\n"))
            .collect::<String>()
            .into_bytes()
    }

    fn head(fx: &Fixture, args: Value) -> TestResult<Value> {
        json_of(run_head(&fx.ws, &serde_json::from_value(args)?))
    }

    fn tail(fx: &Fixture, args: Value) -> TestResult<Value> {
        json_of(run_tail(&fx.ws, &serde_json::from_value(args)?))
    }

    #[test]
    fn head_defaults_lines_and_bytes() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f", &numbered(25))?;
        let value = head(&fx, json!({"path": "f"}))?;
        assert_eq!(value["lines_returned"], 10);
        assert_eq!(
            value["content"]
                .as_str()
                .map(|c| c.starts_with("line1\n") && c.ends_with("line10\n")),
            Some(true)
        );
        assert_eq!(value["more_available"], true);
        assert_eq!(value["truncated"], false);
        let value = head(&fx, json!({"path": "f", "lines": 2}))?;
        assert_eq!(value["content"], "line1\nline2\n");
        let value = head(&fx, json!({"path": "f", "bytes": 8}))?;
        assert_eq!(value["content"], "line1\nli");
        let all = head(&fx, json!({"path": "f", "lines": 1000}))?;
        assert_eq!(all["lines_returned"], 25);
        assert_eq!(all["more_available"], false);
        Ok(())
    }

    #[test]
    fn head_handles_missing_trailing_newline_and_empty() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a", b"x\ny")?;
        fx.write("empty", b"")?;
        let value = head(&fx, json!({"path": "a", "lines": 5}))?;
        assert_eq!(value["content"], "x\ny");
        assert_eq!(value["lines_returned"], 2);
        let value = head(&fx, json!({"path": "empty"}))?;
        assert_eq!(value["content"], "");
        assert_eq!(value["more_available"], false);
        Ok(())
    }

    #[test]
    fn tail_lines_bytes_and_from_line() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f", &numbered(25))?;
        let value = tail(&fx, json!({"path": "f"}))?;
        assert_eq!(value["lines_returned"], 10);
        assert!(
            value["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("line16\n") && c.ends_with("line25\n"))
        );
        assert_eq!(value["more_available"], true);
        assert_eq!(
            tail(&fx, json!({"path": "f", "lines": 1}))?["content"],
            "line25\n"
        );
        assert_eq!(
            tail(&fx, json!({"path": "f", "bytes": 4}))?["content"],
            "e25\n"
        );
        let from = tail(&fx, json!({"path": "f", "from_line": 24}))?;
        assert_eq!(from["content"], "line24\nline25\n");
        let all = tail(&fx, json!({"path": "f", "lines": 100}))?;
        assert_eq!(all["lines_returned"], 25);
        assert_eq!(all["truncated"], false);
        assert_eq!(all["more_available"], false);
        let beyond = tail(&fx, json!({"path": "f", "from_line": 99}))?;
        assert_eq!(beyond["content"], "");
        Ok(())
    }

    #[test]
    fn tail_without_trailing_newline() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f", b"a\nb\nc")?;
        assert_eq!(
            tail(&fx, json!({"path": "f", "lines": 2}))?["content"],
            "b\nc"
        );
        Ok(())
    }

    #[test]
    fn tail_window_partial_is_flagged() -> TestResult {
        let fx = Fixture::new()?;
        // Eine einzige Zeile von 2 MiB: im 1-MiB-Fenster fehlt jeder Umbruch.
        let mut big = vec![b'x'; 2 * 1024 * 1024];
        big.push(b'\n');
        fx.write("big", &big)?;
        let value = tail(&fx, json!({"path": "big", "lines": 3}))?;
        assert_eq!(value["truncated"], true);
        assert!(
            value["bytes_returned"]
                .as_u64()
                .is_some_and(|b| b <= MAX_TEXT_BYTES as u64)
        );
        Ok(())
    }

    #[test]
    fn output_is_capped_at_48k() -> TestResult {
        let fx = Fixture::new()?;
        let line = format!("{}\n", "y".repeat(1000));
        fx.write("wide", line.repeat(200).as_bytes())?;
        let h = head(&fx, json!({"path": "wide", "lines": 200}))?;
        assert_eq!(h["truncated"], true);
        assert!(
            h["bytes_returned"]
                .as_u64()
                .is_some_and(|b| b <= MAX_TEXT_BYTES as u64)
        );
        let t = tail(&fx, json!({"path": "wide", "lines": 200}))?;
        assert_eq!(t["truncated"], true);
        assert!(t["content"].as_str().is_some_and(|c| c.ends_with("y\n")));
        Ok(())
    }

    #[test]
    fn rejects_binary_escape_secret_and_conflicts() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("bin", &[1, 2, 0, 3])?;
        fx.write(".env", b"A=1")?;
        fx.write("f", b"x")?;
        for path in [
            "bin",
            "../outside/secret.txt",
            "link_file",
            ".env",
            "missing",
            "nested",
        ] {
            error_of(run_head(
                &fx.ws,
                &serde_json::from_value(json!({"path": path}))?,
            ))?;
            error_of(run_tail(
                &fx.ws,
                &serde_json::from_value(json!({"path": path}))?,
            ))?;
        }
        error_of(run_head(
            &fx.ws,
            &serde_json::from_value(json!({"path": "f", "lines": 1, "bytes": 1}))?,
        ))?;
        error_of(run_tail(
            &fx.ws,
            &serde_json::from_value(json!({"path": "f", "lines": 1, "from_line": 1}))?,
        ))?;
        error_of(run_tail(
            &fx.ws,
            &serde_json::from_value(json!({"path": "f", "from_line": 0}))?,
        ))?;
        assert!(serde_json::from_value::<HeadArgs>(json!({"path": "f", "n": 3})).is_err());
        Ok(())
    }
}
