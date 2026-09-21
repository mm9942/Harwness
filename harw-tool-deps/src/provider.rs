//! `DepsToolProvider` — bündelt alle fünf Dependency-Werkzeuge.
//!
//! # Verantwortung
//! Dieses Modul enthält keinerlei Logik: die Provider-Struktur samt `tools()`,
//! `executor(name)` und `parallel_safe(name)` erzeugt
//! [`harw_tools::tool_provider!`] aus den Tool-Typen. Name, Spezifikation,
//! Berechtigung und Parallelitäts-Zusage stammen ausschließlich aus den
//! `#[harw_macros::tool]`-Attributen der jeweiligen Module — es gibt hier keine
//! zweite Wahrheit, die von ihnen abweichen könnte.
//!
//! # Schlüsseltypen
//! - [`DepsToolProvider`] — Unit-Struktur mit `impl ToolProvider`.
//!
//! # Berechtigungen
//! - `deps.graph`, `deps.locked`: `ReadWorkspace`.
//! - `deps.source_read`, `deps.source_search`, `deps.source_list`:
//!   `ReadCargoRegistry` — der einzige Pfad dieses Crates aus dem Workspace hinaus.
//!
//! Alle fünf Tools sind read-only und `parallel_safe`.
//!
//! # Nebenläufigkeit
//! [`DepsToolProvider`] ist eine zustandslose Unit-Struktur und damit
//! `Send + Sync + Copy`; pro `executor()`-Aufruf entsteht ein frischer `Arc`.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_deps::DepsToolProvider;
//!
//! let provider = DepsToolProvider::new();
//! assert_eq!(provider.tools().len(), 5);
//! ```

use crate::graph_tool::DepsGraphTool;
use crate::locked_tool::DepsLockedTool;
use crate::source_tool::{DepsSourceListTool, DepsSourceReadTool, DepsSourceSearchTool};

harw_tools::tool_provider! {
    /// Stellt die fünf Dependency-Werkzeuge des Analyse-Modus bereit:
    /// `deps.graph`, `deps.locked`, `deps.source_read`, `deps.source_search`
    /// und `deps.source_list`.
    pub struct DepsToolProvider {
        DepsGraphTool,
        DepsLockedTool,
        DepsSourceReadTool,
        DepsSourceSearchTool,
        DepsSourceListTool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{block_on, sandbox_context, scratch_dir, tool_call};
    use harw_extension_api::contributors::ToolProvider as _;
    use harw_authority::Permission;
    use harw_tools::{ToolName, ToolOutput};
    use std::collections::HashSet;
    use std::fs;

    /// Alle fünf Tools werden beworben.
    #[test]
    fn test_provider_lists_all_five_tools() {
        let provider = DepsToolProvider::new();
        let tools = provider.tools();

        assert_eq!(tools.len(), 5);
        let names: HashSet<&str> = tools.iter().map(harw_tools::ToolSpec::name).collect();
        for expected in [
            "deps.graph",
            "deps.locked",
            "deps.source_read",
            "deps.source_search",
            "deps.source_list",
        ] {
            assert!(names.contains(expected), "{expected} fehlt");
        }
    }

    /// Jedes beworbene Tool ist auch auflösbar — sonst bewirbt der Provider
    /// einen Namen, den er nicht bedienen kann.
    #[test]
    fn test_provider_resolves_every_advertised_tool() {
        let provider = DepsToolProvider::new();
        for spec in provider.tools() {
            assert!(
                provider.executor(&ToolName::new(spec.name())).is_some(),
                "kein Executor für {}",
                spec.name()
            );
        }
        assert!(
            provider
                .executor(&ToolName::new("deps.unbekannt"))
                .is_none(),
            "unbekannte Namen dürfen nicht auf ein Default-Tool fallen"
        );
    }

    /// Alle Tools sind rein lesend und damit nebenläufig sicher; unbekannte
    /// Namen bleiben fail closed.
    #[test]
    fn test_provider_marks_every_tool_parallel_safe() {
        let provider = DepsToolProvider::new();
        for name in DepsToolProvider::TOOL_NAMES.iter().copied() {
            assert!(
                provider.parallel_safe(&ToolName::new(name)),
                "{name} müsste parallel_safe sein"
            );
        }
        assert!(!provider.parallel_safe(&ToolName::new("deps.unbekannt")));
    }

    /// Audit: jedes registrierte Tool deklariert eine Berechtigung, und die
    /// Quellcode-Tools genau `ReadCargoRegistry`.
    #[test]
    fn test_provider_declares_expected_permissions() {
        let declared: Vec<(&str, Option<Permission>)> = DepsToolProvider::TOOL_NAMES
            .iter()
            .copied()
            .zip(DepsToolProvider::TOOL_PERMISSIONS.iter().copied())
            .collect();

        for (name, permission) in &declared {
            assert!(permission.is_some(), "{name} deklariert keine Berechtigung");
        }

        for (name, permission) in &declared {
            let expected = if name.starts_with("deps.source_") {
                Permission::ReadCargoRegistry
            } else {
                Permission::ReadWorkspace
            };
            assert_eq!(
                *permission,
                Some(expected),
                "falsche Berechtigung für {name}"
            );
        }
    }

    /// Ohne `ReadCargoRegistry` verweigert `deps.source_read` den Dienst —
    /// bevor irgendein Pfad angefasst wird.
    #[test]
    fn test_source_read_denies_without_read_cargo_registry() {
        let harness = scratch_dir("provider-denied");
        fs::create_dir_all(harness.join("ws")).expect("Workspace anlegen");
        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace]);

        let provider = DepsToolProvider::new();
        let executor = provider
            .executor(&ToolName::new("deps.source_read"))
            .expect("Executor vorhanden");
        let call = tool_call(
            "deps.source_read",
            serde_json::json!({
                "crate_name": "serde",
                "version": "1.0.228",
                "path": "src/lib.rs",
            }),
        );

        let output = block_on(executor.execute(&context, &call)).expect("Tool läuft");

        match output {
            ToolOutput::Error { message } => assert!(
                message.contains("ReadCargoRegistry"),
                "die fehlende Berechtigung muss benannt werden, war: {message}"
            ),
            other => panic!("Fehlerausgabe erwartet, war: {other:?}"),
        }

        fs::remove_dir_all(&harness).ok();
    }

    /// Dasselbe für `deps.source_search` und `deps.source_list`.
    #[test]
    fn test_source_search_and_list_deny_without_read_cargo_registry() {
        let harness = scratch_dir("provider-denied-two");
        fs::create_dir_all(harness.join("ws")).expect("Workspace anlegen");
        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace]);
        let provider = DepsToolProvider::new();

        for (name, arguments) in [
            (
                "deps.source_search",
                serde_json::json!({
                    "crate_name": "serde",
                    "version": "1.0.228",
                    "pattern": "fn",
                }),
            ),
            (
                "deps.source_list",
                serde_json::json!({
                    "crate_name": "serde",
                    "version": "1.0.228",
                }),
            ),
        ] {
            let executor = provider
                .executor(&ToolName::new(name))
                .expect("Executor vorhanden");
            let call = tool_call(name, arguments);
            let output = block_on(executor.execute(&context, &call)).expect("Tool läuft");

            match output {
                ToolOutput::Error { message } => assert!(
                    message.contains("ReadCargoRegistry"),
                    "{name}: die fehlende Berechtigung muss benannt werden, war: {message}"
                ),
                other => panic!("{name}: Fehlerausgabe erwartet, war: {other:?}"),
            }
        }

        fs::remove_dir_all(&harness).ok();
    }

    /// Der Provider ist zustandslos: `new()` und `default()` sind gleichwertig.
    #[test]
    fn test_provider_is_default_constructible() {
        let from_new = DepsToolProvider::new();
        let from_default = DepsToolProvider;
        assert_eq!(format!("{from_new:?}"), format!("{from_default:?}"));
    }
}
