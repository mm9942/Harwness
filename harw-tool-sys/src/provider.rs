//! `SysToolProvider` — bündelt alle `sys.*`-Werkzeuge.
//!
//! Alle deklarieren `ExecuteProcess` (Vorbild `process.list`: sie lesen
//! Host-Zustand außerhalb des Workspace), sind `parallel_safe` und rein
//! lesend. Keines startet einen Fremdprozess.

use crate::date::SysDateTool;
use crate::env::SysEnvTool;
use crate::id::SysIdTool;
use crate::lsof::SysLsofTool;
use crate::net::SysSsTool;
use crate::pgrep::SysPgrepTool;
use crate::ps::SysPsTool;
use crate::sysinfo::{SysFreeTool, SysUnameTool, SysUptimeTool};
use crate::top::SysTopTool;
use crate::which::SysWhichTool;

harw_tools::tool_provider! {
    /// Stellt die lesenden Prozess-/System-Werkzeuge `sys.*` bereit
    /// (`ps`, `pgrep`, `top`, `free`, `uptime`, `uname`, `env`, `id`, `ss`,
    /// `lsof`, `date`, `which`).
    pub struct SysToolProvider {
        SysPsTool,
        SysPgrepTool,
        SysTopTool,
        SysFreeTool,
        SysUptimeTool,
        SysUnameTool,
        SysEnvTool,
        SysIdTool,
        SysSsTool,
        SysLsofTool,
        SysDateTool,
        SysWhichTool,
    }
}

/// Namen aller Werkzeuge dieses Providers (für Profil-Listen).
pub const SYS_TOOL_NAMES: &[&str] = SysToolProvider::TOOL_NAMES;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestError, TestResult, error_of, json_of, run};
    use harw_authority::Permission;
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{ToolName, ToolSpec};
    use serde_json::json;

    #[test]
    fn exposes_twelve_unique_execute_process_parallel_safe_tools() -> TestResult {
        let provider = SysToolProvider::new();
        let specs = provider.tools();
        assert_eq!(specs.len(), 12);
        let mut names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 12);
        for name in SYS_TOOL_NAMES {
            assert!(name.starts_with("sys."), "{name}");
            assert!(provider.executor(&ToolName::new(*name)).is_some(), "{name}");
            assert!(provider.parallel_safe(&ToolName::new(*name)), "{name}");
        }
        assert!(
            SysToolProvider::TOOL_PERMISSIONS
                .iter()
                .all(|p| *p == Some(Permission::ExecuteProcess))
        );
        Ok(())
    }

    #[test]
    fn descriptions_and_fields_are_documented_for_the_tool_index() -> TestResult {
        for spec in SysToolProvider::new().tools() {
            let ToolSpec::Function(function) = spec;
            let name = function.name.as_str().to_owned();
            assert!(
                function.description.len() > 60,
                "{name}: description too short"
            );
            assert!(
                function.description.contains(". "),
                "{name}: needs a first sentence"
            );
            for (field, schema) in function.parameters.properties.clone().unwrap_or_default() {
                assert!(
                    schema.description.as_ref().is_some_and(|d| d.len() > 8),
                    "{name}.{field} has no description"
                );
            }
            assert!(
                function
                    .parameters
                    .clone()
                    .into_strict()
                    .additional_properties
                    .is_some(),
                "{name}"
            );
        }
        Ok(())
    }

    fn minimal(name: &str) -> serde_json::Value {
        match name {
            "sys.pgrep" => json!({"pattern": "x"}),
            "sys.lsof" => json!({"pid": 1}),
            "sys.which" => json!({"names": ["sh"]}),
            _ => json!({}),
        }
    }

    #[tokio::test]
    async fn every_tool_rejects_unknown_options_and_missing_permission() -> TestResult {
        let fx = Fixture::new()?;
        let provider = SysToolProvider::new();
        let allowed = fx.exec_ctx()?;
        let denied = fx.ctx(vec![Permission::ReadWorkspace])?;
        for name in SYS_TOOL_NAMES {
            let tool = provider
                .executor(&ToolName::new(*name))
                .ok_or(TestError::Missing("executor"))?;
            let mut with_extra = minimal(name);
            with_extra["definitely_not_an_option"] = json!(1);
            match run(tool.as_ref(), &allowed, name, with_extra).await {
                Err(error) => assert!(
                    error.to_string().contains("definitely_not_an_option"),
                    "{name}: {error}"
                ),
                Ok(output) => assert!(
                    error_of(output)?.contains("definitely_not_an_option"),
                    "{name}"
                ),
            }
            match run(tool.as_ref(), &denied, name, minimal(name)).await {
                Err(_) => {}
                Ok(output) => {
                    error_of(output)?;
                }
            }
        }
        // Positivprobe: uname und id liefern echte Werte über den Executor.
        let uname = provider
            .executor(&ToolName::new("sys.uname"))
            .ok_or(TestError::Missing("uname"))?;
        let value = json_of(
            run(
                uname.as_ref(),
                &allowed,
                "sys.uname",
                json!({"kernel_name": true}),
            )
            .await?,
        )?;
        assert_eq!(value["text"], "Linux");
        let id = provider
            .executor(&ToolName::new("sys.id"))
            .ok_or(TestError::Missing("id"))?;
        let value = json_of(run(id.as_ref(), &allowed, "sys.id", json!({"user": true})).await?)?;
        assert_eq!(
            value["text"],
            rustix::process::geteuid().as_raw().to_string()
        );
        Ok(())
    }

    #[tokio::test]
    async fn hostile_arguments_never_panic_or_hang() -> TestResult {
        let fx = Fixture::new()?;
        let provider = SysToolProvider::new();
        let ctx = fx.exec_ctx()?;
        let strings = [
            json!(""),
            json!("\u{0}"),
            json!("é".repeat(3000)),
            json!("(((("),
            json!("*".repeat(600)),
            json!("%"),
        ];
        let numbers = [
            json!(0),
            json!(1),
            json!(u64::MAX),
            json!(-1),
            json!("18446744073709551616"),
            json!(1.5),
            json!("x"),
            json!(i64::MIN),
        ];
        let mut calls = 0usize;
        for name in SYS_TOOL_NAMES {
            if *name == "sys.top" {
                // `top` schläft bis zu 2 s je Aufruf; die Eingabeprüfung testet `top.rs`.
                continue;
            }
            let tool = provider
                .executor(&ToolName::new(*name))
                .ok_or(TestError::Missing("executor"))?;
            for text in &strings {
                for number in &numbers {
                    for arguments in [
                        json!({"pattern": text, "limit": number, "user": text, "ppids": [number], "pid": number}),
                        json!({"pids": [number], "name": text, "sort": text, "fields": [text], "limit": number, "states": [text]}),
                        json!({"names": [text], "prefix": text, "format": text, "offset_minutes": number, "epoch_secs": number, "port": number}),
                        json!({"command_contains": text, "name_contains": text}),
                    ] {
                        calls += 1;
                        match run(tool.as_ref(), &ctx, name, arguments.clone()).await {
                            Ok(output) => {
                                let rendered = serde_json::to_string(&output)?;
                                assert!(
                                    !rendered.contains("internal error"),
                                    "{name} panicked on {arguments}"
                                );
                                assert!(
                                    rendered.len()
                                        <= harw_tool_fsread::budget::MAX_OUTPUT_BYTES + 4096,
                                    "{name}: {} bytes",
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
            let text = std::fs::read_to_string(&path)?.replace("spawn_blocking(", "");
            for needle in &needles {
                assert!(
                    !text.contains(needle.as_str()),
                    "{} contains {needle}",
                    path.display()
                );
            }
        }
        Ok(())
    }
}
