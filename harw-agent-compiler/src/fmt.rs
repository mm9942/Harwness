//! `harw agent fmt`: canonical formatting of definition files.
//!
//! # Decision
//! `toml_edit` is already in the lockfile, so formatting works on its
//! document model and keeps every comment. It is deliberately conservative
//! and changes only layout, never values:
//! 1. The header keys come first, in the DSL order ([`HEADER_ORDER`]);
//!    every other top-level key keeps its relative order. Tables keep
//!    theirs.
//! 2. `key = value` with exactly one space around `=`; a trailing comment
//!    after a value is kept, separated by one space.
//! 3. Trailing whitespace is removed, runs of blank lines collapse to one,
//!    the file starts without blank lines and ends with exactly one newline.
//!
//! Arrays, inline tables and strings are left as written. The result must
//! parse to the same TOML values as the input; if step 3 would change a
//! value (trailing whitespace inside a multi-line string), it is skipped.
//! Formatting is idempotent: `fmt(fmt(x)) == fmt(x)`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use toml_edit::{DocumentMut, Item, Table};

use crate::error::CompileError;

/// The canonical order of the top-level header keys.
pub const HEADER_ORDER: &[&str] = &[
    "schema",
    "id",
    "version",
    "extends",
    "mixins",
    "role",
    "specialization",
    "name",
    "description",
    "reasoning_effort",
    "instructions_file",
    "skills",
];

/// Formats a definition's text.
///
/// # Errors
/// `Err(message)` if the text is not valid TOML.
pub fn format_definition(text: &str) -> Result<String, String> {
    let mut document: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| error.to_string())?;
    let root = document.as_table_mut();
    root.sort_values_by(|left, _, right, _| rank(left.get()).cmp(&rank(right.get())));
    normalize_table(root);
    let structural = document.to_string();
    let tidy = tidy_lines(&structural);
    let same = |a: &str, b: &str| {
        matches!(
            (toml::from_str::<toml::Table>(a), toml::from_str::<toml::Table>(b)),
            (Ok(left), Ok(right)) if left == right
        )
    };
    if same(&tidy, text) {
        Ok(tidy)
    } else if same(&structural, text) {
        Ok(ensure_final_newline(&structural))
    } else {
        Err("formatting would change a value; the file was left as is".to_owned())
    }
}

fn rank(key: &str) -> usize {
    HEADER_ORDER
        .iter()
        .position(|candidate| *candidate == key)
        .unwrap_or(HEADER_ORDER.len())
}

/// Normalizes `key = value` spacing in a table and its sub-tables.
fn normalize_table(table: &mut Table) {
    for (mut key, item) in table.iter_mut() {
        match item {
            Item::Value(value) => {
                key.leaf_decor_mut().set_suffix(" ");
                let decor = value.decor_mut();
                decor.set_prefix(" ");
                let comment = decor
                    .suffix()
                    .and_then(|raw| raw.as_str())
                    .map(str::trim)
                    .filter(|suffix| suffix.starts_with('#'))
                    .map(|suffix| format!(" {suffix}"));
                decor.set_suffix(comment.unwrap_or_default());
            }
            Item::Table(inner) => normalize_table(inner),
            Item::ArrayOfTables(array) => {
                for inner in array.iter_mut() {
                    normalize_table(inner);
                }
            }
            Item::None => {}
        }
    }
}

/// Removes trailing whitespace, collapses blank-line runs, trims the start
/// and ends with one newline.
fn tidy_lines(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut blank = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if blank || out.is_empty() {
                continue;
            }
            blank = true;
        } else {
            blank = false;
        }
        out.push(line);
    }
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

fn ensure_final_newline(text: &str) -> String {
    let mut out = text.trim_end_matches('\n').to_owned();
    out.push('\n');
    out
}

/// The result for one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FmtResult {
    /// The file.
    pub path: PathBuf,
    /// Whether formatting changes it.
    pub changed: bool,
    /// An error (not TOML, would change a value, I/O).
    pub error: Option<String>,
}

/// Formats (or with `check`, only checks) files.
#[must_use]
pub fn format_files(paths: &[PathBuf], check: bool) -> Vec<FmtResult> {
    paths.iter().map(|path| format_file(path, check)).collect()
}

fn format_file(path: &Path, check: bool) -> FmtResult {
    let result = |changed: bool, error: Option<String>| FmtResult {
        path: path.to_path_buf(),
        changed,
        error,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => return result(false, Some(error.to_string())),
    };
    match format_definition(&text) {
        Err(error) => result(false, Some(error)),
        Ok(formatted) if formatted == text => result(false, None),
        Ok(formatted) => {
            if check {
                return result(true, None);
            }
            match std::fs::write(path, formatted) {
                Ok(()) => result(true, None),
                Err(error) => result(true, Some(error.to_string())),
            }
        }
    }
}

/// The definition files below `dir` (`agents/*/definition.toml`,
/// `agents/*.toml`, or `definition.toml` in `dir` itself), sorted.
///
/// # Errors
/// [`CompileError::Io`] if `dir` cannot be listed.
pub fn definition_files(dir: &Path) -> Result<Vec<PathBuf>, CompileError> {
    if dir.is_file() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let own = dir.join("definition.toml");
    if own.is_file() {
        return Ok(vec![own]);
    }
    let agents = if dir.join("agents").is_dir() {
        dir.join("agents")
    } else {
        dir.to_path_buf()
    };
    let mut files = Vec::new();
    let read_dir = std::fs::read_dir(&agents)
        .map_err(CompileError::io(format!("list {}", agents.display())))?;
    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() && path.join("definition.toml").is_file() {
            files.push(path.join("definition.toml"));
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSY: &str = "\n\n# Leading comment stays.\nrole   =    \"worker\"   # the role\nschema=\"harwness.agent/v1\"\nversion = \"1.0.0\"\nid = \"acme.agent.x@1\"   \nspecialization = \"x\"\n\n\n\n[tools]\n# Tools comment stays.\nadmitted   = [\"fs.read\", \"fs.list\"]\n\n\n[spawn]\nmax_depth=0\n";

    #[test]
    fn test_format_orders_header_and_normalizes_spacing() -> Result<(), String> {
        let formatted = format_definition(MESSY)?;
        let position = |needle: &str| {
            formatted
                .find(needle)
                .ok_or_else(|| format!("`{needle}` missing in:\n{formatted}"))
        };
        let schema = position("schema = \"harwness.agent/v1\"\n")?;
        let id = position("id = \"acme.agent.x@1\"\n")?;
        let version = position("version = \"1.0.0\"\n")?;
        let role = position("role = \"worker\" # the role\n")?;
        let specialization = position("specialization = \"x\"\n")?;
        assert!(schema < id && id < version && version < role && role < specialization);
        assert!(
            formatted.starts_with("schema = "),
            "no leading blank lines:\n{formatted}"
        );
        assert!(
            formatted.contains("# Leading comment stays.\nrole"),
            "a comment moves with its key"
        );
        assert!(
            formatted.contains("# Tools comment stays.\nadmitted = [\"fs.read\", \"fs.list\"]\n")
        );
        assert!(formatted.contains("\n[spawn]\nmax_depth = 0\n"));
        assert!(
            !formatted.contains("\n\n\n"),
            "blank-line runs collapse:\n{formatted}"
        );
        assert!(
            !formatted.lines().any(|line| line.ends_with(' ')),
            "no trailing spaces"
        );
        assert!(formatted.ends_with("max_depth = 0\n"));
        Ok(())
    }

    #[test]
    fn test_format_is_idempotent_and_value_preserving() -> Result<(), String> {
        let once = format_definition(MESSY)?;
        let twice = format_definition(&once)?;
        assert_eq!(once, twice);
        let before: toml::Table =
            toml::from_str(MESSY).map_err(|e: toml::de::Error| e.to_string())?;
        let after: toml::Table =
            toml::from_str(&once).map_err(|e: toml::de::Error| e.to_string())?;
        assert_eq!(before, after);
        Ok(())
    }

    #[test]
    fn test_multiline_string_whitespace_is_kept() -> Result<(), String> {
        let text = "schema = \"harwness.agent/v1\"\ndescription = \"\"\"\nline with trailing space   \nend\"\"\"\n";
        let formatted = format_definition(text)?;
        let before: toml::Table =
            toml::from_str(text).map_err(|e: toml::de::Error| e.to_string())?;
        let after: toml::Table =
            toml::from_str(&formatted).map_err(|e: toml::de::Error| e.to_string())?;
        assert_eq!(before, after, "the string value is unchanged");
        Ok(())
    }

    #[test]
    fn test_invalid_toml_is_an_error() {
        assert!(format_definition("schema = [").is_err());
    }
}
