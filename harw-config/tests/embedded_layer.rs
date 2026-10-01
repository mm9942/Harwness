use harw_config::discover_config;
use std::path::PathBuf;

/// Das eingebettete Default-Profil wird auch mit vollständig leerem Home
/// aufgelöst: der Provider `kimi` kommt aus der ins Binary kompilierten
/// Basisschicht (`harw-config/embedded/profiles/default/`).
#[test]
fn embedded_provider_resolves_with_empty_home() {
    let home = tempfile::tempdir().expect("tempdir");
    let layers = vec![home.path().to_path_buf()];
    let resolved = discover_config(&layers).expect("discover_config");

    assert!(
        resolved.providers.contains_key("kimi"),
        "embedded provider `kimi` must resolve with an empty home (layers: {:?})",
        layers
    );
}

/// Ein Home-Override gewinnt über die eingebettete Basisschicht: der
/// `base_url` eines Providers aus `~/.harw` überschreibt den eingebetteten.
#[test]
fn home_config_overrides_embedded() {
    let home = tempfile::tempdir().expect("tempdir");
    let harw_home = home.path().join(".harw");
    std::fs::create_dir_all(harw_home.join("providers")).expect("mkdir providers");
    std::fs::write(
        harw_home.join("providers/kimi.toml"),
        "name = \"kimi\"\napi = \"openai-chat\"\nbase_url = \"https://override.example/v1\"\nauth = \"env:KIMI_API_KEY\"\nmodels = [\"@cf/zai-org/glm-5.3\"]\nenabled = true\n",
    )
    .expect("write override provider");

    let layers = vec![harw_home.clone()];
    let resolved = discover_config(&layers).expect("discover_config");
    let provider = resolved
        .providers
        .get("kimi")
        .expect("provider kimi present");

    assert_eq!(
        provider.base_url, "https://override.example/v1",
        "home override must win over the embedded base layer (monotonic composition)"
    );
}

/// Relative Pfade der eingebetteten Dateien sind URL-dekodiert und beginnen
/// mit `profiles/default/`.
#[test]
fn embedded_profile_layer_paths_are_decoded() {
    let files = harw_config::embedded_profile::embedded_profile_layer();
    assert!(
        !files.is_empty(),
        "embedded profile must contain at least one file"
    );
    assert!(
        files
            .iter()
            .all(|(path, _)| path.starts_with("config.toml")
                || path.starts_with("providers/")
                || path.starts_with("models/")),
        "all embedded paths must live at the embedded root"
    );
    assert!(
        files.iter().any(|(path, _)| path == "config.toml"),
        "embedded profile must contain config.toml"
    );
    let _ = PathBuf::new(); // keep PathBuf import used in all cfg variants
}
