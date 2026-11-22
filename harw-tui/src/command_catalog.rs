//! Statischer Hilfe-Katalog für `/`-Befehle: TUI-lokale Befehle,
//! Unterkommando-Hinweise, Nutzungszeilen und geplante Befehle.
//!
//! # Verantwortung
//! - [`local_command_specs`]: Spezifikationen der rein TUI-lokal abgefangenen
//!   Befehle (`/tools`, `/exit`, …) plus Ersatz-Spezifikationen für Befehle,
//!   deren Operation (noch) nicht registriert ist (`/workbench`, `/kanban`,
//!   `/palace`, `/dream`, `/diary`, `/models`, `/mode`, `/matrix`). Beim Einmischen über
//!   `CommandRegistry::with_local_specs` gewinnt immer die Operation; der
//!   Ersatz entfällt dann.
//! - [`enrich`]: ergänzt eine aus einer Operation abgeleitete Spezifikation um
//!   Kurzbeschreibung, Nutzungszeile und Unterkommandos.
//! - [`subcommand_hints`]: Unterkommando-Grammatik je Befehl; die Einträge
//!   folgen den tatsächlichen Parsern in `harw-ops` bzw.
//!   `crate::tools_command`.
//! - [`planned_commands`]: Befehle aus dem Interaktionsvertrag, die weder als
//!   Operation noch lokal existieren — für die Hilfe („geplant").
//!
//! Die Daten (Hinweistabellen, Zusammenfassungen, geplante und lokale
//! Befehle) liegen in `harw_ops::command_meta`; dieses Modul übersetzt sie
//! nur über `tr` und baut daraus TUI-Spezifikationen.
//!
//! # Nebenläufigkeit
//! Nur reine Funktionen über `const`-Daten; kein geteilter Zustand.
//!
//! # Fehlertypen
//! Keine — ungültige Namen in den Tabellen würden beim Bau still entfallen
//! und von den Tests erkannt.

use crate::command::{
    BusyAvailability, CommandDomain, CommandScope, CommandSpec, OutputSurface, PermissionTier,
    SubcommandHint,
};

use crate::command::{
    BusyAvailability, CommandDomain, CommandScope, CommandSpec, OutputSurface, PermissionTier,
    SubcommandHint,
};

use crate::i18n::tr;
use harw_operations::operation::OperationDomain;
use harw_ops::command_meta::{self, HintEntry};

/// Owned metadata is translated by identity; syntax and supplied metadata are payload.
fn localized(hint: &HintEntry) -> SubcommandHint {
    SubcommandHint::new(hint.name, hint.args, tr(hint.key, hint.english))
}

/// Bildet die Operations-Domäne auf die TUI-Domäne ab.
fn map_domain(domain: OperationDomain) -> CommandDomain {
    match domain {
        OperationDomain::Session => CommandDomain::SessionLifecycle,
        OperationDomain::Execution => CommandDomain::Execution,
        _ => CommandDomain::Misc,
    }
}

// ---------------------------------------------------------------------------
// Öffentliche Funktionen
// ---------------------------------------------------------------------------

/// Liefert die Unterkommando-Hinweise eines Befehls (ohne führendes `/`).
///
/// # Rückgabe
/// Die lokalisierte Hinweisliste oder eine leere Liste für unbekannte Befehle.
pub(crate) fn subcommand_hints(name: &str) -> Vec<SubcommandHint> {
    command_meta::help_for(name)
        .map_or_else(Vec::new, |help| help.hints.iter().map(localized).collect())
}

/// Localized planned summaries, keyed by owned command identity.
pub(crate) fn planned_commands() -> Vec<(&'static str, &'static str)> {
    command_meta::PLANNED_COMMANDS
        .iter()
        .map(|(name, key, english)| (*name, tr(key, english)))
        .collect()
}

/// Nutzungszeile aus der Katalogtabelle, falls vorhanden.
fn usage_for(name: &str) -> Option<&'static str> {
    command_meta::help_for(name)
        .map(|help| help.usage_key.map_or(help.usage, |key| tr(key, help.usage)))
}

/// Ergänzt eine Spezifikation um Katalogwissen.
///
/// # Beschreibung
/// The caller must establish built-in or TUI-local ownership first; registry
/// adapters validate concrete metadata identity before calling this helper.
/// Complete messages are retained; terminal layout owns clipping/wrapping.
///
/// - `summary`: catalog overrides win; other complete summaries stay unchanged.
/// - `subcommands`: nur gesetzt, wenn noch leer.
/// - `usage`: nur gesetzt, wenn noch leer — aus der Katalogtabelle, sonst
///   `"/<name>"`.
///
/// Idempotent: ein zweiter Aufruf ändert nichts mehr.
pub(crate) fn enrich(spec: &mut CommandSpec) {
    let name = spec.name.as_str().to_owned();
    if let Some((key, english)) = command_meta::summary_override(&name) {
        spec.summary = tr(key, english).to_owned();
    }
    if name == "settings" {
        let (key, english) = command_meta::SETTINGS_SUMMARY;
        spec.summary = tr(key, english).to_owned();
        spec.subcommands = command_meta::SETTINGS_HINTS.iter().map(localized).collect();
    }
    if spec.subcommands.is_empty() {
        spec.subcommands = subcommand_hints(&name);
    }
    if spec.usage.is_empty() {
        spec.usage = usage_for(&name).map_or_else(|| format!("/{name}"), str::to_owned);
    }
}

/// Baut eine lokale Spezifikation; `None` nur bei ungültigem Namen (von den
/// Tests ausgeschlossen).
fn local_spec(
    name: &str,
    domain: CommandDomain,
    permission: PermissionTier,
    busy: BusyAvailability,
    summary: &str,
    usage: &str,
) -> Option<CommandSpec> {
    let mut spec = CommandSpec::new(
        name,
        Vec::<String>::new(),
        CommandScope::TuiOnly,
        permission,
        OutputSurface::Inline,
        domain,
    )
    .ok()?
    .with_help(summary, usage, &subcommand_hints(name))
    .local();
    spec.busy = busy;
    enrich(&mut spec);
    Some(spec)
}

/// Namen der Ersatz-Spezifikationen für (noch) fehlende Operationen.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const FALLBACK_COMMANDS: &[&str] = &["mode", "matrix", "plan"];

/// Spezifikationen aller TUI-lokalen Befehle plus Ersatz-Spezifikationen
/// ([`FALLBACK_COMMANDS`]).
///
/// # Beschreibung
/// Alle Einträge tragen [`crate::command::CommandOrigin::TuiLocal`] und
/// [`CommandScope::TuiOnly`]. Beim Einmischen via
/// `CommandRegistry::with_local_specs` gewinnt bei Namens-/Alias-Kollision
/// die Operation — Ersatz-Spezifikationen verschwinden also automatisch,
/// sobald die Operation registriert ist. Daten: `command_meta::LOCAL_COMMANDS`.
pub(crate) fn local_command_specs() -> Vec<CommandSpec> {
    command_meta::LOCAL_COMMANDS
        .iter()
        .filter_map(|meta| {
            local_spec(
                meta.name,
                map_domain(meta.domain),
                meta.permission,
                meta.busy,
                tr(meta.key, meta.english),
                meta.usage,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandRegistry;
    use crate::command::{CommandName, CommandOrigin};
    use crate::test_support::{TestResult, ctx};

    /// Echte lokale Befehle (keine Ersatz-Spezifikationen).
    const REAL_LOCAL: &[&str] = &[
        "tools", "resume", "sessions", "exit", "clear", "verbose", "keys", "whoami", "rename",
        // Runde 5, Teil L:
        "btw", "image",
    ];

    /// Befehle, deren Operation fehlen darf. `matrix` behält nur seine
    /// Ersatz-Spezifikation. Runde 5, Teil F: die `plan`-Operation gibt es
    /// nur mit Plan-Diensten (`[tools.plan]`, TUI-Vorgabe), nie im
    /// eingebauten Katalog; `/plan` selbst ist lokal.
    const ALLOWED_MISSING: &[&str] = &["plan"];

    #[test]
    fn local_specs_cover_every_local_and_fallback_name_once() {
        let specs = local_command_specs();
        let mut expected: Vec<&str> = REAL_LOCAL.to_vec();
        expected.extend_from_slice(FALLBACK_COMMANDS);
        assert_eq!(specs.len(), expected.len(), "a local spec failed to build");
        for name in expected {
            let hits = specs.iter().filter(|s| s.name.as_str() == name).count();
            assert_eq!(hits, 1, "local spec '{name}' must appear exactly once");
        }
    }

    #[test]
    fn local_specs_are_tui_local_with_help() {
        for spec in local_command_specs() {
            assert_eq!(spec.origin, CommandOrigin::TuiLocal, "{}", spec.name);
            assert_eq!(spec.scope, CommandScope::TuiOnly, "{}", spec.name);
            assert!(!spec.summary.is_empty(), "{} needs a summary", spec.name);
            assert!(
                spec.usage.starts_with(&format!("/{}", spec.name)),
                "{} usage must start with its name, got {:?}",
                spec.name,
                spec.usage
            );
        }
    }

    #[test]
    fn hint_table_names_and_enriched_hints_are_valid_unique_and_non_empty() -> TestResult {
        for (index, help) in command_meta::HINT_TABLE.iter().enumerate() {
            let (name, usage, hints) = (&help.name, &help.usage, help.hints);
            CommandName::parse(*name).map_err(ctx("hinted name must be valid"))?;
            let mut spec = CommandSpec::new(
                *name,
                Vec::<String>::new(),
                CommandScope::TuiOnly,
                PermissionTier::Operator,
                OutputSurface::Inline,
                CommandDomain::Misc,
            )
            .map_err(ctx("hinted spec"))?;
            enrich(&mut spec);
            if *name == "settings" {
                assert!(
                    hints.is_empty(),
                    "settings hints must reuse dynamic settings metadata"
                );
                assert_eq!(
                    spec.subcommands
                        .iter()
                        .map(|hint| (hint.name, hint.args))
                        .collect::<Vec<_>>(),
                    [("show", ""), ("set", "<key> <value>"), ("reset", "<key>")]
                );
                assert_eq!(
                    spec.subcommands[0].summary,
                    tr("settings.title", "Settings")
                );
                assert_eq!(
                    spec.subcommands[1].summary,
                    tr("settings.scope", "Active profile · applies immediately")
                );
                assert_eq!(spec.subcommands[2].summary, spec.subcommands[1].summary);
            } else {
                assert!(!hints.is_empty(), "{name} has an empty static hint list");
                assert_eq!(spec.subcommands.len(), hints.len());
            }
            assert!(
                !spec.subcommands.is_empty(),
                "{name} has an empty enriched hint list"
            );
            assert!(usage.starts_with(&format!("/{name}")), "{name} usage");
            assert_eq!(Some(spec.usage.as_str()), usage_for(name));
            assert!(
                command_meta::HINT_TABLE[index + 1..]
                    .iter()
                    .all(|h| h.name != *name),
                "{name} appears twice in HINT_TABLE"
            );
            for hint in &spec.subcommands {
                assert!(!hint.name.is_empty() && !hint.summary.is_empty());
                assert!(!hint.name.contains(' '), "{name}: {:?}", hint.name);
            }
        }
        Ok(())
    }

    /// Drift: jeder Befehl mit Hinweisen existiert als Operation oder als
    /// echter lokaler Befehl (Ausnahmen nur über `ALLOWED_MISSING`).
    #[test]
    fn every_hinted_command_exists_as_operation_or_local() -> TestResult {
        let built_in = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        for help in command_meta::HINT_TABLE {
            let name = &help.name;
            let exists = built_in.find(name).is_some()
                || REAL_LOCAL.contains(name)
                || ALLOWED_MISSING.contains(name);
            assert!(
                exists,
                "hinted command '/{name}' is neither an operation nor a local command"
            );
        }
        Ok(())
    }

    /// Drift: geplante Befehle dürfen weder als Operation noch lokal existieren
    /// — sonst aus `PLANNED_COMMANDS` entfernen.
    #[test]
    fn planned_commands_are_not_yet_implemented() -> TestResult {
        let built_in = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let locals = local_command_specs();
        for (name, summary) in planned_commands() {
            CommandName::parse(name).map_err(ctx("planned name must be valid"))?;
            assert!(!summary.is_empty());
            assert!(
                built_in.find(name).is_none(),
                "'/{name}' is implemented as an operation; remove it from command_meta::PLANNED_COMMANDS"
            );
            assert!(
                locals.iter().all(|s| s.name.as_str() != name),
                "'/{name}' is implemented locally; remove it from command_meta::PLANNED_COMMANDS"
            );
        }
        Ok(())
    }

    #[test]
    fn subcommand_hints_follow_the_op_grammar() {
        let names =
            |cmd: &str| -> Vec<&str> { subcommand_hints(cmd).iter().map(|h| h.name).collect() };
        assert_eq!(names("model"), ["show", "list", "switch"]);
        assert_eq!(names("provider"), ["show", "list", "test"]);
        assert_eq!(names("models"), ["show", "set", "reset", "pick", "worker"]);
        assert_eq!(names("sandbox-lease"), ["status", "revoke"]);
        assert!(names("mode").contains(&"default"));
        assert!(names("tools").contains(&"profile"));
        assert_eq!(names("matrix"), ["show", "list", "replay", "compare"]);
        assert_eq!(names("jobs"), ["list", "show", "stop", "logs"]);
        assert_eq!(names("dream"), ["list", "show", "run", "status", "review"]);
        assert_eq!(
            names("palace"),
            [
                "list",
                "show",
                "search",
                "promote",
                "supersede",
                "edit",
                "link"
            ]
        );
        assert_eq!(
            names("diary"),
            ["today", "show", "search", "agents", "note"]
        );
        assert!(names("memory").contains(&"promote"));
        assert!(names("memory").contains(&"topics"));
        for sub in ["edit", "comment", "evidence", "approve", "reject"] {
            assert!(names("kanban").contains(&sub), "{sub}");
        }
        assert!(names("workbench").contains(&"retention"));
        // Runde 5, Teil E.
        for sub in ["allow", "deny", "rules", "rm", "log"] {
            assert!(names("permissions").contains(&sub), "{sub}");
        }
        assert!(subcommand_hints("no-such-command").is_empty());
    }

    #[test]
    fn enrich_fills_help_fields_and_is_idempotent() -> TestResult {
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let Some(model) = registry.find("model") else {
            return Err(crate::test_support::TestError::Missing("model spec"));
        };
        let mut spec = model.clone();
        spec.summary = "x".repeat(200);
        spec.usage.clear();
        spec.subcommands.clear();
        enrich(&mut spec);
        assert_eq!(spec.summary, "x".repeat(200));
        assert_eq!(spec.usage, "/model [show|list|switch <model-id>]");
        assert_eq!(spec.subcommands.len(), 3);
        let once = spec.clone();
        enrich(&mut spec);
        assert_eq!(spec, once, "enrich must be idempotent");
        Ok(())
    }

    #[test]
    fn enrich_overrides_the_long_sandbox_lease_summary() -> TestResult {
        let mut spec = CommandSpec::new(
            "sandbox-lease",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::Execution,
        )
        .map_err(ctx("valid spec"))?;
        spec.summary = "lang ".repeat(100);
        enrich(&mut spec);
        assert_eq!(
            spec.summary,
            "Show or revoke the host permit for shell.exec"
        );
        Ok(())
    }

    #[test]
    fn every_owned_hint_has_a_stable_key_and_translates_in_all_ten_locales() {
        use crate::i18n::{tr_for, with_language};
        use harw_config::UiLanguage;
        let mut keys = std::collections::BTreeSet::new();
        let mut count = 0;
        for help in command_meta::HINT_TABLE {
            let (name, hints) = (&help.name, help.hints);
            for hint in hints {
                count += 1;
                assert!(keys.insert(hint.key), "duplicate key: {}", hint.key);
            }
            for language in UiLanguage::ALL {
                with_language(language, || {
                    let localized = subcommand_hints(name);
                    assert_eq!(localized.len(), hints.len());
                    for (actual, owned) in localized.iter().zip(hints) {
                        assert_eq!((actual.name, actual.args), (owned.name, owned.args));
                        assert_eq!(actual.summary, tr_for(language, owned.key, owned.english));
                        if language == UiLanguage::English {
                            assert_eq!(actual.summary, owned.english);
                        } else {
                            assert_ne!(
                                actual.summary, owned.english,
                                "{} / {language:?}",
                                owned.key
                            );
                        }
                    }
                });
            }
        }
        assert_eq!(
            count, 141,
            "all owned hints, including both workbench note rows"
        );
        let notes: Vec<_> = subcommand_hints("workbench")
            .into_iter()
            .filter(|hint| hint.name == "note")
            .collect();
        assert_eq!(notes.len(), 2);
        assert_ne!(notes[0].args, notes[1].args);
        assert_ne!(notes[0].summary, notes[1].summary);
    }

    #[test]
    fn local_planned_override_and_settings_metadata_rebuild_for_all_locales() -> TestResult {
        use crate::i18n::{tr_for, with_language};
        use harw_config::UiLanguage;
        let english = local_command_specs();
        let planned = planned_commands();
        assert_eq!(planned.len(), 15);
        assert_eq!(subcommand_hints("model")[2].args, "<model-id>");
        assert_eq!(subcommand_hints("export")[1].name, "--datei");
        assert_eq!(subcommand_hints("export")[1].args, "<path>");
        for language in UiLanguage::ALL {
            with_language(language, || -> TestResult {
                hint_table_names_and_enriched_hints_are_valid_unique_and_non_empty()?;
                let localized = local_command_specs();
                for (spec, original) in localized.iter().zip(&english) {
                    assert_eq!(spec.name, original.name);
                    assert_eq!(spec.aliases, original.aliases);
                    assert_eq!(spec.permission, original.permission);
                    assert_eq!(spec.busy, original.busy);
                    assert_eq!(spec.origin, original.origin);
                    assert_eq!(
                        spec.usage.split(" — ").next(),
                        original.usage.split(" — ").next()
                    );
                    if language != UiLanguage::English {
                        assert_ne!(
                            spec.summary, original.summary,
                            "{} / {language:?}",
                            spec.name
                        );
                    }
                }
                for ((name, summary), (original, english)) in
                    planned_commands().iter().zip(&planned)
                {
                    assert_eq!(name, original);
                    if language != UiLanguage::English {
                        assert_ne!(summary, english);
                    }
                }
                for (name, key, fallback) in command_meta::SUMMARY_OVERRIDES {
                    let mut spec = CommandSpec::new(
                        *name,
                        Vec::<String>::new(),
                        CommandScope::TuiOnly,
                        PermissionTier::Operator,
                        OutputSurface::Inline,
                        CommandDomain::Misc,
                    )
                    .map_err(ctx("override spec"))?;
                    enrich(&mut spec);
                    assert_eq!(spec.summary, tr_for(language, key, fallback));
                }
                let syntax = "/matrix [show|list|replay|compare <run> <run>]";
                assert_eq!(
                    usage_for("matrix").and_then(|usage| usage.split(" — ").next()),
                    Some(syntax)
                );
                Ok(())
            })?;
        }
        assert_eq!(tr("settings.title", "Settings"), "Settings");
        Ok(())
    }

    #[test]
    fn enrich_preserves_supplied_syntax_hints_and_unknown_long_descriptions() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        for name in ["external", "model"] {
            let mut spec = CommandSpec::new(
                name,
                ["custom"],
                CommandScope::TuiOnly,
                PermissionTier::Operator,
                OutputSurface::Inline,
                CommandDomain::Misc,
            )
            .map_err(ctx("supplied spec"))?;
            spec.summary = if name == "external" {
                "Unbekannter unveränderter Text {value}. ".repeat(10)
            } else {
                "Supplied model description".to_owned()
            };
            spec.usage = format!("/{name} <modell-id> --datei <pfad>");
            spec.subcommands = vec![SubcommandHint::new(
                "switch",
                "<modell-id>",
                "Gelieferter Text {value}",
            )];
            let supplied = spec.clone();
            for language in UiLanguage::ALL {
                with_language(language, || enrich(&mut spec));
                assert_eq!(spec, supplied, "{name} / {language:?}");
            }
        }
        Ok(())
    }

    #[test]
    fn enrich_preserves_long_complete_operation_summaries() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        let registry = CommandRegistry::built_in().map_err(ctx("built-in"))?;
        let model = registry
            .find("model")
            .ok_or(crate::test_support::TestError::Missing("model"))?;
        // A complete second sentence and wide text must survive metadata refresh.
        let supplied = format!(
            "Complete description with a second sentence. {}",
            "日本語 ".repeat(60)
        );
        for language in UiLanguage::ALL {
            with_language(language, || {
                let mut spec = model.clone();
                spec.summary = supplied.clone();
                enrich(&mut spec);
                assert_eq!(spec.summary, supplied);
            });
        }
        Ok(())
    }

    /// Kanonischer Abzug des Katalogs in Englisch (ausserhalb `with_language`).
    fn popup_dump() -> String {
        use std::fmt::Write as _;
        fn subs(out: &mut String, spec: &CommandSpec) {
            for hint in &spec.subcommands {
                let _ = write!(out, "\tS\t{}\t{}\t{}", hint.name, hint.args, hint.summary);
            }
            out.push('\n');
        }
        let mut out = String::new();
        for spec in local_command_specs() {
            let _ = write!(
                out,
                "L\t{}\t{}\t{}\t{:?}\t{:?}\t{:?}",
                spec.name, spec.summary, spec.usage, spec.busy, spec.permission, spec.domain
            );
            subs(&mut out, &spec);
        }
        let names = command_meta::HINT_TABLE
            .iter()
            .map(|help| help.name)
            .chain(std::iter::once("settings"));
        for name in names {
            let Ok(mut spec) = CommandSpec::new(
                name,
                Vec::<String>::new(),
                CommandScope::TuiOnly,
                PermissionTier::Operator,
                OutputSurface::Inline,
                CommandDomain::Misc,
            ) else {
                continue;
            };
            enrich(&mut spec);
            let _ = write!(out, "H\t{}\t{}\t{}", spec.name, spec.summary, spec.usage);
            subs(&mut out, &spec);
        }
        for (name, summary) in planned_commands() {
            let _ = writeln!(out, "P\t{name}\t{summary}");
        }
        out
    }

    /// Abnahme "TUI-Popup unveraendert": Abzug gegen die vor dem Rueckbau
    /// erzeugte Datei. `HARW_BLESS_POPUP=1` schreibt sie neu.
    #[test]
    fn popup_catalog_snapshot_is_unchanged() -> TestResult {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/command_popup_en.snap"
        );
        let dump = popup_dump();
        if std::env::var_os("HARW_BLESS_POPUP").is_some() {
            let dir = std::path::Path::new(path)
                .parent()
                .ok_or(crate::test_support::TestError::Missing("snapshot dir"))?;
            std::fs::create_dir_all(dir).map_err(ctx("create tests/data"))?;
            std::fs::write(path, &dump).map_err(ctx("write snapshot"))?;
            return Ok(());
        }
        let expected = std::fs::read_to_string(path).map_err(ctx(
            "snapshot missing: run once with HARW_BLESS_POPUP=1 on the base",
        ))?;
        assert_eq!(
            dump, expected,
            "popup catalog changed; bless only if intended"
        );
        Ok(())
    }
}
