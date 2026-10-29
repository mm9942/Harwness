//! Presentation of trusted, additive built-in command results. Payloads and CLI
//! text remain operation-owned; no text matching, persistence inference or TLS.
use crate::i18n::{tr_args_for, tr_for};
use harw_config::UiLanguage;
use harw_operations::{OpOutput, OperationMeta};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectiveTiming {
    Current,
    NextTurn,
    NextUiaTurn,
    NextSession,
    NextConfigLoad,
    NewAgents,
    NextToolCall,
    RoutingUnconfirmed,
}
impl EffectiveTiming {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "current" => Self::Current,
            "next_turn" => Self::NextTurn,
            "next_uia_turn" => Self::NextUiaTurn,
            "next_session" => Self::NextSession,
            "next_config_load" => Self::NextConfigLoad,
            "new_agents" => Self::NewAgents,
            "next_tool_call" => Self::NextToolCall,
            "routing_unconfirmed" => Self::RoutingUnconfirmed,
            _ => return None,
        })
    }
    fn text(self, language: UiLanguage) -> &'static str {
        match self {
            Self::Current => "",
            Self::NextTurn => tr_for(
                language,
                "operation_feedback.next_turn",
                "Effective from the next turn.",
            ),
            Self::NextUiaTurn => tr_for(
                language,
                "operation_feedback.next_uia_turn",
                "Effective from the next UIA turn.",
            ),
            Self::NextSession => tr_for(
                language,
                "operation_feedback.next_session",
                "Applies to the next session if the configuration is saved; see any save note below.",
            ),
            Self::NextConfigLoad => tr_for(
                language,
                "operation_feedback.next_config_load",
                "Effective after the next config load (next session); this does not run the skill.",
            ),
            Self::NewAgents => tr_for(
                language,
                "operation_feedback.new_agents",
                "Applies to newly started agents; running agents keep their model.",
            ),
            Self::NextToolCall => tr_for(
                language,
                "operation_feedback.next_tool_call",
                "Effective from the next tool call, for this session only.",
            ),
            Self::RoutingUnconfirmed => tr_for(
                language,
                "operation_feedback.routing_unconfirmed",
                "Worker selection requested; current-session routing is not confirmed by this result.",
            ),
        }
    }
}

/// Only returned after both concrete metadata provenance and semantic schema
/// validation. Callers must retain this value rather than reparsing arbitrary data.
pub(crate) struct Presentation {
    pub text: String,
    pub timing: EffectiveTiming,
}
fn nullable<'a>(data: &'a Value, key: &str) -> Option<Option<&'a str>> {
    let value = data.get(key)?;
    if value.is_null() {
        Some(None)
    } else {
        Some(Some(value.as_str()?))
    }
}
fn description(language: UiLanguage, mode: &str) -> Option<&'static str> {
    Some(match mode {
        "chat" => tr_for(
            language,
            "pickers.summary_chat",
            "Conversation without tool restrictions",
        ),
        "plan" => tr_for(
            language,
            "pickers.summary_plan",
            "Plan and research without making changes",
        ),
        "explore" => tr_for(
            language,
            "pickers.summary_explore",
            "Read-only tools for exploration",
        ),
        "work" => tr_for(
            language,
            "pickers.summary_work",
            "Execute with all tools, including writing and shell",
        ),
        "shell" => tr_for(
            language,
            "pickers.summary_shell",
            "Like work; host tools require user-approved sandbox access",
        ),
        "ask" => tr_for(
            language,
            "pickers.approval_ask",
            "Confirm every tool call individually",
        ),
        "auto" => tr_for(
            language,
            "pickers.approval_auto",
            "harw approves read tools; the auto classifier judges the rest and asks when unsure",
        ),
        "full" => tr_for(
            language,
            "pickers.approval_full",
            "No tool call requires confirmation",
        ),
        _ => return None,
    })
}

pub(crate) fn project(
    meta: &OperationMeta,
    language: UiLanguage,
    output: &OpOutput,
) -> Option<Presentation> {
    if !crate::registry::is_builtin(meta) {
        return None;
    }
    let data = output.data.as_ref()?.get("ui_feedback")?;
    let kind = data.get("kind")?.as_str()?;
    let timing = EffectiveTiming::parse(data.get("effective")?.as_str()?)?;
    let t = |key, english, args: &[(&str, &str)]| tr_args_for(language, key, english, args);
    let mut lines = Vec::new();
    match (meta.name, kind) {
        ("skills", "skill_state") if timing == EffectiveTiming::NextConfigLoad => {
            let enabled = data.get("enabled")?.as_bool()?;
            let changed = data.get("changed")?.as_bool()?;
            let state = if enabled {
                tr_for(language, "operation_feedback.activated", "Activated")
            } else {
                tr_for(language, "operation_feedback.deactivated", "Deactivated")
            };
            lines.push(t(
                "operation_feedback.skill",
                "Skill {name}: {state} ({manifest}).",
                &[
                    ("name", data.get("name")?.as_str()?),
                    ("state", state),
                    ("manifest", data.get("manifest")?.as_str()?),
                ],
            ));
            if !changed {
                lines.push(
                    tr_for(language, "operation_feedback.already", "Already set.").to_owned(),
                );
            }
        }
        ("model" | "uia-model" | "uia-worker-model", "model_selection") => {
            let target = data.get("target")?.as_str()?;
            let expected = match meta.name {
                "model" => ("session", EffectiveTiming::NextTurn),
                "uia-model" => ("uia", EffectiveTiming::NextUiaTurn),
                _ => ("uia-worker", EffectiveTiming::NextSession),
            };
            if (target, timing) != expected {
                return None;
            }
            let model = nullable(data, "model")?;
            // UIA always supplies this nullable field, including when a model
            // is selected. Never discard incompatible cleared-model detail.
            let cleared_model = if target == "uia" {
                let cleared = nullable(data, "cleared_model")?;
                if model.is_some() && cleared.is_some() {
                    return None;
                }
                cleared
            } else {
                None
            };
            lines.push(t(
                "operation_feedback.model",
                "Model selection ({target}): {provider}/{model}.",
                &[
                    ("target", target),
                    ("provider", data.get("provider")?.as_str()?),
                    ("model", model.unwrap_or("—")),
                ],
            ));
            if model.is_none() {
                if target != "uia" {
                    return None;
                }
                if let Some(previous) = cleared_model {
                    lines.push(t(
                        "operation_feedback.cleared",
                        "Incompatible model cleared: {model}.",
                        &[("model", previous)],
                    ));
                }
                lines.push(
                    tr_for(
                        language,
                        "operation_feedback.no_model",
                        "No model selected; use /uia-model list to choose one.",
                    )
                    .to_owned(),
                );
            }
        }
        ("models", "role_model") => {
            let live = data.get("live")?.as_bool()?;
            if timing
                != if live {
                    EffectiveTiming::NewAgents
                } else {
                    EffectiveTiming::NextSession
                }
            {
                return None;
            }
            let role = data.get("role")?.as_str()?;
            harw_config::ModelRole::parse(role)?;
            let provider = nullable(data, "provider")?;
            let model = nullable(data, "model")?;
            match data.get("action")?.as_str()? {
                "set" => lines.push(t(
                    "operation_feedback.model",
                    "Model selection ({target}): {provider}/{model}.",
                    &[("target", role), ("provider", provider?), ("model", model?)],
                )),
                "reset" if model.is_none() && provider.is_none() => lines.push(t(
                    "operation_feedback.role_reset",
                    "Model selection reset for {role}; the role default applies.",
                    &[("role", role)],
                )),
                _ => return None,
            }
        }
        ("models", "worker_model") if timing == EffectiveTiming::RoutingUnconfirmed => {
            let roles: Option<Vec<_>> = data
                .get("roles")?
                .as_array()?
                .iter()
                .map(Value::as_str)
                .collect();
            let roles = roles?;
            if roles.is_empty()
                || roles
                    .iter()
                    .any(|role| !harw_config::UIA_WORKER_ROLES.contains(role))
            {
                return None;
            }
            lines.push(t(
                "operation_feedback.workers",
                "Worker roles {roles}: {choice}.",
                &[
                    ("roles", &roles.join(", ")),
                    ("choice", data.get("choice")?.as_str()?),
                ],
            ));
            if let Some(provider) = nullable(data, "fallback_provider")? {
                lines.push(t(
                    "operation_feedback.fallback",
                    "Provider {provider} is not logged in; the role follows UIA until login.",
                    &[("provider", provider)],
                ));
            }
        }
        ("effort" | "uia-effort", "effort") => {
            let target = data.get("target")?.as_str()?;
            if target
                != if meta.name == "effort" {
                    "session"
                } else {
                    "uia"
                }
            {
                return None;
            }
            let action = data.get("action")?.as_str()?;
            if !matches!(action, "set" | "reset" | "show") {
                return None;
            }
            if timing
                != if action == "show" {
                    EffectiveTiming::Current
                } else if target == "session" {
                    EffectiveTiming::NextTurn
                } else {
                    EffectiveTiming::NextSession
                }
            {
                return None;
            }
            let changed = data.get("changed")?;
            if changed.is_null() != (target == "uia" || action == "show") {
                return None;
            }
            if !changed.is_null() {
                if !changed.as_bool()? {
                    lines.push(
                        tr_for(language, "operation_feedback.already", "Already set.").to_owned(),
                    );
                }
            }
            let level = nullable(data, "level")?;
            if let Some(level) = level {
                level.parse::<harw_types::ReasoningEffort>().ok()?;
            }
            if (action == "set" && level.is_none()) || (action == "reset" && level.is_some()) {
                return None;
            }
            let default = if target == "session" {
                tr_for(
                    language,
                    "operation_feedback.provider_default",
                    "Provider default",
                )
            } else {
                tr_for(language, "operation_feedback.role_default", "Role default")
            };
            lines.push(t(
                "operation_feedback.effort",
                "Reasoning effort ({target}): {level}.",
                &[("target", target), ("level", level.unwrap_or(default))],
            ));
            if action == "reset" {
                lines.push(
                    tr_for(language, "operation_feedback.reset", "Selection reset.").to_owned(),
                );
            }
        }
        ("mode", "mode") => {
            let mode = nullable(data, "mode")?;
            if mode
                .is_some_and(|mode| !matches!(mode, "chat" | "plan" | "explore" | "work" | "shell"))
            {
                return None;
            }
            match data.get("action")?.as_str()? {
                "switch" if timing == EffectiveTiming::NextTurn => {
                    description(language, mode?)?;
                    lines.push(t(
                        "operation_feedback.mode_switch",
                        "Mode requested: {mode}.",
                        &[("mode", mode?)],
                    ));
                }
                "default" if timing == EffectiveTiming::NextSession => {
                    description(language, mode?)?;
                    lines.push(t(
                        "operation_feedback.mode_default",
                        "Default mode selected: {mode}.",
                        &[("mode", mode?)],
                    ));
                }
                "show" if timing == EffectiveTiming::Current => {
                    if nullable(data, "default")?.is_some_and(|mode| {
                        !matches!(mode, "chat" | "plan" | "explore" | "work" | "shell")
                    }) {
                        return None;
                    }
                    let unknown =
                        tr_for(language, "operation_feedback.unspecified", "(unspecified)");
                    lines.push(t(
                        "operation_feedback.mode_show",
                        "Mode: {mode}\nDefault for new sessions: {default}\nAvailable modes:",
                        &[
                            ("mode", mode.unwrap_or(unknown)),
                            ("default", nullable(data, "default")?.unwrap_or(unknown)),
                        ],
                    ));
                    for mode in ["chat", "plan", "explore", "work", "shell"] {
                        lines.push(format!("{mode}: {}", description(language, mode)?));
                    }
                    lines.push("/mode <mode> · /mode default <mode>".to_owned());
                }
                _ => return None,
            }
        }
        ("permissions", "approval") if timing == EffectiveTiming::NextToolCall => {
            let mode = data.get("mode")?.as_str()?;
            if !data.get("changed")?.as_bool()? {
                lines.push(
                    tr_for(language, "operation_feedback.already", "Already set.").to_owned(),
                );
            }
            if !matches!(mode, "ask" | "auto" | "full") {
                return None;
            }
            lines.push(t(
                "operation_feedback.approval",
                "Approval mode: {mode} — {description}.",
                &[
                    ("mode", mode),
                    ("description", description(language, mode)?),
                ],
            ));
        }
        _ => return None,
    }
    if kind == "model_selection" && matches!(meta.name, "model" | "uia-model") {
        if !data.get("changed")?.as_bool()? {
            lines.push(tr_for(language, "operation_feedback.already", "Already set.").to_owned());
        }
    }
    if timing != EffectiveTiming::Current {
        lines.push(timing.text(language).to_owned());
    }
    // These optional values are required, nullable fields on their schemas.
    if matches!(kind, "model_selection" | "role_model" | "worker_model") {
        if let Some(warning) = nullable(data, "warning")? {
            lines.push(t(
                "operation_feedback.warning",
                "Warning: {detail}",
                &[("detail", warning)],
            ));
        }
    }
    if matches!(
        kind,
        "model_selection" | "role_model" | "worker_model" | "effort" | "mode"
    ) {
        if let Some(note) = nullable(data, "note")? {
            lines.push(note.to_owned());
        }
    }
    Some(Presentation {
        text: lines.join("\n"),
        timing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_operations::Operation;
    use serde_json::json;

    fn skill_output(enabled: bool, changed: bool) -> OpOutput {
        OpOutput {
            text: "original aktiviert user text".into(),
            data: Some(
                json!({"extra":{"keep":[1,"payload"]},"ui_feedback":{"kind":"skill_state","name":"review {state}","enabled":enabled,"changed":changed,"manifest":"/profile/skills/review/skill.toml","effective":"next_config_load"}}),
            ),
        }
    }
    #[test]
    fn activation_labels_are_exact_in_all_ten_projections() {
        let output = skill_output(true, true);
        let before = output.data.clone();
        for (language, label) in UiLanguage::ALL.into_iter().zip([
            "Activated",
            "Aktiviert",
            "Activado",
            "Activé",
            "Ativado",
            "有効化済み",
            "已启用",
            "활성화됨",
            "Đã kích hoạt",
            "सक्रिय",
        ]) {
            let shown = project(harw_ops::skills::SkillsOperation.meta(), language, &output)
                .expect("recognized skill result");
            assert!(shown.text.contains(label), "{language:?}: {}", shown.text);
            assert!(shown.text.contains("review {state}"));
            assert!(shown.text.contains("/profile/skills/review/skill.toml"));
            assert_eq!(shown.timing, EffectiveTiming::NextConfigLoad);
        }
        assert_eq!(output.data, before);
        let already = project(
            harw_ops::skills::SkillsOperation.meta(),
            UiLanguage::English,
            &skill_output(true, false),
        )
        .expect("already");
        assert!(already.text.contains("Already set."));
        assert!(already.text.contains("next config load (next session)"));
        assert!(already.text.contains("does not run the skill"));
        let disabled = project(
            harw_ops::skills::SkillsOperation.meta(),
            UiLanguage::German,
            &skill_output(false, true),
        )
        .expect("disabled");
        assert!(disabled.text.contains(": Deaktiviert ("));
    }
    #[test]
    fn incomplete_unrelated_and_cloned_metadata_are_never_projected() {
        let meta = harw_ops::skills::SkillsOperation.meta();
        let output = skill_output(true, true);
        assert!(project(&meta.clone(), UiLanguage::German, &output).is_none());
        assert!(
            project(
                harw_ops::effort::EffortOperation.meta(),
                UiLanguage::German,
                &output
            )
            .is_none()
        );
        for data in [
            Value::Null,
            json!({"kind":"skill_state"}),
            json!({"ui_feedback":{"kind":"fake","effective":"next_config_load"}}),
            json!({"ui_feedback":{"kind":"skill_state","name":"user aktiviert","enabled":true,"changed":true,"manifest":"p","effective":"now"}}),
        ] {
            let output = OpOutput {
                text: "user aktiviert /skills and arbitrary text".into(),
                data: Some(data.clone()),
            };
            assert!(project(meta, UiLanguage::German, &output).is_none());
            assert_eq!(output.text, "user aktiviert /skills and arbitrary text");
            assert_eq!(output.data, Some(data));
        }
        for (field, value) in [
            ("enabled", json!("true")),
            ("changed", Value::Null),
            ("manifest", json!(42)),
            ("effective", json!("next_turn")),
        ] {
            let mut output = skill_output(true, true);
            output.data.as_mut().unwrap()["ui_feedback"][field] = value;
            assert!(
                project(meta, UiLanguage::German, &output).is_none(),
                "{field}"
            );
        }
    }
    #[test]
    fn warnings_notes_and_root_fields_survive_without_save_claims() {
        let output = OpOutput {
            text: "legacy".into(),
            data: Some(
                json!({"persisted":false,"source":"explizit gewählt","ui_feedback":{"kind":"role_model","action":"set","role":"explorer","provider":"p","model":"m","live":true,"effective":"new_agents","warning":"backend lifecycle detail {raw}","note":"arbitrary persistence failure, not success"}}),
            ),
        };
        let before = output.data.clone();
        let shown = project(
            harw_ops::models::ModelsOperation.meta(),
            UiLanguage::English,
            &output,
        )
        .expect("role");
        assert!(shown.text.contains("running agents keep their model"));
        assert!(shown.text.contains("backend lifecycle detail {raw}"));
        assert!(
            shown
                .text
                .ends_with("arbitrary persistence failure, not success")
        );
        assert!(!shown.text.contains("saved"));
        assert_eq!(output.data, before);
    }
    fn assert_fallback_preserves(meta: &OperationMeta, feedback: Value) {
        let data = json!({"root_payload":{"user":"aktiviert {model}"},"ui_feedback":feedback});
        let original = "original output with user/model detail aktiviert";
        let mut output = OpOutput {
            text: original.to_owned(),
            data: Some(data.clone()),
        };
        // Mirror the shared dispatch application: rejected projection must leave
        // both the operation's text and its complete original data untouched.
        let projected = project(meta, UiLanguage::German, &output);
        assert!(
            projected.is_none(),
            "malformed feedback projected: {feedback}"
        );
        if let Some(projected) = projected {
            output.text = projected.text;
        }
        assert_eq!(output.text, original);
        assert_eq!(output.data, Some(data));
    }

    #[test]
    fn uia_model_requires_complete_consistent_cleared_state() {
        let meta = harw_ops::model::UiaModelOperation.meta();
        let valid = json!({"kind":"model_selection","target":"uia","provider":"p","model":"m","changed":true,"effective":"next_uia_turn","cleared_model":null,"warning":null,"note":null});
        let output = OpOutput {
            text: "legacy".to_owned(),
            data: Some(json!({"ui_feedback":valid})),
        };
        assert!(project(meta, UiLanguage::English, &output).is_some());
        let mut missing = valid.clone();
        missing
            .as_object_mut()
            .expect("object")
            .remove("cleared_model");
        assert_fallback_preserves(meta, missing);
        for invalid in [
            json!(7),
            json!(true),
            json!([]),
            json!({}),
            json!("silently lost detail"),
        ] {
            let mut malformed = valid.clone();
            malformed["cleared_model"] = invalid;
            assert_fallback_preserves(meta, malformed);
        }
        for cleared in [Value::Null, json!("old incompatible model")] {
            let mut valid_empty = valid.clone();
            valid_empty["model"] = Value::Null;
            valid_empty["cleared_model"] = cleared.clone();
            let output = OpOutput {
                text: "legacy".into(),
                data: Some(json!({"ui_feedback":valid_empty})),
            };
            let shown =
                project(meta, UiLanguage::English, &output).expect("valid empty UIA selection");
            assert!(shown.text.contains("No model selected"));
            if let Some(previous) = cleared.as_str() {
                assert!(shown.text.contains(previous));
            }
        }
    }

    #[test]
    fn every_semantic_variant_rejects_missing_and_wrong_type_required_fields() {
        let cases: Vec<(&OperationMeta, Value)> = vec![
            (
                harw_ops::skills::SkillsOperation.meta(),
                json!({"kind":"skill_state","name":"review","enabled":true,"changed":true,"manifest":"/profile/skill.toml","effective":"next_config_load"}),
            ),
            (
                harw_ops::model::ModelOperation.meta(),
                json!({"kind":"model_selection","target":"session","provider":"p","model":"m","changed":true,"effective":"next_turn","warning":null,"note":null}),
            ),
            (
                harw_ops::model::UiaModelOperation.meta(),
                json!({"kind":"model_selection","target":"uia","provider":"p","model":"m","changed":true,"effective":"next_uia_turn","cleared_model":null,"warning":null,"note":null}),
            ),
            (
                harw_ops::model::UiaWorkerModelOperation.meta(),
                json!({"kind":"model_selection","target":"uia-worker","provider":"p","model":"m","effective":"next_session","warning":null,"note":null}),
            ),
            (
                harw_ops::models::ModelsOperation.meta(),
                json!({"kind":"role_model","action":"set","role":"explorer","provider":"p","model":"m","live":true,"effective":"new_agents","warning":null,"note":null}),
            ),
            (
                harw_ops::models::ModelsOperation.meta(),
                json!({"kind":"role_model","action":"reset","role":"explorer","provider":null,"model":null,"live":false,"effective":"next_session","warning":null,"note":null}),
            ),
            (
                harw_ops::models::ModelsOperation.meta(),
                json!({"kind":"worker_model","roles":["uia-writer"],"choice":"uia","effective":"routing_unconfirmed","warning":null,"note":null,"fallback_provider":null}),
            ),
            (
                harw_ops::effort::EffortOperation.meta(),
                json!({"kind":"effort","target":"session","action":"set","level":"high","changed":true,"effective":"next_turn","note":null}),
            ),
            (
                harw_ops::effort::EffortOperation.meta(),
                json!({"kind":"effort","target":"session","action":"reset","level":null,"changed":true,"effective":"next_turn","note":null}),
            ),
            (
                harw_ops::effort::EffortOperation.meta(),
                json!({"kind":"effort","target":"session","action":"show","level":null,"changed":null,"effective":"current","note":null}),
            ),
            (
                harw_ops::effort::UiaEffortOperation.meta(),
                json!({"kind":"effort","target":"uia","action":"set","level":"high","changed":null,"effective":"next_session","note":null}),
            ),
            (
                harw_ops::effort::UiaEffortOperation.meta(),
                json!({"kind":"effort","target":"uia","action":"reset","level":null,"changed":null,"effective":"next_session","note":null}),
            ),
            (
                harw_ops::effort::UiaEffortOperation.meta(),
                json!({"kind":"effort","target":"uia","action":"show","level":null,"changed":null,"effective":"current","note":null}),
            ),
            (
                harw_ops::mode::ModeOperation.meta(),
                json!({"kind":"mode","action":"switch","mode":"plan","effective":"next_turn","note":null}),
            ),
            (
                harw_ops::mode::ModeOperation.meta(),
                json!({"kind":"mode","action":"default","mode":"work","effective":"next_session","note":null}),
            ),
            (
                harw_ops::mode::ModeOperation.meta(),
                json!({"kind":"mode","action":"show","mode":null,"default":null,"effective":"current","note":null}),
            ),
            (
                harw_ops::permissions::PermissionsOperation.meta(),
                json!({"kind":"approval","mode":"ask","changed":true,"effective":"next_tool_call"}),
            ),
        ];
        for (meta, valid) in cases {
            let output = OpOutput {
                text: "legacy".into(),
                data: Some(json!({"ui_feedback":valid})),
            };
            assert!(
                project(meta, UiLanguage::English, &output).is_some(),
                "valid {}: {valid}",
                meta.name
            );
            for field in valid.as_object().expect("object").keys() {
                let mut missing = valid.clone();
                missing.as_object_mut().expect("object").remove(field);
                assert_fallback_preserves(meta, missing);
                let mut wrong_type = valid.clone();
                wrong_type[field] = json!({"invalid":true});
                assert_fallback_preserves(meta, wrong_type);
            }
        }
    }
}
