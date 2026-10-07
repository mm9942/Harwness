//! `fsread.cat` — begrenztes, `cat`-artiges Lesen mit Zeilennummern.
//!
//! Liest ab `offset` höchstens `max_bytes` (Standard und Maximum 48 KiB) und
//! liefert `next_offset` zum Weiterlesen. Mit `number_lines` (`cat -n`) werden
//! Zeilen im Format `%6d<TAB>` nummeriert; die Nummern sind absolute
//! Zeilennummern der Datei (das Zählen bis `offset` ist auf 8 MiB begrenzt).
//! Binärdateien werden abgelehnt, ungültiges UTF-8 beim Ausgeben ersetzt.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{MAX_TEXT_BYTES, clip_lossy, flag, ok};
use crate::io::{MAX_READ_BYTES, looks_binary, open_text, read_prefix};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.cat";

/// Argumente für `fsread.cat`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct CatArgs {
    /// File relative to the workspace root.
    pub path: String,
    /// -n / --number: prefix each output line with its absolute line number.
    #[serde(default)]
    pub number_lines: Option<bool>,
    /// Byte offset to start at (default 0). With number_lines at most 8 MiB.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub offset: Option<u64>,
    /// Maximum bytes to return (default and maximum 49152).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_bytes: Option<usize>,
}

/// Nummeriert Zeilen im `cat -n`-Format, beginnend bei `first`.
#[must_use]
pub fn number(text: &str, first: u64) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for (line_no, line) in (first..).zip(text.split_inclusive('\n')) {
        out.push_str(&format!("{line_no:>6}\t"));
        out.push_str(line);
    }
    out
}

/// Führt `fsread.cat` aus.
#[must_use]
pub fn run(root: &Path, args: &CatArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        let (rel, mut file) = open_text(scope, &args.path)?;
        let shown = rel.display();
        let size = file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| e.to_string())?;
        let offset = args.offset.unwrap_or(0);
        let numbered = flag(args.number_lines);
        let want = args
            .max_bytes
            .unwrap_or(MAX_TEXT_BYTES)
            .clamp(1, MAX_TEXT_BYTES);
        if offset > size {
            return Err(format!(
                "offset {offset} is beyond the end of the file ({size} bytes)"
            ));
        }
        // Erste Zeilennummer: Zeilenumbrüche vor `offset` zählen.
        let mut first_line = 1u64;
        if numbered && offset > 0 {
            if offset > MAX_READ_BYTES {
                return Err("number_lines with an offset above 8 MiB is not supported".to_owned());
            }
            let mut before = Vec::new();
            file.by_ref()
                .take(offset)
                .read_to_end(&mut before)
                .map_err(|e| e.to_string())?;
            first_line += before.iter().filter(|b| **b == b'\n').count() as u64;
        } else if offset > 0 {
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| e.to_string())?;
        }
        let (data, more) = read_prefix(&mut file, want as u64).map_err(|e| e.to_string())?;
        if looks_binary(&data) {
            return Err(format!(
                "'{shown}' looks like a binary file; use fsread.file or fsread.hash instead"
            ));
        }
        let (text, _) = clip_lossy(&data, MAX_TEXT_BYTES);
        let content = if numbered {
            number(&text, first_line)
        } else {
            text
        };
        let end = offset + data.len() as u64;
        let truncated = more;
        let summary = format!(
            "{} bytes of {shown} from offset {offset} ({size} bytes total){}",
            data.len(),
            if more { ", more available" } else { "" }
        );
        Ok(ok(
            TOOL,
            summary,
            json!({
                "path": shown,
                "content": content,
                "offset": offset,
                "bytes_returned": data.len(),
                "next_offset": if more { Some(end) } else { None },
                "file_size": size,
                "truncated": truncated,
                "more_available": more,
            }),
        ))
    })
}

/// Liest eine Datei begrenzt wie `cat`.
#[harw_macros::tool(
    name = "fsread.cat",
    description = "Reads a bounded slice of a workspace text file like cat, optionally with line numbers (-n) and a byte offset; returns next_offset to continue. Use when you need file contents with stable line numbers instead of running cat. Returns JSON {content, offset, bytes_returned, next_offset, file_size, truncated}; at most 48 KiB per call, binary files refused, secret files (.env, keys) never read.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_cat(
    context: &ToolExecutionContext,
    args: CatArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};
    use serde_json::Value;

    fn cat(fx: &Fixture, args: Value) -> TestResult<Value> {
        json_of(run(&fx.ws, &serde_json::from_value(args)?))
    }

    #[test]
    fn numbers_lines_like_cat_n() -> TestResult {
        assert_eq!(number("a\nb", 1), "     1\ta\n     2\tb");
        assert_eq!(number("", 1), "");
        Ok(())
    }

    #[test]
    fn plain_and_numbered_with_offset() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f", b"one\ntwo\nthree\n")?;
        let value = cat(&fx, json!({"path": "f"}))?;
        assert_eq!(value["content"], "one\ntwo\nthree\n");
        assert_eq!(value["more_available"], false);
        assert!(value["next_offset"].is_null());
        let value = cat(&fx, json!({"path": "f", "number_lines": true, "offset": 4}))?;
        assert_eq!(value["content"], "     2\ttwo\n     3\tthree\n");
        Ok(())
    }

    #[test]
    fn paging_with_next_offset() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f", b"abcdefghij")?;
        let first = cat(&fx, json!({"path": "f", "max_bytes": 4}))?;
        assert_eq!(first["content"], "abcd");
        assert_eq!(first["next_offset"], 4);
        let second = cat(&fx, json!({"path": "f", "max_bytes": 4, "offset": 4}))?;
        assert_eq!(second["content"], "efgh");
        let third = cat(&fx, json!({"path": "f", "max_bytes": 4, "offset": 8}))?;
        assert_eq!(third["content"], "ij");
        assert!(third["next_offset"].is_null());
        Ok(())
    }

    #[test]
    fn rejects_bad_offset_binary_secret_escape() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("f", b"abc")?;
        fx.write("bin", &[0, 1, 2])?;
        fx.write("keys/server.pem", b"-----BEGIN-----")?;
        for args in [
            json!({"path": "f", "offset": 99}),
            json!({"path": "bin"}),
            json!({"path": "keys/server.pem"}),
            json!({"path": "../outside/secret.txt"}),
            json!({"path": "link_file"}),
            json!({"path": "loop/f"}),
        ] {
            error_of(run(&fx.ws, &serde_json::from_value(args)?))?;
        }
        Ok(())
    }

    #[test]
    fn hostile_max_bytes_is_clamped() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("big", &vec![b'x'; 100_000])?;
        let value = cat(
            &fx,
            json!({"path": "big", "max_bytes": 18446744073709551615u64}),
        )?;
        assert_eq!(value["bytes_returned"], MAX_TEXT_BYTES);
        assert_eq!(value["truncated"], true);
        Ok(())
    }
}
