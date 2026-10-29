//! Operator-only interface preferences, validated and persisted in the bound profile.
//!
//! A retained ConfigWriter lock covers fresh read, candidate resolution, save and
//! live publication. Only the selected field is mirrored; other live/session
//! choices keep their own governed mutation paths. Errors never echo config text.

use std::path::PathBuf;
use std::sync::Arc;

use harw_config::{ConfigError, ConfigWriter, ResolvedConfig, TuiSection, UiLanguage};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use serde_json::json;

/// Explicit `/settings show|set <key> <value>|reset <key>` arguments.
#[derive(Debug, Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct SettingsArgs {
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    #[serde(default)]
    #[raw(nth = 1)]
    pub key: Option<String>,
    #[serde(default)]
    #[raw(nth = 2)]
    pub value: Option<String>,
    /// Captured to reject accidental extra arguments rather than silently ignore them.
    #[serde(default)]
    #[raw(nth = 3)]
    pub extra: Option<String>,
}

#[derive(Clone, Copy)]
enum Setting {
    Language,
    Theme,
    Reasoning,
    ChildStream,
    StatusExpiry,
}

impl Setting {
    const ALL: [Self; 5] = [
        Self::Language,
        Self::Theme,
        Self::Reasoning,
        Self::ChildStream,
        Self::StatusExpiry,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Language => "tui.language",
            Self::Theme => "tui.theme",
            Self::Reasoning => "tui.reasoning",
            Self::ChildStream => "tui.child_stream",
            Self::StatusExpiry => "tui.status_expiry",
        }
    }

    fn parse(key: Option<&str>) -> Result<Self, OpError> {
        Self::ALL
            .into_iter()
            .find(|setting| Some(setting.key()) == key)
            .ok_or_else(usage)
    }

    fn choices(self) -> Vec<&'static str> {
        match self {
            Self::Language => UiLanguage::ALL
                .into_iter()
                .map(UiLanguage::as_str)
                .collect(),
            Self::Theme => vec!["auto", "dark", "light"],
            Self::Reasoning => vec!["message", "collapsed", "hidden"],
            Self::ChildStream => vec!["orchestrators", "all", "none"],
            Self::StatusExpiry => vec!["consolidate", "hide"],
        }
    }

    fn value(self, tui: &TuiSection) -> &str {
        match self {
            Self::Language => tui.language.as_str(),
            // Legacy aliases remain valid on disk and in the live config.
            Self::Theme => match tui.theme.as_str() {
                "default-dark" => "dark",
                "default-light" => "light",
                other => other,
            },
            Self::Reasoning => tui.reasoning.as_str(),
            Self::ChildStream => tui.child_stream.as_str(),
            Self::StatusExpiry => match tui.status_expiry {
                harw_config::StatusExpiryMode::Consolidate => "consolidate",
                harw_config::StatusExpiryMode::Hide => "hide",
            },
        }
    }

    fn publish(self, source: &TuiSection, target: &mut TuiSection) {
        match self {
            Self::Language => target.language = source.language,
            Self::Theme => target.theme.clone_from(&source.theme),
            Self::Reasoning => target.reasoning = source.reasoning,
            Self::ChildStream => target.child_stream = source.child_stream,
            Self::StatusExpiry => target.status_expiry = source.status_expiry,
        }
    }
}

fn usage() -> OpError {
    OpError::InvalidArguments("Usage: /settings show | set <tui.language|tui.theme|tui.reasoning|tui.child_stream|tui.status_expiry> <value> | reset <key>".to_owned())
}

// TOML parse diagnostics can contain source lines, including unrelated secrets.
fn config_error(error: ConfigError) -> OpError {
    match error {
        ConfigError::Invalid(message) if message == "configuration busy: another writer holds the lock; retry" || message == "configuration conflict: reopen the file before saving" => OpError::Execution(message),
        _ => OpError::Execution("Settings validation or persistence failed; configuration and live preferences were not changed".to_owned()),
    }
}

/// Preserve runtime precedence while pinning profile/project identity to the session.
struct SettingsLayers {
    trusted: Vec<PathBuf>,
    restricted_repo: Option<PathBuf>,
    project_settings: Option<PathBuf>,
}

fn settings_layers(home: &harw_home::ResolvedHomeContext) -> Result<SettingsLayers, OpError> {
    let report = harw_home::config_layers_report_at(&home.home, &home.project.root)
        .map_err(|_| OpError::Execution("Settings config layers are unavailable".to_owned()))?;
    let mut trusted = vec![home.home.clone(), home.profile_dir.clone()];
    trusted.extend(report.layers.into_iter().skip(2));
    let project_settings = home
        .project_settings_path
        .is_file()
        .then(|| home.project_settings_path.clone());
    if let Some(directory) = home.project_settings_path.parent()
        && (project_settings.is_some() || directory.join("config.toml").is_file())
    {
        trusted.push(directory.to_path_buf());
    }
    Ok(SettingsLayers {
        trusted,
        restricted_repo: report.untrusted_repo,
        project_settings,
    })
}

fn current(ctx: &OpContext) -> Result<Arc<ResolvedConfig>, OpError> {
    if let Some(live) = ctx.service::<crate::live_config::SharedLiveConfig>() {
        return Ok(live.current());
    }
    if let Some(config) = ctx.service::<Arc<ResolvedConfig>>() {
        return Ok(Arc::clone(config));
    }
    let home = crate::config_util::bound_home(ctx)?;
    let layers = settings_layers(home)?;
    harw_config::discover_config_with_restricted_and_project_settings(
        &layers.trusted,
        layers.restricted_repo.as_deref(),
        layers.project_settings.as_deref(),
    )
    .map(Arc::new)
    .map_err(config_error)
}

fn output(tui: &TuiSection) -> OpOutput {
    let rows: Vec<_> = Setting::ALL
        .into_iter()
        .map(|setting| {
            json!({
                "key": setting.key(), "value": setting.value(tui), "choices": setting.choices(),
                "scope": "profile", "apply": "now",
            })
        })
        .collect();
    OpOutput {
        text: Setting::ALL
            .into_iter()
            .map(|setting| format!("{} = {} (profile, now)", setting.key(), setting.value(tui)))
            .collect::<Vec<_>>()
            .join("\n"),
        data: Some(json!({"language": tui.language.as_str(), "rows": rows})),
    }
}

fn handle_settings(ctx: &OpContext, args: SettingsArgs) -> Result<OpOutput, OpError> {
    let action = args.action.as_deref().unwrap_or("show");
    if args.extra.is_some() {
        return Err(usage());
    }
    if action == "show" {
        if args.key.is_some() || args.value.is_some() {
            return Err(usage());
        }
        return Ok(output(&current(ctx)?.harness.tui));
    }
    let setting = Setting::parse(args.key.as_deref())?;
    match action {
        "set"
            if args
                .value
                .as_deref()
                .is_some_and(|value| setting.choices().contains(&value)) => {}
        "reset" if args.value.is_none() => {}
        _ => return Err(usage()),
    }
    // The path is derived exclusively from the immutable bound active profile.
    let home = crate::config_util::bound_home(ctx)?;
    let path = home.profile_dir.join("config.toml");
    let mut writer = ConfigWriter::open_locked(&path).map_err(config_error)?;
    match args.value.as_deref() {
        Some(value) => writer
            .set_value(setting.key(), toml_edit::value(value))
            .map_err(config_error)?,
        None => {
            writer.remove_value(setting.key());
        }
    }
    let layers = settings_layers(home)?;
    let candidate = harw_config::discover_config_with_override(
        &layers.trusted,
        layers.restricted_repo.as_deref(),
        layers.project_settings.as_deref(),
        &path,
        &writer.document(),
    )
    .map_err(config_error)?;
    candidate.validate().map_err(config_error)?;
    // Resolve/validate reset inheritance before the first persistent change.
    writer.save().map_err(config_error)?;
    crate::live_config::mirror(ctx, |config| {
        setting.publish(&candidate.harness.tui, &mut config.harness.tui)
    });
    let result = match ctx.service::<crate::live_config::SharedLiveConfig>() {
        Some(live) => output(&live.current().harness.tui),
        None => output(&candidate.harness.tui),
    };
    // Retain serialization through publication and construction of its result.
    drop(writer);
    Ok(result)
}

/// Display or change a validated interface preference in the active profile.
#[operation(
    name = "settings",
    summary = "Show or change interface preferences (show|set|reset).",
    domain = "session",
    permission = "operator",
    category = "session",
    command(path = "/settings", visibility = "tui_only", busy = "immediate")
)]
async fn settings(ctx: &OpContext, args: SettingsArgs) -> Result<OpOutput, OpError> {
    handle_settings(ctx, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_config::{LiveConfig, SharedLiveConfig};
    use crate::test_support::{TestResult, ctx};
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
    use std::sync::Arc;

    fn fixture(home: &harw_home::ResolvedHomeContext) -> TestResult<(OpContext, SharedLiveConfig)> {
        let config = harw_config::discover_config(&[home.home.clone(), home.profile_dir.clone()])
            .map_err(ctx("discover"))?;
        let live = Arc::new(LiveConfig::new(Arc::new(config)));
        let mut services = ServiceMap::new();
        services.insert(Arc::new(home.clone()));
        services.insert(Arc::clone(&live));
        Ok((crate::knowledge_test_support::op_context(services)?, live))
    }

    fn command(context: &OpContext, input: &str) -> Result<OpOutput, OpError> {
        let tokens: Vec<_> = input.split_whitespace().map(str::to_owned).collect();
        handle_settings(context, SettingsArgs::from_raw_args(&tokens)?)
    }

    #[test]
    fn typed_changes_preserve_comments_and_other_profiles() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        std::fs::create_dir_all(&home.profile_dir).map_err(ctx("profile"))?;
        let path = home.profile_dir.join("config.toml");
        std::fs::write(&path, "# retained comment\ndefault_model = 'keep-model'\n[tui]\n# display preference\nlanguage = 'en'\n").map_err(ctx("seed"))?;
        let other = home.home.join("profiles/other/config.toml");
        std::fs::create_dir_all(
            other
                .parent()
                .ok_or(crate::test_support::TestError::Missing("parent"))?,
        )
        .map_err(ctx("other profile"))?;
        std::fs::write(&other, "[tui]\nlanguage='ko'\n").map_err(ctx("seed other"))?;
        let (context, live) = fixture(&home)?;
        for input in [
            "set tui.language ja",
            "set tui.theme auto",
            "set tui.reasoning hidden",
            "set tui.child_stream none",
            "set tui.status_expiry hide",
        ] {
            command(&context, input).map_err(ctx("set"))?;
        }
        assert_eq!(
            live.current().harness.tui.language,
            harw_config::UiLanguage::Japanese
        );
        assert_eq!(live.current().harness.tui.reasoning.as_str(), "hidden");
        assert_eq!(live.current().harness.tui.child_stream.as_str(), "none");
        assert_eq!(
            live.current().harness.tui.status_expiry,
            harw_config::StatusExpiryMode::Hide
        );
        assert_eq!(
            live.current().harness.default_model.as_deref(),
            Some("keep-model")
        );
        let content = std::fs::read_to_string(&path).map_err(ctx("read"))?;
        assert!(content.contains("# retained comment"));
        assert!(content.contains("# display preference"));
        assert!(content.contains("default_model = 'keep-model'"));
        assert_eq!(
            std::fs::read_to_string(other).map_err(ctx("other read"))?,
            "[tui]\nlanguage='ko'\n"
        );
        assert!(!home.home.join("config.toml").exists());
        Ok(())
    }

    #[test]
    fn reset_resolves_inheritance_and_defaults_before_publishing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        std::fs::write(
            home.home.join("config.toml"),
            "[tui]\nlanguage='de'\ntheme='default-light'\n",
        )
        .map_err(ctx("root"))?;
        let (context, live) = fixture(&home)?;
        command(&context, "set tui.language ja").map_err(ctx("set"))?;
        command(&context, "reset tui.language").map_err(ctx("reset"))?;
        assert_eq!(
            live.current().harness.tui.language,
            harw_config::UiLanguage::German
        );
        let profile = std::fs::read_to_string(home.profile_dir.join("config.toml"))
            .map_err(ctx("read profile"))?;
        assert!(!profile.contains("language"));
        let result = command(&context, "reset tui.theme").map_err(ctx("reset theme"))?;
        assert_eq!(live.current().harness.tui.theme, "default-light");
        assert_eq!(
            result
                .data
                .as_ref()
                .and_then(|data| data["rows"][1]["value"].as_str()),
            Some("light")
        );
        std::fs::write(home.home.join("config.toml"), "").map_err(ctx("clear root"))?;
        command(&context, "reset tui.language").map_err(ctx("default reset"))?;
        assert_eq!(
            live.current().harness.tui.language,
            harw_config::UiLanguage::English
        );
        Ok(())
    }

    #[test]
    fn invalid_arguments_and_failed_commits_leave_disk_and_live_unchanged() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        let (context, live) = fixture(&home)?;
        command(&context, "set tui.language de").map_err(ctx("initial set"))?;
        let path = home.profile_dir.join("config.toml");
        let before = std::fs::read_to_string(&path).map_err(ctx("before"))?;
        for input in [
            "set tui.language it",
            "set tui.language DE",
            "set tui.theme custom",
            "set tui.reasoning all",
            "set tui.child_stream off",
            "set tui.status_expiry 30",
            "set default_model nope",
            "reset permissions",
            "show extra",
            "set tui.language ja extra",
            "reset tui.language extra",
        ] {
            assert!(command(&context, input).is_err(), "{input}");
        }
        let lock = harw_config::ConfigWriter::open_locked(&path).map_err(ctx("hold lock"))?;
        assert!(command(&context, "set tui.language ja").is_err());
        drop(lock);
        // A backup directory makes the actual save fail after candidate validation.
        std::fs::create_dir(path.with_extension("toml.bak.4294967295"))
            .map_err(ctx("block backup"))?;
        assert!(command(&context, "set tui.language ja").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).map_err(ctx("after"))?,
            before
        );
        assert_eq!(
            live.current().harness.tui.language,
            harw_config::UiLanguage::German
        );
        Ok(())
    }

    #[test]
    fn malformed_inheritance_rejects_reset_before_disk_commit() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        let (context, live) = fixture(&home)?;
        command(&context, "set tui.language de").map_err(ctx("initial set"))?;
        let path = home.profile_dir.join("config.toml");
        let before = std::fs::read_to_string(&path).map_err(ctx("before"))?;
        std::fs::write(
            home.home.join("config.toml"),
            "[tui]\nlanguage='invalid-secret-marker'\n",
        )
        .map_err(ctx("bad root"))?;
        let error = command(&context, "reset tui.language")
            .err()
            .ok_or(crate::test_support::TestError::Missing("reset error"))?;
        assert!(!error.to_string().contains("invalid-secret-marker"));
        assert_eq!(std::fs::read_to_string(path).map_err(ctx("after"))?, before);
        assert_eq!(
            live.current().harness.tui.language,
            harw_config::UiLanguage::German
        );
        Ok(())
    }

    #[test]
    fn reset_inherits_bound_project_settings_without_writing_them() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        let (context, live) = fixture(&home)?;
        command(&context, "set tui.language de").map_err(ctx("set"))?;
        let settings_path = &home.project_settings_path;
        std::fs::create_dir_all(settings_path.parent().ok_or(
            crate::test_support::TestError::Missing("project settings parent"),
        )?)
        .map_err(ctx("project settings directory"))?;
        let source = "# project preference\n[tui]\nlanguage='ja'\n";
        std::fs::write(settings_path, source).map_err(ctx("project settings"))?;
        command(&context, "reset tui.language").map_err(ctx("reset"))?;
        assert_eq!(
            live.current().harness.tui.language,
            harw_config::UiLanguage::Japanese
        );
        assert_eq!(
            std::fs::read_to_string(settings_path).map_err(ctx("read project settings"))?,
            source
        );
        Ok(())
    }

    #[test]
    fn named_active_profile_and_fresh_file_edits_are_preserved() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let default_home = crate::config_util::test_home_context(dir.path())?;
        std::fs::create_dir_all(&default_home.profile_dir).map_err(ctx("default profile"))?;
        let default_path = default_home.profile_dir.join("config.toml");
        std::fs::write(&default_path, "[tui]\nlanguage='ko'\n").map_err(ctx("default source"))?;
        let home = harw_home::ResolvedHomeContext::new(
            dir.path(),
            "named".to_owned(),
            default_home.project.clone(),
        )
        .map_err(ctx("named home"))?;
        let (context, live) = fixture(&home)?;
        command(&context, "set tui.language de").map_err(ctx("initial"))?;
        let path = home.profile_dir.join("config.toml");
        let source = std::fs::read_to_string(&path).map_err(ctx("read"))?;
        std::fs::write(
            &path,
            format!("# fresh edit\ndefault_model='external-model'\n{source}"),
        )
        .map_err(ctx("external edit"))?;
        command(&context, "set tui.theme light").map_err(ctx("fresh write"))?;
        assert!(
            std::fs::read_to_string(&path)
                .map_err(ctx("fresh read"))?
                .contains("# fresh edit\ndefault_model='external-model'")
        );
        assert_eq!(live.current().harness.default_model, None);
        assert_eq!(live.current().harness.tui.theme, "light");
        assert_eq!(
            std::fs::read_to_string(default_path).map_err(ctx("default unchanged"))?,
            "[tui]\nlanguage='ko'\n"
        );
        Ok(())
    }

    #[test]
    fn show_is_allowlisted_and_settings_are_operator_only() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        let (context, live) = fixture(&home)?;
        live.update(|config| {
            config
                .env_layer
                .insert("PRIVATE_TOKEN".to_owned(), "secret-marker".to_owned());
            config.harness.default_model = Some("private-model".to_owned());
        });
        let output = command(&context, "show").map_err(ctx("show"))?;
        let data = output
            .data
            .ok_or(crate::test_support::TestError::Missing("settings data"))?;
        assert_eq!(data["language"], "en");
        assert_eq!(data["rows"][1]["value"], "dark");
        assert_eq!(data["rows"].as_array().map(Vec::len), Some(5));
        assert!(!data.to_string().contains("secret-marker"));
        assert!(!data.to_string().contains("private-model"));
        for row in data["rows"]
            .as_array()
            .ok_or(crate::test_support::TestError::Missing("rows"))?
        {
            assert_eq!(row["scope"], "profile");
            assert_eq!(row["apply"], "now");
            assert!(row["choices"].is_array());
        }
        let mut registry = harw_operations::registry::OperationRegistry::new();
        crate::register_all(&mut registry);
        let op = registry.find_by_command("/settings").ok_or(
            crate::test_support::TestError::Missing("settings registration"),
        )?;
        assert_eq!(
            op.meta().permission,
            harw_operations::PermissionTier::Operator
        );
        assert!(
            op.meta()
                .surfaces
                .iter()
                .all(|surface| matches!(surface, harw_operations::Surface::Command { .. }))
        );
        Ok(())
    }
}
