//! `FsToolProvider` — aggregierter ToolProvider für alle sechs FS-Tools.
//!
//! # Verantwortung
//! Dieses Modul fasst `fs.read`, `fs.write`, `fs.list`, `fs.search`,
//! `fs.glob` und `fs.grep` unter einem einzigen [`ToolProvider`]-Implementor
//! zusammen. Der Provider ist zustandslos bezüglich Pfaden — alle
//! Pfad-Entscheidungen erfolgen pro-Call aus dem [`ToolExecutionContext`].
//!
//! # Migrationsentscheidung (AP W2-01..03)
//! `fs.glob` und `fs.grep` sind über `#[harw_macros::tool]` erzeugt
//! ([`crate::glob::FsGlobTool`], [`crate::grep::FsGrepTool`]) und wären damit
//! Kandidaten für `harw_tools::tool_provider!`. Die vier bestehenden Tools
//! (`fs.read`, `fs.write`, `fs.list`, `fs.search`) sind jedoch handgeschriebene
//! Executor-Structs mit eigenen Konfigurationsfeldern (`max_bytes`,
//! `max_entries`, `max_matches`, `max_depth`), die keine der von
//! `tool_provider!` vorausgesetzten Consts/Methoden (`NAME`, `PERMISSION`,
//! `PARALLEL_SAFE`, `spec()`, `impl Default`) bereitstellen. Eine Umstellung
//! aller sechs Tools auf `tool_provider!` würde deshalb eine Migration aller
//! vier bestehenden Executors auf `#[harw_macros::tool]` voraussetzen — ein
//! großer, hier explizit nicht beauftragter Diff. Diese Datei bleibt daher
//! minimal-invasiv: `FsToolProvider` bleibt die handgeschriebene Struktur,
//! `tools()`/`executor()`/`parallel_safe()` bekommen zwei zusätzliche Arme
//! für `fs.glob`/`fs.grep`. Die Migration der vier übrigen Tools auf
//! `#[harw_macros::tool]` (und anschließend auf `tool_provider!`) ist als
//! Folge-AP vorgemerkt.
//!
//! # Schlüsseltypen
//! - [`FsToolProvider`]
//!
//! # Nebenläufigkeit
//! [`FsToolProvider`] ist `Send + Sync`. Lesende Tools (`fs.read`, `fs.list`,
//! `fs.search`, `fs.glob`, `fs.grep`) sind als `parallel_safe` markiert.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_tool_fs::FsToolProvider;
//!
//! let provider = FsToolProvider::new();
//! ```

use crate::glob::FsGlobTool;
use crate::grep::FsGrepTool;
use crate::list::{DEFAULT_MAX_ENTRIES, FsListExecutor};
use crate::read::FsReadExecutor;
use crate::search::{DEFAULT_MAX_DEPTH, DEFAULT_MAX_MATCHES, FsSearchExecutor};
use crate::write::FsWriteExecutor;
use harw_extension_api::contributors::ToolProvider;
use harw_tools::{
    ToolExecutor, ToolName, ToolSpec,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::FunctionToolSpec,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Standard-Maximum für lesbare Bytes in `fs.read`.
pub const DEFAULT_MAX_READ_BYTES: u64 = 65_536;

/// Aggregierter ToolProvider für alle sechs Filesystem-Tools.
///
/// # Description
/// Stellt `fs.read`, `fs.write`, `fs.list`, `fs.search`, `fs.glob` und
/// `fs.grep` bereit. Konfiguration der vier handgeschriebenen Tools erfolgt
/// über den Konstruktor; `fs.glob`/`fs.grep` sind zustandslose,
/// makro-generierte Unit-Structs ohne Konfigurationsfelder. Pfad- und
/// Berechtigungs-Entscheidungen erfolgen per-Call aus dem
/// [`harw_tools::executor::ToolExecutionContext`].
///
/// # Concurrency
/// `Send + Sync`; kein Shared State außer unveränderlichen Konfigurationsfeldern.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_fs::FsToolProvider;
/// use harw_extension_api::contributors::ToolProvider;
///
/// let provider = FsToolProvider::new();
/// let tools = provider.tools(); // returns 6 ToolSpecs
/// ```
pub struct FsToolProvider {
    /// Maximale Bytes pro `fs.read`-Aufruf.
    max_read_bytes: u64,
    /// Maximale Einträge pro `fs.list`-Aufruf.
    max_list_entries: usize,
    /// Maximale Treffer pro `fs.search`-Aufruf.
    max_search_matches: usize,
    /// Maximale Rekursionstiefe für `fs.search`.
    max_search_depth: usize,
}

impl FsToolProvider {
    /// Erstellt einen neuen `FsToolProvider` mit Standard-Konfiguration.
    ///
    /// # Returns
    /// Ein `FsToolProvider` mit Default-Werten für alle Limits.
    ///
    /// # Concurrency
    /// Thread-safe; kein Shared State.
    #[must_use]
    pub fn new() -> Self {
        Self::with_defaults()
    }

    /// Erstellt einen neuen `FsToolProvider` mit expliziten Default-Werten.
    ///
    /// # Returns
    /// Ein `FsToolProvider` mit allen Limits auf ihren Defaults.
    ///
    /// # Concurrency
    /// Thread-safe.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self {
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
            max_list_entries: DEFAULT_MAX_ENTRIES,
            max_search_matches: DEFAULT_MAX_MATCHES,
            max_search_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

impl Default for FsToolProvider {
    /// Erstellt einen `FsToolProvider` mit Standard-Konfiguration.
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl ToolProvider for FsToolProvider {
    /// Gibt die Liste aller sechs FS-Tools zurück.
    ///
    /// # Returns
    /// `Vec<ToolSpec>` mit sechs Einträgen: `fs.read`, `fs.write`, `fs.list`,
    /// `fs.search`, `fs.glob`, `fs.grep`.
    ///
    /// # Concurrency
    /// Zustandslos; sicher für parallele Aufrufe.
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            fs_read_spec(),
            fs_write_spec(),
            fs_list_spec(),
            fs_search_spec(),
            FsGlobTool::spec(),
            FsGrepTool::spec(),
        ]
    }

    /// Gibt den Executor für den angegebenen Tool-Namen zurück.
    ///
    /// # Arguments
    /// - `name` (`&ToolName`): angefragter Tool-Name.
    ///
    /// # Returns
    /// `Some(Arc<dyn ToolExecutor>)` für bekannte Tool-Namen, sonst `None`.
    ///
    /// # Concurrency
    /// Thread-safe; erzeugt einen neuen Arc pro Aufruf.
    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        match name.as_str() {
            "fs.read" => Some(Arc::new(FsReadExecutor {
                max_bytes: self.max_read_bytes,
            })),
            "fs.write" => Some(Arc::new(FsWriteExecutor)),
            "fs.list" => Some(Arc::new(FsListExecutor {
                max_entries: self.max_list_entries,
            })),
            "fs.search" => Some(Arc::new(FsSearchExecutor {
                max_matches: self.max_search_matches,
                max_depth: self.max_search_depth,
            })),
            "fs.glob" => Some(Arc::new(FsGlobTool)),
            "fs.grep" => Some(Arc::new(FsGrepTool)),
            _ => None,
        }
    }

    /// Gibt an, ob parallele Aufrufe für den angegebenen Tool-Namen sicher sind.
    ///
    /// # Arguments
    /// - `name` (`&ToolName`): angefragter Tool-Name.
    ///
    /// # Returns
    /// `true` für `fs.read`, `fs.list`, `fs.search`, `fs.glob`, `fs.grep`
    /// (Reads sind commutative). `false` für `fs.write` und unbekannte Namen.
    ///
    /// # Concurrency
    /// Zustandslos.
    fn parallel_safe(&self, name: &ToolName) -> bool {
        matches!(
            name.as_str(),
            "fs.read" | "fs.list" | "fs.search" | "fs.glob" | "fs.grep"
        )
    }
}

// ---------------------------------------------------------------------------
// Tool spec builders
// ---------------------------------------------------------------------------

fn fs_read_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Path to the file to read, relative to the workspace root.".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "max_bytes".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            description: Some(
                "Optional byte limit for this call. Cannot exceed the provider's configured \
                 limit (at most 65536)."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "offset".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            description: Some(
                "Optional byte offset to start reading at. Use the offset reported in the \
                 truncation hint to continue reading a large file."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );

    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.read"),
        description:
            "Read a file relative to the workspace root. Returns file content as UTF-8 text. \
             Binary files are returned with lossy UTF-8 decoding. \
             Path traversal (e.g. '../') and symlinks are rejected. Directories cannot be read. \
             Content longer than min(max_bytes, 64 KiB) is truncated, not refused; the reply \
             then ends with a hint naming the offset to continue from."
                .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["path".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

fn fs_write_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Path to the file to write, relative to the workspace root.".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "content".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Content to write to the file (UTF-8).".to_owned()),
            ..Default::default()
        },
    );

    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.write"),
        description: "Write content to a file relative to the workspace root, atomically. \
             Creates the file if it does not exist; overwrites if it does. Parent directories \
             are not created. Requires WriteWorkspace permission. Path traversal, symlinked \
             path components and the protected areas .git/ and .harw/ are rejected."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["path".to_owned(), "content".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

fn fs_list_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Directory path to list, relative to the workspace root.".to_owned()),
            ..Default::default()
        },
    );
    props.insert(
        "max_entries".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            description: Some(
                "Maximum number of directory entries to return. Default: 200.".to_owned(),
            ),
            ..Default::default()
        },
    );

    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.list"),
        description: "List the contents of a directory relative to the workspace root. \
             Returns {entries: [{name, kind, size}], stopped?}, sorted by name. \
             'kind' is 'file', 'dir', or 'other' (symlinks are reported as 'other' and are \
             never followed). 'stopped' names the limit that cut the listing short. \
             Requires ReadWorkspace permission."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["path".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

fn fs_search_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "query".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Search term (case-sensitive substring match against file lines).".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Start path for recursive search, relative to workspace root. Default: '.'."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "max_matches".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            description: Some(
                "Maximum number of matching lines to return. Default: 100, maximum 1000."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );

    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.search"),
        description:
            "Recursively search files for a query string (case-sensitive substring match). \
             Returns {matches: [{path, line, text}], stopped?, skipped_files?}. \
             Symlinks are never followed. Skips .git, target, and node_modules directories. \
             Depth defaults to 8 levels (hard maximum 32); at most 1000 matches, 50000 \
             entries, 10 seconds and 64 KiB of output. Files larger than 8 MiB are skipped. \
             'stopped' names the limit that cut the search short. \
             Requires ReadWorkspace permission."
                .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["query".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_sandbox_with_permissions(root: &Path, permissions: Vec<Permission>) -> SandboxSpec {
        let ws_dir = root.join("ws");
        fs::create_dir_all(&ws_dir).unwrap();
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .unwrap();
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .unwrap();
        SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions))
    }

    fn make_ctx(sandbox: SandboxSpec) -> harw_tools::executor::ToolExecutionContext {
        harw_tools::executor::ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
    }

    fn make_call(name: &str, args: serde_json::Value) -> harw_tools::ToolCall {
        harw_tools::ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments: args,
        }
    }

    #[test]
    fn test_fs_tool_provider_lists_six_tools() {
        let provider = FsToolProvider::new();
        let tools = provider.tools();
        assert_eq!(tools.len(), 6, "expected exactly 6 tools");

        let names: HashSet<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains("fs.read"), "missing fs.read");
        assert!(names.contains("fs.write"), "missing fs.write");
        assert!(names.contains("fs.list"), "missing fs.list");
        assert!(names.contains("fs.search"), "missing fs.search");
        assert!(names.contains("fs.glob"), "missing fs.glob");
        assert!(names.contains("fs.grep"), "missing fs.grep");
    }

    #[test]
    fn test_fs_tool_provider_parallel_safe_flags() {
        let provider = FsToolProvider::new();

        assert!(provider.parallel_safe(&ToolName::new("fs.read")));
        assert!(provider.parallel_safe(&ToolName::new("fs.list")));
        assert!(provider.parallel_safe(&ToolName::new("fs.search")));
        assert!(provider.parallel_safe(&ToolName::new("fs.glob")));
        assert!(provider.parallel_safe(&ToolName::new("fs.grep")));
        assert!(!provider.parallel_safe(&ToolName::new("fs.write")));
        assert!(!provider.parallel_safe(&ToolName::new("unknown")));
    }

    #[test]
    fn test_fs_tool_provider_returns_executor_for_each_tool() {
        let provider = FsToolProvider::new();

        assert!(provider.executor(&ToolName::new("fs.read")).is_some());
        assert!(provider.executor(&ToolName::new("fs.write")).is_some());
        assert!(provider.executor(&ToolName::new("fs.list")).is_some());
        assert!(provider.executor(&ToolName::new("fs.search")).is_some());
        assert!(provider.executor(&ToolName::new("fs.glob")).is_some());
        assert!(provider.executor(&ToolName::new("fs.grep")).is_some());
        assert!(provider.executor(&ToolName::new("unknown")).is_none());
    }

    #[test]
    fn test_fs_tool_provider_default_equals_new() {
        let provider_default = FsToolProvider::default();
        let provider_new = FsToolProvider::new();

        // Both should return the same tool names
        let tools_default = provider_default.tools();
        let tools_new = provider_new.tools();
        let names_default: Vec<&str> = tools_default.iter().map(|t| t.name()).collect();
        let names_new: Vec<&str> = tools_new.iter().map(|t| t.name()).collect();
        assert_eq!(names_default, names_new);
    }

    #[test]
    fn test_fs_tool_provider_full_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("greet.txt"), "Hello, Harwness!").unwrap();

        let provider = FsToolProvider::new();
        let executor = provider.executor(&ToolName::new("fs.read")).unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call("fs.read", serde_json::json!({ "path": "greet.txt" }));

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result = rt.block_on(executor.execute(&ctx, &call)).unwrap();
        match result {
            harw_tools::ToolOutput::Text { content } => assert_eq!(content, "Hello, Harwness!"),
            other => panic!("expected text output, got: {other:?}"),
        }
    }
}
