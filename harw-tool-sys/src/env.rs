//! `sys.env` — Umgebungsvariablen des Harness-Prozesses, Geheimnisse maskiert.
//!
//! Gelesen wird ausschließlich die eigene Umgebung (`std::env::vars_os`),
//! nicht die fremder Prozesse. Werte werden nach [`crate::mask`] maskiert
//! (Name **oder** Aussehen); es gibt **keine** Option, Werte offen
//! auszugeben. Gefiltert wird nur nach Namen, nie nach Werten — damit ist der
//! Filter kein Orakel für maskierte Inhalte. Werte über 1024 Zeichen werden
//! gekürzt.

use crate::mask::mask_env_value;
use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{Collector, fail, limit_or, ok};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::ffi::OsString;

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.env";

/// Standard-Limit.
pub const DEFAULT_LIMIT: usize = 200;

/// Hartes Limit.
pub const HARD_LIMIT: usize = 1_000;

/// Höchstlänge eines ausgegebenen Werts in Zeichen.
pub const MAX_VALUE_CHARS: usize = 1024;

/// Argumente für `sys.env`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct EnvArgs {
    /// Only these variable names (exact match, at most 64).
    #[serde(default)]
    pub names: Option<Vec<String>>,
    /// Only variables whose name starts with this prefix (case-sensitive).
    #[serde(default)]
    pub prefix: Option<String>,
    /// Only variables whose name contains this text (case-insensitive).
    #[serde(default)]
    pub name_contains: Option<String>,
    /// Maximum number of variables (default 200, hard 1000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

/// Führt `sys.env` über `vars` aus (in Tests eine künstliche Umgebung).
#[must_use]
pub fn run(vars: Vec<(OsString, OsString)>, args: &EnvArgs) -> ToolOutput {
    if let Some(names) = &args.names {
        if names.len() > 64 {
            return fail(TOOL, "too many names (max 64)");
        }
        if names
            .iter()
            .any(|n| n.is_empty() || n.contains('=') || n.contains('\0') || n.len() > 256)
        {
            return fail(TOOL, "names must be 1-256 bytes without '=' or NUL");
        }
    }
    for text in [args.prefix.as_deref(), args.name_contains.as_deref()]
        .into_iter()
        .flatten()
    {
        if text.is_empty() || text.len() > 256 || text.contains('\0') {
            return fail(
                TOOL,
                "prefix and name_contains must be 1-256 bytes without NUL",
            );
        }
    }
    let limit = limit_or(args.limit, DEFAULT_LIMIT, HARD_LIMIT);
    let contains = args.name_contains.as_deref().map(str::to_lowercase);
    let mut entries: Vec<(String, String)> = vars
        .into_iter()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .filter(|(name, _)| {
            args.names
                .as_ref()
                .is_none_or(|names| names.iter().any(|n| n == name))
        })
        .filter(|(name, _)| {
            args.prefix
                .as_deref()
                .is_none_or(|prefix| name.starts_with(prefix))
        })
        .filter(|(name, _)| {
            contains
                .as_deref()
                .is_none_or(|needle| name.to_lowercase().contains(needle))
        })
        .collect();
    entries.sort();
    let matched = entries.len();
    let mut out = Collector::new(limit);
    let mut masked = 0usize;
    for (name, value) in entries {
        let (shown, was_masked) = mask_env_value(&name, &value);
        let (shown, clipped) = if shown.chars().count() > MAX_VALUE_CHARS {
            let mut short: String = shown.chars().take(MAX_VALUE_CHARS - 1).collect();
            short.push('…');
            (short, true)
        } else {
            (shown, false)
        };
        let mut entry = json!({"name": name, "value": shown, "masked": was_masked});
        if clipped {
            entry["value_truncated"] = json!(true);
        }
        if !out.push(entry) {
            break;
        }
        masked += usize::from(was_masked);
    }
    let truncated = out.truncated();
    let stopped = out.stop_reason();
    let count = out.len();
    ok(
        TOOL,
        format!("{count} of {matched} variables, {masked} masked"),
        json!({
            "variables": out.into_items(),
            "count": count,
            "matched": matched,
            "masked": masked,
            "truncated": truncated,
            "stopped": stopped,
        }),
    )
}

/// Zeigt Umgebungsvariablen wie `env`, Geheimnisse maskiert.
#[harw_macros::tool(
    name = "sys.env",
    description = "Lists environment variables of the agent process like env, with secrets masked: values of names containing KEY, TOKEN, SECRET, PASSWORD, CREDENTIAL, AUTH and so on, and values that look like tokens or URLs with passwords, are replaced by ***. Use when you need to know a variable (PATH, HOME, LANG, proxy settings) instead of running env or printenv. Filter by exact names, prefix or name substring; filters never look at values. There is no way to reveal masked values. Returns JSON {variables:[{name,value,masked}], count, masked, truncated}.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_env(_context: &ToolExecutionContext, args: EnvArgs) -> Result<ToolOutput, ToolsError> {
    run_blocking(TOOL, move || run(std::env::vars_os().collect(), &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, error_of, json_of};
    use serde_json::Value;

    fn vars(items: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        items
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    fn env(items: &[(&str, &str)], args: Value) -> TestResult<Value> {
        json_of(run(vars(items), &serde_json::from_value(args)?))
    }

    fn sample() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/u"),
            ("API_KEY", "abc123"),
            ("GITHUB_TOKEN", "ghp_0123456789abcdefghijklmnopqrstuvwxyz"),
            ("DB_PASSWORD", "hunter2"),
            ("HARMLESS_NAME", "sk-liveabcdefghijklmnop"),
            ("DATABASE_URL", "postgres://app:pw123@db/app"),
            ("GIT_AUTHOR_NAME", "Ada"),
            ("PWD", "/work"),
        ]
    }

    #[test]
    fn secrets_are_masked_everywhere_and_never_revealed() -> TestResult {
        let value = env(&sample(), json!({}))?;
        let text = serde_json::to_string(&value)?;
        for leaked in ["abc123", "ghp_0123", "hunter2", "sk-live", "pw123"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        let by_name = |name: &str| -> TestResult<Value> {
            value["variables"]
                .as_array()
                .and_then(|a| a.iter().find(|v| v["name"] == name))
                .cloned()
                .ok_or(TestError::Missing("variable"))
        };
        assert_eq!(by_name("PATH")?["value"], "/usr/bin:/bin");
        assert_eq!(by_name("PATH")?["masked"], false);
        assert_eq!(by_name("API_KEY")?["value"], "***");
        assert_eq!(by_name("HARMLESS_NAME")?["value"], "***");
        assert_eq!(by_name("GIT_AUTHOR_NAME")?["value"], "Ada");
        assert_eq!(by_name("PWD")?["masked"], false);
        assert!(
            by_name("DATABASE_URL")?["value"]
                .as_str()
                .is_some_and(|v| v.contains(":***@"))
        );
        assert_eq!(value["masked"], 5);
        Ok(())
    }

    #[test]
    fn filters_apply_to_names_only_and_output_is_sorted() -> TestResult {
        let items = sample();
        let by_prefix = env(&items, json!({"prefix": "GIT"}))?;
        assert_eq!(by_prefix["count"], 2);
        let exact = env(&items, json!({"names": ["HOME", "PATH", "NOPE"]}))?;
        let names: Vec<&str> = exact["variables"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v["name"].as_str()).collect())
            .unwrap_or_default();
        assert_eq!(names, vec!["HOME", "PATH"]);
        let contains = env(&items, json!({"name_contains": "token"}))?;
        assert_eq!(contains["count"], 1);
        // Ein Filter auf den Wert gibt es nicht: der Aufruf wird abgelehnt.
        assert!(serde_json::from_value::<EnvArgs>(json!({"value_contains": "hunter2"})).is_err());
        assert!(serde_json::from_value::<EnvArgs>(json!({"reveal": true})).is_err());
        let all = env(&items, json!({}))?;
        let order: Vec<String> = all["variables"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
        Ok(())
    }

    #[test]
    fn limit_clip_and_invalid_input() -> TestResult {
        let long = "x ".repeat(2000);
        let items = [("LONG", long.as_str()), ("A", "1"), ("B", "2")];
        let value = env(&items, json!({"limit": 2}))?;
        assert_eq!(value["count"], 2);
        assert_eq!(value["truncated"], true);
        let value = env(&items, json!({"names": ["LONG"]}))?;
        assert_eq!(value["variables"][0]["value_truncated"], true);
        assert!(
            value["variables"][0]["value"]
                .as_str()
                .is_some_and(|v| v.chars().count() <= MAX_VALUE_CHARS)
        );
        for bad in [
            json!({"names": ["A=B"]}),
            json!({"names": [""]}),
            json!({"prefix": ""}),
            json!({"name_contains": "a\u{0}"}),
            json!({"names": (0..65).map(|i| format!("N{i}")).collect::<Vec<_>>()}),
        ] {
            error_of(run(vars(&items), &serde_json::from_value(bad.clone())?))
                .map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        Ok(())
    }

    #[test]
    fn non_utf8_values_do_not_panic() -> TestResult {
        use std::os::unix::ffi::OsStringExt;
        let weird = vec![(
            OsString::from("WEIRD"),
            OsString::from_vec(vec![0xff, 0xfe, b'a']),
        )];
        let value = json_of(run(weird, &serde_json::from_value(json!({}))?))?;
        assert!(value["variables"][0]["value"].is_string());
        Ok(())
    }
}
