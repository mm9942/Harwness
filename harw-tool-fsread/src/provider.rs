//! `FsreadToolProvider` — bündelt alle `fsread.*`-Werkzeuge.
//!
//! Alle Werkzeuge deklarieren `ReadWorkspace`, sind `parallel_safe` und
//! rein lesend; keines startet einen Fremdprozess. Namen, Spezifikationen und
//! Berechtigungen stammen ausschließlich aus den von `#[harw_macros::tool]`
//! erzeugten Konstanten (`TOOL_NAMES`, `TOOL_PERMISSIONS`).

use crate::cat::FsreadCatTool;
use crate::df::FsreadDfTool;
use crate::diff::FsreadDiffTool;
use crate::du::FsreadDuTool;
use crate::file::FsreadFileTool;
use crate::find::FsreadFindTool;
use crate::hash::FsreadHashTool;
use crate::headtail::{FsreadHeadTool, FsreadTailTool};
use crate::json::FsreadJsonTool;
use crate::links::{FsreadReadlinkTool, FsreadRealpathTool};
use crate::ls::FsreadLsTool;
use crate::stat::FsreadStatTool;
use crate::tree::FsreadTreeTool;
use crate::wc::FsreadWcTool;

harw_tools::tool_provider! {
    /// Stellt die rein lesenden Dateisystem-Werkzeuge `fsread.*` bereit
    /// (`ls`, `stat`, `find`, `du`, `df`, `wc`, `head`, `tail`, `cat`, `file`,
    /// `tree`, `readlink`, `realpath`, `hash`, `diff`, `json`).
    pub struct FsreadToolProvider {
        FsreadLsTool,
        FsreadStatTool,
        FsreadFindTool,
        FsreadDuTool,
        FsreadDfTool,
        FsreadWcTool,
        FsreadHeadTool,
        FsreadTailTool,
        FsreadCatTool,
        FsreadFileTool,
        FsreadTreeTool,
        FsreadReadlinkTool,
        FsreadRealpathTool,
        FsreadHashTool,
        FsreadDiffTool,
        FsreadJsonTool,
    }
}

/// Namen aller Werkzeuge dieses Providers (für Profil-Listen).
pub const FSREAD_TOOL_NAMES: &[&str] = FsreadToolProvider::TOOL_NAMES;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestError, TestResult, error_of, json_of, run};
    use harw_authority::Permission;
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{ToolName, ToolSpec};
    use serde_json::json;

    #[test]
    fn exposes_sixteen_unique_read_only_parallel_safe_tools() -> TestResult {
        let provider = FsreadToolProvider::new();
        let specs = provider.tools();
        assert_eq!(specs.len(), 16);
        let mut names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 16);
        for name in FSREAD_TOOL_NAMES {
            assert!(name.starts_with("fsread."), "{name}");
            assert!(provider.executor(&ToolName::new(*name)).is_some(), "{name}");
            assert!(provider.parallel_safe(&ToolName::new(*name)), "{name}");
        }
        assert!(provider.executor(&ToolName::new("fsread.nope")).is_none());
        assert!(
            FsreadToolProvider::TOOL_PERMISSIONS
                .iter()
                .all(|p| *p == Some(Permission::ReadWorkspace))
        );
        Ok(())
    }

    #[test]
    fn descriptions_and_fields_are_documented_for_the_tool_index() -> TestResult {
        for spec in FsreadToolProvider::new().tools() {
            let ToolSpec::Function(function) = spec;
            let name = function.name.as_str().to_owned();
            assert!(
                function.description.len() > 60,
                "{name}: description too short"
            );
            // Erster Satz muss vollständig sein (Tool-Index schneidet dort).
            assert!(
                function.description.contains(". "),
                "{name}: needs a first sentence"
            );
            let properties = function.parameters.properties.clone().unwrap_or_default();
            for (field, schema) in &properties {
                assert!(
                    schema.description.as_ref().is_some_and(|d| d.len() > 8),
                    "{name}.{field} has no description"
                );
            }
            // Pflichtfelder sind ein Teil der Properties.
            for required in function.parameters.required.clone().unwrap_or_default() {
                assert!(properties.contains_key(&required), "{name}: {required}");
            }
            // Strict-Umwandlung darf nicht scheitern und setzt additionalProperties=false.
            let strict = function.parameters.clone().into_strict();
            assert!(strict.additional_properties.is_some(), "{name}");
        }
        Ok(())
    }

    /// Kleinste gültige Argumente je Werkzeug.
    fn minimal(name: &str) -> serde_json::Value {
        match name {
            "fsread.stat" | "fsread.wc" | "fsread.file" | "fsread.hash" => {
                json!({"paths": ["f.txt"]})
            }
            "fsread.head" | "fsread.tail" | "fsread.cat" | "fsread.readlink"
            | "fsread.realpath" | "fsread.json" => {
                json!({"path": "f.txt"})
            }
            "fsread.diff" => json!({"a": "f.txt", "b": "f.txt"}),
            _ => json!({}),
        }
    }

    #[tokio::test]
    async fn every_tool_rejects_unknown_options_and_missing_permission() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f.txt", b"x\n")?;
        let provider = FsreadToolProvider::new();
        let read = fx.read_ctx()?;
        let none = fx.ctx(vec![Permission::WriteWorkspace])?;
        for name in FSREAD_TOOL_NAMES {
            let tool = provider
                .executor(&ToolName::new(*name))
                .ok_or(TestError::Missing("executor"))?;
            // Eine Option, die kein Werkzeug kennt: abgelehnt, nicht ignoriert.
            let mut with_extra = minimal(name);
            with_extra["definitely_not_an_option"] = json!(1);
            let outcome = run(tool.as_ref(), &read, name, with_extra).await;
            match outcome {
                Err(error) => assert!(
                    error.to_string().contains("definitely_not_an_option"),
                    "{name}: {error}"
                ),
                Ok(output) => {
                    let message = error_of(output)?;
                    assert!(
                        message.contains("definitely_not_an_option"),
                        "{name}: {message}"
                    );
                }
            }
            // Ohne ReadWorkspace: nichts läuft.
            let denied = run(tool.as_ref(), &none, name, minimal(name)).await;
            match denied {
                Err(_) => {}
                Ok(output) => {
                    error_of(output)?;
                }
            }
        }
        // Positivprobe über den Executor.
        let ls = provider
            .executor(&ToolName::new("fsread.ls"))
            .ok_or(TestError::Missing("ls"))?;
        let value = json_of(run(ls.as_ref(), &read, "fsread.ls", json!({})).await?)?;
        assert_eq!(value["entries"][0]["name"], "f.txt");
        Ok(())
    }

    #[tokio::test]
    async fn hostile_arguments_never_panic_or_hang() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("f.txt", b"x\ny\n")?;
        fx.write("bin", &[0, 159, 146, 150])?;
        fx.write("deep/a/b/c/d/e/f/g/h/i/j/k.txt", b"1")?;
        let provider = FsreadToolProvider::new();
        let read = fx.read_ctx()?;
        let nasty_strings = [
            json!(""),
            json!("\u{0}"),
            json!("a\u{0}b"),
            json!("../".repeat(500)),
            json!("/".repeat(5000)),
            json!("é".repeat(3000)),
            json!("[[[[[[[[[["),
            json!("*".repeat(600)),
            json!("."),
            json!("link_dir/../../.."),
            json!("\u{202e}rtl"),
            json!("f.txt/"),
            json!("bin"),
        ];
        let nasty_numbers = [
            json!(0),
            json!(1),
            json!(u64::MAX),
            json!(-1),
            json!("18446744073709551616"),
            json!(1.5),
            json!("x"),
        ];
        let mut calls = 0usize;
        for name in FSREAD_TOOL_NAMES {
            let tool = provider
                .executor(&ToolName::new(*name))
                .ok_or(TestError::Missing("executor"))?;
            for text in &nasty_strings {
                for number in &nasty_numbers {
                    let attempts = [
                        json!({"path": text, "limit": number, "max_depth": number, "lines": number, "bytes": number, "query": text, "context": number, "offset": number, "max_bytes": number}),
                        json!({"paths": [text, text], "from_line": number}),
                        json!({"a": text, "b": text, "context": number}),
                        json!({"path": text, "name": text, "iname": text, "path_glob": text, "min_size": number, "modified_within_secs": number}),
                    ];
                    for arguments in attempts {
                        calls += 1;
                        // Ein Panic im Blocking-Task wird als „internal error“ gemeldet.
                        match run(tool.as_ref(), &read, name, arguments.clone()).await {
                            Ok(output) => {
                                let rendered = serde_json::to_string(&output)?;
                                assert!(
                                    !rendered.contains("internal error"),
                                    "{name} panicked on {arguments}"
                                );
                                assert!(
                                    rendered.len() <= crate::budget::MAX_OUTPUT_BYTES + 4096,
                                    "{name}: output of {} bytes on {arguments}",
                                    rendered.len()
                                );
                            }
                            Err(error) => assert!(!error.to_string().is_empty()),
                        }
                    }
                }
            }
        }
        assert!(calls > 1000);
        Ok(())
    }

    #[test]
    fn sources_never_start_a_process() -> TestResult {
        // Die Nadeln werden zusammengesetzt, damit diese Datei sich nicht selbst findet.
        let needles = [
            ["std::proc", "ess::Command"].concat(),
            ["tokio::proc", "ess"].concat(),
            ["Command::", "new("].concat(),
            [".spa", "wn("].concat(),
            ["libc::", "system"].concat(),
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            for needle in &needles {
                // `spawn_blocking` ist kein Prozessstart.
                let cleaned = text.replace("spawn_blocking(", "");
                assert!(
                    !cleaned.contains(needle.as_str()),
                    "{} contains {needle}",
                    path.display()
                );
            }
        }
        Ok(())
    }
}
