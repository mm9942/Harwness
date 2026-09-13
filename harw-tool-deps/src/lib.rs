//! `harw-tool-deps` — Dependency-Werkzeuge für den Analyse-Modus.
//!
//! # Zweck
//! Ein Explore-Agent soll **belegen** können, wie eine Abhängigkeit wirklich
//! funktioniert, statt sie zu erraten. Dieses Crate liefert dafür zwei
//! Bausteine:
//!
//! 1. **Bottom-up-Wissen über den eigenen Workspace** — `deps.graph` liefert
//!    den Crate-Graphen in Leaf-first-Ebenen, `deps.locked` die exakt
//!    gebauten Dependency-Versionen aus `Cargo.lock`.
//! 2. **Lesezugriff auf Dependency-Quellcode** — `deps.source_read`,
//!    `deps.source_search` und `deps.source_list` öffnen den bereits entpackten
//!    Quellcode im lokalen Cargo-Registry-Cache
//!    (`$CARGO_HOME/registry/src`), den der eigene Build ohnehin kompiliert.
//!
//! # Sicherheitskontrakt
//! Dieses Crate ist die einzige Tool-Oberfläche des Harness, die absichtlich
//! **außerhalb** des Workspace liest. Dafür gilt:
//!
//! - **Read-only.** Es gibt keinen schreibenden Codepfad — weder im Workspace
//!   noch in der Registry.
//! - **Eigene Berechtigung.** Der Registry-Zugriff hängt ausschließlich an
//!   [`harw_sandbox::Permission::ReadCargoRegistry`]. Sie benennt genau einen
//!   fest verdrahteten Pfadbaum und keine vom Aufrufer wählbare Position.
//!   `deps.graph`/`deps.locked` bleiben bei
//!   [`harw_sandbox::Permission::ReadWorkspace`].
//! - **Permission vor Deserialisierung.** Der von `#[harw_macros::tool]`
//!   erzeugte Prolog prüft die Berechtigung, bevor die vom Modell
//!   kontrollierten Argumente überhaupt geparst werden.
//! - **Zweistufiges Containment.** Registry-Pfade werden erst lexikalisch
//!   gegen die Registry-Wurzel geprüft (`resolve_contained`, weist `../` ab)
//!   und danach kanonisiert gegen das Crate-Verzeichnis
//!   ([`source_tool::RegistryAccess::resolve_checked`], weist Symlinks nach
//!   außen und Ausflüge in fremde Crates ab).
//! - **Traversierung folgt keinem Symlink.** Suche und Auflistung überspringen
//!   Symlinks vollständig, statt sie aufzulösen.
//! - **Harte Limits.** Dateigröße (256 KiB Standard, 1 MiB hart), Trefferzahl
//!   (50/500), Verzeichnistiefe (8) und Einträge pro Ebene (200) sind gedeckelt;
//!   ein vom Modell übergebenes Limit kann das harte Maximum nie überschreiten.
//! - **Kein Netz, kein Subprozess.** Weder `cargo` noch HTTP werden aufgerufen;
//!   gelesen wird nur, was lokal bereits liegt.
//!
//! # Schlüsseltypen
//! - [`DepsToolProvider`] — der `ToolProvider` mit allen fünf Tools.
//! - [`DepsToolError`] / `DepsToolResult` — crate-weiter Fehlertyp.
//! - [`source_tool::RegistryAccess`] — Registry-Wurzel plus Containment-Prüfung.
//!
//! # Fehler
//! Alle Hilfsfunktionen liefern [`DepsToolError`]; die Tool-Executors wandeln
//! jeden Fehler in `Ok(ToolOutput::Error)` — fail closed, ohne Panic.
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync` und zustandslos. Alle fünf Tools sind
//! `parallel_safe`, weil sie ausschließlich lesen.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_deps::DepsToolProvider;
//!
//! let provider = DepsToolProvider::new();
//! for spec in provider.tools() {
//!     println!("{}", spec.name());
//! }
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod graph_tool;
pub mod locked_tool;
pub mod provider;
pub mod source_tool;

pub use error::{DepsToolError, DepsToolResult};
pub use graph_tool::{CrateSummary, DepsGraphArgs, DepsGraphTool, graph_as_json};
pub use locked_tool::{
    DepsLockedArgs, DepsLockedTool, LockedOverview, LockedSummary, external_summary, locked_detail,
};
pub use provider::DepsToolProvider;
pub use source_tool::{
    DepsSourceListArgs, DepsSourceListTool, DepsSourceReadArgs, DepsSourceReadTool,
    DepsSourceSearchArgs, DepsSourceSearchTool, RegistryAccess, SearchOutcome, SourceEntry,
    SourceFile, SourceListing, SourceMatch, list_source, read_source_file, search_source,
};

/// Gemeinsame Fixture-Helfer der Unit-Tests dieses Crates.
///
/// Bewusst ohne `tempfile`-Abhängigkeit: die Verzeichnisse entstehen mit
/// `std::fs` unterhalb von [`std::env::temp_dir`] (Muster aus
/// `harw-sandbox/src/lib.rs`). Kein Test benötigt Netzzugriff oder einen
/// Subprozess.
#[cfg(test)]
pub(crate) mod test_support {
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_tools::{ToolCall, ToolExecutionContext, ToolName};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Legt ein frisches, leeres Scratch-Verzeichnis für ein Label an.
    ///
    /// Der Prozess-PID im Namen trennt parallele Testläufe, das Label die
    /// Tests innerhalb eines Laufs — jedes Label darf deshalb nur einmal
    /// vorkommen.
    pub(crate) fn scratch_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("harw-tool-deps-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("Scratch-Verzeichnis anlegen");
        dir
    }

    /// Baut einen Ausführungskontext, dessen Workspace `<harness>/ws` ist.
    pub(crate) fn sandbox_context(
        harness_root: &Path,
        permissions: Vec<Permission>,
    ) -> ToolExecutionContext {
        let workspace_dir = harness_root.join("ws");
        fs::create_dir_all(&workspace_dir).expect("Workspace-Verzeichnis anlegen");

        let registry = WorkspaceRegistry::build(
            harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .expect("Workspace-Registry bauen");
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .expect("Workspace auflösen");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));

        ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
    }

    /// Baut einen Tool-Aufruf mit den angegebenen JSON-Argumenten.
    pub(crate) fn tool_call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments,
        }
    }

    /// Führt ein Future auf einer Einzel-Thread-Runtime aus.
    pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("Tokio-Runtime bauen")
            .block_on(future)
    }

    /// Legt unter `<harness>/ws` einen Mini-Workspace `a -> b -> c` an und
    /// liefert `(harness_root, workspace_root)`.
    pub(crate) fn mini_workspace(label: &str) -> (PathBuf, PathBuf) {
        let harness = scratch_dir(label);
        let workspace = harness.join("ws");
        fs::create_dir_all(&workspace).expect("Workspace-Verzeichnis anlegen");

        write_member(&workspace, "a", &[], &["serde"]);
        write_member(&workspace, "b", &["a"], &[]);
        write_member(&workspace, "c", &["b"], &[]);
        fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\n",
        )
        .expect("Wurzel-Manifest schreiben");

        (harness, workspace)
    }

    /// Schreibt ein Member-Manifest mit internen (`path`) und externen Deps.
    fn write_member(workspace: &Path, name: &str, internal: &[&str], external: &[&str]) {
        let dir = workspace.join(name);
        fs::create_dir_all(&dir).expect("Member-Verzeichnis anlegen");

        let mut dependencies = String::new();
        for dep in internal {
            dependencies.push_str(&format!("{dep} = {{ path = \"../{dep}\" }}\n"));
        }
        for dep in external {
            dependencies.push_str(&format!("{dep} = \"1\"\n"));
        }
        let manifest = format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependencies}"
        );
        fs::write(dir.join("Cargo.toml"), manifest).expect("Member-Manifest schreiben");
    }

    /// Schreibt eine `Cargo.lock` mit einem externen und einem Path-Paket.
    pub(crate) fn write_lockfile(workspace: &Path) {
        let content = r#"
version = 4

[[package]]
name = "serde"
version = "1.0.228"
source = "registry+https://example.invalid/index"
checksum = "abc123"

[[package]]
name = "harw-types"
version = "0.2.0"
"#;
        fs::write(workspace.join("Cargo.lock"), content).expect("Cargo.lock schreiben");
    }
}
