//! Operator-only interface preferences, validated and persisted in the bound profile.
//!
//! A retained ConfigWriter lock covers fresh read, candidate resolution, save and
//! live publication. Only the selected field is mirrored; other live/session
//! choices keep their own governed mutation paths. Errors never echo config text.
//!
//! PL-100 WP 1.9: `dod.watch.mode` (`off|notify|triage`) is the only DoD key on
//! the allow-list. `/settings show` also prints the read-only DoD values
//! (`[dod.triage]` model/effort/max_tokens, the config-only effective watch
//! mode, `autoconnect` and `[dod.approve] tui_tty`, see
//! `crate::dod_model::TriageSettings`) as text and under `data.dod`; the triage
//! model changes only through `/dod model set|clear`, `tui_tty` only in the
//! config file. The values are the session's; the provider of the triage model
//! (its locality in `data.dod.triage`) is looked up in the home and the active
//! profile only (`crate::dod_model::trusted_config`, threat B15), like the
//! doctor does. The WP 0.3 egress gate is the doctor's
//! (`crate::dod_model::EGRESS_DOCTOR_HINT`). A DoD key is written only when
//! `dod` and `dod.watch` are real tables in the profile config:
//! `ConfigWriter::set_value` would replace an inline table with an empty one
//! and drop its siblings (e.g. `tui_tty`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use harw_config::{
    ConfigError, ConfigWriter, DodWatchMode, ProviderToml, ResolvedConfig, UiLanguage,
};
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
    /// `[dod.watch] mode` (PL-100 WP 1.9).
    WatchMode,
}

impl Setting {
    const ALL: [Self; 6] = [
        Self::Language,
        Self::Theme,
        Self::Reasoning,
        Self::ChildStream,
        Self::StatusExpiry,
        Self::WatchMode,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Language => "tui.language",
            Self::Theme => "tui.theme",
            Self::Reasoning => "tui.reasoning",
            Self::ChildStream => "tui.child_stream",
            Self::StatusExpiry => "tui.status_expiry",
            Self::WatchMode => "dod.watch.mode",
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
            Self::WatchMode => DodWatchMode::ALL
                .into_iter()
                .map(DodWatchMode::as_str)
                .collect(),
        }
    }

    // Whether the key's parent tables must already be real tables (or
    // absent) before `set_value`/`remove_value` touch them.
    fn requires_table_parents(self) -> bool {
        matches!(self, Self::WatchMode)
    }

    fn value(self, config: &ResolvedConfig) -> &str {
        let tui = &config.harness.tui;
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
            Self::WatchMode => config.dod.watch.mode.as_str(),
        }
    }

    fn publish(self, source: &ResolvedConfig, target: &mut ResolvedConfig) {
        let (from, to) = (&source.harness.tui, &mut target.harness.tui);
        match self {
            Self::Language => to.language = from.language,
            Self::Theme => to.theme.clone_from(&from.theme),
            Self::Reasoning => to.reasoning = from.reasoning,
            Self::ChildStream => to.child_stream = from.child_stream,
            Self::StatusExpiry => to.status_expiry = from.status_expiry,
            Self::WatchMode => target.dod.watch.mode = source.dod.watch.mode,
        }
    }
}

fn usage() -> OpError {
    OpError::InvalidArguments("Usage: /settings show | set <tui.language|tui.theme|tui.reasoning|tui.child_stream|tui.status_expiry|dod.watch.mode> <value> | reset <key>".to_owned())
}

// Refusal when a DoD key's parent is an inline table or a plain value in the
// profile config (writing would drop its sibling keys).
const INLINE_PARENT: &str = "dod or dod.watch is not a plain table in the profile config; change dod.watch.mode there by hand";

// Whether every parent of `key` in `document` is absent or a real (header or
// dotted) table, so `set_value`/`remove_value` edit in place instead of
// replacing an inline table.
fn table_parents_are_plain(document: &str, key: &str) -> bool {
    let Ok(document) = document.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    let segments: Vec<&str> = key.split('.').collect();
    let Some((_, parents)) = segments.split_last() else {
        return false;
    };
    let mut item = document.as_item();
    for segment in parents {
        match item.get(*segment) {
            None => return true,
            Some(child) if child.is_table() => item = child,
            Some(_) => return false,
        }
    }
    true
}

// Read-only DoD lines of `/settings show` (PL-100 WP 1.9).
fn dod_lines(dod: &crate::dod_model::TriageSettings) -> Vec<String> {
    let bounded = crate::dod::bounded;
    // Config-only: an empty `egress_allow_cidrs` (WP 0.3) is the doctor's.
    let egress = if dod.effective_watch_mode == DodWatchMode::Triage {
        format!("; {}", crate::dod_model::EGRESS_DOCTOR_HINT)
    } else {
        String::new()
    };
    let mut lines = vec![format!(
        "dod.watch.effective_config = {} (read-only{egress})",
        dod.effective_watch_mode.as_str()
    )];
    lines.extend(
        dod.notes()
            .into_iter()
            .map(|note| format!("dod.watch: {note}")),
    );
    lines.push(format!(
        "dod.watch.autoconnect = {} (read-only)",
        dod.autoconnect
    ));
    lines.push(if dod.tui_tty {
        format!(
            "dod.approve.tui_tty = true (read-only; {})",
            crate::dod_model::TUI_TTY_NOTE
        )
    } else {
        "dod.approve.tui_tty = false (read-only)".to_owned()
    });
    lines.push(format!(
        "dod.triage.model = {} (read-only; {})",
        dod.model
            .as_deref()
            .map_or_else(|| "(not set)".to_owned(), bounded),
        crate::dod_model::TRIAGE_MODEL_HINT
    ));
    lines.push(format!(
        "dod.triage.effort = {} (read-only)",
        dod.effort
            .as_deref()
            .map_or_else(|| "(model default)".to_owned(), bounded)
    ));
    lines.push(format!(
        "dod.triage.max_tokens = {} (read-only)",
        dod.max_tokens
            .map_or_else(|| "(dispatcher default)".to_owned(), |n| n.to_string())
    ));
    lines
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
    let report = harw_home::config_layers_report_at(&home.home, &home.config_discovery_cwd)
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

// The providers of the bound home and its active profile, for the locality
// of the triage provider in `data.dod` (never a project's, threat B15);
// empty (locality `unknown`) without a bound home or with an unreadable
// configuration.
fn trusted_providers(ctx: &OpContext) -> HashMap<String, ProviderToml> {
    let Ok(home) = crate::config_util::bound_home(ctx) else {
        return HashMap::new();
    };
    match crate::dod_model::trusted_config(&home.home, &home.profile_dir) {
        Ok(config) => config.providers,
        Err(error) => {
            tracing::debug!(error = %error, "settings: trusted providers unavailable");
            HashMap::new()
        }
    }
}

fn output(config: &ResolvedConfig, providers: &HashMap<String, ProviderToml>) -> OpOutput {
    let rows: Vec<_> = Setting::ALL
        .into_iter()
        .map(|setting| {
            json!({
                "key": setting.key(), "value": setting.value(config), "choices": setting.choices(),
                "scope": "profile", "apply": "now",
            })
        })
        .collect();
    let dod = crate::dod_model::TriageSettings::from_config(&config.dod, providers);
    let mut lines: Vec<String> = Setting::ALL
        .into_iter()
        .map(|setting| {
            format!(
                "{} = {} (profile, now)",
                setting.key(),
                setting.value(config)
            )
        })
        .collect();
    lines.extend(dod_lines(&dod));
    OpOutput {
        text: lines.join("\n"),
        data: Some(json!({
            "language": config.harness.tui.language.as_str(),
            "rows": rows,
            "dod": dod.view(),
        })),
    }
}

fn handle_settings(ctx: &OpContext, args: SettingsArgs) -> Result<OpOutput, OpError> {
    handle_settings_with_save(ctx, args, ConfigWriter::save_with_outcome)
}

fn handle_settings_with_save(
    ctx: &OpContext,
    args: SettingsArgs,
    save: impl FnOnce(&ConfigWriter) -> harw_config::ConfigResult<harw_config::ConfigSaveOutcome>,
) -> Result<OpOutput, OpError> {
    let action = args.action.as_deref().unwrap_or("show");
    if args.extra.is_some() {
        return Err(usage());
    }
    if action == "show" {
        if args.key.is_some() || args.value.is_some() {
            return Err(usage());
        }
        return Ok(output(&current(ctx)?, &trusted_providers(ctx)));
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
    if setting.requires_table_parents()
        && !table_parents_are_plain(&writer.document(), setting.key())
    {
        return Err(OpError::Execution(INLINE_PARENT.to_owned()));
    }
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
    let outcome = save(&writer).map_err(config_error)?;
    crate::live_config::mirror(ctx, |config| setting.publish(&candidate, config));
    let providers = trusted_providers(ctx);
    let mut result = match ctx.service::<crate::live_config::SharedLiveConfig>() {
        Some(live) => output(&live.current(), &providers),
        None => output(&candidate, &providers),
    };
    if matches!(
        outcome,
        harw_config::ConfigSaveOutcome::DurabilityUncertain(_)
    ) {
        result.text.push_str("\nWarning: settings were saved and applied, but durability across a crash is uncertain.");
        if let Some(data) = result.data.as_mut() {
            data["warning"] = json!("durability_uncertain");
        }
    }
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
    fn reset_keeps_startup_nested_repository_above_custom_marker_root() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home_path = dir.path().join("home");
        let group = dir.path().join("group");
        let repo = group.join("repo");
        std::fs::create_dir_all(&home_path).map_err(ctx("home"))?;
        std::fs::create_dir_all(repo.join(".git")).map_err(ctx("nested repository"))?;
        std::fs::create_dir_all(repo.join(".harw")).map_err(ctx("repository config directory"))?;
        std::fs::write(group.join("CUSTOM_ROOT"), "").map_err(ctx("custom marker"))?;
        std::fs::write(
            home_path.join("config.toml"),
            "project_root_markers=['CUSTOM_ROOT']\n[tui]\nlanguage='de'\n",
        )
        .map_err(ctx("home config"))?;
        std::fs::write(repo.join(".harw/config.toml"), "[tui]\nlanguage='ja'\n")
            .map_err(ctx("repository preference"))?;
        harw_home::trust_project(&home_path, &repo).map_err(ctx("trust repository"))?;
        let project = harw_home::discover_project(&repo, &["CUSTOM_ROOT".to_owned()])
            .map_err(ctx("custom project"))?;
        assert_eq!(project.root, group);
        let home = harw_home::ResolvedHomeContext::new(&home_path, "default".to_owned(), project)
            .and_then(|context| context.with_config_discovery_cwd(&repo))
            .map_err(ctx("bind home"))?;
        let (context, live) = fixture(&home)?;
        let startup =
            harw_home::config_layers_report_at(&home.home, &repo).map_err(ctx("startup layers"))?;
        let expected =
            harw_config::discover_config(&startup.layers).map_err(ctx("startup config"))?;
        assert_eq!(
            expected.harness.tui.language,
            harw_config::UiLanguage::Japanese
        );
        live.update(|config| config.harness.tui.language = expected.harness.tui.language);
        command(&context, "reset tui.language").map_err(ctx("reset"))?;
        assert_eq!(
            live.current().harness.tui.language,
            expected.harness.tui.language
        );
        assert_eq!(
            command(&context, "show")
                .map_err(ctx("show"))?
                .data
                .as_ref()
                .map(|data| &data["language"]),
            Some(&json!("ja"))
        );
        Ok(())
    }

    #[test]
    fn committed_sync_warning_updates_live_and_reports_uncertain_durability() -> TestResult {
        use harw_operations::FromRawArgs;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        let (context, live) = fixture(&home)?;
        let tokens = ["set", "tui.language", "ja"].map(str::to_owned);
        let args = SettingsArgs::from_raw_args(&tokens).map_err(ctx("arguments"))?;
        let output = handle_settings_with_save(&context, args, |writer| {
            writer.save_with_outcome()?;
            Ok(harw_config::ConfigSaveOutcome::DurabilityUncertain(
                std::io::Error::other("private injected fsync detail"),
            ))
        })
        .map_err(ctx("committed result"))?;
        assert_eq!(
            live.current().harness.tui.language,
            harw_config::UiLanguage::Japanese
        );
        let config = harw_config::discover_config(&[home.home.clone(), home.profile_dir.clone()])
            .map_err(ctx("read committed profile"))?;
        assert_eq!(
            config.harness.tui.language,
            harw_config::UiLanguage::Japanese
        );
        assert_eq!(
            output.data.as_ref().map(|data| &data["warning"]),
            Some(&json!("durability_uncertain"))
        );
        assert!(output.text.contains("saved and applied"));
        assert!(!output.text.contains("were not changed"));
        assert!(!output.text.contains("private injected"));
        assert!(
            !output
                .data
                .map(|data| data.to_string())
                .unwrap_or_default()
                .contains("private injected")
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
        assert_eq!(data["rows"].as_array().map(Vec::len), Some(6));
        assert_eq!(data["rows"][5]["key"], "dod.watch.mode");
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

    /// PL-100 WP 1.9: `dod.watch.mode` is allow-listed with its three
    /// choices; `/settings show` prints the read-only DoD values.
    #[test]
    fn dod_watch_mode_is_allowlisted_and_dod_values_are_shown_read_only() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        let (context, live) = fixture(&home)?;
        let output = command(&context, "show").map_err(ctx("show"))?;
        assert!(
            output
                .text
                .contains("dod.watch.mode = triage (profile, now)")
        );
        assert!(
            output
                .text
                .contains("dod.watch.effective_config = notify (read-only)")
        );
        assert!(!output.text.contains(crate::dod_model::EGRESS_DOCTOR_HINT));
        assert!(output.text.contains(crate::dod_model::NO_TRIAGE_MODEL_NOTE));
        assert!(output.text.contains(crate::dod_model::TUI_TTY_NOTE));
        assert!(output.text.contains(crate::dod_model::TRIAGE_MODEL_HINT));
        let data = output
            .data
            .ok_or(crate::test_support::TestError::Missing("settings data"))?;
        let row = data["rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["key"] == "dod.watch.mode"))
            .ok_or(crate::test_support::TestError::Missing("watch row"))?;
        assert_eq!(row["value"], "triage");
        assert_eq!(row["choices"], json!(["off", "notify", "triage"]));
        assert_eq!(data["dod"]["approve"]["tui_tty"], true);
        assert_eq!(
            data["dod"]["approve"]["note"],
            crate::dod_model::TUI_TTY_NOTE
        );
        assert_eq!(data["dod"]["triage"]["model"], serde_json::Value::Null);
        assert_eq!(data["dod"]["watch"]["effective_config"], "notify");

        command(&context, "set dod.watch.mode notify").map_err(ctx("set"))?;
        assert_eq!(
            live.current().dod.watch.mode,
            harw_config::DodWatchMode::Notify
        );
        let path = home.profile_dir.join("config.toml");
        let content = std::fs::read_to_string(&path).map_err(ctx("read"))?;
        assert!(content.contains("mode = \"notify\""), "{content}");
        command(&context, "reset dod.watch.mode").map_err(ctx("reset"))?;
        assert_eq!(
            live.current().dod.watch.mode,
            harw_config::DodWatchMode::Triage
        );
        Ok(())
    }

    #[test]
    fn dod_settings_outside_the_allowlist_are_rejected_and_change_nothing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        std::fs::create_dir_all(&home.profile_dir).map_err(ctx("profile"))?;
        let path = home.profile_dir.join("config.toml");
        let source = "[dod.approve]\ntui_tty = true\n";
        std::fs::write(&path, source).map_err(ctx("seed"))?;
        let (context, live) = fixture(&home)?;
        for input in [
            "set dod.watch.mode auto",
            "set dod.watch.mode Triage",
            "set dod.approve.tui_tty false",
            "set dod.triage.model local/x",
            "set dod.watch.autoconnect false",
            "reset dod.triage.model",
            "reset dod.approve.tui_tty",
        ] {
            assert!(command(&context, input).is_err(), "{input}");
        }
        assert_eq!(
            std::fs::read_to_string(&path).map_err(ctx("after"))?,
            source
        );
        assert_eq!(
            live.current().dod.watch.mode,
            harw_config::DodWatchMode::Triage
        );
        Ok(())
    }

    /// With header tables the write edits `[dod.watch]` in place: the
    /// sibling keys `[dod.approve] tui_tty` and `[dod.watch] autoconnect`
    /// stay, on disk and in the live configuration.
    #[test]
    fn dod_watch_mode_keeps_sibling_keys_of_header_tables() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        std::fs::create_dir_all(&home.profile_dir).map_err(ctx("profile"))?;
        let path = home.profile_dir.join("config.toml");
        std::fs::write(
            &path,
            "[dod.approve]\ntui_tty = false\n[dod.watch]\nautoconnect = false\n",
        )
        .map_err(ctx("seed"))?;
        let (context, live) = fixture(&home)?;
        command(&context, "set dod.watch.mode notify").map_err(ctx("set"))?;
        let content = std::fs::read_to_string(&path).map_err(ctx("read"))?;
        assert!(content.contains("tui_tty = false"), "{content}");
        assert!(content.contains("autoconnect = false"), "{content}");
        assert!(content.contains("mode = \"notify\""), "{content}");
        let config = harw_config::discover_config(&[home.home.clone(), home.profile_dir.clone()])
            .map_err(ctx("reread"))?;
        assert_eq!(config.dod.watch.mode, harw_config::DodWatchMode::Notify);
        assert!(!config.dod.watch.autoconnect);
        assert!(!config.dod.approve.tui_tty);
        let current = live.current();
        assert_eq!(current.dod.watch.mode, harw_config::DodWatchMode::Notify);
        assert!(!current.dod.approve.tui_tty);
        assert!(!current.dod.watch.autoconnect);
        Ok(())
    }

    /// `data.dod` looks the triage provider up in home and profile only
    /// (threat B15), not among the session's providers, which a trusted
    /// project layer may redefine.
    #[test]
    fn dod_view_resolves_the_triage_provider_from_home_and_profile() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        std::fs::create_dir_all(home.home.join("providers")).map_err(ctx("providers"))?;
        std::fs::write(
            home.home.join("providers").join("p.toml"),
            "name = \"p\"\napi = \"openai\"\nbase_url = \"http://127.0.0.1:11434/v1\"\n",
        )
        .map_err(ctx("provider"))?;
        std::fs::write(
            home.home.join("config.toml"),
            "[dod.triage]\nmodel = \"p/m\"\n",
        )
        .map_err(ctx("home config"))?;
        let (context, live) = fixture(&home)?;
        let remote: ProviderToml = toml::from_str(
            "name = \"p\"\napi = \"openai\"\nbase_url = \"https://api.example.com/v1\"\n",
        )
        .map_err(ctx("session provider"))?;
        live.update(|config| {
            config.providers.insert("p".to_owned(), remote);
        });
        let output = command(&context, "show").map_err(ctx("show"))?;
        let data = output
            .data
            .ok_or(crate::test_support::TestError::Missing("settings data"))?;
        assert_eq!(data["dod"]["triage"]["model"], "p/m");
        assert_eq!(data["dod"]["triage"]["locality"], "local");
        assert!(
            output.text.contains(&format!(
                "dod.watch.effective_config = triage (read-only; {})",
                crate::dod_model::EGRESS_DOCTOR_HINT
            )),
            "{}",
            output.text
        );
        Ok(())
    }

    /// `set_value` would replace an inline `dod` table and drop `tui_tty`;
    /// the write is refused and the file stays byte-identical.
    #[test]
    fn dod_watch_mode_refuses_inline_parent_tables() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        std::fs::create_dir_all(&home.profile_dir).map_err(ctx("profile"))?;
        let path = home.profile_dir.join("config.toml");
        let (context, live) = fixture(&home)?;
        for source in [
            "dod = { approve = { tui_tty = false } }\n",
            "[dod]\nwatch = { autoconnect = false }\n",
        ] {
            std::fs::write(&path, source).map_err(ctx("seed"))?;
            for input in ["set dod.watch.mode off", "reset dod.watch.mode"] {
                match command(&context, input) {
                    Err(OpError::Execution(message)) => {
                        assert_eq!(message, INLINE_PARENT, "{input}");
                    }
                    other => {
                        return Err(crate::test_support::TestError::Unexpected(format!(
                            "{input}: {other:?}"
                        )));
                    }
                }
                assert_eq!(
                    std::fs::read_to_string(&path).map_err(ctx("after"))?,
                    source
                );
            }
        }
        assert_eq!(
            live.current().dod.watch.mode,
            harw_config::DodWatchMode::Triage
        );
        assert!(table_parents_are_plain("", "dod.watch.mode"));
        assert!(table_parents_are_plain(
            "[dod.watch]\nmode = 'off'\n",
            "dod.watch.mode"
        ));
        assert!(table_parents_are_plain(
            "dod.watch.mode = 'off'\n",
            "dod.watch.mode"
        ));
        assert!(!table_parents_are_plain("dod = 1\n", "dod.watch.mode"));
        assert!(!table_parents_are_plain(
            "not toml = = \n",
            "dod.watch.mode"
        ));
        Ok(())
    }
}
