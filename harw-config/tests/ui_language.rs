use harw_config::{ConfigWriter, HarnessConfig, LayerRole, UiLanguage, merge_layer_toml_into};
mod common;
use common::{TestResult, ctx};

#[test]
fn languages_are_strict_and_round_trip_with_english_default() -> TestResult {
    assert_eq!(HarnessConfig::default().tui.language, UiLanguage::English);
    for (language, code) in UiLanguage::ALL.into_iter().zip([
        "en", "de", "es", "fr", "pt", "ja", "zh-CN", "ko", "vi", "hi",
    ]) {
        assert_eq!(language.as_str(), code);
        assert_eq!(
            code.parse::<UiLanguage>().map_err(ctx("parse language"))?,
            language
        );
        assert!(!language.native_name().is_empty());
        let config: HarnessConfig =
            toml::from_str(&format!("[tui]\nlanguage = {code:?}\n")).map_err(ctx("deserialize"))?;
        assert_eq!(config.tui.language, language);
        assert_eq!(
            serde_json::to_value(language).map_err(ctx("serialize"))?,
            code
        );
    }
    for invalid in ["", "EN", "zh", "zh-cn", "it", " de"] {
        assert!(invalid.parse::<UiLanguage>().is_err());
        assert!(
            toml::from_str::<HarnessConfig>(&format!("[tui]\nlanguage = {invalid:?}")).is_err()
        );
    }
    let legacy: HarnessConfig = toml::from_str("[tui]\ntheme = 'light'").map_err(ctx("legacy"))?;
    assert_eq!(legacy.tui.language, UiLanguage::English);
    Ok(())
}

#[test]
fn locale_obeys_profile_merge_and_untrusted_project_scope() -> TestResult {
    let mut config = HarnessConfig::default();
    let path = std::path::Path::new("config.toml");
    for (source, role, expected) in [
        (
            "[tui]\nlanguage='de'",
            LayerRole::Baseline,
            UiLanguage::German,
        ),
        (
            "[tui]\ntheme='light'",
            LayerRole::Refinement,
            UiLanguage::German,
        ),
        (
            "[tui]\nlanguage='ja'",
            LayerRole::Refinement,
            UiLanguage::Japanese,
        ),
        (
            "[tui]\nlanguage='fr'",
            LayerRole::UntrustedProject,
            UiLanguage::Japanese,
        ),
    ] {
        let incoming = toml::from_str(source).map_err(ctx("parse layer"))?;
        merge_layer_toml_into(&mut config, incoming, source, role, path).map_err(ctx("merge"))?;
        assert_eq!(config.tui.language, expected);
    }
    let scope = harw_config::FIELD_TABLE
        .iter()
        .find(|field| field.path == "tui.language");
    assert!(scope.is_some_and(|field| field.scope == harw_config::Scope::Profile));
    Ok(())
}

#[test]
fn competing_writers_refuse_busy_and_stale_commits() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let path = dir.path().join("config.toml");
    let mut stale = ConfigWriter::open(&path).map_err(ctx("open stale"))?;
    let mut locked = ConfigWriter::open_locked(&path).map_err(ctx("open locked"))?;
    assert!(ConfigWriter::open(&path).is_err());
    assert!(stale.save().is_err());
    locked
        .set_value("tui.language", toml_edit::value("de"))
        .map_err(ctx("edit"))?;
    locked.save().map_err(ctx("save"))?;
    drop(locked);
    stale
        .set_value("tui.theme", toml_edit::value("light"))
        .map_err(ctx("stale edit"))?;
    assert!(stale.save().is_err());
    let fresh = ConfigWriter::open(&path).map_err(ctx("reopen"))?;
    assert_eq!(fresh.get_value("tui.language").as_deref(), Some("de"));
    assert_eq!(fresh.get_value("tui.theme"), None);
    Ok(())
}

#[test]
fn candidate_discovery_resolves_without_persisting_and_rejects_foreign_paths() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let root = dir.path().join("home");
    let profile = root.join("profiles/active");
    std::fs::create_dir_all(&profile).map_err(ctx("profile"))?;
    std::fs::write(root.join("config.toml"), "[tui]\nlanguage='de'\n").map_err(ctx("root"))?;
    let path = profile.join("config.toml");
    let layers = [root, profile];
    let resolved = harw_config::discover_config_with_override(
        &layers,
        None,
        None,
        &path,
        "[tui]\nlanguage='ja'\n",
    )
    .map_err(ctx("candidate"))?;
    assert_eq!(resolved.harness.tui.language, UiLanguage::Japanese);
    assert!(!path.exists());
    let reset = harw_config::discover_config_with_override(&layers, None, None, &path, "[tui]\n")
        .map_err(ctx("reset candidate"))?;
    assert_eq!(reset.harness.tui.language, UiLanguage::German);
    assert!(
        harw_config::discover_config_with_override(
            &layers,
            None,
            None,
            &dir.path().join("foreign.toml"),
            ""
        )
        .is_err()
    );
    assert!(!path.exists());
    Ok(())
}
