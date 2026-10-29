//! `FsToolProvider` — aggregierter ToolProvider für alle acht FS-Tools.
//!
//! # Verantwortung
//! Dieses Modul fasst `fs.read`, `fs.write`, `fs.edit`, `fs.list`,
//! `fs.search`, `fs.glob` und `fs.grep` unter einem einzigen [`ToolProvider`]-Implementor
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

use crate::edit::FsEditExecutor;
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

/// Aggregierter ToolProvider für alle acht Filesystem-Tools.
///
/// # Description
/// Stellt `fs.read`, `fs.write`, `fs.edit`, `fs.list`, `fs.search`, `fs.glob`
/// und `fs.grep` bereit. Konfiguration der vier handgeschriebenen Tools erfolgt
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
/// let tools = provider.tools(); // returns 7 ToolSpecs
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
    /// Names of the eight tools, in [`ToolProvider::tools`] order (the same
    /// table `harw_tools::tool_provider!` generates for macro providers).
    pub const TOOL_NAMES: &'static [&'static str] = &[
        "fs.read",
        "fs.view_image",
        "fs.write",
        "fs.edit",
        "fs.list",
        "fs.search",
        FsGlobTool::NAME,
        FsGrepTool::NAME,
    ];

    /// Activity descriptors of the tools (PL-71, PL-97 M2), index-aligned with
    /// [`Self::TOOL_NAMES`], in the shape of `tool_provider!`'s
    /// `TOOL_ACTIVITIES`: reads, listings and searches are `observe`,
    /// `fs.write`/`fs.edit` are `mutate`; the resource key is always the `path`
    /// argument.
    ///
    /// Descriptive only: never a permission, never part of a `ToolSpec`.
    pub const TOOL_ACTIVITIES: &'static [Option<harw_tools::ActivityDescriptor>] = &[
        FsReadExecutor::ACTIVITY,
        crate::FsViewImageExecutor::ACTIVITY,
        FsWriteExecutor::ACTIVITY,
        FsEditExecutor::ACTIVITY,
        FsListExecutor::ACTIVITY,
        FsSearchExecutor::ACTIVITY,
        FsGlobTool::ACTIVITY,
        FsGrepTool::ACTIVITY,
    ];

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
    /// Gibt die Liste aller acht FS-Tools zurück.
    ///
    /// # Returns
    /// `Vec<ToolSpec>` mit acht Einträgen: `fs.read`, `fs.view_image`, `fs.write`, `fs.edit`,
    /// `fs.list`, `fs.search`, `fs.glob`, `fs.grep`.
    ///
    /// # Concurrency
    /// Zustandslos; sicher für parallele Aufrufe.
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            fs_read_spec(),
            fs_view_image_spec(),
            fs_write_spec(),
            fs_edit_spec(),
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
            "fs.view_image" => Some(Arc::new(crate::FsViewImageExecutor)),
            "fs.write" => Some(Arc::new(FsWriteExecutor)),
            "fs.edit" => Some(Arc::new(FsEditExecutor)),
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
    /// (Reads sind commutative). `false` für `fs.write`, `fs.edit` und unbekannte
    /// Namen.
    ///
    /// # Concurrency
    /// Zustandslos.
    fn parallel_safe(&self, name: &ToolName) -> bool {
        matches!(
            name.as_str(),
            "fs.read" | "fs.view_image" | "fs.list" | "fs.search" | "fs.glob" | "fs.grep"
        )
    }
}

// ---------------------------------------------------------------------------
// Tool spec builders
// ---------------------------------------------------------------------------

/// Beschreibt die geschützten Wurzelkomponenten für Tool-Beschreibungen
/// (`.git/` und `.harw/` — dazu, falls verschieden, der aktuell aktive,
/// projekt-lokale Name einer personalisierten harw (#22,
/// [`harw_home::project_dir_name`])). Muss mit
/// [`crate::write::protected_component`] übereinstimmen.
fn protected_areas_description() -> String {
    let named = harw_home::project_dir_name();
    if named.eq_ignore_ascii_case(".harw") {
        ".git/ and .harw/".to_owned()
    } else {
        format!(".git/, .harw/ and {named}/")
    }
}

fn fs_view_image_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Image file path relative to the workspace root.".to_owned()),
            ..Default::default()
        },
    );
    props.insert(
        "detail".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Image detail: low, high, or auto. Omit for provider default.".to_owned(),
            ),
            enum_values: Some(vec![
                serde_json::json!("low"),
                serde_json::json!("high"),
                serde_json::json!("auto"),
            ]),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.view_image"),
        description: "Open a PNG, JPEG, GIF or WebP image from the workspace to inspect its actual visual content. The next request receives native image input, then you can continue reasoning and tool use. Uses existing read permissions, image size and pixel limits; GIF uses its first frame. Text-only models receive an explicit vision_unavailable result; never infer visual observations from image metadata. No model switching occurs.".to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object), properties: Some(props),
            required: Some(vec!["path".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

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
                "Byte mode only: byte limit for this call (at most 65536). null (or 0) = not \
                 set."
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
                "Byte mode only: byte offset to start at; use the offset from the truncation \
                 hint to continue. null = not set; 0 is ignored when line mode or tail is used."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "line".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            description: Some(
                "Line mode: 1-based first line. null = 1. With 'offset'/'max_bytes' or 'tail' \
                 it must be null (or 1)."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "limit".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            description: Some(
                "Line mode: number of lines (null = 400, max 2000). With 'tail' it caps the \
                 tail line count. Must be null in byte mode."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "tail".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            description: Some(
                "Tail mode: return the last N lines (e.g. logs) with real line numbers, max \
                 2000. null (or 0) = not set. Requires 'offset'/'max_bytes' null and 'line' \
                 null (or 1)."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );

    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.read"),
        description:
            "Liest eine Textdatei mit Zeilennummern. Genau ein Modus; alle nicht benutzten \
             Parameter auf null setzen (null = nicht gesetzt): (1) Zeilenmodus, Standard: \
             `line` (Vorgabe 1) + `limit` (Vorgabe 400, max 2000). (2) Tail: `tail` = letzte N \
             Zeilen, z. B. für Logs (`limit` begrenzt zusätzlich). (3) Byte-Modus (Rückfall): \
             `offset`/`max_bytes`. Für bestimmte Stellen zuerst `fs.grep` (liefert \
             Zeilennummern), dann `fs.read` mit `line`. Kein `sed`/`head`/`tail`/`grep` über \
             `shell.exec` nötig. Für PDFs `doc.read_pdf`, für Bilder `fs.view_image` verwenden."
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
        description: format!(
            "Write content to a file relative to the workspace root, atomically. \
             Creates the file if it does not exist; overwrites if it does. Parent directories \
             are not created. Requires WriteWorkspace permission. Symlinks in the path are \
             followed only when their target stays inside the workspace; path traversal, \
             symlinks leading outside and the protected areas {} are rejected. {}",
            protected_areas_description(),
            crate::write::FS_WRITE_STEERING
        ),
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

fn fs_edit_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Path to the existing file to edit, relative to the workspace root.".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "old_string".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Exact text to replace (non-empty). Without replace_all it must occur exactly \
                 once; include enough surrounding context to make it unique."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "new_string".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Replacement text (must differ from old_string).".to_owned()),
            ..Default::default()
        },
    );
    props.insert(
        "replace_all".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Boolean),
            description: Some(
                "Replace every occurrence of old_string instead of exactly one. Default: false."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );

    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.edit"),
        description: format!(
            "Replace text in an existing UTF-8 file relative to the workspace root, \
             atomically. old_string must occur exactly once unless replace_all is true; \
             otherwise the call fails and reports the match count. Returns {{path, \
             replacements, diff_excerpt}}. Prefer fs.edit over fs.write for changes to \
             existing files. Requires WriteWorkspace permission. Symlinks in the path are \
             followed only inside the workspace; path traversal, symlinks leading outside and \
             the protected areas {} are rejected; files larger than 8 MiB are \
             not edited. {}",
            protected_areas_description(),
            crate::edit::FS_EDIT_STEERING
        ),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec![
                "path".to_owned(),
                "old_string".to_owned(),
                "new_string".to_owned(),
            ]),
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
                "Maximum number of directory entries to return. null (or 0) = default 200."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );

    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.list"),
        description: "List the contents of a directory relative to the workspace root. \
             Returns {entries: [{name, kind, size}], stopped?}, sorted by name. \
             'kind' is 'file', 'dir', or 'other' (symlinks inside the listing are reported as \
             'other'). A symlink in 'path' itself is followed: freely inside the workspace, \
             after user approval outside it. 'stopped' names the limit that cut the listing \
             short. Requires ReadWorkspace permission."
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
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_sandbox_with_permissions(
        root: &Path,
        permissions: Vec<Permission>,
    ) -> TestResult<SandboxSpec> {
        let ws_dir = root.join("ws");
        fs::create_dir_all(&ws_dir)?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("binding"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions),
        ))
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
    fn test_fs_tool_provider_lists_eight_tools() -> TestResult {
        let provider = FsToolProvider::new();
        let tools = provider.tools();
        assert_eq!(tools.len(), 8, "expected exactly 8 tools");
        assert!(tools.iter().any(|tool| tool.name() == "fs.view_image"));

        let names: HashSet<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains("fs.read"), "missing fs.read");
        assert!(names.contains("fs.write"), "missing fs.write");
        assert!(names.contains("fs.edit"), "missing fs.edit");
        assert!(names.contains("fs.list"), "missing fs.list");
        assert!(names.contains("fs.search"), "missing fs.search");
        assert!(names.contains("fs.glob"), "missing fs.glob");
        assert!(names.contains("fs.grep"), "missing fs.grep");
        Ok(())
    }

    #[test]
    fn test_fs_tool_provider_parallel_safe_flags() -> TestResult {
        let provider = FsToolProvider::new();

        assert!(provider.parallel_safe(&ToolName::new("fs.read")));
        assert!(provider.parallel_safe(&ToolName::new("fs.list")));
        assert!(provider.parallel_safe(&ToolName::new("fs.search")));
        assert!(provider.parallel_safe(&ToolName::new("fs.glob")));
        assert!(provider.parallel_safe(&ToolName::new("fs.grep")));
        assert!(!provider.parallel_safe(&ToolName::new("fs.write")));
        assert!(!provider.parallel_safe(&ToolName::new("fs.edit")));
        assert!(!provider.parallel_safe(&ToolName::new("unknown")));
        Ok(())
    }

    #[test]
    fn test_fs_tool_provider_returns_executor_for_each_tool() -> TestResult {
        let provider = FsToolProvider::new();

        assert!(provider.executor(&ToolName::new("fs.read")).is_some());
        assert!(provider.executor(&ToolName::new("fs.write")).is_some());
        assert!(provider.executor(&ToolName::new("fs.edit")).is_some());
        assert!(provider.executor(&ToolName::new("fs.list")).is_some());
        assert!(provider.executor(&ToolName::new("fs.search")).is_some());
        assert!(provider.executor(&ToolName::new("fs.glob")).is_some());
        assert!(provider.executor(&ToolName::new("fs.grep")).is_some());
        assert!(provider.executor(&ToolName::new("unknown")).is_none());
        Ok(())
    }

    #[test]
    fn test_fs_tool_provider_default_equals_new() -> TestResult {
        let provider_default = FsToolProvider::default();
        let provider_new = FsToolProvider::new();

        // Both should return the same tool names
        let tools_default = provider_default.tools();
        let tools_new = provider_new.tools();
        let names_default: Vec<&str> = tools_default.iter().map(|t| t.name()).collect();
        let names_new: Vec<&str> = tools_new.iter().map(|t| t.name()).collect();
        assert_eq!(names_default, names_new);
        Ok(())
    }

    #[test]
    fn test_fs_tool_provider_full_read_roundtrip() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(ws.join("greet.txt"), "Hello, Harwness!")?;

        let provider = FsToolProvider::new();
        let executor = provider
            .executor(&ToolName::new("fs.read"))
            .ok_or(TestError::Missing("fs.read executor"))?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        // W1-03: der Standardmodus von `fs.read` ist jetzt der Zeilenmodus;
        // `offset: 0` erzwingt hier explizit den (unveränderten) Byte-Modus,
        // damit dieser Roundtrip-Test weiterhin reine Byte-Ausgabe prüft.
        let call = make_call(
            "fs.read",
            serde_json::json!({ "path": "greet.txt", "offset": 0 }),
        );

        let rt = tokio::runtime::Builder::new_current_thread().build()?;
        let result = rt.block_on(executor.execute(&ctx, &call))?;
        match result {
            harw_tools::ToolOutput::Text { content } => assert_eq!(content, "Hello, Harwness!"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// R18 F7 (EX-04): die Beschreibungen von `fs.edit`/`fs.write` lenken
    /// Dateiänderungen weg von Shell-Heredocs und `python3 -`.
    #[test]
    fn test_fs_edit_and_write_specs_carry_the_steering_sentences() {
        let ToolSpec::Function(edit) = fs_edit_spec();
        assert!(
            edit.description.contains("python3 -"),
            "{}",
            edit.description
        );
        assert!(
            edit.description.contains(crate::edit::FS_EDIT_STEERING),
            "{}",
            edit.description
        );
        let ToolSpec::Function(write) = fs_write_spec();
        assert!(
            write.description.contains(crate::write::FS_WRITE_STEERING),
            "{}",
            write.description
        );
    }

    /// PL-97 M2: `TOOL_NAMES` folgt der Reihenfolge von `tools()`.
    #[test]
    fn test_fs_tool_names_follow_tools_order() {
        let tools = FsToolProvider::new().tools();
        let names: Vec<&str> = tools.iter().map(ToolSpec::name).collect();
        assert_eq!(names, FsToolProvider::TOOL_NAMES);
        assert_eq!(
            FsToolProvider::TOOL_ACTIVITIES.len(),
            FsToolProvider::TOOL_NAMES.len()
        );
    }

    /// PL-97 M2 (erste Migrationswelle, PL-71 §Migration): jedes `fs.*`-Tool
    /// trägt einen bekannten `filesystem`-Deskriptor; Lesen/Listen/Suchen ist
    /// `observe`, Schreiben/Editieren `mutate`, und das Ressourcenfeld ist ein
    /// Argument der modell-sichtbaren Spezifikation.
    #[test]
    fn test_fs_tool_activities_are_annotated() -> TestResult {
        let specs = FsToolProvider::new().tools();
        for ((name, activity), spec) in FsToolProvider::TOOL_NAMES
            .iter()
            .zip(FsToolProvider::TOOL_ACTIVITIES)
            .zip(&specs)
        {
            let activity = activity
                .as_ref()
                .ok_or(TestError::Missing("every fs.* tool declares an activity"))?;
            assert!(activity.is_known(), "{name}: {activity}");
            assert_eq!(activity.domain, "filesystem", "{name}");
            let verb = name
                .strip_prefix("fs.")
                .ok_or(TestError::Missing("fs. prefix"))?;
            assert_eq!(activity.verb, verb, "{name}");
            let expected = if matches!(*name, "fs.write" | "fs.edit") {
                harw_tools::Effect::Mutate
            } else {
                harw_tools::Effect::Observe
            };
            assert_eq!(activity.effect, expected, "{name}");
            assert_eq!(activity.resource_field.as_deref(), Some("path"), "{name}");
            let ToolSpec::Function(function) = spec;
            let properties = function.parameters.properties.clone().unwrap_or_default();
            assert!(
                properties.contains_key("path"),
                "{name}: the resource field must be an argument of the spec"
            );
        }
        // Die makro-generierten Tools liefern denselben Deskriptor wie die Tabelle.
        assert_eq!(FsGlobTool::ACTIVITY, FsToolProvider::TOOL_ACTIVITIES[6]);
        assert_eq!(FsGrepTool::ACTIVITY, FsToolProvider::TOOL_ACTIVITIES[7]);
        assert_eq!(FsGlobTool::SKILL, None);
        Ok(())
    }

    /// PL-97 M2: Die Activity-Metadaten ändern die modell-sichtbaren
    /// Spezifikationen nicht. Die Golden-Datei wurde **vor** der Annotation
    /// erfasst; absichtliche Änderungen werden mit
    /// `HARW_UPDATE_GOLDEN=1 cargo test -p harw-tool-fs` neu geschrieben.
    #[test]
    fn test_fs_tool_specs_match_golden() -> TestResult {
        let actual = serde_json::to_value(FsToolProvider::new().tools())?;
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join("fs_tool_specs.json");
        if std::env::var_os("HARW_UPDATE_GOLDEN").is_some() {
            let parent = path
                .parent()
                .ok_or(TestError::Missing("golden file has a parent directory"))?;
            fs::create_dir_all(parent)?;
            let mut text = serde_json::to_string_pretty(&actual)?;
            text.push('\n');
            fs::write(&path, text)?;
            return Ok(());
        }
        let expected: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        if expected != actual {
            return Err(TestError::Unexpected(format!(
                "fs tool specs differ from golden\nexpected: {expected:#}\nactual: {actual:#}"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_spec_explains_modes_and_null() {
        // Bugreport Runde 5: im Strict-Schema sind alle Felder required; die
        // Beschreibung muss sagen, dass ungenutzte Felder null sein dürfen
        // und welcher Modus welche Felder nutzt.
        let ToolSpec::Function(spec) = fs_read_spec();
        assert!(spec.description.contains("null = nicht gesetzt"));
        for mode in ["Zeilenmodus", "Tail", "Byte-Modus"] {
            assert!(spec.description.contains(mode), "{mode}");
        }
        let props = spec.parameters.properties.unwrap_or_default();
        for field in ["max_bytes", "offset", "line", "limit", "tail"] {
            let description = props
                .get(field)
                .and_then(|schema| schema.description.clone())
                .unwrap_or_default();
            assert!(description.contains("null"), "{field}: {description}");
        }
    }
}
