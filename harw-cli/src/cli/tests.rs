//! Parse-Tests der `harw`-Grammatik.
//!
//! Enthält die aus dem früheren `cli.rs` übernommenen Tests sowie Tests für
//! den neuen Befehlsbaum, die globalen und Sitzungs-Flags und alle
//! versteckten älteren Schreibweisen.

use std::path::PathBuf;

use clap::Parser;

use super::*;
use crate::test_support::{TestError, TestResult, ctx};

#[test]
fn connect_telegram_pairing_parses() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "connect",
        "--channel",
        "telegram",
        "--pair",
        "ABCD-EFGH",
    ])
    .map_err(ctx("Telegram connect must parse"))?;
    let Some(Command::Connect { channel, pair }) = cli.command else {
        return Err(TestError::Unexpected("expected connect command".into()));
    };
    assert_eq!(channel, Channel::Telegram);
    assert_eq!(pair.as_deref(), Some("ABCD-EFGH"));
    Ok(())
}

#[test]
fn resume_is_absent_without_the_flag() -> TestResult {
    let cli = Cli::try_parse_from(["harw"]).map_err(ctx("bare harw parses"))?;

    assert_eq!(cli.chat.resume, None);
    Ok(())
}

#[test]
fn resume_without_a_value_requests_interactive_selection() -> TestResult {
    let short = Cli::try_parse_from(["harw", "-r"]).map_err(ctx("short resume parses"))?;
    let long = Cli::try_parse_from(["harw", "--resume"]).map_err(ctx("long resume parses"))?;

    assert_eq!(short.chat.resume, Some(None));
    assert_eq!(long.chat.resume, Some(None));
    Ok(())
}

#[test]
fn resume_with_a_value_targets_that_session() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "--resume", "session-42"])
        .map_err(ctx("resume selector parses"))?;

    assert_eq!(cli.chat.resume, Some(Some("session-42".to_owned())));
    Ok(())
}

#[test]
fn resume_preserves_a_positional_prompt_when_explicitly_separated() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "-r", "--", "continue this"])
        .map_err(ctx("resume with prompt parses"))?;

    assert_eq!(cli.chat.resume, Some(None));
    assert_eq!(cli.chat.prompt.as_deref(), Some("continue this"));
    Ok(())
}

#[test]
fn resume_does_not_consume_a_subcommand_name() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "--resume", "doctor"])
        .map_err(ctx("resume before subcommand parses"))?;

    assert_eq!(cli.chat.resume, Some(None));
    assert!(matches!(cli.command, Some(Command::Doctor { .. })));
    Ok(())
}

#[test]
fn test_mode_flag_sets_the_requested_mode() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "--mode", "explore"])
        .map_err(ctx("`harw --mode explore` sollte parsen"))?;

    assert_eq!(cli.global.mode.as_deref(), Some("explore"));
    Ok(())
}

#[test]
fn test_mode_flag_rejects_an_unknown_value() {
    // `--mode` wird beim Parsen über `ModeParser` gegen
    // `harw_core::InteractionMode::parse` geprüft; ein unbekannter Modus ist
    // ein Parse-Fehler.
    let result = Cli::try_parse_from(["harw", "--mode", "bogus"]);

    assert!(
        result.is_err(),
        "ein unbekannter --mode-Wert muss scheitern, bekam {result:?}"
    );
}

#[test]
fn test_goal_flag_sets_the_goal_statement() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "--goal", "Alle Tests grün"])
        .map_err(ctx("`harw --goal ...` sollte parsen"))?;

    assert_eq!(cli.global.goal.as_deref(), Some("Alle Tests grün"));
    Ok(())
}

#[test]
fn test_analyze_without_arguments_defaults_to_bottom_up_whole_workspace() -> TestResult {
    let cli =
        Cli::try_parse_from(["harw", "analyze"]).map_err(ctx("`harw analyze` sollte parsen"))?;

    let Some(Command::Analyze(args)) = cli.command else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Analyze, bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args.crate_name, None);
    assert_eq!(args.order, AnalyzeOrder::BottomUp);
    assert!(!args.bottom_up);
    assert!(!args.top_down);
    assert_eq!(args.effective_order(), AnalyzeOrder::BottomUp);
    Ok(())
}

#[test]
fn test_analyze_with_crate_dry_run_and_max_parallel() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "analyze",
        "harw-plan",
        "--dry-run",
        "--max-parallel",
        "4",
    ])
    .map_err(ctx(
        "`harw analyze harw-plan --dry-run --max-parallel 4` sollte parsen",
    ))?;

    let Some(Command::Analyze(args)) = cli.command else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Analyze, bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args.crate_name.as_deref(), Some("harw-plan"));
    assert!(args.dry_run);
    assert_eq!(args.max_parallel, Some(4));
    Ok(())
}

#[test]
fn test_bottom_up_and_top_down_conflict_the_same_way_in_both_orders() -> TestResult {
    let forward = Cli::try_parse_from(["harw", "analyze", "--bottom-up", "--top-down"]);
    let backward = Cli::try_parse_from(["harw", "analyze", "--top-down", "--bottom-up"]);

    // Dokumentiertes Ergebnis: beide Flags gemeinsam sind ein Parse-Fehler,
    // unabhängig von der Reihenfolge — kein stiller Vorrang eines Flags.
    match (forward, backward) {
        (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind()),
        (a, b) => {
            return Err(TestError::Unexpected(format!(
                "beide Reihenfolgen müssen gleich scheitern, bekam {a:?} / {b:?}"
            )));
        }
    }
    Ok(())
}

#[test]
fn test_project_trust_subcommands_parse() -> TestResult {
    let trust = Cli::try_parse_from(["harw", "project", "trust", "/tmp/proj"])
        .map_err(ctx("`harw project trust /tmp/proj` sollte parsen"))?;
    let Some(Command::Project {
        action: ProjectAction::Trust { path },
    }) = trust.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Project(Trust), bekam {:?}",
            trust.command
        )));
    };
    assert_eq!(path, Some(PathBuf::from("/tmp/proj")));

    let untrust = Cli::try_parse_from(["harw", "project", "untrust"])
        .map_err(ctx("`harw project untrust` sollte parsen"))?;
    let Some(Command::Project {
        action: ProjectAction::Untrust { path },
    }) = untrust.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Project(Untrust), bekam {:?}",
            untrust.command
        )));
    };
    assert_eq!(path, None);

    let status = Cli::try_parse_from(["harw", "project", "status", "."])
        .map_err(ctx("`harw project status .` sollte parsen"))?;
    let Some(Command::Project {
        action: ProjectAction::Status { path },
    }) = status.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Project(Status), bekam {:?}",
            status.command
        )));
    };
    assert_eq!(path, Some(PathBuf::from(".")));
    Ok(())
}

#[test]
fn test_verbose_and_add_dir_flags_parse_and_repeat() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "--verbose",
        "--add-dir",
        "/tmp/a",
        "--add-dir",
        "/tmp/b",
    ])
    .map_err(ctx("`--verbose --add-dir ...` sollte parsen"))?;

    assert!(cli.global.verbose);
    assert_eq!(
        cli.global.add_dir,
        vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
    );
    Ok(())
}

#[test]
fn test_verbose_and_add_dir_default_to_empty() -> TestResult {
    let cli = Cli::try_parse_from(["harw"]).map_err(ctx("bare harw parses"))?;

    assert!(!cli.global.verbose);
    assert!(cli.global.add_dir.is_empty());
    Ok(())
}

#[test]
fn test_settings_without_action_parses_for_interactive_menu() -> TestResult {
    let cli =
        Cli::try_parse_from(["harw", "settings"]).map_err(ctx("`harw settings` sollte parsen"))?;

    assert!(matches!(
        cli.command,
        Some(Command::Config { action: None })
    ));
    Ok(())
}

#[test]
fn test_settings_provider_add_parses_all_flags() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "settings",
        "provider",
        "add",
        "test",
        "--api",
        "openai-chat",
        "--base-url",
        "http://localhost:1",
        "--auth",
        "env:X",
        "--models",
        "a,b",
    ])
    .map_err(ctx("`harw settings provider add ...` sollte parsen"))?;

    let Some(Command::Config {
        action:
            Some(SettingsAction::Provider {
                action:
                    SettingsProviderAction::Add {
                        name,
                        api,
                        base_url,
                        auth,
                        models,
                    },
            }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Config(Provider(Add)), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(name, "test");
    assert_eq!(api, ApiDialect::OpenaiChat);
    assert_eq!(api.as_str(), "openai-chat");
    assert_eq!(base_url, "http://localhost:1");
    assert_eq!(auth.as_deref(), Some("env:X"));
    assert_eq!(models, vec!["a".to_owned(), "b".to_owned()]);
    Ok(())
}

#[test]
fn test_settings_model_default_parses() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "settings", "model", "default", "gpt-5.4"])
        .map_err(ctx("`harw settings model default ...` sollte parsen"))?;

    let Some(Command::Config {
        action:
            Some(SettingsAction::Model {
                action: SettingsModelAction::Default { id },
            }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Config(Model(Default)), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(id, "gpt-5.4");
    Ok(())
}

#[test]
fn test_settings_get_set_default_to_global_scope() -> TestResult {
    let get = Cli::try_parse_from(["harw", "settings", "get", "default_model"])
        .map_err(ctx("`harw settings get ...` sollte parsen"))?;
    let Some(Command::Config {
        action: Some(SettingsAction::Get { key, scope }),
    }) = get.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Config(Get), bekam {:?}",
            get.command
        )));
    };
    assert_eq!(key, "default_model");
    assert!(!scope.global);
    assert!(!scope.project);

    let set = Cli::try_parse_from([
        "harw",
        "settings",
        "set",
        "default_model",
        "gpt-5.4",
        "--project",
    ])
    .map_err(ctx("`harw settings set ... --project` sollte parsen"))?;
    let Some(Command::Config {
        action: Some(SettingsAction::Set { key, value, scope }),
    }) = set.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Config(Set), bekam {:?}",
            set.command
        )));
    };
    assert_eq!(key, "default_model");
    assert_eq!(value.as_deref(), Some("gpt-5.4"));
    assert!(scope.project);
    assert!(!scope.global);
    Ok(())
}

#[test]
fn test_settings_scope_flags_conflict_in_both_orders() -> TestResult {
    let forward = Cli::try_parse_from([
        "harw",
        "settings",
        "get",
        "default_model",
        "--global",
        "--project",
    ]);
    let backward = Cli::try_parse_from([
        "harw",
        "settings",
        "get",
        "default_model",
        "--project",
        "--global",
    ]);

    match (forward, backward) {
        (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind()),
        (a, b) => {
            return Err(TestError::Unexpected(format!(
                "beide Reihenfolgen müssen gleich scheitern, bekam {a:?} / {b:?}"
            )));
        }
    }
    Ok(())
}

#[test]
fn test_settings_permissions_allow_and_deny_parse() -> TestResult {
    let allow = Cli::try_parse_from([
        "harw",
        "settings",
        "permissions",
        "allow",
        "shell.exec",
        "--pattern",
        "cargo check",
    ])
    .map_err(ctx("`harw settings permissions allow ...` sollte parsen"))?;
    assert!(matches!(
        allow.command,
        Some(Command::Config {
            action: Some(SettingsAction::Permissions {
                action: SettingsPermissionsAction::Allow { .. },
            }),
        })
    ));

    let deny = Cli::try_parse_from(["harw", "settings", "permissions", "deny", "fs.write"])
        .map_err(ctx("`harw settings permissions deny ...` sollte parsen"))?;
    assert!(matches!(
        deny.command,
        Some(Command::Config {
            action: Some(SettingsAction::Permissions {
                action: SettingsPermissionsAction::Deny { .. },
            }),
        })
    ));
    Ok(())
}

#[test]
fn test_models_without_action_parses_for_list() -> TestResult {
    let cli =
        Cli::try_parse_from(["harw", "models"]).map_err(ctx("`harw models` sollte parsen"))?;
    assert!(matches!(cli.command, Some(Command::Model { action: None })));
    Ok(())
}

#[test]
fn test_models_scan_parses_provider_and_flags() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "models",
        "scan",
        "openrouter",
        "--add",
        "--free-only",
    ])
    .map_err(ctx("`harw models scan ...` sollte parsen"))?;
    let Some(Command::Model {
        action:
            Some(ModelsAction::Scan {
                provider,
                add,
                free_only,
                prune,
            }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Model(Scan), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(provider.as_deref(), Some("openrouter"));
    assert!(add);
    assert!(free_only);
    assert!(!prune);
    Ok(())
}

#[test]
fn test_models_scan_without_provider_defaults_to_all() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "models", "scan"])
        .map_err(ctx("`harw models scan` sollte parsen"))?;
    let Some(Command::Model {
        action:
            Some(ModelsAction::Scan {
                provider,
                add,
                free_only,
                prune,
            }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Model(Scan), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(provider, None);
    assert!(!add);
    assert!(!free_only);
    assert!(!prune);
    Ok(())
}

#[test]
fn test_models_scan_parses_prune_flag() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "models", "scan", "--prune"])
        .map_err(ctx("`harw models scan --prune` sollte parsen"))?;
    let Some(Command::Model {
        action: Some(ModelsAction::Scan { prune, .. }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Model(Scan), bekam {:?}",
            cli.command
        )));
    };
    assert!(prune);
    Ok(())
}

#[test]
fn test_models_add_and_delete_parse_targets_and_picker_mode() -> TestResult {
    let cli =
        Cli::try_parse_from(["harw", "models", "add"]).map_err(ctx("picker mode should parse"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Model {
            action: Some(ModelsAction::Add { target: None })
        })
    ));

    let cli = Cli::try_parse_from(["harw", "models", "add", "mistral/mistral-medium-2604"])
        .map_err(ctx("add target should parse"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Model {
            action: Some(ModelsAction::Add { target: Some(target) })
        }) if target == "mistral/mistral-medium-2604"
    ));

    let cli = Cli::try_parse_from(["harw", "models", "delete", "openrouter/meta-llama/x"])
        .map_err(ctx("delete target should parse"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Model {
            action: Some(ModelsAction::Remove { target })
        }) if target == "openrouter/meta-llama/x"
    ));
    Ok(())
}

#[test]
fn test_models_internal_show_parses() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "models", "internal"])
        .map_err(ctx("`harw models internal` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Model {
            action: Some(ModelsAction::Internal { action: None }),
        })
    ));

    let cli = Cli::try_parse_from(["harw", "models", "internal", "show"])
        .map_err(ctx("`harw models internal show` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Model {
            action: Some(ModelsAction::Internal {
                action: Some(InternalAction::Show),
            }),
        })
    ));
    Ok(())
}

#[test]
fn test_models_internal_set_parses_point_model_and_provider() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "models",
        "internal",
        "set",
        "session-title",
        "nvidia/nemotron-3.5-lightning",
        "--provider",
        "openrouter",
    ])
    .map_err(ctx("`harw models internal set ...` sollte parsen"))?;
    let Some(Command::Model {
        action:
            Some(ModelsAction::Internal {
                action:
                    Some(InternalAction::Set {
                        point,
                        model,
                        provider,
                    }),
            }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Model(Internal(Set)), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(point, "session-title");
    assert_eq!(model, "nvidia/nemotron-3.5-lightning");
    assert_eq!(provider.as_deref(), Some("openrouter"));
    Ok(())
}

#[test]
fn test_models_internal_main_and_reset_parse() -> TestResult {
    let main = Cli::try_parse_from(["harw", "models", "internal", "main", "explorer"])
        .map_err(ctx("`harw models internal main ...` sollte parsen"))?;
    assert!(matches!(
        main.command,
        Some(Command::Model {
            action: Some(ModelsAction::Internal {
                action: Some(InternalAction::Main { .. }),
            }),
        })
    ));

    let reset = Cli::try_parse_from(["harw", "models", "internal", "reset", "research"])
        .map_err(ctx("`harw models internal reset ...` sollte parsen"))?;
    assert!(matches!(
        reset.command,
        Some(Command::Model {
            action: Some(ModelsAction::Internal {
                action: Some(InternalAction::Reset { .. }),
            }),
        })
    ));
    Ok(())
}

#[test]
fn test_models_internal_openrouter_defaults_accepts_on_off_only() -> TestResult {
    let on = Cli::try_parse_from(["harw", "models", "internal", "openrouter-defaults", "on"])
        .map_err(ctx("`on` sollte parsen"))?;
    assert!(matches!(
        on.command,
        Some(Command::Model {
            action: Some(ModelsAction::Internal {
                action: Some(InternalAction::OpenrouterDefaults { state: OnOff::On }),
            }),
        })
    ));

    let invalid =
        Cli::try_parse_from(["harw", "models", "internal", "openrouter-defaults", "maybe"]);
    assert!(invalid.is_err(), "ungültiger Zustand muss scheitern");
    Ok(())
}

#[test]
fn test_models_default_parses() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "models", "default", "gpt-5.4"])
        .map_err(ctx("`harw models default ...` sollte parsen"))?;
    let Some(Command::Model {
        action: Some(ModelsAction::Default { id }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Model(Default), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(id, "gpt-5.4");
    Ok(())
}

#[test]
fn test_web_rejects_config_dir_flag() {
    let result = Cli::try_parse_from(["harw", "web", "--config-dir", "/tmp/cfg"]);

    assert!(
        result.is_err(),
        "`harw web --config-dir` darf nicht mehr parsen, bekam {result:?}"
    );
}

#[test]
fn test_cloudflare_mcp_setup_and_check_parse() -> TestResult {
    let setup = Cli::try_parse_from(["harw", "mcp", "setup", "cloudflare"])
        .map_err(ctx("Cloudflare MCP setup muss parsen"))?;
    assert!(matches!(
        setup.command,
        Some(Command::Mcp {
            action: McpAction::Setup {
                server: McpServer::Cloudflare
            }
        })
    ));

    let check = Cli::try_parse_from(["harw", "mcp", "check"])
        .map_err(ctx("Cloudflare MCP check muss parsen"))?;
    assert!(matches!(
        check.command,
        Some(Command::Mcp {
            action: McpAction::Check {
                server: McpServer::Cloudflare
            }
        })
    ));
    Ok(())
}

#[test]
fn test_lens_build_parses_with_source_and_force() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "lens", "build", "--source", "docs", "--force"])
        .map_err(ctx("`harw lens build --source docs --force` muss parsen"))?;
    let Some(Command::Lens {
        action: Some(LensAction::Build { source, force }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Lens(Build), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(source, Some(LensSource::Docs));
    assert!(force);
    Ok(())
}

#[test]
fn test_lens_build_without_flags_defaults_source_to_none_and_force_to_false() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "lens", "build"])
        .map_err(ctx("`harw lens build` muss parsen"))?;
    let Some(Command::Lens {
        action: Some(LensAction::Build { source, force }),
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Lens(Build), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(source, None);
    assert!(!force);
    Ok(())
}

#[test]
fn test_lens_status_parses() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "lens", "status"])
        .map_err(ctx("`harw lens status` muss parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Lens {
            action: Some(LensAction::Status)
        })
    ));
    Ok(())
}

#[test]
fn test_lens_without_subcommand_parses_with_no_action() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "lens"]).map_err(ctx("`harw lens` muss parsen"))?;
    assert!(matches!(cli.command, Some(Command::Lens { action: None })));
    Ok(())
}
