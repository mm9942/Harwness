//! Agent tools over the phone's device handles (Termux shell, SAF files).
//!
//! The Kotlin side registers [`ShellExecutor`] and [`FileAccess`] through
//! `MobileCore::set_device_tools`. In standalone mode this module turns them into
//! agent tools and mounts them on every hosted session through an assembly
//! contributor (see `harw_local_host::LocalHost::open_with_contributors`):
//!
//! - `device_roots`, `device_list`, `device_read`: read the granted trees.
//! - `device_write`: create-only; an existing file is never replaced.
//! - `device_search`: file-name and text search built on list and read, bounded
//!   in depth, entries, bytes, matches and time.
//! - `termux_run`: one argv command in Termux; registered only when the shell
//!   reports itself available.
//!
//! Approval: none of these tools is in the runtime's auto-approved list, so every
//! call goes through the normal tool approval. `termux_run` is additionally in
//! `harw_registry_defaults::ALWAYS_ASK_TOOLS`, so an allow rule or the auto-mode
//! gate cannot skip the question either (only Full Access, which never asks).
//!
//! The per-profile switch (`device_tools_enabled`) defaults to off while the
//! on-device GGUF model is the active provider and to on for every other provider.

use std::path::Path;
use std::sync::Arc;

use crate::{FileAccess, MobileError, ShellExecutor, home::AppHome};

/// The handles the platform registered: `(shell, files)`.
pub(crate) type DeviceHandles = (Option<Arc<dyn ShellExecutor>>, Option<Arc<dyn FileAccess>>);

/// Whether the device tools are mounted for the next on-device host start.
///
/// An explicit choice wins; without one the tools are off while the on-device
/// GGUF model is the active provider and on for every other provider.
#[uniffi::export]
pub fn device_tools_enabled(files_dir: String) -> Result<bool, MobileError> {
    AppHome::open(Path::new(&files_dir))?.device_tools_enabled()
}

/// Sets the device-tools switch; `None` returns to the per-provider default.
/// It takes effect when the on-device host is started again.
#[uniffi::export]
pub fn set_device_tools_enabled(
    files_dir: String,
    enabled: Option<bool>,
) -> Result<(), MobileError> {
    AppHome::open(Path::new(&files_dir))?.set_device_tools_setting(enabled)
}

#[cfg(feature = "local-host")]
pub(crate) use tools::DeviceToolsContributor;

#[cfg(feature = "local-host")]
mod tools {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use harw_local_host::ext::{
        AssemblyContributor, AssemblyInputs, AssemblyParts, FunctionToolSpec, JsonSchema,
        RuntimeResult, ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName,
        ToolOutput, ToolProvider, ToolSpec, ToolsError, serde_json,
    };
    use serde::Deserialize;
    use serde_json::{Value, json};

    use crate::device::{
        DeviceError, SafEntry, ShellRequest, check_shell_request, device_limits,
        normalize_device_path,
    };
    use crate::{FileAccess, ShellExecutor};

    const TOOL_ROOTS: &str = "device_roots";
    const TOOL_LIST: &str = "device_list";
    const TOOL_READ: &str = "device_read";
    const TOOL_WRITE: &str = "device_write";
    const TOOL_SEARCH: &str = "device_search";
    const TOOL_SHELL: &str = "termux_run";

    /// Entries returned by one `device_list`.
    const MAX_LIST_ENTRIES: usize = 500;
    /// Bytes of file text returned by one `device_read` (also the default).
    const MAX_READ_TEXT_BYTES: u64 = 65_536;
    const MAX_ROOT_ID_BYTES: usize = 256;

    const SEARCH_MAX_DEPTH: usize = 8;
    const SEARCH_MAX_VISITED: usize = 3_000;
    const SEARCH_MAX_CONTENT_FILES: usize = 200;
    const SEARCH_MAX_FILE_BYTES: u64 = 1_048_576;
    const SEARCH_READ_BYTES: u64 = 65_536;
    const SEARCH_DEFAULT_MATCHES: usize = 30;
    const SEARCH_MAX_MATCHES: usize = 100;
    const SEARCH_LINES_PER_FILE: usize = 3;
    const SEARCH_LINE_CHARS: usize = 200;
    const SEARCH_MAX_QUERY_BYTES: usize = 256;
    const SEARCH_TIME_BUDGET: Duration = Duration::from_secs(25);

    /// Failure of one tool call.
    enum Fail {
        /// The model's arguments are wrong.
        Args(String),
        /// The call was valid but failed; reported to the model as a tool error.
        Msg(String),
    }

    impl From<DeviceError> for Fail {
        fn from(error: DeviceError) -> Self {
            Self::Msg(error.to_string())
        }
    }

    fn args<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T, Fail> {
        serde_json::from_value(call.arguments.clone()).map_err(|e| Fail::Args(e.to_string()))
    }

    fn root_id(raw: String) -> Result<String, Fail> {
        if raw.is_empty() || raw.len() > MAX_ROOT_ID_BYTES || raw.contains('\0') {
            return Err(Fail::Args("root must be an id from device_roots".to_owned()));
        }
        Ok(raw)
    }

    fn rel_path(raw: Option<String>) -> Result<String, Fail> {
        Ok(normalize_device_path(raw.unwrap_or_default())?)
    }

    // ----- schemas -------------------------------------------------------

    fn schema(properties: Value, required: &[&str]) -> JsonSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": properties,
            "required": required,
        }))
        .unwrap_or_default()
    }

    fn spec(name: &str, description: &str, parameters: JsonSchema) -> ToolSpec {
        ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(name),
            description: description.to_owned(),
            parameters,
            strict: false,
        })
    }

    fn root_prop() -> Value {
        json!({"type": "string", "description": "Root id from device_roots."})
    }

    fn path_prop(what: &str) -> Value {
        json!({"type": "string", "description": what})
    }

    fn specs(has_files: bool, has_shell: bool) -> Vec<ToolSpec> {
        let mut out = Vec::new();
        if has_files {
            out.push(spec(
                TOOL_ROOTS,
                "List the folders on this phone that the owner granted to the agent.",
                schema(json!({}), &[]),
            ));
            out.push(spec(
                TOOL_LIST,
                "List one directory of a granted folder.",
                schema(
                    json!({
                        "root": root_prop(),
                        "path": path_prop("Relative path with '/' separators; empty for the root."),
                    }),
                    &["root"],
                ),
            ));
            out.push(spec(
                TOOL_READ,
                "Read a text file of a granted folder (truncated to 64 KiB).",
                schema(
                    json!({
                        "root": root_prop(),
                        "path": path_prop("Relative path of the file."),
                        "max_bytes": {"type": "integer", "description": "Optional byte limit, at most 65536."},
                    }),
                    &["root", "path"],
                ),
            ));
            out.push(spec(
                TOOL_WRITE,
                "Create a new text file in a writable granted folder. Never replaces an existing file.",
                schema(
                    json!({
                        "root": root_prop(),
                        "path": path_prop("Relative path of the new file."),
                        "content": {"type": "string", "description": "UTF-8 text of the file."},
                    }),
                    &["root", "path", "content"],
                ),
            ));
            out.push(spec(
                TOOL_SEARCH,
                "Search a granted folder by file name and optionally by text inside files. Bounded; the result says when it stopped early.",
                schema(
                    json!({
                        "root": root_prop(),
                        "path": path_prop("Relative directory to start from; empty for the root."),
                        "query": {"type": "string", "description": "Case-insensitive text."},
                        "content": {"type": "boolean", "description": "Also search inside text files (default false)."},
                        "limit": {"type": "integer", "description": "Maximum matches (default 30, at most 100)."},
                    }),
                    &["root", "query"],
                ),
            ));
        }
        if has_shell {
            out.push(spec(
                TOOL_SHELL,
                "Run one command in Termux on this phone. `argv` is the program and its arguments, with no shell interpretation (use [\"sh\", \"-c\", \"...\"] for a script). The owner confirms every call. Output is truncated and the command has a timeout.",
                schema(
                    json!({
                        "argv": {"type": "array", "items": {"type": "string"}, "description": "Program and arguments."},
                        "cwd": path_prop("Optional working directory."),
                        "timeout_ms": {"type": "integer", "description": "Optional timeout, at most 120000."},
                    }),
                    &["argv"],
                ),
            ));
        }
        out
    }

    // ----- provider ------------------------------------------------------

    struct DeviceToolProvider {
        shell: Option<Arc<dyn ShellExecutor>>,
        files: Option<Arc<dyn FileAccess>>,
    }

    impl ToolProvider for DeviceToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            specs(self.files.is_some(), self.shell.is_some())
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            let known = match name.as_str() {
                TOOL_ROOTS | TOOL_LIST | TOOL_READ | TOOL_WRITE | TOOL_SEARCH => {
                    self.files.is_some()
                }
                TOOL_SHELL => self.shell.is_some(),
                _ => false,
            };
            known.then(|| {
                Arc::new(DeviceToolExecutor {
                    shell: self.shell.clone(),
                    files: self.files.clone(),
                }) as Arc<dyn ToolExecutor>
            })
        }
    }

    /// Mounts the device tools on every hosted session.
    pub(crate) struct DeviceToolsContributor {
        shell: Option<Arc<dyn ShellExecutor>>,
        files: Option<Arc<dyn FileAccess>>,
    }

    impl DeviceToolsContributor {
        pub(crate) fn new(
            shell: Option<Arc<dyn ShellExecutor>>,
            files: Option<Arc<dyn FileAccess>>,
        ) -> Self {
            Self { shell, files }
        }
    }

    impl AssemblyContributor for DeviceToolsContributor {
        fn contribute(
            &self,
            _inputs: &AssemblyInputs<'_>,
            parts: &mut AssemblyParts,
        ) -> RuntimeResult<()> {
            let shell = self.shell.clone().filter(|shell| shell.available());
            if shell.is_none() && self.files.is_none() {
                return Ok(());
            }
            let provider = DeviceToolProvider {
                shell,
                files: self.files.clone(),
            };
            let registry = std::mem::take(&mut parts.registry);
            parts.registry = registry.tool_provider(Arc::new(provider));
            Ok(())
        }
    }

    // ----- executor ------------------------------------------------------

    struct DeviceToolExecutor {
        shell: Option<Arc<dyn ShellExecutor>>,
        files: Option<Arc<dyn FileAccess>>,
    }

    impl ToolExecutor for DeviceToolExecutor {
        fn execute<'a>(
            &'a self,
            context: &'a ToolExecutionContext,
            call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            let shell = self.shell.clone();
            let files = self.files.clone();
            // The platform calls block: run them off the async workers, and keep the
            // context (its lifetime guard) alive until the last side effect.
            let context = context.clone();
            let call = call.clone();
            Box::pin(async move {
                let name = call.name.as_str().to_owned();
                let work = tokio::task::spawn_blocking(move || {
                    let result = run(shell.as_ref(), files.as_ref(), &context, &call);
                    drop(context);
                    result
                });
                match work.await {
                    Ok(Ok(output)) => Ok(output),
                    Ok(Err(Fail::Args(reason))) => {
                        Err(ToolsError::InvalidArguments { name, reason })
                    }
                    Ok(Err(Fail::Msg(message))) => {
                        Ok(ToolOutput::error(format!("{name}: {message}")))
                    }
                    Err(_) => Ok(ToolOutput::error(format!("{name}: the device call failed"))),
                }
            })
        }
    }

    fn files_of(files: Option<&Arc<dyn FileAccess>>) -> Result<&dyn FileAccess, Fail> {
        files
            .map(AsRef::as_ref)
            .ok_or_else(|| Fail::Msg("file access is not available".to_owned()))
    }

    fn run(
        shell: Option<&Arc<dyn ShellExecutor>>,
        files: Option<&Arc<dyn FileAccess>>,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, Fail> {
        match call.name.as_str() {
            TOOL_ROOTS => roots(files_of(files)?),
            TOOL_LIST => list(files_of(files)?, call),
            TOOL_READ => read(files_of(files)?, call),
            TOOL_WRITE => write(files_of(files)?, call),
            TOOL_SEARCH => search(files_of(files)?, context, call),
            TOOL_SHELL => {
                let shell = shell
                    .map(AsRef::as_ref)
                    .ok_or_else(|| Fail::Msg("Termux is not available".to_owned()))?;
                shell_run(shell, call)
            }
            other => Err(Fail::Msg(format!("unknown device tool {other}"))),
        }
    }

    fn roots(files: &dyn FileAccess) -> Result<ToolOutput, Fail> {
        let roots: Vec<Value> = files
            .roots()?
            .into_iter()
            .map(|r| json!({"id": r.id, "label": r.label, "writable": r.writable}))
            .collect();
        Ok(ToolOutput::json(json!({"roots": roots})))
    }

    fn entry_json(entry: &SafEntry) -> Value {
        json!({
            "name": entry.name,
            "path": entry.path,
            "is_dir": entry.is_dir,
            "size_bytes": entry.size_bytes,
            "modified_ms": entry.modified_ms,
        })
    }

    #[derive(Deserialize)]
    struct ListArgs {
        root: String,
        path: Option<String>,
    }

    fn list(files: &dyn FileAccess, call: &ToolCall) -> Result<ToolOutput, Fail> {
        let a: ListArgs = args(call)?;
        let root = root_id(a.root)?;
        let path = rel_path(a.path)?;
        let entries = files.list(root, path.clone())?;
        let total = entries.len();
        let shown: Vec<Value> = entries
            .iter()
            .take(MAX_LIST_ENTRIES)
            .map(entry_json)
            .collect();
        Ok(ToolOutput::json(json!({
            "path": path,
            "entries": shown,
            "truncated": total > MAX_LIST_ENTRIES,
            "total": total,
        })))
    }

    #[derive(Deserialize)]
    struct ReadArgs {
        root: String,
        path: String,
        max_bytes: Option<u64>,
    }

    /// Text of `bytes`; `None` for binary data.
    fn as_text(bytes: Vec<u8>) -> Option<String> {
        match String::from_utf8(bytes) {
            Ok(text) => Some(text),
            Err(error) => {
                let bytes = error.into_bytes();
                // A cut inside a multi-byte character is fine; NUL bytes mean binary.
                if bytes.contains(&0) {
                    None
                } else {
                    Some(String::from_utf8_lossy(&bytes).into_owned())
                }
            }
        }
    }

    fn read(files: &dyn FileAccess, call: &ToolCall) -> Result<ToolOutput, Fail> {
        let a: ReadArgs = args(call)?;
        let root = root_id(a.root)?;
        let path = rel_path(Some(a.path))?;
        if path.is_empty() {
            return Err(Fail::Args("path must name a file".to_owned()));
        }
        let limit = a
            .max_bytes
            .unwrap_or(MAX_READ_TEXT_BYTES)
            .clamp(1, MAX_READ_TEXT_BYTES)
            .min(device_limits().max_read_bytes);
        // One byte more than the limit tells whether the file was cut.
        let mut bytes = files.read(root, path.clone(), limit + 1)?;
        let truncated = bytes.len() as u64 > limit;
        bytes.truncate(limit as usize);
        let content =
            as_text(bytes).ok_or_else(|| Fail::Msg("the file is binary, not text".to_owned()))?;
        Ok(ToolOutput::json(json!({
            "path": path,
            "truncated": truncated,
            "content": content,
        })))
    }

    #[derive(Deserialize)]
    struct WriteArgs {
        root: String,
        path: String,
        content: String,
    }

    fn write(files: &dyn FileAccess, call: &ToolCall) -> Result<ToolOutput, Fail> {
        let a: WriteArgs = args(call)?;
        let root = root_id(a.root)?;
        let path = rel_path(Some(a.path))?;
        if path.is_empty() {
            return Err(Fail::Args("path must name a file".to_owned()));
        }
        if a.content.len() as u64 > device_limits().max_write_bytes {
            return Err(Fail::Msg("the content is too large".to_owned()));
        }
        let writable = files
            .roots()?
            .iter()
            .find(|r| r.id == root)
            .map(|r| r.writable)
            .ok_or_else(|| Fail::Msg("unknown root".to_owned()))?;
        if !writable {
            return Err(Fail::Msg("this folder is read-only".to_owned()));
        }
        let size = a.content.len();
        files.write(root, path.clone(), a.content.into_bytes())?;
        Ok(ToolOutput::text(format!("created {path} ({size} bytes)")))
    }

    #[derive(Deserialize)]
    struct SearchArgs {
        root: String,
        path: Option<String>,
        query: String,
        content: Option<bool>,
        limit: Option<usize>,
    }

    fn clip_line(line: &str) -> String {
        let line = line.trim();
        let mut text: String = line.chars().take(SEARCH_LINE_CHARS).collect();
        if line.chars().count() > SEARCH_LINE_CHARS {
            text.push_str("...");
        }
        text
    }

    fn search(
        files: &dyn FileAccess,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, Fail> {
        let a: SearchArgs = args(call)?;
        let root = root_id(a.root)?;
        let start = rel_path(a.path)?;
        let query = a.query.trim().to_lowercase();
        if query.is_empty() || query.len() > SEARCH_MAX_QUERY_BYTES {
            return Err(Fail::Args("query must be 1 to 256 bytes".to_owned()));
        }
        let by_content = a.content.unwrap_or(false);
        let limit = a
            .limit
            .unwrap_or(SEARCH_DEFAULT_MATCHES)
            .clamp(1, SEARCH_MAX_MATCHES);
        let deadline = Instant::now() + SEARCH_TIME_BUDGET;

        let mut matches: Vec<Value> = Vec::new();
        let mut visited = 0_usize;
        let mut content_files = 0_usize;
        let mut stopped: Option<&str> = None;
        let mut queue: VecDeque<(String, usize)> = VecDeque::from([(start, 0)]);

        'walk: while let Some((dir, depth)) = queue.pop_front() {
            let entries = match files.list(root.clone(), dir) {
                Ok(entries) => entries,
                Err(DeviceError::Disabled | DeviceError::Unavailable { .. }) => {
                    return Err(Fail::Msg("file access is not available".to_owned()));
                }
                // An unreadable subdirectory does not end the search.
                Err(_) => continue,
            };
            for entry in entries {
                if context.cancel().is_some_and(|c| c.is_cancelled()) {
                    stopped = Some("cancelled");
                    break 'walk;
                }
                if Instant::now() >= deadline {
                    stopped = Some("time budget used up");
                    break 'walk;
                }
                if visited >= SEARCH_MAX_VISITED {
                    stopped = Some("entry limit reached");
                    break 'walk;
                }
                visited += 1;
                let Ok(path) = normalize_device_path(entry.path.clone()) else {
                    continue;
                };
                if entry.name.to_lowercase().contains(&query) {
                    matches.push(json!({"path": path, "kind": "name", "is_dir": entry.is_dir}));
                    if matches.len() >= limit {
                        stopped = Some("match limit reached");
                        break 'walk;
                    }
                }
                if entry.is_dir {
                    if depth < SEARCH_MAX_DEPTH {
                        queue.push_back((path, depth + 1));
                    }
                    continue;
                }
                if !by_content
                    || entry.size_bytes == 0
                    || entry.size_bytes > SEARCH_MAX_FILE_BYTES
                    || content_files >= SEARCH_MAX_CONTENT_FILES
                {
                    continue;
                }
                content_files += 1;
                let Ok(bytes) = files.read(root.clone(), path.clone(), SEARCH_READ_BYTES) else {
                    continue;
                };
                let Ok(text) = String::from_utf8(bytes) else {
                    continue;
                };
                let mut hits = 0;
                for (index, line) in text.lines().enumerate() {
                    if line.to_lowercase().contains(&query) {
                        matches.push(json!({
                            "path": path,
                            "kind": "content",
                            "line": index + 1,
                            "text": clip_line(line),
                        }));
                        hits += 1;
                        if matches.len() >= limit {
                            stopped = Some("match limit reached");
                            break 'walk;
                        }
                        if hits >= SEARCH_LINES_PER_FILE {
                            break;
                        }
                    }
                }
            }
        }
        Ok(ToolOutput::json(json!({
            "matches": matches,
            "visited": visited,
            "content_files_read": content_files,
            "stopped_early": stopped,
        })))
    }

    #[derive(Deserialize)]
    struct ShellArgs {
        argv: Vec<String>,
        cwd: Option<String>,
        timeout_ms: Option<u64>,
    }

    fn shell_run(shell: &dyn ShellExecutor, call: &ToolCall) -> Result<ToolOutput, Fail> {
        let a: ShellArgs = args(call)?;
        let request = check_shell_request(ShellRequest {
            argv: a.argv,
            cwd: a.cwd,
            timeout_ms: a.timeout_ms.unwrap_or(0),
            max_output_bytes: 0,
        })
        .map_err(|e| match e {
            DeviceError::Invalid { .. } | DeviceError::InvalidPath { .. } => {
                Fail::Args(e.to_string())
            }
            other => Fail::Msg(other.to_string()),
        })?;
        if !shell.available() {
            return Err(Fail::Msg("Termux is not available".to_owned()));
        }
        let out = shell.run(request)?;
        Ok(ToolOutput::json(json!({
            "exit_code": out.exit_code,
            "stdout": out.stdout,
            "stderr": out.stderr,
            "truncated": out.truncated,
            "timed_out": out.timed_out,
        })))
    }
}
