//! Exhaustivitäts-Tests für `FIELD_TABLE` (Paket C, `docs/design/config-scopes.md`
//! Abschnitt 7a).
//!
//! Jeder Test destrukturiert eine `HarnessConfig`- oder Section-Struct-Instanz
//! **ohne** `..`-Rest-Pattern. Ein künftig hinzugefügtes Feld auf einer
//! dieser Structs löst dadurch einen **Compile-Fehler** (E0027 „pattern does
//! not mention field") aus, statt einen stillschweigend fehlenden
//! `FIELD_TABLE`-Eintrag unentdeckt zu lassen. Für jedes destrukturierte
//! Blattfeld wird zusätzlich zur Laufzeit geprüft, dass `FIELD_TABLE` genau
//! einen Eintrag mit dem erwarteten, gepunkteten Pfad trägt.
//!
//! Die Laufzeit-Sanity-Checks "FIELD_TABLE.len() == 88" und "keine
//! doppelten Pfade" existieren bereits in `harw-config/src/scope.rs`
//! (`mod merge_rule_tests`, Paket A) und werden hier bewusst **nicht**
//! dupliziert — dieser Datei obliegt ausschließlich der destrukturierende
//! Exhaustivitäts-Mechanismus, den Abschnitt 7a der Spezifikation Paket C
//! zuordnet.
//!
//! Ausschließlich öffentliche API (`harw_config::*`) — keine internen
//! Test-Helfer aus `scope.rs`/`merge.rs`/`discovery.rs`.

use std::str::FromStr;

use harw_config::harness_config::{
    CompactionToml, GuardsToml, OnboardingSection, OnboardingSeen, ReasoningWeightsToml,
};
use harw_config::{
    CargoSandboxModeToml, CargoSandboxToml, FIELD_TABLE, HarnessConfig, InternalModelChoice,
    InternalModelsToml, LoggingSection, McpListenerSection, McpPrincipalToml, ModeSection,
    PermissionsSection, PlanSection, PolicySection, ResearchSection, RuleToml, SandboxSection,
    SecretRef, SessionSection, TmuxOperationModeToml, TmuxSandboxToml, ToolsSection, TuiSection,
};

/// Prüft, dass `FIELD_TABLE` **genau einen** Eintrag mit `path` trägt
/// (Abschnitt 7a: „muss einen Eintrag in FIELD_TABLE haben, GENAU EINEN").
fn assert_path_in_field_table_exactly_once(path: &str) {
    let count = FIELD_TABLE.iter().filter(|f| f.path == path).count();
    assert_eq!(
        count, 1,
        "FIELD_TABLE sollte genau einen Eintrag fuer {path:?} haben, hat {count}"
    );
}

// ---------------------------------------------------------------------
// HarnessConfig (Top-Level, Abschnitt 1.1) — Muster wörtlich aus
// Abschnitt 7a der Spezifikation übernommen.
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_harness_config() {
    let HarnessConfig {
        config_version,
        workspace_root,
        default_provider,
        default_model,
        active_agent_definition,
        active_uia_definition,
        uia_provider,
        uia_model,
        uia_worker_model,
        policy_profile,
        logging: _,
        tui: _,
        session: _,
        policy: _,
        mcp_listener: _,
        onboarding: _,
        tools: _,
        mode: _,
        research: _,
        permissions: _,
        sandbox: _,
        project_root_markers,
        internal_models: _,
        compaction: _,
        reasoning: _,
        guards: _,
        base_dir: _, // #[serde(skip)], kein TOML-Feld, keine FIELD_TABLE-Zeile
    } = HarnessConfig::default();
    // Kein `..` — ein neues Feld auf HarnessConfig, das hier nicht
    // aufgeführt wird, ist ein Compile-Fehler (E0027), keine Laufzeitprobe.
    let _ = (
        config_version,
        workspace_root,
        default_provider,
        default_model,
        active_agent_definition,
        active_uia_definition,
        uia_provider,
        uia_model,
        uia_worker_model,
        policy_profile,
        project_root_markers,
    );
    for path in [
        "config_version",
        "workspace_root",
        "default_provider",
        "default_model",
        "active_agent_definition",
        "active_uia_definition",
        "uia_provider",
        "uia_model",
        "uia_worker_model",
        "policy_profile",
        "project_root_markers",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [logging] (Abschnitt 1.2, 3 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_logging_section() {
    let LoggingSection {
        level,
        target_module_paths,
        json,
    } = LoggingSection::default();
    let _ = (level, target_module_paths, json);
    for path in ["logging.level", "logging.target_module_paths", "logging.json"] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [tui] (Abschnitt 1.3, 2 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_tui_section() {
    let TuiSection {
        theme,
        keybindings_file,
    } = TuiSection::default();
    let _ = (theme, keybindings_file);
    for path in ["tui.theme", "tui.keybindings_file"] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [session] (Abschnitt 1.4, 5 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_session_section() {
    let SessionSection {
        store_dir,
        journal_format,
        retention_days,
        title_generation,
        title_model,
    } = SessionSection::default();
    let _ = (store_dir, journal_format, retention_days, title_generation, title_model);
    for path in [
        "session.store_dir",
        "session.journal_format",
        "session.retention_days",
        "session.title_generation",
        "session.title_model",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [policy] (Abschnitt 1.5, 2 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_policy_section() {
    let PolicySection {
        default_visibility_scope,
        require_approval_for,
    } = PolicySection::default();
    let _ = (default_visibility_scope, require_approval_for);
    for path in ["policy.default_visibility_scope", "policy.require_approval_for"] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [mcp_listener] (Abschnitt 1.6, 4 Container-Felder + 5 Principal-Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_mcp_listener_section() {
    let McpListenerSection {
        enabled,
        listen_addr,
        path,
        principals,
    } = McpListenerSection::default();
    let _ = (enabled, listen_addr, path, principals);
    for field_path in [
        "mcp_listener.enabled",
        "mcp_listener.listen_addr",
        "mcp_listener.path",
        "mcp_listener.principals",
    ] {
        assert_path_in_field_table_exactly_once(field_path);
    }
}

#[test]
fn test_field_table_exhaustive_mcp_principal_toml() {
    let principal = McpPrincipalToml {
        id: "p1".to_owned(),
        credential_ref: SecretRef::from_str("env:P1_TOKEN").unwrap(),
        tenant: "mia".to_owned(),
        workspace: "harwness".to_owned(),
        job_capabilities: vec![],
    };
    let McpPrincipalToml {
        id,
        credential_ref,
        tenant,
        workspace,
        job_capabilities,
    } = principal;
    let _ = (id, credential_ref, tenant, workspace, job_capabilities);
    for field_path in [
        "mcp_listener.principals[].id",
        "mcp_listener.principals[].credential_ref",
        "mcp_listener.principals[].tenant",
        "mcp_listener.principals[].workspace",
        "mcp_listener.principals[].job_capabilities",
    ] {
        assert_path_in_field_table_exactly_once(field_path);
    }
}

// ---------------------------------------------------------------------
// [onboarding] (Abschnitt 1.7, 1 Container-Feld + 3 Seen-Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_onboarding_section() {
    let OnboardingSection { seen } = OnboardingSection::default();
    let _ = seen;
    assert_path_in_field_table_exactly_once("onboarding.seen");
}

#[test]
fn test_field_table_exhaustive_onboarding_seen() {
    let OnboardingSeen {
        provider,
        model,
        channel,
    } = OnboardingSeen::default();
    let _ = (provider, model, channel);
    for path in [
        "onboarding.seen.provider",
        "onboarding.seen.model",
        "onboarding.seen.channel",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [tools.plan] (Abschnitt 1.8, ToolsSection hat nur `plan`, 9 Felder auf
// PlanSection selbst)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_tools_section() {
    // `ToolsSection` hat aktuell nur das eine Feld `plan` — keine eigene
    // FIELD_TABLE-Zeile fuer den Container "tools.plan" selbst (nur fuer
    // dessen Unterfelder, s. naechster Test). Die Destrukturierung sichert
    // dennoch zu, dass ein kuenftiges zweites `[tools.*]`-Geschwisterfeld
    // (z. B. `[tools.search]`) hier einen Compile-Fehler ausloest.
    let ToolsSection { plan } = ToolsSection::default();
    let _ = plan;
}

#[test]
fn test_field_table_exhaustive_plan_section() {
    let PlanSection {
        enabled,
        persist,
        require_for_complex_work,
        validate_dependency_cycles,
        validate_write_conflicts,
        max_nodes,
        require_exploration_for,
        exploration_ttl_secs,
        max_expand_depth,
    } = PlanSection::default();
    let _ = (
        enabled,
        persist,
        require_for_complex_work,
        validate_dependency_cycles,
        validate_write_conflicts,
        max_nodes,
        require_exploration_for,
        exploration_ttl_secs,
        max_expand_depth,
    );
    for path in [
        "tools.plan.enabled",
        "tools.plan.persist",
        "tools.plan.require_for_complex_work",
        "tools.plan.validate_dependency_cycles",
        "tools.plan.validate_write_conflicts",
        "tools.plan.max_nodes",
        "tools.plan.require_exploration_for",
        "tools.plan.exploration_ttl_secs",
        "tools.plan.max_expand_depth",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [mode] (Abschnitt 1.9, 1 Feld)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_mode_section() {
    let ModeSection { default } = ModeSection::default();
    let _ = default;
    assert_path_in_field_table_exactly_once("mode.default");
}

// ---------------------------------------------------------------------
// [research] (Abschnitt 1.10, 5 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_research_section() {
    let ResearchSection {
        network_allow_hosts,
        cargo_registry_read,
        max_fetch_bytes,
        fetch_timeout_secs,
        cache_ttl_secs,
    } = ResearchSection::default();
    let _ = (
        network_allow_hosts,
        cargo_registry_read,
        max_fetch_bytes,
        fetch_timeout_secs,
        cache_ttl_secs,
    );
    for path in [
        "research.network_allow_hosts",
        "research.cargo_registry_read",
        "research.max_fetch_bytes",
        "research.fetch_timeout_secs",
        "research.cache_ttl_secs",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [permissions] (Abschnitt 1.11, 5 Container-Felder + RuleToml 2 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_permissions_section() {
    let PermissionsSection {
        default_mode,
        approval_timeout_secs,
        allow,
        deny,
        extra_roots,
    } = PermissionsSection::default();
    let _ = (default_mode, approval_timeout_secs, allow, deny, extra_roots);
    for path in [
        "permissions.default_mode",
        "permissions.approval_timeout_secs",
        "permissions.allow",
        "permissions.deny",
        "permissions.extra_roots",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

#[test]
fn test_field_table_exhaustive_rule_toml() {
    let rule = RuleToml {
        tool: "shell.exec".to_owned(),
        pattern: None,
    };
    let RuleToml { tool, pattern } = rule;
    let _ = (tool, pattern);
    for path in ["RuleToml.tool", "RuleToml.pattern"] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [sandbox] (Abschnitt 1.12, 2 Container-Felder + Cargo 4 + Tmux 2)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_sandbox_section() {
    let SandboxSection { cargo, tmux } = SandboxSection::default();
    let _ = (cargo, tmux);
    for path in ["sandbox.cargo", "sandbox.tmux"] {
        assert_path_in_field_table_exactly_once(path);
    }
}

#[test]
fn test_field_table_exhaustive_cargo_sandbox_toml() {
    let cargo = CargoSandboxToml {
        mode: CargoSandboxModeToml::Inspect,
        cargo_bin: "/opt/harw/cargo".to_owned(),
        rustup_home: "/opt/harw/rustup".to_owned(),
        cargo_home: "/opt/harw/cargo-home".to_owned(),
    };
    let CargoSandboxToml {
        mode,
        cargo_bin,
        rustup_home,
        cargo_home,
    } = cargo;
    let _ = (mode, cargo_bin, rustup_home, cargo_home);
    for path in [
        "sandbox.cargo.mode",
        "sandbox.cargo.cargo_bin",
        "sandbox.cargo.rustup_home",
        "sandbox.cargo.cargo_home",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

#[test]
fn test_field_table_exhaustive_tmux_sandbox_toml() {
    let tmux = TmuxSandboxToml {
        mode: TmuxOperationModeToml::Inspect,
        socket_path: "/tmp/tmux-1000/default".to_owned(),
    };
    let TmuxSandboxToml { mode, socket_path } = tmux;
    let _ = (mode, socket_path);
    for path in ["sandbox.tmux.mode", "sandbox.tmux.socket_path"] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [internal_models] (Abschnitt 1.13, 9 Container-Felder + InternalModelChoice)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_internal_models_toml() {
    let InternalModelsToml {
        use_openrouter_defaults,
        session_title,
        compaction_summary,
        memory_consolidation,
        dream_reflection,
        explorer,
        research,
        worker_simple,
        worker_complex,
    } = InternalModelsToml::default();
    let _ = (
        use_openrouter_defaults,
        session_title,
        compaction_summary,
        memory_consolidation,
        dream_reflection,
        explorer,
        research,
        worker_simple,
        worker_complex,
    );
    for path in [
        "internal_models.use_openrouter_defaults",
        "internal_models.session_title",
        "internal_models.compaction_summary",
        "internal_models.memory_consolidation",
        "internal_models.dream_reflection",
        "internal_models.explorer",
        "internal_models.research",
        "internal_models.worker_simple",
        "internal_models.worker_complex",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

#[test]
fn test_field_table_exhaustive_internal_model_choice() {
    let InternalModelChoice { provider, model } = InternalModelChoice::default();
    let _ = (provider, model);
    // Ein einziger FIELD_TABLE-Eintrag deckt bewusst beide Felder ab
    // (Abschnitt 6.3/1.13: "InternalModelChoice.provider/.model" reist als
    // atomarer CompositeMember jeder der acht Modellstellen).
    assert_path_in_field_table_exactly_once("InternalModelChoice.provider/.model");
}

// ---------------------------------------------------------------------
// [compaction] (Abschnitt 1.14, 1 Feld)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_compaction_toml() {
    let CompactionToml {
        absolute_ceiling_tokens,
    } = CompactionToml::default();
    let _ = absolute_ceiling_tokens;
    assert_path_in_field_table_exactly_once("compaction.absolute_ceiling_tokens");
}

// ---------------------------------------------------------------------
// [reasoning] (Abschnitt 1.15, 6 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_reasoning_weights_toml() {
    let ReasoningWeightsToml {
        uia,
        root_orchestrator,
        root_orchestrator_with_subs,
        sub_orchestrator,
        worker_complex,
        worker_simple,
    } = ReasoningWeightsToml::default();
    let _ = (
        uia,
        root_orchestrator,
        root_orchestrator_with_subs,
        sub_orchestrator,
        worker_complex,
        worker_simple,
    );
    for path in [
        "reasoning.uia",
        "reasoning.root_orchestrator",
        "reasoning.root_orchestrator_with_subs",
        "reasoning.sub_orchestrator",
        "reasoning.worker_complex",
        "reasoning.worker_simple",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [guards] (Abschnitt 1.16, 6 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_guards_toml() {
    let GuardsToml {
        enabled,
        repeated_failure_warn,
        repeated_failure_abort,
        no_progress_rounds_warn,
        no_progress_rounds_abort,
        plan_stale_rounds,
    } = GuardsToml::default();
    let _ = (
        enabled,
        repeated_failure_warn,
        repeated_failure_abort,
        no_progress_rounds_warn,
        no_progress_rounds_abort,
        plan_stale_rounds,
    );
    for path in [
        "guards.enabled",
        "guards.repeated_failure_warn",
        "guards.repeated_failure_abort",
        "guards.no_progress_rounds_warn",
        "guards.no_progress_rounds_abort",
        "guards.plan_stale_rounds",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}
