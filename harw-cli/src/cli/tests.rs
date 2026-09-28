//! Parse-Tests der `harw`-Grammatik.
//!
//! Enthält die aus dem früheren `cli.rs` übernommenen Tests sowie Tests für
//! den neuen Befehlsbaum, die globalen und Sitzungs-Flags und alle
//! versteckten älteren Schreibweisen.

use std::path::PathBuf;

use clap::Parser;
use harw_extension_api::ApprovalMode;

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
                        ..
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

// ── harw web: --system / --systemd-socket / --socket-group (H9) ─────────

/// Zerlegt `harw web …` in (socket, system, systemd_socket, socket_group).
type WebFlags = (Option<PathBuf>, bool, bool, Option<String>);

#[test]
fn web_tailnet_flag_parses_with_default_and_explicit_port() -> TestResult {
    for (args, port) in [
        (&["harw", "web", "--tailnet"][..], 8443),
        (
            &["harw", "web", "--tailnet", "--tailnet-port", "9443"][..],
            9443,
        ),
    ] {
        let cli = Cli::try_parse_from(args).map_err(ctx("`harw web --tailnet` sollte parsen"))?;
        match cli.command {
            Some(Command::Web {
                tailnet,
                tailnet_port,
                ..
            }) => {
                assert!(tailnet);
                assert_eq!(tailnet_port, port);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete web, bekam {other:?}"
                )));
            }
        }
    }
    assert!(
        Cli::try_parse_from(["harw", "web", "--tailnet-port", "9443"]).is_err(),
        "--tailnet-port ohne --tailnet muss abgelehnt werden"
    );
    Ok(())
}

fn parse_web(args: &[&str]) -> TestResult<WebFlags> {
    let cli = Cli::try_parse_from(args).map_err(ctx("`harw web …` sollte parsen"))?;
    match cli.command {
        Some(Command::Web {
            socket,
            system,
            systemd_socket,
            socket_group,
            ..
        }) => Ok((socket, system, systemd_socket, socket_group)),
        other => Err(TestError::Unexpected(format!(
            "erwartete web, bekam {other:?}"
        ))),
    }
}

#[test]
fn test_web_without_flags_is_the_dev_fallback() -> TestResult {
    assert_eq!(parse_web(&["harw", "web"])?, (None, false, false, None));
    Ok(())
}

#[test]
fn test_web_system_flags_parse() -> TestResult {
    assert_eq!(
        parse_web(&["harw", "web", "--system"])?,
        (None, true, false, None)
    );
    assert_eq!(
        parse_web(&[
            "harw",
            "web",
            "--system",
            "--socket",
            "/run/harw/infra/control.sock",
            "--socket-group",
            "harw-control",
        ])?,
        (
            Some(PathBuf::from("/run/harw/infra/control.sock")),
            true,
            false,
            Some("harw-control".to_owned())
        )
    );
    assert_eq!(
        parse_web(&["harw", "web", "--systemd-socket"])?,
        (None, false, true, None)
    );
    assert_eq!(
        parse_web(&["harw", "web", "--system", "--systemd-socket"])?,
        (None, true, true, None)
    );
    Ok(())
}

#[test]
fn test_web_rejects_contradicting_listener_flags() {
    for args in [
        &["harw", "web", "--systemd-socket", "--socket", "/tmp/x.sock"][..],
        &[
            "harw",
            "web",
            "--system",
            "--systemd-socket",
            "--socket-group",
            "g",
        ][..],
        &["harw", "web", "--socket-group", "g"][..],
    ] {
        let result = Cli::try_parse_from(args);
        assert!(
            result.is_err(),
            "{args:?} darf nicht parsen, bekam {result:?}"
        );
    }
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

// ── Gesamte Grammatik ───────────────────────────────────────────────────

#[test]
fn test_command_passes_clap_debug_assertions() {
    // Prüft u. a. doppelte Argument-IDs, Kurzflags und Konflikt-Verweise im
    // gesamten Baum, auch in versteckten Varianten.
    command().debug_assert();
}

#[test]
fn test_command_parses_the_same_as_the_derived_parser() -> TestResult {
    let matches = command()
        .try_get_matches_from(["harw", "--json", "session", "list", "--all"])
        .map_err(ctx("`command()` muss `session list --all` parsen"))?;
    assert!(matches.get_flag("json"));
    let Some(("session", session)) = matches.subcommand() else {
        return Err(TestError::Unexpected(format!(
            "erwartete Subcommand `session`, bekam {:?}",
            matches.subcommand_name()
        )));
    };
    assert_eq!(session.subcommand_name(), Some("list"));
    Ok(())
}

#[test]
fn test_legacy_commands_are_hidden_but_still_known() -> TestResult {
    let cmd = command();
    for name in ["connect", "lens", "uia", "catalog", "run", "classify"] {
        let sub = cmd
            .find_subcommand(name)
            .ok_or_else(|| TestError::Unexpected(format!("`{name}` fehlt in der Grammatik")))?;
        assert!(sub.is_hide_set(), "`{name}` muss versteckt sein");
    }
    for name in [
        "chat",
        "exec",
        "session",
        "config",
        "provider",
        "model",
        "agent",
        "knowledge",
        "jobs",
        "channel",
        "debug",
    ] {
        let sub = cmd
            .find_subcommand(name)
            .ok_or_else(|| TestError::Unexpected(format!("`{name}` fehlt in der Grammatik")))?;
        assert!(!sub.is_hide_set(), "`{name}` muss sichtbar sein");
    }
    Ok(())
}

// ── Arbeiten: chat, exec, session ───────────────────────────────────────

#[test]
fn test_bare_harw_starts_chat_without_subcommand() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "hallo"]).map_err(ctx("`harw hallo` sollte parsen"))?;
    assert!(cli.command.is_none());
    assert_eq!(cli.chat.prompt.as_deref(), Some("hallo"));
    assert!(!cli.chat.all);
    Ok(())
}

#[test]
fn test_root_all_flag_combines_with_resume() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "-r", "--all"])
        .map_err(ctx("`harw -r --all` sollte parsen"))?;
    assert_eq!(cli.chat.resume, Some(None));
    assert!(cli.chat.all);
    Ok(())
}

#[test]
fn test_chat_subcommand_parses_prompt_resume_and_all() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "chat", "--all", "-r", "abc123"])
        .map_err(ctx("`harw chat --all -r abc123` sollte parsen"))?;
    let Some(Command::Chat(args)) = cli.command else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Chat, bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args.resume, Some(Some("abc123".to_owned())));
    assert!(args.all);
    assert_eq!(args.prompt, None);

    let cli = Cli::try_parse_from(["harw", "chat", "erkläre das Projekt"])
        .map_err(ctx("`harw chat PROMPT` sollte parsen"))?;
    let Some(Command::Chat(args)) = cli.command else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Chat, bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args.prompt.as_deref(), Some("erkläre das Projekt"));
    assert_eq!(args.resume, None);
    Ok(())
}

#[test]
fn test_all_flag_is_not_global() {
    let result = Cli::try_parse_from(["harw", "doctor", "--all"]);
    assert!(
        result.is_err(),
        "`--all` gehört nur zu chat bzw. `session list`, bekam {result:?}"
    );
}

#[test]
fn test_exec_collects_all_prompt_words() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "exec", "fix", "the", "tests"])
        .map_err(ctx("`harw exec fix the tests` sollte parsen"))?;
    let Some(Command::Exec(args)) = cli.command else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Exec, bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args.prompt, vec!["fix", "the", "tests"]);
    Ok(())
}

#[test]
fn test_exec_requires_a_prompt() {
    let result = Cli::try_parse_from(["harw", "exec"]);
    assert!(
        result.is_err(),
        "`harw exec` ohne Prompt muss scheitern, bekam {result:?}"
    );
}

#[test]
fn test_exec_accepts_session_flags_before_the_subcommand() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "--approval",
        "full",
        "--model",
        "openrouter/x",
        "exec",
        "los",
    ])
    .map_err(ctx("Sitzungs-Flags vor `exec` sollten parsen"))?;
    assert_eq!(cli.global.approval, Some(ApprovalMode::FullAccess));
    assert_eq!(cli.global.model.as_deref(), Some("openrouter/x"));
    assert!(matches!(cli.command, Some(Command::Exec(_))));
    Ok(())
}

#[test]
fn test_session_list_show_and_resume_parse() -> TestResult {
    let list = Cli::try_parse_from(["harw", "session", "list", "--all"])
        .map_err(ctx("`harw session list --all` sollte parsen"))?;
    assert!(matches!(
        list.command,
        Some(Command::Session {
            action: SessionAction::List { all: true }
        })
    ));

    let list = Cli::try_parse_from(["harw", "session", "list"])
        .map_err(ctx("`harw session list` sollte parsen"))?;
    assert!(matches!(
        list.command,
        Some(Command::Session {
            action: SessionAction::List { all: false }
        })
    ));

    let show = Cli::try_parse_from(["harw", "session", "show", "abc"])
        .map_err(ctx("`harw session show abc` sollte parsen"))?;
    assert!(matches!(
        show.command,
        Some(Command::Session {
            action: SessionAction::Show { id }
        }) if id == "abc"
    ));

    let resume = Cli::try_parse_from(["harw", "session", "resume", "abc"])
        .map_err(ctx("`harw session resume abc` sollte parsen"))?;
    assert!(matches!(
        resume.command,
        Some(Command::Session {
            action: SessionAction::Resume { id }
        }) if id == "abc"
    ));

    let missing = Cli::try_parse_from(["harw", "session", "show"]);
    assert!(missing.is_err(), "`session show` ohne ID muss scheitern");
    Ok(())
}

// ── Konfiguration: config, provider, model ──────────────────────────────

#[test]
fn test_config_is_the_canonical_name_of_settings() -> TestResult {
    let cli =
        Cli::try_parse_from(["harw", "config"]).map_err(ctx("`harw config` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Config { action: None })
    ));

    let cli = Cli::try_parse_from(["harw", "config", "get", "default_model"])
        .map_err(ctx("`harw config get ...` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Config {
            action: Some(SettingsAction::Get { key, .. })
        }) if key == "default_model"
    ));
    Ok(())
}

#[test]
fn test_config_permissions_set_mode_does_not_leak_into_global_flags() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "config", "permissions", "set-mode", "full"])
        .map_err(ctx("`harw config permissions set-mode full` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Config {
            action: Some(SettingsAction::Permissions {
                action: SettingsPermissionsAction::SetMode { .. },
            }),
        })
    ));
    assert_eq!(cli.global.mode, None);
    assert_eq!(cli.global.approval, None);
    assert!(cli.global.session_flags_used().is_empty());
    Ok(())
}

#[test]
fn test_provider_add_parses_all_flags() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "provider",
        "add",
        "lokal",
        "--api",
        "ollama",
        "--base-url",
        "http://localhost:11434",
        "--models",
        "a,b",
    ])
    .map_err(ctx("`harw provider add ...` sollte parsen"))?;
    let Some(Command::Provider {
        action:
            ProviderAction::Add {
                name,
                api,
                base_url,
                auth,
                models,
                no_auth,
                ..
            },
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Provider(Add), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(name, "lokal");
    assert_eq!(api, ApiDialect::Ollama);
    assert_eq!(base_url, "http://localhost:11434");
    assert_eq!(auth, None);
    assert_eq!(models, vec!["a".to_owned(), "b".to_owned()]);
    assert!(!no_auth);
    Ok(())
}

/// Runde 7, Teil L1: `--no-auth` und `--auth-header` parsen; `--no-auth`
/// schließt `--auth` aus.
#[test]
fn test_provider_add_parses_no_auth_and_auth_header() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "provider",
        "add",
        "vllm",
        "--api",
        "openai-chat",
        "--base-url",
        "http://localhost:8000/v1",
        "--no-auth",
    ])
    .map_err(ctx("`harw provider add --no-auth` sollte parsen"))?;
    let Some(Command::Provider {
        action:
            ProviderAction::Add {
                no_auth,
                auth_header,
                ..
            },
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Provider(Add), bekam {:?}",
            cli.command
        )));
    };
    assert!(no_auth);
    assert_eq!(auth_header, None);

    let cli = Cli::try_parse_from([
        "harw",
        "provider",
        "add",
        "gw",
        "--api",
        "openai-chat",
        "--base-url",
        "https://gw.example/v1",
        "--auth",
        "env:GW_KEY",
        "--auth-header",
        "x-api-key",
    ])
    .map_err(ctx("`--auth-header x-api-key` sollte parsen"))?;
    let Some(Command::Provider {
        action: ProviderAction::Add { auth_header, .. },
    }) = cli.command
    else {
        return Err(TestError::Unexpected("erwartete Provider(Add)".to_owned()));
    };
    assert_eq!(auth_header.as_deref(), Some("x-api-key"));

    for conflicting in [
        vec!["--no-auth", "--auth", "env:X"],
        vec!["--no-auth", "--auth-header", "bearer"],
        vec!["--auth-header", "basic"],
    ] {
        let mut args = vec![
            "harw",
            "provider",
            "add",
            "x",
            "--api",
            "openai-chat",
            "--base-url",
            "http://localhost:1/v1",
        ];
        args.extend(conflicting.iter().copied());
        assert!(Cli::try_parse_from(args).is_err(), "{conflicting:?}");
    }
    Ok(())
}

#[test]
fn test_provider_scan_list_and_toggles_parse() -> TestResult {
    let scan = Cli::try_parse_from([
        "harw",
        "provider",
        "scan",
        "openrouter",
        "--free-only",
        "--prune",
    ])
    .map_err(ctx("`harw provider scan ...` sollte parsen"))?;
    let Some(Command::Provider {
        action:
            ProviderAction::Scan {
                provider,
                free_only,
                prune,
            },
    }) = scan.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Provider(Scan), bekam {:?}",
            scan.command
        )));
    };
    assert_eq!(provider.as_deref(), Some("openrouter"));
    assert!(free_only);
    assert!(prune);

    let scan_all = Cli::try_parse_from(["harw", "provider", "scan"])
        .map_err(ctx("`harw provider scan` sollte parsen"))?;
    assert!(matches!(
        scan_all.command,
        Some(Command::Provider {
            action: ProviderAction::Scan {
                provider: None,
                free_only: false,
                prune: false,
            }
        })
    ));

    let list = Cli::try_parse_from(["harw", "provider", "list"])
        .map_err(ctx("`harw provider list` sollte parsen"))?;
    assert!(matches!(
        list.command,
        Some(Command::Provider {
            action: ProviderAction::List
        })
    ));

    for (verb, check) in [("remove", 0_u8), ("enable", 1), ("disable", 2)] {
        let cli = Cli::try_parse_from(["harw", "provider", verb, "x"])
            .map_err(ctx("`harw provider remove|enable|disable x` sollte parsen"))?;
        let ok = match (check, &cli.command) {
            (
                0,
                Some(Command::Provider {
                    action: ProviderAction::Remove { name },
                }),
            )
            | (
                1,
                Some(Command::Provider {
                    action: ProviderAction::Enable { name },
                }),
            )
            | (
                2,
                Some(Command::Provider {
                    action: ProviderAction::Disable { name },
                }),
            ) => name == "x",
            _ => false,
        };
        assert!(ok, "`provider {verb} x` falsch geparst: {:?}", cli.command);
    }
    Ok(())
}

#[test]
fn test_model_catalog_refresh_and_remove_parse() -> TestResult {
    let catalog = Cli::try_parse_from(["harw", "model", "catalog", "--refresh"])
        .map_err(ctx("`harw model catalog --refresh` sollte parsen"))?;
    assert!(matches!(
        catalog.command,
        Some(Command::Model {
            action: Some(ModelsAction::Catalog { refresh: true })
        })
    ));

    let catalog = Cli::try_parse_from(["harw", "model", "catalog"])
        .map_err(ctx("`harw model catalog` sollte parsen"))?;
    assert!(matches!(
        catalog.command,
        Some(Command::Model {
            action: Some(ModelsAction::Catalog { refresh: false })
        })
    ));

    let remove = Cli::try_parse_from(["harw", "model", "remove", "openrouter/x"])
        .map_err(ctx("`harw model remove ...` sollte parsen"))?;
    assert!(matches!(
        remove.command,
        Some(Command::Model {
            action: Some(ModelsAction::Remove { target })
        }) if target == "openrouter/x"
    ));

    let bare = Cli::try_parse_from(["harw", "model"]).map_err(ctx("`harw model` sollte parsen"))?;
    assert!(matches!(
        bare.command,
        Some(Command::Model { action: None })
    ));
    Ok(())
}

#[test]
fn test_model_internal_set_positional_model_does_not_set_global_model() -> TestResult {
    // Das positionale `model` von `internal set` und das globale `--model`
    // tragen verschiedene IDs; der Wert darf nicht nach oben durchsickern.
    let cli = Cli::try_parse_from(["harw", "model", "internal", "set", "explorer", "some/model"])
        .map_err(ctx("`harw model internal set ...` sollte parsen"))?;
    assert_eq!(cli.global.model, None);
    assert!(matches!(
        cli.command,
        Some(Command::Model {
            action: Some(ModelsAction::Internal {
                action: Some(InternalAction::Set { model, .. }),
            }),
        }) if model == "some/model"
    ));
    Ok(())
}

// ── Agenten und Wissen: agent, knowledge, jobs ──────────────────────────

#[test]
fn test_agent_skills_and_plugins_pass_hyphen_arguments_through() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw", "agent", "skills", "install", "--force", "-x", "pfad",
    ])
    .map_err(ctx(
        "`harw agent skills install --force -x pfad` sollte parsen",
    ))?;
    let Some(Command::Agent {
        action: AgentAction::Skills { args },
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Agent(Skills), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args, vec!["install", "--force", "-x", "pfad"]);

    let cli = Cli::try_parse_from(["harw", "agent", "skills"])
        .map_err(ctx("`harw agent skills` ohne Argumente sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Agent {
            action: AgentAction::Skills { args }
        }) if args.is_empty()
    ));

    let cli = Cli::try_parse_from(["harw", "--json", "agent", "plugins", "list"])
        .map_err(ctx("`harw --json agent plugins list` sollte parsen"))?;
    assert!(cli.global.json);
    assert!(matches!(
        cli.command,
        Some(Command::Agent {
            action: AgentAction::Plugins { args }
        }) if args == vec!["list".to_owned()]
    ));

    // Plan R9, Teil C: `harw agent list [SUCHE]` zeigt den Agenten-Roster.
    let cli = Cli::try_parse_from(["harw", "agent", "list", "analyse"])
        .map_err(ctx("`harw agent list analyse` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Agent {
            action: AgentAction::List { query }
        }) if query.as_deref() == Some("analyse")
    ));

    let cli = Cli::try_parse_from(["harw", "agent", "uia-new"])
        .map_err(ctx("`harw agent uia-new` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Agent {
            action: AgentAction::UiaNew
        })
    ));
    Ok(())
}

#[test]
fn test_knowledge_memory_proposals_and_index_parse() -> TestResult {
    let memory = Cli::try_parse_from(["harw", "knowledge", "memory", "search", "rust --tief"])
        .map_err(ctx("`harw knowledge memory search ...` sollte parsen"))?;
    assert!(matches!(
        memory.command,
        Some(Command::Knowledge {
            action: KnowledgeAction::Memory { args }
        }) if args == vec!["search".to_owned(), "rust --tief".to_owned()]
    ));

    let proposals = Cli::try_parse_from(["harw", "knowledge", "proposals", "list"])
        .map_err(ctx("`harw knowledge proposals list` sollte parsen"))?;
    assert!(matches!(
        proposals.command,
        Some(Command::Knowledge {
            action: KnowledgeAction::Proposals { args }
        }) if args == vec!["list".to_owned()]
    ));

    let index = Cli::try_parse_from(["harw", "knowledge", "index"])
        .map_err(ctx("`harw knowledge index` sollte parsen"))?;
    assert!(matches!(
        index.command,
        Some(Command::Knowledge {
            action: KnowledgeAction::Index { action: None }
        })
    ));

    let build = Cli::try_parse_from([
        "harw",
        "knowledge",
        "index",
        "build",
        "--source",
        "knowledge",
    ])
    .map_err(ctx(
        "`harw knowledge index build --source knowledge` sollte parsen",
    ))?;
    assert!(matches!(
        build.command,
        Some(Command::Knowledge {
            action: KnowledgeAction::Index {
                action: Some(LensAction::Build {
                    source: Some(LensSource::Knowledge),
                    force: false,
                })
            }
        })
    ));
    Ok(())
}

#[test]
fn test_jobs_approve_note_and_deny_reason_parse() -> TestResult {
    let approve = Cli::try_parse_from(["harw", "jobs", "approve", "job-1", "--note", "passt"])
        .map_err(ctx("`harw jobs approve job-1 --note passt` sollte parsen"))?;
    let Some(Command::Jobs {
        action: JobsAction::Approve { id, note },
    }) = approve.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Jobs(Approve), bekam {:?}",
            approve.command
        )));
    };
    assert_eq!(id, "job-1");
    assert_eq!(note.as_deref(), Some("passt"));

    let deny = Cli::try_parse_from(["harw", "jobs", "deny", "job-2", "--reason", "zu riskant"])
        .map_err(ctx("`harw jobs deny job-2 --reason ...` sollte parsen"))?;
    let Some(Command::Jobs {
        action: JobsAction::Deny { id, reason },
    }) = deny.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Jobs(Deny), bekam {:?}",
            deny.command
        )));
    };
    assert_eq!(id, "job-2");
    assert_eq!(reason.as_deref(), Some("zu riskant"));

    let approve_plain = Cli::try_parse_from(["harw", "jobs", "approve", "job-3"])
        .map_err(ctx("`harw jobs approve job-3` sollte parsen"))?;
    assert!(matches!(
        approve_plain.command,
        Some(Command::Jobs {
            action: JobsAction::Approve { note: None, .. }
        })
    ));
    Ok(())
}

#[test]
fn test_jobs_list_show_cancel_retry_parse() -> TestResult {
    let list = Cli::try_parse_from(["harw", "jobs", "list"])
        .map_err(ctx("`harw jobs list` sollte parsen"))?;
    assert!(matches!(
        list.command,
        Some(Command::Jobs {
            action: JobsAction::List { filter: None, .. }
        })
    ));

    let filtered = Cli::try_parse_from(["harw", "jobs", "list", "offen"])
        .map_err(ctx("`harw jobs list offen` sollte parsen"))?;
    assert!(matches!(
        filtered.command,
        Some(Command::Jobs {
            action: JobsAction::List { filter: Some(f), .. }
        }) if f == "offen"
    ));

    for verb in ["show", "cancel", "retry"] {
        let cli = Cli::try_parse_from(["harw", "jobs", verb, "job-9"])
            .map_err(ctx("`harw jobs show|cancel|retry ID` sollte parsen"))?;
        let ok = matches!(
            &cli.command,
            Some(Command::Jobs {
                action: JobsAction::Show { id } | JobsAction::Cancel { id } | JobsAction::Retry { id }
            }) if id == "job-9"
        );
        assert!(ok, "`jobs {verb} job-9` falsch geparst: {:?}", cli.command);
    }
    Ok(())
}

// ── Dienste und System: channel, debug, sandbox ─────────────────────────

#[test]
fn test_channel_connect_telegram_with_pairing_code_parses() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "channel",
        "connect",
        "telegram",
        "--pair",
        "ABCD-EFGH",
    ])
    .map_err(ctx(
        "`harw channel connect telegram --pair ...` sollte parsen",
    ))?;
    let Some(Command::Channel {
        action: ChannelAction::Connect { channel, pair },
    }) = cli.command
    else {
        return Err(TestError::Unexpected(format!(
            "erwartete Channel(Connect), bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(channel, Channel::Telegram);
    assert_eq!(pair.as_deref(), Some("ABCD-EFGH"));

    let misspelled = Cli::try_parse_from(["harw", "channel", "connect", "telegaram"]);
    assert!(misspelled.is_err(), "`telegaram` muss abgelehnt werden");
    Ok(())
}

#[test]
fn test_debug_echo_and_classify_parse() -> TestResult {
    let echo = Cli::try_parse_from(["harw", "debug", "echo", "hallo", "welt"])
        .map_err(ctx("`harw debug echo hallo welt` sollte parsen"))?;
    assert!(matches!(
        echo.command,
        Some(Command::Debug {
            action: DebugAction::Echo { input }
        }) if input == vec!["hallo".to_owned(), "welt".to_owned()]
    ));

    let classify = Cli::try_parse_from(["harw", "debug", "classify", "was", "ist", "das"])
        .map_err(ctx("`harw debug classify ...` sollte parsen"))?;
    assert!(matches!(
        classify.command,
        Some(Command::Debug {
            action: DebugAction::Classify { input }
        }) if input.len() == 3
    ));

    let empty = Cli::try_parse_from(["harw", "debug", "echo"]);
    assert!(empty.is_err(), "`debug echo` ohne Eingabe muss scheitern");
    Ok(())
}

#[test]
fn test_sandbox_without_action_and_with_status_parse() -> TestResult {
    let bare =
        Cli::try_parse_from(["harw", "sandbox"]).map_err(ctx("`harw sandbox` sollte parsen"))?;
    assert!(matches!(
        bare.command,
        Some(Command::Sandbox { action: None })
    ));

    let status = Cli::try_parse_from(["harw", "sandbox", "status"])
        .map_err(ctx("`harw sandbox status` sollte parsen"))?;
    assert!(matches!(
        status.command,
        Some(Command::Sandbox {
            action: Some(SandboxAction::Status)
        })
    ));
    Ok(())
}

// ── Versteckte ältere Schreibweisen ─────────────────────────────────────

#[test]
fn test_legacy_settings_provider_add_maps_to_config() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "settings",
        "provider",
        "add",
        "x",
        "--api",
        "openai-chat",
        "--base-url",
        "http://localhost:1",
    ])
    .map_err(ctx("`harw settings provider add ...` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Config {
            action: Some(SettingsAction::Provider {
                action: SettingsProviderAction::Add { .. }
            })
        })
    ));
    Ok(())
}

#[test]
fn test_legacy_models_delete_maps_to_model_remove() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "models", "delete", "a/b"])
        .map_err(ctx("`harw models delete a/b` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Model {
            action: Some(ModelsAction::Remove { target })
        }) if target == "a/b"
    ));
    Ok(())
}

#[test]
fn test_legacy_connect_without_pair_parses() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "connect", "--channel", "telegram"])
        .map_err(ctx("`harw connect --channel telegram` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Connect {
            channel: Channel::Telegram,
            pair: None
        })
    ));
    Ok(())
}

#[test]
fn test_legacy_uia_new_parses() -> TestResult {
    let cli =
        Cli::try_parse_from(["harw", "uia", "new"]).map_err(ctx("`harw uia new` sollte parsen"))?;
    assert!(matches!(
        cli.command,
        Some(Command::Uia {
            action: UiaAction::New
        })
    ));
    Ok(())
}

#[test]
fn test_legacy_run_classify_and_catalog_parse() -> TestResult {
    let run = Cli::try_parse_from(["harw", "run", "hallo", "welt"])
        .map_err(ctx("`harw run hallo welt` sollte parsen"))?;
    assert!(matches!(
        run.command,
        Some(Command::Run { input }) if input == vec!["hallo".to_owned(), "welt".to_owned()]
    ));

    let classify = Cli::try_parse_from(["harw", "classify", "eine", "zeile"])
        .map_err(ctx("`harw classify ...` sollte parsen"))?;
    assert!(matches!(
        classify.command,
        Some(Command::Classify { input }) if input.len() == 2
    ));

    let catalog = Cli::try_parse_from(["harw", "catalog", "--refresh"])
        .map_err(ctx("`harw catalog --refresh` sollte parsen"))?;
    assert!(matches!(
        catalog.command,
        Some(Command::Catalog { refresh: true })
    ));

    let catalog =
        Cli::try_parse_from(["harw", "catalog"]).map_err(ctx("`harw catalog` sollte parsen"))?;
    assert!(matches!(
        catalog.command,
        Some(Command::Catalog { refresh: false })
    ));
    Ok(())
}

// ── Globale und Sitzungs-Flags ──────────────────────────────────────────

#[test]
fn test_approval_flag_accepts_all_three_modes() -> TestResult {
    for (raw, expected) in [
        ("ask", ApprovalMode::AlwaysAsk),
        ("auto", ApprovalMode::Delegated),
        ("full", ApprovalMode::FullAccess),
    ] {
        let cli = Cli::try_parse_from(["harw", "--approval", raw])
            .map_err(ctx("`harw --approval ask|auto|full` sollte parsen"))?;
        assert_eq!(cli.global.approval, Some(expected), "Wert `{raw}`");
    }

    let cli = Cli::try_parse_from(["harw", "chat", "--approval", "ask"])
        .map_err(ctx("`--approval` hinter `chat` sollte parsen"))?;
    assert_eq!(cli.global.approval, Some(ApprovalMode::AlwaysAsk));
    Ok(())
}

#[test]
fn test_approval_flag_rejects_an_unknown_value() -> TestResult {
    let Err(error) = Cli::try_parse_from(["harw", "--approval", "immer"]) else {
        return Err(TestError::Unexpected(
            "ein unbekannter --approval-Wert muss scheitern".into(),
        ));
    };
    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
    Ok(())
}

#[test]
fn test_profile_cwd_json_and_verbose_are_global() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "session",
        "list",
        "--profile",
        "arbeit",
        "-C",
        "/tmp/projekt",
        "--json",
        "-v",
    ])
    .map_err(ctx("globale Flags hinter `session list` sollten parsen"))?;
    assert_eq!(cli.global.profile.as_deref(), Some("arbeit"));
    assert_eq!(cli.global.cwd, Some(PathBuf::from("/tmp/projekt")));
    assert!(cli.global.json);
    assert!(cli.global.verbose);
    assert_eq!(cli.global.output(), crate::output::OutputFormat::Json);

    let cli = Cli::try_parse_from(["harw", "--cwd", "/tmp/b", "--profile", "p", "doctor"])
        .map_err(ctx("`--cwd`/`--profile` vor dem Befehl sollten parsen"))?;
    assert_eq!(cli.global.cwd, Some(PathBuf::from("/tmp/b")));
    assert_eq!(cli.global.profile.as_deref(), Some("p"));
    assert!(matches!(cli.command, Some(Command::Doctor { .. })));
    Ok(())
}

#[test]
fn test_global_defaults_match_default_impl() -> TestResult {
    let cli = Cli::try_parse_from(["harw"]).map_err(ctx("bare harw parses"))?;
    let defaults = GlobalArgs::default();
    assert_eq!(cli.global.log, defaults.log);
    assert_eq!(cli.global.log, "info");
    assert_eq!(cli.global.home, None);
    assert_eq!(cli.global.profile, None);
    assert_eq!(cli.global.cwd, None);
    assert!(!cli.global.json);
    assert!(!cli.global.verbose);
    assert!(!cli.global.log_sensitive);
    assert_eq!(cli.global.output(), crate::output::OutputFormat::Text);
    assert!(cli.global.session_flags_used().is_empty());
    Ok(())
}

#[test]
fn test_session_flags_used_lists_every_set_flag_in_order() -> TestResult {
    let cli = Cli::try_parse_from([
        "harw",
        "--add-dir",
        "/tmp/a",
        "--goal",
        "Ziel",
        // Ein echter Agentenname: `uia` ist zugleich das versteckte
        // Alt-Unterkommando und gewinnt wegen `subcommand_precedence_over_arg`.
        "--agent",
        "explorer",
        "--model",
        "m",
        "--approval",
        "auto",
        "--mode",
        "explore",
    ])
    .map_err(ctx("alle Sitzungs-Flags sollten parsen"))?;
    assert_eq!(
        cli.global.session_flags_used(),
        vec![
            "--mode",
            "--approval",
            "--model",
            "--goal",
            "--agent",
            "--add-dir"
        ]
    );
    Ok(())
}

#[test]
fn test_agent_flag_sets_the_session_agent() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "--agent", "planner"])
        .map_err(ctx("`harw --agent planner` sollte parsen"))?;
    assert_eq!(cli.global.agent.as_deref(), Some("planner"));
    assert_eq!(cli.global.session_flags_used(), vec!["--agent"]);
    assert!(cli.command.is_none());

    for argv in [
        vec!["harw", "chat", "--agent", "uia"],
        vec!["harw", "exec", "--agent", "uia", "hallo"],
        vec!["harw", "analyze", "--agent", "uia"],
    ] {
        let cli = Cli::try_parse_from(argv.clone()).map_err(ctx("--agent hinter Subcommand"))?;
        assert_eq!(cli.global.agent.as_deref(), Some("uia"), "{argv:?}");
    }
    Ok(())
}

#[test]
fn test_agent_flag_does_not_capture_agent_subcommand() -> TestResult {
    // Der Subcommand `harw agent …` darf nicht als `--agent` erscheinen.
    let cli = Cli::try_parse_from(["harw", "agent", "skills"])
        .map_err(ctx("`harw agent skills` sollte parsen"))?;
    assert_eq!(cli.global.agent, None);
    assert!(cli.global.session_flags_used().is_empty());
    Ok(())
}

#[test]
fn test_session_flags_still_parse_on_other_commands() -> TestResult {
    // Das Ablehnen bei Nicht-Sitzungsbefehlen geschieht nach dem Parsen;
    // die Grammatik selbst akzeptiert die globalen Flags überall.
    let cli = Cli::try_parse_from(["harw", "doctor", "--mode", "explore"])
        .map_err(ctx("`harw doctor --mode explore` sollte parsen"))?;
    assert_eq!(cli.global.session_flags_used(), vec!["--mode"]);
    Ok(())
}

// ── analyze --order ─────────────────────────────────────────────────────

#[test]
fn test_analyze_order_top_down_parses() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "analyze", "--order", "top-down"])
        .map_err(ctx("`harw analyze --order top-down` sollte parsen"))?;
    let Some(Command::Analyze(args)) = cli.command else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Analyze, bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args.order, AnalyzeOrder::TopDown);
    assert_eq!(args.effective_order(), AnalyzeOrder::TopDown);

    let invalid = Cli::try_parse_from(["harw", "analyze", "--order", "seitwärts"]);
    assert!(invalid.is_err(), "unbekannte Reihenfolge muss scheitern");
    Ok(())
}

#[test]
fn test_analyze_hidden_order_flags_still_work_alone() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "analyze", "--top-down"])
        .map_err(ctx("`harw analyze --top-down` sollte parsen"))?;
    let Some(Command::Analyze(args)) = cli.command else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Analyze, bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args.effective_order(), AnalyzeOrder::TopDown);

    let cli = Cli::try_parse_from(["harw", "analyze", "--bottom-up"])
        .map_err(ctx("`harw analyze --bottom-up` sollte parsen"))?;
    let Some(Command::Analyze(args)) = cli.command else {
        return Err(TestError::Unexpected(format!(
            "erwartete Command::Analyze, bekam {:?}",
            cli.command
        )));
    };
    assert_eq!(args.effective_order(), AnalyzeOrder::BottomUp);
    Ok(())
}

#[test]
fn test_hidden_order_flags_conflict_with_order() {
    for argv in [
        ["harw", "analyze", "--bottom-up", "--order", "top-down"],
        ["harw", "analyze", "--order", "top-down", "--bottom-up"],
        ["harw", "analyze", "--bottom-up", "--order", "bottom-up"],
        ["harw", "analyze", "--top-down", "--order", "bottom-up"],
        ["harw", "analyze", "--order", "top-down", "--top-down"],
    ] {
        let result = Cli::try_parse_from(argv);
        assert!(
            matches!(&result, Err(error) if error.kind() == clap::error::ErrorKind::ArgumentConflict),
            "{argv:?} muss mit einem Konflikt scheitern, bekam {result:?}"
        );
    }
}

#[test]
fn auth_prune_parses_with_and_without_provider() -> TestResult {
    let all = Cli::try_parse_from(["harw", "auth", "prune"]).map_err(ctx("auth prune"))?;
    assert!(
        matches!(
            all.command,
            Some(Command::Auth {
                action: AuthAction::Prune { provider: None },
                ..
            })
        ),
        "{:?}",
        all.command
    );
    let one = Cli::try_parse_from(["harw", "auth", "prune", "openai"])
        .map_err(ctx("auth prune openai"))?;
    assert!(
        matches!(
            one.command,
            Some(Command::Auth {
                action: AuthAction::Prune { provider: Some(ref name) },
                ..
            }) if name == "openai"
        ),
        "{:?}",
        one.command
    );
    Ok(())
}

// ── #22 Welle 2B: die Compiler-Befehle von `harw agent` ─────────────────

fn agent_action(args: &[&str]) -> TestResult<AgentAction> {
    let mut argv = vec!["harw", "agent"];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(argv).map_err(ctx("harw agent … sollte parsen"))?;
    match cli.command {
        Some(Command::Agent { action }) => Ok(action),
        other => Err(TestError::Unexpected(format!(
            "erwartete Agent, bekam {other:?}"
        ))),
    }
}

#[test]
fn test_agent_compiler_subcommands_parse() -> TestResult {
    assert!(matches!(
        agent_action(&["check", "a", "./b"])?,
        AgentAction::Check { targets } if targets == ["a", "./b"]
    ));
    assert!(
        matches!(agent_action(&["check"])?, AgentAction::Check { targets } if targets.is_empty())
    );
    let AgentAction::Build {
        agent,
        interface,
        native,
        artifact_only,
        runner,
        output,
        target_triple,
        harw_src,
    } = agent_action(&[
        "build",
        "evidence-critic",
        "--interface",
        "cli,mcp",
        "--native",
        "--runner",
        "/r",
        "-o",
        "out",
        "--target",
        "aarch64-unknown-linux-gnu",
        "--harw-src",
        "/src",
    ])?
    else {
        return Err(TestError::Unexpected("build".into()));
    };
    assert_eq!(agent, "evidence-critic");
    assert_eq!(interface, ["cli", "mcp"]);
    assert!(native && !artifact_only);
    assert_eq!(runner, Some(PathBuf::from("/r")));
    assert_eq!(output, Some(PathBuf::from("out")));
    assert_eq!(target_triple.as_deref(), Some("aarch64-unknown-linux-gnu"));
    assert_eq!(harw_src, Some(PathBuf::from("/src")));
    assert!(
        Cli::try_parse_from(["harw", "agent", "build", "x", "--native", "--artifact-only"])
            .is_err(),
        "--native and --artifact-only exclude each other"
    );
    assert!(matches!(
        agent_action(&["build", "x", "--artifact-only"])?,
        AgentAction::Build {
            artifact_only: true,
            ..
        }
    ));
    assert!(matches!(
        agent_action(&["inspect", "./ec"])?,
        AgentAction::Inspect { .. }
    ));
    assert!(matches!(
        agent_action(&[
            "graph",
            "--all",
            "--format",
            "mermaid",
            "--kind",
            "delegation"
        ])?,
        AgentAction::Graph {
            all: true,
            agent: None,
            ..
        }
    ));
    assert!(
        Cli::try_parse_from(["harw", "agent", "graph"]).is_err(),
        "a name or --all"
    );
    assert!(matches!(
        agent_action(&["explain", "HARW-PATCH-003"])?,
        AgentAction::Explain { field: None, .. }
    ));
    assert!(matches!(
        agent_action(&["explain", "ec", "tools.admitted"])?,
        AgentAction::Explain { field: Some(field), .. } if field == "tools.admitted"
    ));
    assert!(matches!(
        agent_action(&["new", "x", "--role", "child-orchestrator", "--extends", "a.agent.b@1", "--dir", "d"])?,
        AgentAction::New { role, extends: Some(_), dir: Some(_), .. } if role == "child-orchestrator"
    ));
    assert!(matches!(
        agent_action(&["fmt", "--check"])?,
        AgentAction::Fmt { check: true, .. }
    ));
    assert!(matches!(
        agent_action(&["diff", "a", "b@1.0.0"])?,
        AgentAction::Diff { .. }
    ));
    assert!(matches!(
        agent_action(&["test"])?,
        AgentAction::Test { agent: None }
    ));
    assert!(matches!(
        agent_action(&["run", "ec", "Prüfe", "die", "Quelle"])?,
        AgentAction::Run { prompt, .. } if prompt.len() == 3
    ));
    assert!(matches!(
        agent_action(&["versions", "ec"])?,
        AgentAction::Versions { .. }
    ));
    assert!(matches!(
        agent_action(&["use", "ec", "1.0.0"])?,
        AgentAction::Use { .. }
    ));
    assert!(matches!(
        agent_action(&[
            "clean",
            "--all",
            "--older-than",
            "7",
            "--keep",
            "2",
            "--dry-run"
        ])?,
        AgentAction::Clean {
            all: true,
            older_than: Some(7),
            keep: Some(2),
            dry_run: true
        }
    ));
    assert!(matches!(agent_action(&["doctor"])?, AgentAction::Doctor));
    assert!(matches!(
        agent_action(&["install-record", "--source-dir", "/src", "--bindir", "/bin"])?,
        AgentAction::InstallRecord { .. }
    ));
    assert!(matches!(
        agent_action(&["auto-build-uia", "--release-lock"])?,
        AgentAction::AutoBuildUia { release_lock: true }
    ));
    let cli = Cli::try_parse_from(["harw", "agent", "check", "x", "--json"])
        .map_err(ctx("--json nach dem Unterbefehl"))?;
    assert!(cli.global.json, "every command supports --json");
    Ok(())
}

#[test]
fn test_agent_compiler_actions_map_to_commands() -> TestResult {
    use harw_agent_compiler::commands::AgentCommand;
    let command = crate::agent_cmd::compiler_command(agent_action(&[
        "build",
        "ec",
        "--interface",
        "cli",
        "--interface",
        "mcp",
    ])?)
    .map_err(TestError::Unexpected)?;
    let AgentCommand::Build(args) = command else {
        return Err(TestError::Unexpected("build".into()));
    };
    assert_eq!(
        args.interfaces,
        Some(vec![
            harw_agent_dsl::ir_v2::Interface::Cli,
            harw_agent_dsl::ir_v2::Interface::Mcp
        ])
    );
    assert!(
        crate::agent_cmd::compiler_command(agent_action(&["build", "ec", "--interface", "grpc"])?)
            .is_err()
    );
    assert!(
        crate::agent_cmd::compiler_command(agent_action(&["graph", "x", "--format", "svg"])?)
            .is_err()
    );
    Ok(())
}

// ── install --print-systemd (Crypto-Masterplan v2 H10) ──────────────────

#[test]
fn test_install_print_systemd_without_unit_parses_to_all() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "install", "--print-systemd"])
        .map_err(ctx("`harw install --print-systemd` sollte parsen"))?;
    let Some(Command::Install { print_systemd }) = cli.command else {
        return Err(TestError::Unexpected("erwartete install".into()));
    };
    assert_eq!(print_systemd, Some(None));
    Ok(())
}

#[test]
fn test_install_print_systemd_with_unit_parses_the_name() -> TestResult {
    let cli = Cli::try_parse_from(["harw", "install", "--print-systemd", "harw-warden.socket"])
        .map_err(ctx("`harw install --print-systemd UNIT` sollte parsen"))?;
    let Some(Command::Install { print_systemd }) = cli.command else {
        return Err(TestError::Unexpected("erwartete install".into()));
    };
    assert_eq!(print_systemd, Some(Some("harw-warden.socket".to_owned())));
    Ok(())
}

#[test]
fn test_install_without_flag_parses_to_none() -> TestResult {
    let cli =
        Cli::try_parse_from(["harw", "install"]).map_err(ctx("`harw install` sollte parsen"))?;
    let Some(Command::Install { print_systemd }) = cli.command else {
        return Err(TestError::Unexpected("erwartete install".into()));
    };
    assert_eq!(print_systemd, None);
    Ok(())
}
