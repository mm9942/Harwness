//! `SysToolProvider` — bündelt alle `sys.*`-Werkzeuge.
//!
//! Alle deklarieren `ExecuteProcess` (Vorbild `process.list`: sie lesen
//! Host-Zustand außerhalb des Workspace), sind `parallel_safe` und rein
//! lesend. Keines startet einen Fremdprozess.
//!
//! # Umfang der Prozesswerkzeuge
//! `sys.ps`, `sys.pgrep` und `sys.top` tragen die [`ScopePolicy`] des
//! Providers. Die Vorgabe ([`SysToolProvider::new`]) erlaubt nur den eigenen
//! Umfang; erst [`SysToolProvider::with_approval_mode`] lässt `scope = "all"`
//! unter Full Access zu (PL-100 Phase 4).
//!
//! # Examples
//! ```
//! use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
//! use harw_tool_sys::SysToolProvider;
//!
//! let own_only = SysToolProvider::new();
//! assert!(!own_only.scope_policy().allows_all());
//! let mode = ApprovalModeCell::new(ApprovalMode::FullAccess);
//! let provider = SysToolProvider::new().with_approval_mode(mode);
//! assert!(provider.scope_policy().allows_all());
//! ```

use crate::date::SysDateTool;
use crate::env::SysEnvTool;
use crate::id::SysIdTool;
use crate::lsof::SysLsofTool;
use crate::net::SysSsTool;
use crate::pgrep::SysPgrepTool;
use crate::ps::SysPsTool;
use crate::scope::ScopePolicy;
use crate::sysinfo::{SysFreeTool, SysUnameTool, SysUptimeTool};
use crate::top::SysTopTool;
use crate::which::SysWhichTool;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_tools::Permission;

/// Name des gebündelten Skills aller `sys.*`-Werkzeuge.
const SKILL: &str = "sys-tools";

/// Stellt die lesenden Prozess-/System-Werkzeuge `sys.*` bereit
/// (`ps`, `pgrep`, `top`, `free`, `uptime`, `uname`, `env`, `id`, `ss`,
/// `lsof`, `date`, `which`).
///
/// # Concurrency
/// `Clone` teilt die Freigabe-Zelle der [`ScopePolicy`]; jeder
/// `executor()`-Aufruf baut einen frischen Executor über einem Klon.
#[derive(Debug, Clone, Default)]
pub struct SysToolProvider {
    /// Umfangs-Politik für `sys.ps`, `sys.pgrep` und `sys.top`.
    scope: ScopePolicy,
}

impl SysToolProvider {
    /// Provider mit der Vorgabe-Politik: nur der eigene Umfang.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lässt `scope = "all"` zu, solange `mode` Full Access zeigt.
    ///
    /// # Arguments
    /// - `mode`: die Freigabe-Zelle der Sitzung (dieselbe wie die der
    ///   Freigabepolitik; ein Klon teilt den Modus).
    #[must_use]
    pub fn with_approval_mode(mut self, mode: ApprovalModeCell) -> Self {
        self.scope = self.scope.with_full_access_gate(mode);
        self
    }

    /// Ersetzt die Umfangs-Politik (z. B. mit zusätzlichen Job-Wurzeln).
    #[must_use]
    pub fn with_scope_policy(mut self, policy: ScopePolicy) -> Self {
        self.scope = policy;
        self
    }

    /// Die aktuelle Umfangs-Politik.
    #[must_use]
    pub fn scope_policy(&self) -> &ScopePolicy {
        &self.scope
    }
}

harw_tools::tool_provider! {
    impl for SysToolProvider as provider, parallel_safe: all {
        SysPsTool::NAME => {
            spec: SysPsTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysPsTool::new(provider.scope.clone()),
        },
        SysPgrepTool::NAME => {
            spec: SysPgrepTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysPgrepTool::new(provider.scope.clone()),
        },
        SysTopTool::NAME => {
            spec: SysTopTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysTopTool::new(provider.scope.clone()),
        },
        SysFreeTool::NAME => {
            spec: SysFreeTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysFreeTool,
        },
        SysUptimeTool::NAME => {
            spec: SysUptimeTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysUptimeTool,
        },
        SysUnameTool::NAME => {
            spec: SysUnameTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysUnameTool,
        },
        SysEnvTool::NAME => {
            spec: SysEnvTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysEnvTool,
        },
        SysIdTool::NAME => {
            spec: SysIdTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysIdTool,
        },
        SysSsTool::NAME => {
            spec: SysSsTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysSsTool,
        },
        SysLsofTool::NAME => {
            spec: SysLsofTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysLsofTool,
        },
        SysDateTool::NAME => {
            spec: SysDateTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysDateTool,
        },
        SysWhichTool::NAME => {
            spec: SysWhichTool::spec(),
            permission: Permission::ExecuteProcess,
            skill: SKILL,
            executor: SysWhichTool,
        },
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

    /// Die handgeschriebene Tabelle stimmt mit den Konstanten überein, die
    /// `#[harw_macros::tool]` an jedem Werkzeug erzeugt (Name, Berechtigung,
    /// Skill, Parallelität), in derselben Reihenfolge wie bisher.
    #[test]
    fn provider_table_matches_the_tool_attributes() {
        let declared = [
            (
                SysPsTool::NAME,
                SysPsTool::PERMISSION,
                SysPsTool::SKILL,
                SysPsTool::PARALLEL_SAFE,
            ),
            (
                SysPgrepTool::NAME,
                SysPgrepTool::PERMISSION,
                SysPgrepTool::SKILL,
                SysPgrepTool::PARALLEL_SAFE,
            ),
            (
                SysTopTool::NAME,
                SysTopTool::PERMISSION,
                SysTopTool::SKILL,
                SysTopTool::PARALLEL_SAFE,
            ),
            (
                SysFreeTool::NAME,
                SysFreeTool::PERMISSION,
                SysFreeTool::SKILL,
                SysFreeTool::PARALLEL_SAFE,
            ),
            (
                SysUptimeTool::NAME,
                SysUptimeTool::PERMISSION,
                SysUptimeTool::SKILL,
                SysUptimeTool::PARALLEL_SAFE,
            ),
            (
                SysUnameTool::NAME,
                SysUnameTool::PERMISSION,
                SysUnameTool::SKILL,
                SysUnameTool::PARALLEL_SAFE,
            ),
            (
                SysEnvTool::NAME,
                SysEnvTool::PERMISSION,
                SysEnvTool::SKILL,
                SysEnvTool::PARALLEL_SAFE,
            ),
            (
                SysIdTool::NAME,
                SysIdTool::PERMISSION,
                SysIdTool::SKILL,
                SysIdTool::PARALLEL_SAFE,
            ),
            (
                SysSsTool::NAME,
                SysSsTool::PERMISSION,
                SysSsTool::SKILL,
                SysSsTool::PARALLEL_SAFE,
            ),
            (
                SysLsofTool::NAME,
                SysLsofTool::PERMISSION,
                SysLsofTool::SKILL,
                SysLsofTool::PARALLEL_SAFE,
            ),
            (
                SysDateTool::NAME,
                SysDateTool::PERMISSION,
                SysDateTool::SKILL,
                SysDateTool::PARALLEL_SAFE,
            ),
            (
                SysWhichTool::NAME,
                SysWhichTool::PERMISSION,
                SysWhichTool::SKILL,
                SysWhichTool::PARALLEL_SAFE,
            ),
        ];
        assert_eq!(SysToolProvider::TOOL_NAMES.len(), declared.len());
        for (index, (name, permission, skill, parallel)) in declared.into_iter().enumerate() {
            assert_eq!(SysToolProvider::TOOL_NAMES.get(index), Some(&name));
            assert_eq!(
                SysToolProvider::TOOL_PERMISSIONS.get(index),
                Some(&permission)
            );
            assert_eq!(SysToolProvider::TOOL_SKILLS.get(index), Some(&skill));
            assert_eq!(SysToolProvider::TOOL_ACTIVITIES.get(index), Some(&None));
            assert_eq!(
                SysToolProvider::tool_parallel_safe(name),
                parallel,
                "{name}"
            );
        }
    }

    /// `scope = "all"` erreicht die Werkzeuge nur über einen Provider mit
    /// Freigabe-Zelle im Modus Full Access.
    #[tokio::test]
    async fn scope_all_reaches_the_tools_only_under_full_access() -> TestResult {
        use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};

        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let mode = ApprovalModeCell::new(ApprovalMode::Delegated);
        let gated = SysToolProvider::new().with_approval_mode(mode.clone());
        let plain = SysToolProvider::new();
        for (name, arguments) in [
            ("sys.ps", json!({"scope": "all", "limit": 1})),
            ("sys.pgrep", json!({"pattern": "x", "scope": "all"})),
        ] {
            for provider in [&plain, &gated] {
                let tool = provider
                    .executor(&ToolName::new(name))
                    .ok_or(TestError::Missing("executor"))?;
                let message = error_of(run(tool.as_ref(), &ctx, name, arguments.clone()).await?)?;
                assert!(message.contains("Full Access"), "{name}: {message}");
            }
            mode.set(ApprovalMode::FullAccess);
            let tool = gated
                .executor(&ToolName::new(name))
                .ok_or(TestError::Missing("executor"))?;
            let value = json_of(run(tool.as_ref(), &ctx, name, arguments.clone()).await?)?;
            assert_eq!(value["scope"], "all", "{name}");
            // Der ungegatete Provider bleibt dabei.
            let tool = plain
                .executor(&ToolName::new(name))
                .ok_or(TestError::Missing("executor"))?;
            assert!(
                error_of(run(tool.as_ref(), &ctx, name, arguments).await?)?.contains("Full Access")
            );
            mode.set(ApprovalMode::Delegated);
        }
        // Ohne `scope` bleibt es beim eigenen Umfang.
        let ps = gated
            .executor(&ToolName::new("sys.ps"))
            .ok_or(TestError::Missing("executor"))?;
        let value = json_of(run(ps.as_ref(), &ctx, "sys.ps", json!({"limit": 1})).await?)?;
        assert_ne!(value["scope"], "all");
        Ok(())
    }

    /// Returns the value text right after `key =` (a top-level key on its own
    /// line) up to the end of `text`.
    fn manifest_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
        let mut offset = 0usize;
        for line in text.split_inclusive('\n') {
            let key_len = line
                .split_once('=')
                .filter(|(lhs, _)| lhs.trim() == key)
                .map(|(lhs, _)| lhs.len());
            if let Some(len) = key_len {
                return text.get(offset + len + 1..).map(str::trim_start);
            }
            offset += line.len();
        }
        None
    }

    /// Reads `name` and the `tools` array of a bundled skill manifest.
    ///
    /// The manifests are flat TOML with plain basic strings; any missing or
    /// malformed entry is an error, so a format change fails this test
    /// instead of letting it pass vacuously.
    fn manifest_name_and_tools(text: &str) -> TestResult<(&str, Vec<&str>)> {
        let name = manifest_value(text, "name")
            .and_then(|value| value.lines().next())
            .and_then(|line| line.trim().strip_prefix('"'))
            .and_then(|line| line.strip_suffix('"'))
            .ok_or(TestError::Missing("skill.toml: name"))?;
        let list = manifest_value(text, "tools")
            .and_then(|value| value.strip_prefix('['))
            .and_then(|value| value.split_once(']'))
            .map(|(list, _)| list)
            .ok_or(TestError::Missing("skill.toml: tools"))?;
        // Zwischen den Strings darf nur `,` und Leerraum stehen.
        let well_formed = list.matches('"').count() % 2 == 0
            && list
                .split('"')
                .step_by(2)
                .all(|gap| gap.chars().all(|c| c == ',' || c.is_whitespace()));
        if !well_formed {
            return Err(TestError::Unexpected(format!(
                "skill.toml: malformed tools list: {list}"
            )));
        }
        let tools: Vec<&str> = list.split('"').skip(1).step_by(2).collect();
        Ok((name, tools))
    }

    /// PL-97 M2/S4: every tool links to the bundled `sys-tools` skill, and
    /// that skill's manifest lists exactly this provider's tools.
    #[test]
    fn tool_skills_match_the_bundled_skill_manifest() -> TestResult {
        use std::collections::BTreeSet;

        let (skill, listed) =
            manifest_name_and_tools(include_str!("../skills/sys-tools/skill.toml"))?;
        assert_eq!(skill, "sys-tools");
        assert_eq!(SysToolProvider::TOOL_SKILLS.len(), SYS_TOOL_NAMES.len());
        for (tool, linked) in SYS_TOOL_NAMES.iter().zip(SysToolProvider::TOOL_SKILLS) {
            assert_eq!(*linked, Some(skill), "{tool}");
        }
        let manifest_tools: BTreeSet<&str> = listed.iter().copied().collect();
        assert_eq!(
            manifest_tools.len(),
            listed.len(),
            "duplicate in skill.toml"
        );
        let provided: BTreeSet<&str> = SYS_TOOL_NAMES.iter().copied().collect();
        assert_eq!(manifest_tools, provided);
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
