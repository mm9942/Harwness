//! Merge-Verhaltens-Tests für `crate::merge::merge_layer_into` und
//! `discover_config_with_restricted` (Paket C, `docs/design/config-scopes.md`
//! Abschnitt 7h — alle 27 spezifizierten Tests, inkl. der vier durch die
//! R1/R2-Entscheidung vom 2026-09-21 neu hinzugekommenen #24–#27).
//!
//! Ausschließlich öffentliche API (`harw_config::*`) — kein Zugriff auf
//! interne `#[cfg(test)]`-Module aus `scope.rs`/`merge.rs`/`discovery.rs`.
//! Das Tempdir-/Layer-Aufbau-Muster (`test_directory`/`write_layer_file`)
//! ist bewusst identisch zu dem in `harw-config/src/discovery.rs`
//! (`mod tests`) verwendeten Muster nachgebaut, da die dortigen Helfer
//! modul-privat und aus `tests/` nicht erreichbar sind.

mod common;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{TestError, TestResult, ctx};
use harw_config::{
    CargoSandboxModeToml, CargoSandboxToml, HarnessConfig, LayerRole, McpJobCapabilityToml,
    McpPrincipalToml, RuleToml, SecretRef, discover_config_with_restricted, merge_layer_into,
};

// ---------------------------------------------------------------------
// Test-Fixture-Helfer — Muster identisch zu harw-config/src/discovery.rs
// (`mod tests`: `test_directory`/`write_layer_file`), hier als eigene
// Kopie, weil aus `tests/` nicht modul-privat importierbar.
// ---------------------------------------------------------------------

static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

fn test_directory(label: &str) -> TestResult<PathBuf> {
    let unique = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "harw-config-scope-merge-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).map_err(ctx("Testverzeichnis anlegen"))?;
    Ok(directory)
}

fn write_layer_file(base: &Path, rel: &str, contents: &str) -> TestResult {
    let path = base.join(rel);
    let parent = path
        .parent()
        .ok_or(TestError::Missing("Elternverzeichnis des Layer-Pfads"))?;
    std::fs::create_dir_all(parent).map_err(ctx("Layer-Verzeichnis anlegen"))?;
    std::fs::write(&path, contents).map_err(ctx("Layer-Datei schreiben"))?;
    Ok(())
}

/// Fester Beispielpfad für direkte `merge_layer_into`-Aufrufe, die keinen
/// echten Tempdir-Layer brauchen (nur `ScopeDiagnostic::file`).
fn layer_path() -> PathBuf {
    PathBuf::from("/home/user/.harw/profiles/default/config.toml")
}

fn raw_from(src: &str) -> TestResult<toml::Value> {
    toml::from_str(src).map_err(ctx("valid toml fixture"))
}

// =======================================================================
// Abschnitt 7h, Tests 1–11: ein Test pro `MergeRule`-Variante
// =======================================================================

// Test 1: ProfileReplaces — Profil lässt das Feld unbenutzt, Home-Wert
// bleibt erhalten statt auf den Section-Default zu fallen (Kernbeweis des
// Bugfix, Abschnitt 4).
#[test]
fn test_merge_layer_into_profile_replaces_keeps_home_value_when_profile_omits_field() -> TestResult
{
    let mut trusted = HarnessConfig::default();
    trusted.logging.level = "debug".to_owned();
    trusted.tui.theme = "midnight".to_owned();
    let incoming = HarnessConfig::default();
    // Profil-Layer setzt nur ein völlig anderes Feld — [logging]/[tui]
    // tauchen im rohen TOML dieses Layers gar nicht auf.
    let raw = raw_from(r#"default_provider = "anthropic""#)?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.logging.level, "debug");
    assert_eq!(trusted.tui.theme, "midnight");
    assert!(diagnostics.is_empty());
    Ok(())
}

// Test 2: ProfileReplaces-Sonderfall `internal_models` — Profil setzt nur
// `session_title`, `compaction_summary` aus Home bleibt erhalten (Beweis,
// dass `merge_internal_models` unverändert eingebunden ist, Abschnitt 7f).
#[test]
fn test_merge_layer_into_profile_replaces_internal_models_partial_override_keeps_sibling_choice()
-> TestResult {
    use harw_config::InternalModelChoice;

    let mut trusted = HarnessConfig::default();
    trusted.internal_models.compaction_summary = Some(InternalModelChoice {
        provider: Some("anthropic".to_owned()),
        model: Some("claude-x".to_owned()),
    });

    let mut incoming = HarnessConfig::default();
    incoming.internal_models.session_title = Some(InternalModelChoice {
        provider: Some("openrouter".to_owned()),
        model: Some("nemotron".to_owned()),
    });
    let raw = raw_from(
        r#"
            [internal_models.session_title]
            provider = "openrouter"
            model = "nemotron"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(
        trusted.internal_models.session_title,
        Some(InternalModelChoice {
            provider: Some("openrouter".to_owned()),
            model: Some("nemotron".to_owned()),
        })
    );
    assert_eq!(
        trusted.internal_models.compaction_summary,
        Some(InternalModelChoice {
            provider: Some("anthropic".to_owned()),
            model: Some("claude-x".to_owned()),
        }),
        "compaction_summary muss aus dem Home-Stand erhalten bleiben"
    );
    assert!(diagnostics.is_empty());
    Ok(())
}

// Test 3: GlobalOnly — Profil versucht sandbox.cargo zu ändern.
#[test]
fn test_merge_layer_into_global_only_rejects_profile_override_of_sandbox_cargo() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.sandbox.cargo = Some(CargoSandboxToml {
        mode: CargoSandboxModeToml::Inspect,
        cargo_bin: "/opt/harw/cargo".to_owned(),
        rustup_home: "/opt/harw/rustup".to_owned(),
        cargo_home: "/opt/harw/cargo-home".to_owned(),
    });

    let mut incoming = HarnessConfig::default();
    incoming.sandbox.cargo = Some(CargoSandboxToml {
        mode: CargoSandboxModeToml::Fetch,
        cargo_bin: "/evil/cargo".to_owned(),
        rustup_home: "/opt/harw/rustup".to_owned(),
        cargo_home: "/opt/harw/cargo-home".to_owned(),
    });
    let raw = raw_from(
        r#"
            [sandbox.cargo]
            mode = "fetch"
            cargo_bin = "/evil/cargo"
            rustup_home = "/opt/harw/rustup"
            cargo_home = "/opt/harw/cargo-home"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    let cargo = trusted
        .sandbox
        .cargo
        .as_ref()
        .ok_or(TestError::Missing("sandbox.cargo"))?;
    assert_eq!(cargo.cargo_bin, "/opt/harw/cargo");
    // Abweichung von Abschnitt 7h/Test 3: die Spezifikation erwartet dort
    // `field == "sandbox.cargo.cargo_bin"`; der tatsächliche Code
    // (`merge.rs::merge_sandbox`) vergleicht `sandbox.cargo` als ein
    // atomares `Option<CargoSandboxToml>` per `global_only` — die
    // Diagnostic trägt daher `field == "sandbox.cargo"`, nicht das
    // Unterfeld. Siehe Abschlussbericht.
    assert!(diagnostics.iter().any(|d| d.field == "sandbox.cargo"));
    Ok(())
}

// Test 4: Union — beide Layer-Einträge überleben.
#[test]
fn test_merge_layer_into_union_combines_policy_require_approval_for_from_both_layers() -> TestResult
{
    let mut trusted = HarnessConfig::default();
    trusted.policy.require_approval_for = vec!["shell.exec".to_owned()];
    let mut incoming = HarnessConfig::default();
    incoming.policy.require_approval_for = vec!["fs.write".to_owned()];
    let raw = raw_from(
        r#"
            [policy]
            require_approval_for = ["fs.write"]
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.policy.require_approval_for.len(), 2);
    assert!(
        trusted
            .policy
            .require_approval_for
            .contains(&"shell.exec".to_owned())
    );
    assert!(
        trusted
            .policy
            .require_approval_for
            .contains(&"fs.write".to_owned())
    );
    assert!(
        diagnostics.is_empty(),
        "Union kann keinen Lockerungsversuch ablehnen"
    );
    Ok(())
}

// Test 5: Intersection — ein neuer Eintrag wird verworfen + diagnostiziert.
#[test]
fn test_merge_layer_into_intersection_drops_new_permissions_allow_entry() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.permissions.allow = vec![
        RuleToml {
            tool: "shell.exec".to_owned(),
            pattern: None,
        },
        RuleToml {
            tool: "fs.read".to_owned(),
            pattern: None,
        },
    ];
    let mut incoming = HarnessConfig::default();
    incoming.permissions.allow = vec![
        RuleToml {
            tool: "shell.exec".to_owned(),
            pattern: None,
        },
        RuleToml {
            tool: "fs.write".to_owned(),
            pattern: None,
        },
    ];
    let raw = raw_from(
        r#"
            [[permissions.allow]]
            tool = "shell.exec"
            [[permissions.allow]]
            tool = "fs.write"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.permissions.allow.len(), 1);
    assert_eq!(trusted.permissions.allow[0].tool, "shell.exec");
    assert!(diagnostics.iter().any(|d| d.field == "permissions.allow"));
    Ok(())
}

// Test 6a: MinBound — ein höherer Profil-Wert wird verworfen.
#[test]
fn test_merge_layer_into_min_bound_rejects_higher_tools_plan_max_nodes() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.tools.plan.max_nodes = 64;
    let mut incoming = HarnessConfig::default();
    incoming.tools.plan.max_nodes = 999;
    let raw = raw_from(
        r#"
            [tools.plan]
            max_nodes = 999
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.tools.plan.max_nodes, 64);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "tools.plan.max_nodes")
    );
    Ok(())
}

// Test 6b: MinBound — ein niedrigerer Profil-Wert wird übernommen, keine
// Diagnostic (legitime Verengung).
#[test]
fn test_merge_layer_into_min_bound_accepts_lower_tools_plan_max_nodes() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.tools.plan.max_nodes = 64;
    let mut incoming = HarnessConfig::default();
    incoming.tools.plan.max_nodes = 10;
    let raw = raw_from(
        r#"
            [tools.plan]
            max_nodes = 10
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.tools.plan.max_nodes, 10);
    assert!(
        diagnostics
            .iter()
            .all(|d| d.field != "tools.plan.max_nodes")
    );
    Ok(())
}

// Test 7: AndBool — Profil versucht einen global deaktivierten Listener zu
// aktivieren.
#[test]
fn test_merge_layer_into_and_bool_keeps_mcp_listener_disabled_when_profile_tries_to_enable()
-> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.mcp_listener.enabled = false;
    let mut incoming = HarnessConfig::default();
    incoming.mcp_listener.enabled = true;
    let raw = raw_from(
        r#"
            [mcp_listener]
            enabled = true
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert!(!trusted.mcp_listener.enabled);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "mcp_listener.enabled")
    );
    Ok(())
}

// Test 8: OrBool — Profil versucht einen global aktiven Wächter
// abzuschalten.
#[test]
fn test_merge_layer_into_or_bool_keeps_guards_enabled_when_profile_tries_to_disable() -> TestResult
{
    let mut trusted = HarnessConfig::default();
    trusted.guards.enabled = Some(true);
    let mut incoming = HarnessConfig::default();
    incoming.guards.enabled = Some(false);
    let raw = raw_from(
        r#"
            [guards]
            enabled = false
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.guards.enabled, Some(true));
    assert!(diagnostics.iter().any(|d| d.field == "guards.enabled"));
    Ok(())
}

// Test 9a: StricterOf — Profil versucht einen lockereren Wert zu wählen.
#[test]
fn test_merge_layer_into_stricter_of_rejects_looser_permissions_default_mode() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.permissions.default_mode = Some("auto".to_owned());
    let mut incoming = HarnessConfig::default();
    incoming.permissions.default_mode = Some("full".to_owned());
    let raw = raw_from(
        r#"
            [permissions]
            default_mode = "full"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.permissions.default_mode.as_deref(), Some("auto"));
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "permissions.default_mode")
    );
    Ok(())
}

// Test 9b: StricterOf — Profil wählt einen strengeren Wert, keine
// Diagnostic.
#[test]
fn test_merge_layer_into_stricter_of_accepts_stricter_permissions_default_mode() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.permissions.default_mode = Some("auto".to_owned());
    let mut incoming = HarnessConfig::default();
    incoming.permissions.default_mode = Some("ask".to_owned());
    let raw = raw_from(
        r#"
            [permissions]
            default_mode = "ask"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.permissions.default_mode.as_deref(), Some("ask"));
    assert!(
        diagnostics
            .iter()
            .all(|d| d.field != "permissions.default_mode")
    );
    Ok(())
}

// Test 10: CompositeMember — ein `RuleToml`-Element wird nur als Ganzes
// verglichen: gleicher `tool`, aber anderes `pattern` gilt als
// unterschiedlicher Eintrag (kein teilweiser Abgleich nur über `tool`).
#[test]
fn test_merge_layer_into_composite_member_rule_toml_uses_full_struct_equality() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.permissions.allow = vec![RuleToml {
        tool: "shell.exec".to_owned(),
        pattern: Some("cargo check".to_owned()),
    }];
    let mut incoming = HarnessConfig::default();
    incoming.permissions.allow = vec![RuleToml {
        tool: "shell.exec".to_owned(),
        pattern: Some("cargo build".to_owned()),
    }];
    let raw = raw_from(
        r#"
            [[permissions.allow]]
            tool = "shell.exec"
            pattern = "cargo build"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    // Die Home-Fassung (tool="shell.exec", pattern="cargo check") ist im
    // Profil-Wert nicht enthalten (unterschiedliches `pattern`), fällt also
    // aus der Schnittmenge heraus. Die Profil-Fassung (anderes `pattern`)
    // ist in der Home-Menge nicht enthalten, wird also verworfen.
    assert!(
        trusted.permissions.allow.is_empty(),
        "gleicher tool-Name mit unterschiedlichem pattern darf nicht als gleicher Eintrag zählen"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "permissions.allow" && d.rejected_value.contains("cargo build"))
    );
    Ok(())
}

// Test 11: PerFileValidated — `config_version` wird nicht vom vorigen
// Layer "geerbt": ein Layer, der das Feld nicht selbst setzt, fällt auf
// seinen eigenen (Default-)Wert zurück statt den Wert des vorigen Layers
// zu übernehmen (kein Merge-Effekt, im Unterschied zu `ProfileReplaces`).
#[test]
fn test_merge_layer_into_per_file_validated_config_version_is_never_inherited() -> TestResult {
    let mut trusted = HarnessConfig {
        config_version: 3,
        ..Default::default()
    };
    let incoming = HarnessConfig {
        default_provider: Some("anthropic".to_owned()),
        ..Default::default()
    };
    let raw = raw_from(r#"default_provider = "anthropic""#)?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(
        trusted.config_version, 0,
        "PerFileValidated darf den config_version des vorigen Layers nicht uebernehmen"
    );
    assert!(diagnostics.iter().all(|d| d.field != "config_version"));
    Ok(())
}

// =======================================================================
// Abschnitt 7h: ein Test pro sicherheitskritischem (🔒) Feld (8 Stück)
// =======================================================================

// Test 12 (🔒 mcp_listener.enabled).
#[test]
fn test_security_critical_mcp_listener_enabled_and_bool_rejects_loosening() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.mcp_listener.enabled = false;
    let mut incoming = HarnessConfig::default();
    incoming.mcp_listener.enabled = true;
    let raw = raw_from("[mcp_listener]\nenabled = true\n")?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert!(!trusted.mcp_listener.enabled);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "mcp_listener.enabled")
    );
    Ok(())
}

// Test 13 (🔒 mcp_listener.listen_addr).
#[test]
fn test_security_critical_mcp_listener_listen_addr_global_only_rejects_override() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.mcp_listener.listen_addr = "127.0.0.1:1337".to_owned();
    let mut incoming = HarnessConfig::default();
    incoming.mcp_listener.listen_addr = "0.0.0.0:1337".to_owned();
    let raw = raw_from("[mcp_listener]\nlisten_addr = \"0.0.0.0:1337\"\n")?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.mcp_listener.listen_addr, "127.0.0.1:1337");
    assert!(diagnostics.iter().any(
        |d| d.field == "mcp_listener.listen_addr" && d.rejected_value.contains("0.0.0.0:1337")
    ));
    Ok(())
}

// Test 14 (🔒 mcp_listener.principals — Hinzufügen wird verworfen; seit R2
// `Intersection` nach `id`, nicht mehr `GlobalOnly`).
#[test]
fn test_security_critical_mcp_listener_principals_intersection_rejects_addition() -> TestResult {
    let p1 = McpPrincipalToml {
        id: "p1".to_owned(),
        credential_ref: SecretRef::from_str("env:P1_TOKEN").map_err(ctx("secret ref p1"))?,
        tenant: "alice".to_owned(),
        workspace: "harwness".to_owned(),
        job_capabilities: vec![],
    };
    let intruder = McpPrincipalToml {
        id: "intruder".to_owned(),
        credential_ref: SecretRef::from_str("env:INTRUDER_TOKEN")
            .map_err(ctx("secret ref intruder"))?,
        tenant: "evil".to_owned(),
        workspace: "evil".to_owned(),
        job_capabilities: vec![],
    };

    let mut trusted = HarnessConfig::default();
    trusted.mcp_listener.principals = vec![p1.clone()];
    let mut incoming = HarnessConfig::default();
    incoming.mcp_listener.principals = vec![p1, intruder];
    let raw = raw_from(
        r#"
            [[mcp_listener.principals]]
            id = "p1"
            credential_ref = "env:P1_TOKEN"
            tenant = "alice"
            workspace = "harwness"
            [[mcp_listener.principals]]
            id = "intruder"
            credential_ref = "env:INTRUDER_TOKEN"
            tenant = "evil"
            workspace = "evil"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.mcp_listener.principals.len(), 1);
    assert_eq!(trusted.mcp_listener.principals[0].id, "p1");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "mcp_listener.principals" && d.rejected_value.contains("intruder"))
    );
    Ok(())
}

// Test 15 (🔒 permissions.allow, eigenständige Instanz — s. auch Test 5).
#[test]
fn test_security_critical_permissions_allow_intersection_rejects_new_entry() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.permissions.allow = vec![
        RuleToml {
            tool: "shell.exec".to_owned(),
            pattern: None,
        },
        RuleToml {
            tool: "fs.read".to_owned(),
            pattern: None,
        },
    ];
    let mut incoming = HarnessConfig::default();
    incoming.permissions.allow = vec![
        RuleToml {
            tool: "shell.exec".to_owned(),
            pattern: None,
        },
        RuleToml {
            tool: "network.fetch".to_owned(),
            pattern: None,
        },
    ];
    let raw = raw_from(
        r#"
            [[permissions.allow]]
            tool = "shell.exec"
            [[permissions.allow]]
            tool = "network.fetch"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(
        trusted.permissions.allow,
        vec![RuleToml {
            tool: "shell.exec".to_owned(),
            pattern: None
        }]
    );
    assert!(diagnostics.iter().any(|d| d.field == "permissions.allow"));
    Ok(())
}

// Test 16 (🔒 permissions.extra_roots).
#[test]
fn test_security_critical_permissions_extra_roots_intersection_rejects_new_entry() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.permissions.extra_roots = vec![PathBuf::from("/a")];
    let mut incoming = HarnessConfig::default();
    incoming.permissions.extra_roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
    let raw = raw_from(
        r#"
            [permissions]
            extra_roots = ["/a", "/b"]
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.permissions.extra_roots, vec![PathBuf::from("/a")]);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "permissions.extra_roots")
    );
    Ok(())
}

// Test 17 (🔒 permissions.default_mode — s. auch Test 9a).
#[test]
fn test_security_critical_permissions_default_mode_stricter_of_rejects_looser_value() -> TestResult
{
    let mut trusted = HarnessConfig::default();
    trusted.permissions.default_mode = Some("ask".to_owned());
    let mut incoming = HarnessConfig::default();
    incoming.permissions.default_mode = Some("auto".to_owned());
    let raw = raw_from("[permissions]\ndefault_mode = \"auto\"\n")?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.permissions.default_mode.as_deref(), Some("ask"));
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "permissions.default_mode")
    );
    Ok(())
}

// Test 18 (🔒 sandbox.cargo/sandbox.tmux — s. auch Test 3).
#[test]
fn test_security_critical_sandbox_cargo_global_only_rejects_override() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.sandbox.cargo = Some(CargoSandboxToml {
        mode: CargoSandboxModeToml::BuildOffline,
        cargo_bin: "/opt/harw/toolchain/bin/cargo".to_owned(),
        rustup_home: "/opt/harw/rustup".to_owned(),
        cargo_home: "/var/cache/harw/cargo".to_owned(),
    });
    let mut incoming = HarnessConfig::default();
    incoming.sandbox.cargo = Some(CargoSandboxToml {
        mode: CargoSandboxModeToml::Fetch,
        cargo_bin: "/tmp/attacker-cargo".to_owned(),
        rustup_home: "/opt/harw/rustup".to_owned(),
        cargo_home: "/var/cache/harw/cargo".to_owned(),
    });
    let raw = raw_from(
        r#"
            [sandbox.cargo]
            mode = "fetch"
            cargo_bin = "/tmp/attacker-cargo"
            rustup_home = "/opt/harw/rustup"
            cargo_home = "/var/cache/harw/cargo"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    let cargo = trusted
        .sandbox
        .cargo
        .as_ref()
        .ok_or(TestError::Missing("sandbox.cargo"))?;
    assert_eq!(cargo.cargo_bin, "/opt/harw/toolchain/bin/cargo");
    assert!(diagnostics.iter().any(|d| d.field == "sandbox.cargo"));
    Ok(())
}

// Test 19 (🔒 research.network_allow_hosts).
#[test]
fn test_security_critical_research_network_allow_hosts_intersection_rejects_new_host() -> TestResult
{
    let mut trusted = HarnessConfig::default();
    trusted.research.network_allow_hosts = vec!["docs.rs".to_owned()];
    let mut incoming = HarnessConfig::default();
    incoming.research.network_allow_hosts = vec!["docs.rs".to_owned(), "evil.example".to_owned()];
    let raw = raw_from(
        r#"
            [research]
            network_allow_hosts = ["docs.rs", "evil.example"]
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(
        trusted.research.network_allow_hosts,
        vec!["docs.rs".to_owned()]
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "research.network_allow_hosts"
                && d.rejected_value.contains("evil.example"))
    );
    Ok(())
}

// =======================================================================
// Abschnitt 7h, Test 20: Regressionstest über discover_config_with_restricted
// =======================================================================

// Test 20: „globales require_approval_for überlebt eine Profil-config.toml"
// — vor dem Umbau musste dieser Test rot sein (Abschnitt 0/4: `resolved.
// harness = cfg` ersetzte die gesamte HarnessConfig), jetzt grün.
#[test]
fn test_discover_config_with_restricted_global_require_approval_for_survives_profile_config_toml()
-> TestResult {
    let home = test_directory("regression-home")?;
    let profile = test_directory("regression-profile")?;
    write_layer_file(
        &home,
        "config.toml",
        r#"
            [policy]
            require_approval_for = ["shell.exec"]
        "#,
    )?;
    // Profil-Layer setzt eine andere Sektion, aber KEIN [policy] — exakt
    // der vom Nutzer beschriebene Fall.
    write_layer_file(
        &profile,
        "config.toml",
        r#"
            [mcp_listener]
            enabled = false
        "#,
    )?;

    let resolved = discover_config_with_restricted(&[home.clone(), profile.clone()], None)
        .map_err(ctx("discover_config_with_restricted"))?;

    assert_eq!(
        resolved.harness.policy.require_approval_for,
        vec!["shell.exec".to_owned()]
    );

    std::fs::remove_dir_all(&home).map_err(ctx("home aufräumen"))?;
    std::fs::remove_dir_all(&profile).map_err(ctx("profile aufräumen"))?;
    Ok(())
}

// =======================================================================
// Abschnitt 7h, Tests 21–23: Schutzwirkung des nicht vertrauten
// Projekt-Layers bleibt erhalten (und wächst bewusst, Abschnitt 7c)
// =======================================================================

// Test 21: alle neun heute schon per (jetzt entfernter) `merge_restricted_
// harness` abgedeckten Fälle 1:1 gegen `discover_config_with_restricted`
// reproduziert (Nicht-Regression bei der Umstellung auf `merge_layer_into`).
#[test]
fn test_discover_config_with_restricted_untrusted_project_narrows_nine_legacy_fields() -> TestResult
{
    let home = test_directory("legacy-nine-home")?;
    let repo = test_directory("legacy-nine-repo")?;
    write_layer_file(
        &home,
        "config.toml",
        r#"
            [policy]
            require_approval_for = ["fs.write"]

            [research]
            network_allow_hosts = ["docs.rs", "crates.io"]
        "#,
    )?;
    write_layer_file(
        &repo,
        "config.toml",
        r#"
            [policy]
            require_approval_for = ["shell.exec"]

            [research]
            network_allow_hosts = ["docs.rs", "evil.example"]
            cargo_registry_read = false
            max_fetch_bytes = 100
            fetch_timeout_secs = 5

            [tools.plan]
            validate_dependency_cycles = false
            validate_write_conflicts = false
            max_nodes = 8
            max_expand_depth = 1
        "#,
    )?;

    let resolved =
        discover_config_with_restricted(std::slice::from_ref(&home), Some(repo.as_path()))
            .map_err(ctx("discover_config_with_restricted"))?;

    // 1. policy.require_approval_for — Union.
    let mut approvals = resolved.harness.policy.require_approval_for.clone();
    approvals.sort();
    assert_eq!(
        approvals,
        vec!["fs.write".to_owned(), "shell.exec".to_owned()]
    );
    // 2. research.network_allow_hosts — Intersection.
    assert_eq!(
        resolved.harness.research.network_allow_hosts,
        vec!["docs.rs".to_owned()]
    );
    // 3. research.cargo_registry_read — AND (narrowing accepted).
    assert!(!resolved.harness.research.cargo_registry_read);
    // 4. research.max_fetch_bytes — MinBound.
    assert_eq!(resolved.harness.research.max_fetch_bytes, 100);
    // 5. research.fetch_timeout_secs — MinBound.
    assert_eq!(resolved.harness.research.fetch_timeout_secs, 5);
    // 6. tools.plan.validate_dependency_cycles — OrBool (loosening rejected,
    //    home default is `true`).
    assert!(resolved.harness.tools.plan.validate_dependency_cycles);
    // 7. tools.plan.validate_write_conflicts — OrBool (loosening rejected).
    assert!(resolved.harness.tools.plan.validate_write_conflicts);
    // 8. tools.plan.max_nodes — MinBound.
    assert_eq!(resolved.harness.tools.plan.max_nodes, 8);
    // 9. tools.plan.max_expand_depth — MinBound.
    assert_eq!(resolved.harness.tools.plan.max_expand_depth, 1);

    std::fs::remove_dir_all(&home).map_err(ctx("home aufräumen"))?;
    std::fs::remove_dir_all(&repo).map_err(ctx("repo aufräumen"))?;
    Ok(())
}

// Test 22: ein Feld, das laut Abschnitt 7c NEU für den Projekt-Layer
// geschützt wird (`guards.repeated_failure_warn` — vorher NICHT Teil der
// neun `merge_restricted_harness`-Felder) — Beweis der bewussten
// Schutz-Erweiterung.
#[test]
fn test_discover_config_with_restricted_untrusted_project_narrows_newly_protected_guards_field()
-> TestResult {
    let home = test_directory("newly-protected-home")?;
    let repo = test_directory("newly-protected-repo")?;
    write_layer_file(
        &home,
        "config.toml",
        r#"
            [guards]
            repeated_failure_warn = 2
        "#,
    )?;
    write_layer_file(
        &repo,
        "config.toml",
        r#"
            [guards]
            repeated_failure_warn = 99
        "#,
    )?;

    let resolved =
        discover_config_with_restricted(std::slice::from_ref(&home), Some(repo.as_path()))
            .map_err(ctx("discover_config_with_restricted"))?;

    assert_eq!(resolved.harness.guards.repeated_failure_warn, Some(2));
    assert!(
        resolved
            .scope_warnings
            .iter()
            .any(|d| d.field == "guards.repeated_failure_warn"),
        "der Lockerungsversuch des nicht vertrauten Projekt-Layers muss diagnostiziert werden"
    );

    std::fs::remove_dir_all(&home).map_err(ctx("home aufräumen"))?;
    std::fs::remove_dir_all(&repo).map_err(ctx("repo aufräumen"))?;
    Ok(())
}

// Test 23: [mcp_listener]/sandbox.* bleiben für den nicht vertrauten
// Projekt-Layer wirkungslos — kein Versuch (Listener aktivieren, neuen
// Principal hinzufügen, sandbox.cargo überschreiben) schlägt durch, gleich
// ob er per `ProfileReplaces`-Ausschluss ignoriert oder per `AndBool`/
// `Intersection`/`GlobalOnly` abgelehnt + diagnostiziert wird.
#[test]
fn test_discover_config_with_restricted_untrusted_project_cannot_reach_mcp_listener_or_sandbox()
-> TestResult {
    let home = test_directory("unreachable-home")?;
    let repo = test_directory("unreachable-repo")?;
    write_layer_file(
        &home,
        "config.toml",
        r#"
            [mcp_listener]
            enabled = false

            [[mcp_listener.principals]]
            id = "p1"
            credential_ref = "env:P1_TOKEN"
            tenant = "alice"
            workspace = "harwness"

            [sandbox.cargo]
            mode = "inspect"
            cargo_bin = "/opt/harw/cargo"
            rustup_home = "/opt/harw/rustup"
            cargo_home = "/opt/harw/cargo-home"
        "#,
    )?;
    write_layer_file(
        &repo,
        "config.toml",
        r#"
            [mcp_listener]
            enabled = true

            [[mcp_listener.principals]]
            id = "p1"
            credential_ref = "env:P1_TOKEN"
            tenant = "alice"
            workspace = "harwness"
            [[mcp_listener.principals]]
            id = "intruder"
            credential_ref = "env:EVIL_TOKEN"
            tenant = "evil"
            workspace = "evil"

            [sandbox.cargo]
            mode = "fetch"
            cargo_bin = "/evil/cargo"
            rustup_home = "/opt/harw/rustup"
            cargo_home = "/opt/harw/cargo-home"
        "#,
    )?;

    let resolved =
        discover_config_with_restricted(std::slice::from_ref(&home), Some(repo.as_path()))
            .map_err(ctx("discover_config_with_restricted"))?;

    assert!(
        !resolved.harness.mcp_listener.enabled,
        "Listener darf vom Projekt-Layer nicht aktiviert werden"
    );
    assert_eq!(resolved.harness.mcp_listener.principals.len(), 1);
    assert_eq!(resolved.harness.mcp_listener.principals[0].id, "p1");
    let cargo = resolved
        .harness
        .sandbox
        .cargo
        .as_ref()
        .ok_or(TestError::Missing("sandbox.cargo"))?;
    assert_eq!(cargo.cargo_bin, "/opt/harw/cargo");
    assert!(!resolved.scope_warnings.is_empty());

    std::fs::remove_dir_all(&home).map_err(ctx("home aufräumen"))?;
    std::fs::remove_dir_all(&repo).map_err(ctx("repo aufräumen"))?;
    Ok(())
}

// =======================================================================
// Abschnitt 7h, Tests 24–27: neu durch die R1/R2-Entscheidung vom
// 2026-09-21 nötig gewordene Tests.
// =======================================================================

// Test 24: StricterOf-Fallback für `policy.default_visibility_scope` (R1)
// — ein dritter/unbekannter Wert wird NICHT in die Ordnung einsortiert,
// sondern wie eine GlobalOnly-Abweichung verworfen.
#[test]
fn test_merge_layer_into_stricter_of_falls_back_for_unknown_visibility_scope_value() -> TestResult {
    let mut trusted = HarnessConfig::default();
    trusted.policy.default_visibility_scope = "self".to_owned();
    let mut incoming = HarnessConfig::default();
    incoming.policy.default_visibility_scope = "team".to_owned();
    let raw = raw_from(
        r#"
            [policy]
            default_visibility_scope = "team"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.policy.default_visibility_scope, "self");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "policy.default_visibility_scope"
                && d.rejected_value.contains("team"))
    );
    Ok(())
}

// Test 25: mcp_listener.principals — Entfernen (R2): Profil setzt eine
// Teilmenge der Home-IDs → effektiv nur die Teilmenge, KEINE Diagnostic
// (reines Entfernen ist erlaubt).
#[test]
fn test_merge_layer_into_mcp_listener_principals_removal_is_silent() -> TestResult {
    let p1 = McpPrincipalToml {
        id: "p1".to_owned(),
        credential_ref: SecretRef::from_str("env:P1_TOKEN").map_err(ctx("secret ref p1"))?,
        tenant: "alice".to_owned(),
        workspace: "harwness".to_owned(),
        job_capabilities: vec![],
    };
    let p2 = McpPrincipalToml {
        id: "p2".to_owned(),
        credential_ref: SecretRef::from_str("env:P2_TOKEN").map_err(ctx("secret ref p2"))?,
        tenant: "alice".to_owned(),
        workspace: "harwness".to_owned(),
        job_capabilities: vec![],
    };

    let mut trusted = HarnessConfig::default();
    trusted.mcp_listener.principals = vec![p1.clone(), p2];
    let mut incoming = HarnessConfig::default();
    incoming.mcp_listener.principals = vec![p1];
    let raw = raw_from(
        r#"
            [[mcp_listener.principals]]
            id = "p1"
            credential_ref = "env:P1_TOKEN"
            tenant = "alice"
            workspace = "harwness"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.mcp_listener.principals.len(), 1);
    assert_eq!(trusted.mcp_listener.principals[0].id, "p1");
    assert!(
        diagnostics.is_empty(),
        "reines Entfernen darf keine ScopeDiagnostic erzeugen"
    );
    Ok(())
}

// Test 26: mcp_listener.principals — Hinzufügen verworfen (R2): Profil
// fügt eine neue `id` hinzu, die in Home nicht vorkommt → verworfen +
// ScopeDiagnostic.
#[test]
fn test_merge_layer_into_mcp_listener_principals_addition_is_rejected() -> TestResult {
    let p1 = McpPrincipalToml {
        id: "p1".to_owned(),
        credential_ref: SecretRef::from_str("env:P1_TOKEN").map_err(ctx("secret ref p1"))?,
        tenant: "alice".to_owned(),
        workspace: "harwness".to_owned(),
        job_capabilities: vec![],
    };
    let p3 = McpPrincipalToml {
        id: "p3".to_owned(),
        credential_ref: SecretRef::from_str("env:P3_TOKEN").map_err(ctx("secret ref p3"))?,
        tenant: "alice".to_owned(),
        workspace: "harwness".to_owned(),
        job_capabilities: vec![],
    };

    let mut trusted = HarnessConfig::default();
    trusted.mcp_listener.principals = vec![p1.clone()];
    let mut incoming = HarnessConfig::default();
    incoming.mcp_listener.principals = vec![p1, p3];
    let raw = raw_from(
        r#"
            [[mcp_listener.principals]]
            id = "p1"
            credential_ref = "env:P1_TOKEN"
            tenant = "alice"
            workspace = "harwness"
            [[mcp_listener.principals]]
            id = "p3"
            credential_ref = "env:P3_TOKEN"
            tenant = "alice"
            workspace = "harwness"
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.mcp_listener.principals.len(), 1);
    assert_eq!(trusted.mcp_listener.principals[0].id, "p1");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "mcp_listener.principals" && d.rejected_value.contains("p3"))
    );
    Ok(())
}

// Test 27: mcp_listener.principals — Rechteausweitung verworfen (R2):
// gleiche `id`, aber erweiterte `job_capabilities` → die vollständige
// Home-Fassung gewinnt unverändert (kein Feld-Merge innerhalb des
// Elements), ScopeDiagnostic wird erzeugt.
#[test]
fn test_merge_layer_into_mcp_listener_principals_field_change_is_rejected_home_wins() -> TestResult
{
    let home_p1 = McpPrincipalToml {
        id: "p1".to_owned(),
        credential_ref: SecretRef::from_str("env:P1_TOKEN").map_err(ctx("secret ref home"))?,
        tenant: "alice".to_owned(),
        workspace: "harwness".to_owned(),
        job_capabilities: vec![McpJobCapabilityToml::ReadOwn],
    };
    let profile_p1 = McpPrincipalToml {
        id: "p1".to_owned(),
        credential_ref: SecretRef::from_str("env:P1_TOKEN").map_err(ctx("secret ref profile"))?,
        tenant: "alice".to_owned(),
        workspace: "harwness".to_owned(),
        job_capabilities: vec![
            McpJobCapabilityToml::ReadOwn,
            McpJobCapabilityToml::CancelWorkspace,
        ],
    };

    let mut trusted = HarnessConfig::default();
    trusted.mcp_listener.principals = vec![home_p1];
    let mut incoming = HarnessConfig::default();
    incoming.mcp_listener.principals = vec![profile_p1];
    let raw = raw_from(
        r#"
            [[mcp_listener.principals]]
            id = "p1"
            credential_ref = "env:P1_TOKEN"
            tenant = "alice"
            workspace = "harwness"
            job_capabilities = ["read_own", "cancel_workspace"]
        "#,
    )?;

    let diagnostics = merge_layer_into(
        &mut trusted,
        incoming,
        &raw,
        LayerRole::Refinement,
        &layer_path(),
    );

    assert_eq!(trusted.mcp_listener.principals.len(), 1);
    assert_eq!(trusted.mcp_listener.principals[0].id, "p1");
    assert_eq!(
        trusted.mcp_listener.principals[0].job_capabilities,
        vec![McpJobCapabilityToml::ReadOwn],
        "kein Feld-Merge innerhalb des Principal-Eintrags — Home-Fassung gewinnt vollstaendig"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.field == "mcp_listener.principals" && d.rejected_value.contains("p1"))
    );
    Ok(())
}
