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
//! Die Laufzeit-Sanity-Checks "FIELD_TABLE.len() == 92" und "keine
//! doppelten Pfade" existieren bereits in `harw-config/src/scope.rs`
//! (`mod merge_rule_tests`, Paket A) und werden hier bewusst **nicht**
//! dupliziert — dieser Datei obliegt ausschließlich der destrukturierende
//! Exhaustivitäts-Mechanismus, den Abschnitt 7a der Spezifikation Paket C
//! zuordnet.
//!
//! Ausschließlich öffentliche API (`harw_config::*`) — keine internen
//! Test-Helfer aus `scope.rs`/`merge.rs`/`discovery.rs`.

use std::str::FromStr;

mod common;
use common::{TestResult, ctx};

use harw_config::harness_config::{
    CompactionToml, DiaryToml, DreamToml, GuardsToml, HostToml, KnowledgeToml, OnboardingSection,
    OnboardingSeen, ReasoningWeightsToml,
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
        uia_worker_models: _,
        compaction: _,
        reasoning: _,
        guards: _,
        knowledge: _,
        dream: _,
        host: _,
        // Runde 5, Teil K: `[agents]`, eigener Abschnittstest unten.
        agents: _,
        // Runde 5, Teil N: `[shell]`, eigener Abschnittstest unten.
        shell: _,
        // `[jobs]`, eigener Abschnittstest unten.
        jobs: _,
        // `[memory]`, eigener Abschnittstest unten.
        memory: _,
        // `[retention]`, eigener Abschnittstest unten.
        retention: _,
        // #22 Welle 2B: `[agent_compiler]`, eigener Abschnittstest unten.
        agent_compiler: _,
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
    for path in [
        "logging.level",
        "logging.target_module_paths",
        "logging.json",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [tui] (Abschnitt 1.3, 3 Felder; Runde 5, Teil I: +`child_stream`)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_tui_section() {
    let TuiSection {
        theme,
        keybindings_file,
        child_stream,
    } = TuiSection::default();
    let _ = (theme, keybindings_file, child_stream);
    for path in ["tui.theme", "tui.keybindings_file", "tui.child_stream"] {
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
    let _ = (
        store_dir,
        journal_format,
        retention_days,
        title_generation,
        title_model,
    );
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
    for path in [
        "policy.default_visibility_scope",
        "policy.require_approval_for",
    ] {
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
fn test_field_table_exhaustive_mcp_principal_toml() -> TestResult {
    let principal = McpPrincipalToml {
        id: "p1".to_owned(),
        credential_ref: SecretRef::from_str("env:P1_TOKEN")
            .map_err(ctx("env:P1_TOKEN secret ref"))?,
        tenant: "alice".to_owned(),
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
    Ok(())
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
    // `ToolsSection` hat die Felder `plan` und `doc` — keine eigene
    // FIELD_TABLE-Zeile fuer die Container selbst (nur fuer deren
    // Unterfelder, s. die folgenden Tests). Die Destrukturierung sichert
    // dennoch zu, dass ein kuenftiges zweites `[tools.*]`-Geschwisterfeld
    // (z. B. `[tools.search]`) hier einen Compile-Fehler ausloest.
    let ToolsSection { plan, doc } = ToolsSection::default();
    let _ = (plan, doc);
}

// [tools.doc] (Abschnitt 1.8a, 1 Feld)
#[test]
fn test_field_table_exhaustive_doc_section() {
    let harw_config::DocSection { remote_ocr } = harw_config::DocSection::default();
    let _ = remote_ocr;
    assert_path_in_field_table_exactly_once("tools.doc.remote_ocr");
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
        auto_classifier_timeout_secs,
        allow,
        deny,
        extra_roots,
    } = PermissionsSection::default();
    let _ = (
        default_mode,
        approval_timeout_secs,
        auto_classifier_timeout_secs,
        allow,
        deny,
        extra_roots,
    );
    for path in [
        "permissions.default_mode",
        "permissions.approval_timeout_secs",
        "permissions.auto_classifier_timeout_secs",
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
// [internal_models] (Abschnitt 1.13, 11 Container-Felder + InternalModelChoice)
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
        root_orchestrator,
        sub_orchestrator,
        auto_classifier,
        work_driver_judge,
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
        root_orchestrator,
        sub_orchestrator,
        auto_classifier,
        work_driver_judge,
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
        "internal_models.root_orchestrator",
        "internal_models.sub_orchestrator",
        // Runde 5, Teil E.
        "internal_models.auto_classifier",
        "internal_models.work_driver_judge",
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
    // atomarer CompositeMember jeder der zehn Modellstellen).
    assert_path_in_field_table_exactly_once("InternalModelChoice.provider/.model");
}

// ---------------------------------------------------------------------
// [compaction] (Abschnitt 1.14, 1 Feld)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_compaction_toml() {
    let CompactionToml {
        absolute_ceiling_tokens,
        max_history_bytes,
    } = CompactionToml::default();
    let _ = (absolute_ceiling_tokens, max_history_bytes);
    assert_path_in_field_table_exactly_once("compaction.absolute_ceiling_tokens");
    assert_path_in_field_table_exactly_once("compaction.max_history_bytes");
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
// [guards] (Abschnitt 1.16, 8 Felder)
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
        orchestrator_read_warn,
        orchestrator_read_limit,
    } = GuardsToml::default();
    let _ = (
        enabled,
        repeated_failure_warn,
        repeated_failure_abort,
        no_progress_rounds_warn,
        no_progress_rounds_abort,
        plan_stale_rounds,
        orchestrator_read_warn,
        orchestrator_read_limit,
    );
    for path in [
        "guards.enabled",
        "guards.repeated_failure_warn",
        "guards.repeated_failure_abort",
        "guards.no_progress_rounds_warn",
        "guards.no_progress_rounds_abort",
        "guards.plan_stale_rounds",
        "guards.orchestrator_read_warn",
        "guards.orchestrator_read_limit",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [knowledge] (Abschnitt 1.17, 1 Feld)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_knowledge_toml() {
    let KnowledgeToml { diary } = KnowledgeToml::default();
    let DiaryToml { retention_days } = diary;
    let _ = retention_days;
    assert_path_in_field_table_exactly_once("knowledge.diary.retention_days");
}

// ---------------------------------------------------------------------
// [dream] (Abschnitt 1.18, 5 Felder)
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_dream_toml() {
    let DreamToml {
        enabled,
        budget,
        idle_minutes,
        cooldown_minutes,
        schedule,
    } = DreamToml::default();
    let _ = (enabled, budget, idle_minutes, cooldown_minutes, schedule);
    for path in [
        "dream.enabled",
        "dream.budget",
        "dream.idle_minutes",
        "dream.cooldown_minutes",
        "dream.schedule",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [host] (Abschnitt 1.19, 1 Feld) — Runde 5, Teil B
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_host_toml() {
    let HostToml {
        sudo_session_minutes,
    } = HostToml::default();
    let _ = sudo_session_minutes;
    assert_path_in_field_table_exactly_once("host.sudo_session_minutes");
}

// ---------------------------------------------------------------------
// [agents] (Abschnitt 1.21, 4 Felder) — Runde 5, Teil K
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_agent_limits_toml() {
    let harw_config::AgentLimitsToml {
        max_root_orchestrators,
        max_sub_orchestrators,
        max_sub_orchestrator_depth,
        max_spawn_depth,
    } = harw_config::AgentLimitsToml::default();
    let _ = (
        max_root_orchestrators,
        max_sub_orchestrators,
        max_sub_orchestrator_depth,
        max_spawn_depth,
    );
    for path in [
        "agents.max_root_orchestrators",
        "agents.max_sub_orchestrators",
        "agents.max_sub_orchestrator_depth",
        "agents.max_spawn_depth",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [shell] (Abschnitt 1.22, 1 Feld) — Runde 5, Teil N
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_shell_toml() {
    let harw_config::ShellToml { max_timeout_secs } = harw_config::ShellToml::default();
    let _ = max_timeout_secs;
    assert_path_in_field_table_exactly_once("shell.max_timeout_secs");
}

// ---------------------------------------------------------------------
// [jobs] (1 Feld) — Höchstzahl laufender Hintergrund-Jobs
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_jobs_toml() {
    let harw_config::harness_config::JobsToml { max_running } =
        harw_config::harness_config::JobsToml::default();
    let _ = max_running;
    assert_path_in_field_table_exactly_once("jobs.max_running");
}

// ---------------------------------------------------------------------
// [memory] (11 Felder) — Projektgedächtnis
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_memory_section() {
    let harw_config::MemorySection {
        enabled,
        global_enabled,
        token_budget,
        max_facts,
        max_body_bytes,
        max_unused_days,
        consolidate_deadline_secs,
        forget_deadline_secs,
        promote_deadline_secs,
        sweep_deadline_secs,
        context_ledger,
        security_signals,
        llm_extraction,
    } = harw_config::MemorySection::default();
    let _ = (
        enabled,
        global_enabled,
        token_budget,
        max_facts,
        max_body_bytes,
        max_unused_days,
        consolidate_deadline_secs,
        forget_deadline_secs,
        promote_deadline_secs,
        sweep_deadline_secs,
        context_ledger,
        security_signals,
        llm_extraction,
    );
    for path in [
        "memory.enabled",
        "memory.global_enabled",
        "memory.token_budget",
        "memory.max_facts",
        "memory.max_body_bytes",
        "memory.max_unused_days",
        "memory.consolidate_deadline_secs",
        "memory.forget_deadline_secs",
        "memory.promote_deadline_secs",
        "memory.sweep_deadline_secs",
        "memory.context_ledger",
        "memory.security_signals",
        "memory.llm_extraction",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [retention] (je Klasse 5 Felder) — Aufbewahrung flüchtiger Daten
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_retention_section() {
    // Die Klassentabelle ist die einzige Quelle: jede deklarierte Klasse
    // braucht genau die fünf Pfade `retention.<klasse>.<schluessel>`.
    let harw_config::RetentionClassToml {
        enabled,
        max_age_secs,
        max_bytes,
        max_files,
        keep_newest,
    } = harw_config::RetentionClassToml::default();
    let _ = (enabled, max_age_secs, max_bytes, max_files, keep_newest);
    assert_eq!(harw_retention::CLASS_CONFIG_FIELDS.len(), 5);
    assert_eq!(harw_retention::CLASSES.len(), 10);
    for class in harw_retention::CLASSES {
        for field in harw_retention::CLASS_CONFIG_FIELDS {
            assert_path_in_field_table_exactly_once(&format!("retention.{}.{field}", class.id));
        }
    }
    let retention_paths = FIELD_TABLE
        .iter()
        .filter(|f| f.path.starts_with("retention."))
        .count();
    assert_eq!(
        retention_paths,
        harw_retention::CLASSES.len() * harw_retention::CLASS_CONFIG_FIELDS.len()
    );
}

// ---------------------------------------------------------------------
// [agent_compiler] (3 Felder) — #22 Welle 2B
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_agent_compiler_toml() {
    let harw_config::AgentCompilerToml {
        cache_max_bytes,
        keep_versions,
        auto_build_uia,
    } = harw_config::AgentCompilerToml::default();
    let _ = (cache_max_bytes, keep_versions, auto_build_uia);
    for path in [
        "agent_compiler.cache_max_bytes",
        "agent_compiler.keep_versions",
        "agent_compiler.auto_build_uia",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}

// ---------------------------------------------------------------------
// [uia_worker_models] (Abschnitt 1.20, 5 Felder) — Runde 5, Teil G
// ---------------------------------------------------------------------

#[test]
fn test_field_table_exhaustive_uia_worker_models_toml() {
    let harw_config::UiaWorkerModelsToml {
        uia_worker,
        uia_shell_worker,
        uia_writer,
        uia_latex_writer,
        uia_explorer,
    } = harw_config::UiaWorkerModelsToml::default();
    let _ = (
        uia_worker,
        uia_shell_worker,
        uia_writer,
        uia_latex_writer,
        uia_explorer,
    );
    for path in [
        "uia_worker_models.uia_worker",
        "uia_worker_models.uia_shell_worker",
        "uia_worker_models.uia_writer",
        "uia_worker_models.uia_latex_writer",
        "uia_worker_models.uia_explorer",
    ] {
        assert_path_in_field_table_exactly_once(path);
    }
}
