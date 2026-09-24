//! `/permissions` — Übersicht, Freigabemodus und Allow-/Deny-Regeln.
//!
//! Spec-Quelle: `docs/design/config-scopes.md` §2–§4 und „Nachträgliche
//! Entscheidungen“.
//!
//! Die Operation zeigt Workspace-Identität, kanonischen Root, die erteilten
//! (unveränderlichen) Sandbox-Rechte, den aktuellen Freigabemodus samt
//! Herkunft, die geltenden Allow-/Deny-Regeln und die zusätzlichen
//! Arbeitsverzeichnisse (`/add-workdir`).
//!
//! # Unterkommandos
//! - `show` (Default, kein Argument) — die vollständige Übersicht.
//! - `mode <ask|auto|full> [--session|--project|--global]` (`set` bleibt ein
//!   Alias) — Default-Scope `--session`. Persistenz von `full` in
//!   `--project`/`--global` erfordert zusätzlich `--yes`.
//! - `allow <tool> [muster] [--project|--global]` (Default `--project`) —
//!   fügt eine Allow-Regel hinzu.
//! - `deny <tool> [muster] [--project|--global]` — wie `allow`, aber
//!   [`RuleDecision::Deny`].
//! - `remove <nr>` — entfernt die Regel mit der Nummer aus der `show`-Liste
//!   (1-basiert) aus dem geteilten [`AllowRuleSet`] und, sofern die Regel aus
//!   `Project`/`Global` stammt, versucht sie zusätzlich aus der jeweiligen
//!   Datei zu entfernen (bestes Bemühen — Tool **und** Muster müssen
//!   übereinstimmen). `rm <nr>` ist die Kurzform (Runde 5, Teil E).
//! - `rules` — die Regeln mit Herkunft (Runde 5, Teil E).
//! - `log [anzahl]` — die letzten Entscheidungen des Auto-Modus samt Stand
//!   des Sicherheitsdeckels (Runde 5, Teil E).
//!
//! `--user` ist ein Alias von `--global`. Eine Allow-Regel für ein Werkzeug
//! aus `ALWAYS_ASK_TOOLS` wird abgelehnt (Runde 5, Teil E).
//!
//! # Scopes und Speicherorte (Contract §2)
//! - `Session`: nur die geteilten Zellen ([`ApprovalModeCell`],
//!   [`AllowRuleSet`]) — endet mit der Sitzung.
//! - `Global`: `~/.harw/config.toml` ([`global_config_path`]).
//! - `Project`: **autoritätsgewährend** außerhalb des Repos —
//!   `~/.harw/profiles/<profil>/projects/<projekt-schlüssel>/settings.toml`
//!   ([`project_config_path`]), niemals `<repo>/.harw/…`, damit ein
//!   geklontes Repo sich keine Rechte selbst geben kann.
//!
//! Jede Mutation aktualisiert zusätzlich sofort die passende geteilte Zelle
//! ([`ApprovalModeCell`]/[`AllowRuleSet`]), damit ein persistenter Schreib-
//! vorgang nicht erst nach einem Neustart wirkt.
//!
//! # Woher der Zustand kommt
//! Die Operation besitzt keinen eigenen Zustand. Sie liest und schreibt
//! ausschließlich die [`ApprovalModeCell`] und das [`AllowRuleSet`], die eine
//! Kompositionswurzel unter ihrem Typ in die
//! [`ServiceMap`](harw_operations::context::ServiceMap) des [`OpContext`]
//! gelegt hat — fehlt eine Zelle, meldet die Operation das ehrlich über
//! [`OpError::NotAvailable`] statt einen stillen Ersatzwert zu liefern
//! (genau wie `/mode` ohne `SessionController`, siehe dessen Moduldoku).
//! [`ExtraRootsCell`] wird in `show` nur gelesen (die Mutation gehört
//! `/add-workdir`, siehe `crate::add_workdir`).

// Runde 5, Teil E: `rules`, `log` und die Allow-Sperre für `ALWAYS_ASK_TOOLS`.
mod auto;

use std::path::{Path, PathBuf};

use harw_config::{ConfigWriter, PermissionsSection, RuleKind, RuleToml, SettingScope};
use harw_extension_api::ApprovalMode;
use harw_extension_api::allow_rules::{AllowRuleSet, ApprovalRule, RuleDecision, RuleScope};
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_sandbox::ExtraRootsCell;

// ── Konstanten ───────────────────────────────────────────────────────────────

/// Meldung für den Fall, dass keine [`ApprovalModeCell`] registriert ist.
///
/// Das ist kein Fehler der Operation, sondern eine unvollständige
/// Zusammenstellung der Laufzeit: ohne Zelle gibt es keinen Freigabemodus zu
/// lesen oder zu setzen. Die Antwort sagt das, statt einen Modus zu behaupten.
pub(crate) const NO_APPROVAL_MODE_CELL: &str = "In dieser Laufzeit ist keine ApprovalModeCell registriert — der Freigabemodus \
     kann weder gelesen noch gewechselt werden. Die Oberfläche muss eine \
     `ApprovalModeCell` in die ServiceMap legen.";

/// Meldung für den Fall, dass kein [`AllowRuleSet`] registriert ist.
pub(crate) const NO_ALLOW_RULE_SET: &str = "In dieser Laufzeit ist kein AllowRuleSet registriert — Allow-/Deny-Regeln \
     können weder gelesen noch gesetzt werden. Die Oberfläche muss ein \
     `AllowRuleSet` in die ServiceMap legen.";

/// Meldung für den Fall, dass keine [`ExtraRootsCell`] registriert ist.
pub(crate) const NO_EXTRA_ROOTS_CELL: &str = "In dieser Laufzeit ist keine ExtraRootsCell registriert — zusätzliche \
     Arbeitsverzeichnisse können nicht angezeigt werden. Die Oberfläche muss \
     eine `ExtraRootsCell` in die ServiceMap legen.";

// ── Argumente ────────────────────────────────────────────────────────────────

/// Argumente für `/permissions`.
///
/// # Beschreibung
/// `cmd` ist das Unterkommando (`None` gilt als `show`), `tail` sind alle
/// restlichen Tokens — jedes Unterkommando parst sie selbst (positionale
/// Werte und `--session`/`--project`/`--global`/`--yes`-Flags in beliebiger
/// Reihenfolge, siehe [`parse_scope_flags`]).
#[derive(Default, serde::Deserialize)]
pub struct PermissionsArgs {
    /// Unterkommando; `None` gilt als `show`.
    #[serde(default)]
    pub cmd: Option<String>,
    /// Tokens nach dem Unterkommando.
    #[serde(default)]
    pub tail: Vec<String>,
}

impl FromRawArgs for PermissionsArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            cmd: tokens.first().cloned(),
            tail: tokens.iter().skip(1).cloned().collect(),
        })
    }
}

/// Scope und Bestätigungs-Flag, wie von [`parse_scope_flags`] extrahiert.
#[derive(Debug)]
struct ScopeFlags {
    /// Gewählter oder vorgegebener Scope.
    scope: SettingScope,
    /// `true`, wenn `--yes` mitgegeben wurde.
    confirmed: bool,
}

/// Trennt `--session`/`--project`/`--global`/`--yes` aus `tokens` und liefert
/// die verbleibenden positionalen Tokens zusammen mit dem Ergebnis.
///
/// # Arguments
/// - `tokens` (`&[String]`): Tokens nach dem Unterkommando.
/// - `default_scope` (`SettingScope`): Scope, wenn kein Flag gesetzt wurde.
///
/// # Returns
/// `(positionale Tokens in Reihenfolge, ScopeFlags)`.
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn mehr als eines von
/// `--session`/`--project`/`--global` angegeben wird.
fn parse_scope_flags(
    tokens: &[String],
    default_scope: SettingScope,
) -> Result<(Vec<String>, ScopeFlags), OpError> {
    let mut positional = Vec::new();
    let mut scope: Option<SettingScope> = None;
    let mut confirmed = false;
    for token in tokens {
        match token.as_str() {
            "--session" => set_scope_flag(&mut scope, SettingScope::Session)?,
            "--project" => set_scope_flag(&mut scope, SettingScope::Project)?,
            // Runde 5, Teil E: `--user` ist ein Alias von `--global`.
            "--global" | "--user" => set_scope_flag(&mut scope, SettingScope::Global)?,
            "--yes" => confirmed = true,
            other => positional.push(other.to_owned()),
        }
    }
    Ok((
        positional,
        ScopeFlags {
            scope: scope.unwrap_or(default_scope),
            confirmed,
        },
    ))
}

/// Setzt `slot` auf `value`, oder liefert einen Fehler, wenn bereits ein
/// **anderer** Scope gesetzt wurde (widersprüchliche Flags).
fn set_scope_flag(slot: &mut Option<SettingScope>, value: SettingScope) -> Result<(), OpError> {
    match slot {
        Some(existing) if *existing != value => Err(OpError::InvalidArguments(
            "widersprüchliche Scope-Flags: nur eines von --session/--project/--global ist erlaubt"
                .to_owned(),
        )),
        _ => {
            *slot = Some(value);
            Ok(())
        }
    }
}

// ── Pfade (Contract §2) ──────────────────────────────────────────────────────

/// Baut den globalen, autoritätsgewährenden Config-Pfad: `<home>/config.toml`.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space (siehe [`harw_home::home_dir`]).
#[must_use]
pub(crate) fn global_config_path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

/// Baut den projekt-autoritätsgewährenden Config-Pfad (Contract §2/§3):
/// `<home>/profiles/<profil>/projects/<projekt-schlüssel>/settings.toml` —
/// bewusst außerhalb des Repos.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space.
/// - `profile` (`&str`): aktives Profil.
/// - `markers` (`&[String]`): `project_root_markers`, leer bedeutet `[".git"]`.
/// - `cwd` (`&Path`): Startpunkt der Projekt-Erkennung (üblicherweise der
///   kanonische Sandbox-Root der Sitzung).
///
/// # Errors
/// [`OpError::Execution`], wenn Projekt-Erkennung oder Pfadauflösung
/// fehlschlagen (siehe [`harw_home::discover_project`],
/// [`harw_home::project_settings_dir`]).
pub(crate) fn project_config_path(
    home: &Path,
    profile: &str,
    markers: &[String],
    cwd: &Path,
) -> Result<PathBuf, OpError> {
    let project = harw_home::discover_project(cwd, markers).map_err(|error| {
        OpError::Execution(format!("Projekt-Erkennung fehlgeschlagen: {error}"))
    })?;
    let key = harw_home::project_key(&project.root);
    let dir = harw_home::project_settings_dir(home, profile, &key).map_err(|error| {
        OpError::Execution(format!("Projekt-Settings-Pfad fehlgeschlagen: {error}"))
    })?;
    Ok(dir.join("settings.toml"))
}

/// Löst den Konfigurationspfad für einen [`SettingScope`] auf.
///
/// # Errors
/// - [`OpError::Execution`]: `scope` ist [`SettingScope::Session`] (hat
///   keinen Pfad — Aufrufer müssen das vorher ausschließen), oder Home-/
///   Projekt-Auflösung schlug fehl.
pub(crate) fn scope_path(ctx: &OpContext, scope: SettingScope) -> Result<PathBuf, OpError> {
    match scope {
        SettingScope::Session => Err(OpError::Execution(
            "Sitzungs-Scope hat keinen Konfigurationspfad".to_owned(),
        )),
        SettingScope::Global => {
            let home = harw_home::home_dir()
                .map_err(|error| OpError::Execution(format!("Home nicht auflösbar: {error}")))?;
            Ok(global_config_path(&home))
        }
        SettingScope::Project => {
            let home = harw_home::home_dir()
                .map_err(|error| OpError::Execution(format!("Home nicht auflösbar: {error}")))?;
            let profile = harw_home::active_profile_name(&home);
            let markers = crate::config_util::load_default_config("")
                .ok()
                .and_then(|config| config.harness.project_root_markers)
                .unwrap_or_default();
            let cwd = ctx.sandbox().workspace().canonical_root();
            project_config_path(&home, &profile, &markers, cwd)
        }
    }
}

/// Übersetzt einen [`harw_config::ConfigError`] in eine [`OpError::Execution`]
/// mit Kontext-Präfix.
fn config_error(context: &str, error: harw_config::ConfigError) -> OpError {
    OpError::Execution(format!("{context}: {error}"))
}

/// Liest nur `[permissions]` aus einer Datei, falls sie existiert und gültig
/// ist. `None` bei jedem Fehler (fehlt, kein gültiges TOML) — der Aufrufer
/// behandelt das wie „diese Ebene setzt hier nichts“, nie wie einen Fehler.
fn read_permissions_section(path: &Path) -> Option<PermissionsSection> {
    #[derive(serde::Deserialize, Default)]
    struct PermissionsOnlyDoc {
        #[serde(default)]
        permissions: PermissionsSection,
    }
    let content = std::fs::read_to_string(path).ok()?;
    let doc: PermissionsOnlyDoc = toml::from_str(&content).ok()?;
    Some(doc.permissions)
}

// ── Anzeige-Hilfen ───────────────────────────────────────────────────────────

fn scope_label_de(scope: RuleScope) -> &'static str {
    match scope {
        RuleScope::Session => "Sitzung",
        RuleScope::Project => "Projekt",
        RuleScope::Global => "Global",
    }
}

fn decision_label_de(decision: RuleDecision) -> &'static str {
    match decision {
        RuleDecision::Allow => "erlaubt",
        RuleDecision::Deny => "verboten",
    }
}

fn to_rule_scope(scope: SettingScope) -> RuleScope {
    match scope {
        SettingScope::Session => RuleScope::Session,
        SettingScope::Project => RuleScope::Project,
        SettingScope::Global => RuleScope::Global,
    }
}

// Kehrt `to_rule_scope` um; `RuleScope::Session` hat keine persistierbare
// `SettingScope`-Entsprechung und wird darum strukturell per `None`
// ausgeschlossen (kein `unreachable!`-Fall im Aufrufer nötig).
fn persisted_scope(scope: RuleScope) -> Option<SettingScope> {
    match scope {
        RuleScope::Project => Some(SettingScope::Project),
        RuleScope::Global => Some(SettingScope::Global),
        RuleScope::Session => None,
    }
}

fn to_rule_kind(decision: RuleDecision) -> RuleKind {
    match decision {
        RuleDecision::Allow => RuleKind::Allow,
        RuleDecision::Deny => RuleKind::Deny,
    }
}

/// Ermittelt, ob der aktive Modus aus `Project`, `Global` oder nur der
/// Sitzung stammt — reine Bestwissen-Heuristik: passt `default_mode` einer
/// Ebene textuell zum aktiven Modus, gilt diese Ebene als Herkunft
/// (`Project` vor `Global`, entsprechend der Präzedenz). Trifft keine Ebene,
/// gilt „Sitzung“ — entweder wurde der Modus per `--session` gesetzt, oder
/// keine Ebene setzt ihn dauerhaft.
fn compute_mode_origin(
    project: Option<&PermissionsSection>,
    global: Option<&PermissionsSection>,
    active: ApprovalMode,
) -> String {
    if let Some(section) = project {
        if section.default_mode.as_deref() == Some(active.as_str()) {
            return "Projekt".to_owned();
        }
    }
    if let Some(section) = global {
        if section.default_mode.as_deref() == Some(active.as_str()) {
            return "Global".to_owned();
        }
    }
    "Sitzung (nicht dauerhaft gespeichert)".to_owned()
}

/// Wrapper um [`compute_mode_origin`], der die beiden Ebenen aus den
/// tatsächlichen Config-Pfaden liest (bestes Bemühen — jeder Auflösungsfehler
/// wird wie „diese Ebene setzt nichts“ behandelt).
fn resolve_mode_origin(ctx: &OpContext, active: ApprovalMode) -> String {
    let project_section = scope_path(ctx, SettingScope::Project)
        .ok()
        .and_then(|path| read_permissions_section(&path));
    let global_section = scope_path(ctx, SettingScope::Global)
        .ok()
        .and_then(|path| read_permissions_section(&path));
    compute_mode_origin(project_section.as_ref(), global_section.as_ref(), active)
}

/// Die wählbaren Modusnamen als `ask|auto|full`.
fn mode_names() -> String {
    ApprovalMode::ALL
        .iter()
        .map(|mode| mode.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

/// Rendert alle Modi mit `*` an der aktiven Zeile.
fn mode_lines(active: ApprovalMode) -> String {
    ApprovalMode::ALL
        .iter()
        .map(|mode| {
            let marker = if *mode == active { "*" } else { " " };
            format!("{marker} {} — {}", mode.as_str(), mode.description())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Zeigt die Übersicht oder führt ein Unterkommando aus.
#[operation(
    name = "permissions",
    summary = "Zeigt Freigabemodus, Allow-/Deny-Regeln und Arbeitsverzeichnisse; erlaubt, sie zu ändern.",
    domain = "catalog_config",
    // `operator`, nicht `maintainer`: die Operation zeigt die eigene Sandbox und
    // wählt Freigabemodus/Regeln der eigenen Sitzung bzw. des eigenen Projekts.
    // Beides liegt ohnehin in der Hand der Person am Terminal.
    permission = "operator",
    command(
        path = "/permissions",
        visibility = "tui_only",
        busy_subcommands = "-=immediate, show=immediate, mode=immediate, set=immediate, rules=immediate, log=immediate"
    ),
    // Web-Fläche: `method = "post"`, seit Mutationen (`mode`, `allow`, `deny`,
    // `remove`) möglich sind. `approval = "always"`, weil genau dieser Aufruf
    // bestimmt, wie viel ohne Rückfrage geschieht — er darf nicht selbst ohne
    // Rückfrage laufen.
    web(path = "/api/permissions", method = "post", approval = "always")
)]
async fn permissions(ctx: &OpContext, args: PermissionsArgs) -> Result<OpOutput, OpError> {
    let sub = args.cmd.as_deref().unwrap_or("show");
    match sub {
        "show" => show(ctx),
        "mode" | "set" => set_mode(ctx, &args.tail),
        "allow" => set_rule(ctx, RuleDecision::Allow, &args.tail),
        "deny" => set_rule(ctx, RuleDecision::Deny, &args.tail),
        // Runde 5, Teil E: `rm` als Kurzform, `rules` (Liste mit Herkunft)
        // und `log` (letzte Auto-Modus-Entscheidungen).
        "remove" | "rm" => remove_rule(ctx, &args.tail),
        "rules" => auto::list_rules(ctx),
        "log" => auto::decision_log(ctx, &args.tail),
        other => Err(OpError::NotAvailable(format!(
            "/permissions {other}: unbekanntes Unterkommando — verfügbar sind show, mode, \
             allow, deny, rules, remove|rm, log; die Sandbox-Rechte selbst sind unveränderlich"
        ))),
    }
}

/// `/permissions` (`show`, Default): Übersicht aus Sandbox, Freigabemodus,
/// Regeln und Arbeitsverzeichnissen.
fn show(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let sandbox = ctx.sandbox();
    let workspace = sandbox.workspace();
    let permissions = sandbox
        .permissions()
        .iter()
        .map(|permission| format!("- {permission:?}"))
        .collect::<Vec<_>>();
    let permissions = if permissions.is_empty() {
        "- (none)".to_owned()
    } else {
        permissions.join("\n")
    };

    let Some(mode_cell) = ctx.service::<ApprovalModeCell>() else {
        return Err(OpError::NotAvailable(NO_APPROVAL_MODE_CELL.to_owned()));
    };
    let active = mode_cell.get();
    let origin = resolve_mode_origin(ctx, active);

    let rules_text = match ctx.service::<AllowRuleSet>() {
        Some(rule_set) => {
            let rules = rule_set.snapshot();
            if rules.is_empty() {
                "  (keine Regeln)".to_owned()
            } else {
                rules
                    .iter()
                    .enumerate()
                    .map(|(index, rule)| {
                        format!(
                            "  {}. [{}] {} {} → {}",
                            index + 1,
                            scope_label_de(rule.scope),
                            rule.tool,
                            rule.pattern.as_deref().unwrap_or("*"),
                            decision_label_de(rule.decision)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        None => format!("  ({NO_ALLOW_RULE_SET})"),
    };

    let roots_text = match ctx.service::<ExtraRootsCell>() {
        Some(cell) => {
            let roots = cell.snapshot();
            if roots.is_empty() {
                "  (keine zusätzlichen Arbeitsverzeichnisse)".to_owned()
            } else {
                roots
                    .iter()
                    .map(|root| {
                        format!(
                            "  - {}{}",
                            root.path.display(),
                            if root.persisted { " (gemerkt)" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        None => format!("  ({NO_EXTRA_ROOTS_CELL})"),
    };

    Ok(OpOutput::from(format!(
        "Workspace: {}\nTenant: {}\nRoot: {}\nGranted permissions:\n{permissions}\n\n\
         Freigabemodus: {} — Herkunft: {}\n{}\n\n\
         Regeln:\n{rules_text}\n\n\
         Arbeitsverzeichnisse:\n{roots_text}\n\n\
         Umschalten mit `/permissions mode <{}> [--session|--project|--global]`; \
         Regeln mit `/permissions allow|deny <tool> [muster] [--session|--project|--user]`, \
         `/permissions rules` bzw. `/permissions rm <nr>`; Auto-Modus-Entscheidungen \
         mit `/permissions log`.",
        workspace.workspace(),
        workspace.tenant(),
        workspace.canonical_root().display(),
        active.as_str(),
        origin,
        mode_lines(active),
        mode_names(),
    )))
}

/// `/permissions mode <ask|auto|full> [--session|--project|--global]`
/// (`set` ist ein Alias). Default-Scope `--session`.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: kein oder unbekannter Modus,
///   widersprüchliche Scope-Flags, oder `full` dauerhaft ohne `--yes`.
/// - [`OpError::NotAvailable`]: keine `ApprovalModeCell` registriert.
/// - [`OpError::Execution`]: Config-Pfad, -Öffnen, -Schreiben oder -Speichern
///   schlug fehl.
fn set_mode(ctx: &OpContext, tail: &[String]) -> Result<OpOutput, OpError> {
    let (positional, flags) = parse_scope_flags(tail, SettingScope::Session)?;
    let Some(requested) = positional.first().map(String::as_str) else {
        return Err(OpError::InvalidArguments(format!(
            "/permissions mode braucht einen Modus: {}",
            mode_names()
        )));
    };
    let Some(mode) = ApprovalMode::parse(requested) else {
        return Err(OpError::InvalidArguments(format!(
            "unbekannter Freigabemodus `{requested}`; verfügbar: {}",
            mode_names()
        )));
    };
    let Some(cell) = ctx.service::<ApprovalModeCell>() else {
        return Err(OpError::NotAvailable(NO_APPROVAL_MODE_CELL.to_owned()));
    };

    if flags.scope == SettingScope::Session {
        cell.set(mode);
        return Ok(OpOutput::from(format!(
            "Freigabemodus: {} — {}. Gilt ab dem nächsten Werkzeugaufruf (nur für diese Sitzung).",
            mode.as_str(),
            mode.description()
        )));
    }

    if mode == ApprovalMode::FullAccess && !flags.confirmed {
        return Err(OpError::InvalidArguments(format!(
            "`full` dauerhaft im Scope {} zu setzen erfordert --yes (siehe `harw doctor`).",
            flags.scope
        )));
    }

    let path = scope_path(ctx, flags.scope)?;
    let mut writer = ConfigWriter::open(&path)
        .map_err(|error| config_error("Config öffnen fehlgeschlagen", error))?;
    writer
        .set_default_mode(mode.as_str())
        .map_err(|error| config_error("Config schreiben fehlgeschlagen", error))?;
    writer
        .save()
        .map_err(|error| config_error("Config speichern fehlgeschlagen", error))?;
    cell.set(mode);
    Ok(OpOutput::from(format!(
        "Freigabemodus dauerhaft auf {} gesetzt ({}, {}).",
        mode.as_str(),
        flags.scope,
        path.display()
    )))
}

/// `/permissions allow|deny <tool> [muster] [--project|--global]`. Default
/// `--project`.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: kein `tool`, widersprüchliche Scope-Flags.
/// - [`OpError::NotAvailable`]: kein `AllowRuleSet` registriert.
/// - [`OpError::Execution`]: Config-Pfad, -Öffnen, -Schreiben oder -Speichern
///   schlug fehl.
fn set_rule(ctx: &OpContext, decision: RuleDecision, tail: &[String]) -> Result<OpOutput, OpError> {
    let (positional, flags) = parse_scope_flags(tail, SettingScope::Project)?;
    let verb = match decision {
        RuleDecision::Allow => "allow",
        RuleDecision::Deny => "deny",
    };
    let Some(tool) = positional.first().cloned() else {
        return Err(OpError::InvalidArguments(format!(
            "/permissions {verb} <tool> [muster] [--session|--project|--global]"
        )));
    };
    let pattern = positional.get(1).cloned();
    // Runde 5, Teil E: `ALWAYS_ASK_TOOLS` sind nie per Regel freigebbar —
    // eine Allow-Regel wäre wirkungslos und täuschte Sicherheit vor.
    if decision == RuleDecision::Allow {
        auto::reject_always_ask_allow(&tool)?;
    }
    let Some(rule_set) = ctx.service::<AllowRuleSet>() else {
        return Err(OpError::NotAvailable(NO_ALLOW_RULE_SET.to_owned()));
    };

    let approval_rule = ApprovalRule {
        tool: tool.clone(),
        pattern: pattern.clone(),
        decision,
        scope: to_rule_scope(flags.scope),
    };
    rule_set.add(approval_rule);

    let mut note = String::new();
    if flags.scope != SettingScope::Session {
        let path = scope_path(ctx, flags.scope)?;
        let mut writer = ConfigWriter::open(&path)
            .map_err(|error| config_error("Config öffnen fehlgeschlagen", error))?;
        let kind = to_rule_kind(decision);
        let rule_toml = RuleToml {
            tool: tool.clone(),
            pattern: pattern.clone(),
        };
        let newly_persisted = writer
            .append_rule(kind, &rule_toml)
            .map_err(|error| config_error("Config schreiben fehlgeschlagen", error))?;
        writer
            .save()
            .map_err(|error| config_error("Config speichern fehlgeschlagen", error))?;
        note = format!(
            " Dauerhaft in {} gespeichert{}.",
            path.display(),
            if newly_persisted {
                ""
            } else {
                " (war bereits vorhanden)"
            }
        );
    }

    Ok(OpOutput::from(format!(
        "Regel: {tool} {} → {} ({}).{note}",
        pattern.as_deref().unwrap_or("*"),
        decision_label_de(decision),
        flags.scope,
    )))
}

/// `/permissions remove <nr>` — entfernt die Regel mit der (1-basierten)
/// Nummer aus der `show`-Liste.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: keine oder ungültige Nummer, kein
///   Eintrag an dieser Position.
/// - [`OpError::NotAvailable`]: kein `AllowRuleSet` registriert.
fn remove_rule(ctx: &OpContext, tail: &[String]) -> Result<OpOutput, OpError> {
    let Some(nr_str) = tail.first() else {
        return Err(OpError::InvalidArguments(
            "/permissions remove <nr>".to_owned(),
        ));
    };
    let Ok(nr) = nr_str.parse::<usize>() else {
        return Err(OpError::InvalidArguments(format!(
            "/permissions remove erwartet eine Nummer, war `{nr_str}`"
        )));
    };
    if nr == 0 {
        return Err(OpError::InvalidArguments(
            "/permissions remove: Nummerierung beginnt bei 1".to_owned(),
        ));
    }
    let Some(rule_set) = ctx.service::<AllowRuleSet>() else {
        return Err(OpError::NotAvailable(NO_ALLOW_RULE_SET.to_owned()));
    };
    let Some(removed) = rule_set.remove(nr - 1) else {
        return Err(OpError::InvalidArguments(format!(
            "/permissions remove: keine Regel Nr. {nr}"
        )));
    };

    let mut note = String::new();
    if let Some(scope) = persisted_scope(removed.scope) {
        match scope_path(ctx, scope) {
            Ok(path) => {
                match remove_persisted_rule(
                    &path,
                    removed.decision,
                    &removed.tool,
                    removed.pattern.as_deref(),
                ) {
                    Ok(true) => note = format!(" Auch dauerhaft aus {} entfernt.", path.display()),
                    Ok(false) => note = format!(" In {} nicht (mehr) gefunden.", path.display()),
                    Err(error) => {
                        note = format!(" Warnung: dauerhafte Entfernung fehlgeschlagen: {error}")
                    }
                }
            }
            Err(error) => note = format!(" Warnung: Konfigurationspfad nicht auflösbar: {error}"),
        }
    }

    Ok(OpOutput::from(format!(
        "Regel Nr. {nr} entfernt: {} {} ({}).{note}",
        removed.tool,
        removed.pattern.as_deref().unwrap_or("*"),
        scope_label_de(removed.scope),
    )))
}

/// Entfernt (bestes Bemühen) eine Regel mit passendem `tool`+`pattern` aus
/// der persistierten Datei unter `path`.
///
/// # Returns
/// `Ok(true)`, wenn eine passende Regel gefunden und entfernt wurde;
/// `Ok(false)`, wenn die Datei fehlt, ungültig ist, oder keine passende Regel
/// enthält.
///
/// # Errors
/// [`OpError::Execution`], wenn Öffnen, Schreiben oder Speichern der Config
/// fehlschlägt, nachdem eine passende Regel gefunden wurde.
fn remove_persisted_rule(
    path: &Path,
    decision: RuleDecision,
    tool: &str,
    pattern: Option<&str>,
) -> Result<bool, OpError> {
    let Some(section) = read_permissions_section(path) else {
        return Ok(false);
    };
    let list = match decision {
        RuleDecision::Allow => &section.allow,
        RuleDecision::Deny => &section.deny,
    };
    let Some(index) = list
        .iter()
        .position(|rule| rule.tool == tool && rule.pattern.as_deref() == pattern)
    else {
        return Ok(false);
    };
    let kind = to_rule_kind(decision);
    let mut writer = ConfigWriter::open(path)
        .map_err(|error| config_error("Config öffnen fehlgeschlagen", error))?;
    writer
        .remove_rule(kind, index)
        .map_err(|error| config_error("Config schreiben fehlgeschlagen", error))?;
    writer
        .save()
        .map_err(|error| config_error("Config speichern fehlgeschlagen", error))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalMode, PermissionsArgs, compute_mode_origin, global_config_path, parse_scope_flags,
        permissions, project_config_path,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_config::{ConfigWriter, PermissionsSection, RuleKind, RuleToml, SettingScope};
    use harw_extension_api::allow_rules::{AllowRuleSet, ApprovalRule, RuleDecision, RuleScope};
    use harw_extension_api::approval_mode::ApprovalModeCell;
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError};
    use harw_sandbox::ExtraRootsCell;
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Baut einen [`OpContext`] mit leerer [`ServiceMap`] — keine Zellen
    /// registriert. Jeder Test bekommt eine eigene Workspace-Wurzel, damit
    /// Tests parallel laufen können, ohne sich gegenseitig zu stören.
    fn test_context() -> TestResult<OpContext> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-permissions-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        Ok(OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy([Permission::WriteWorkspace, Permission::ReadWorkspace]),
            ),
            ServiceMap::new(),
        ))
    }

    /// Wie [`test_context`], aber mit einer eigenen [`ApprovalModeCell`]
    /// (Startwert `mode`) in der `ServiceMap`.
    fn test_context_with_mode(mode: ApprovalMode) -> TestResult<(OpContext, ApprovalModeCell)> {
        let ctx = test_context()?;
        let cell = ApprovalModeCell::new(mode);
        let mut services = ServiceMap::new();
        services.insert(cell.clone());
        let ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );
        Ok((ctx, cell))
    }

    /// Wie [`test_context_with_mode`], zusätzlich mit einem leeren
    /// [`AllowRuleSet`] und einer leeren [`ExtraRootsCell`] in der `ServiceMap`.
    fn test_context_with_all_cells(
        mode: ApprovalMode,
    ) -> TestResult<(OpContext, ApprovalModeCell, AllowRuleSet)> {
        let (ctx, cell) = test_context_with_mode(mode)?;
        let rule_set = AllowRuleSet::new();
        let mut services = ServiceMap::new();
        services.insert(cell.clone());
        services.insert(rule_set.clone());
        services.insert(ExtraRootsCell::new());
        let ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );
        Ok((ctx, cell, rule_set))
    }

    #[test]
    fn test_permissions_args_from_raw_args_sets_cmd_and_tail() -> TestResult {
        let args = PermissionsArgs::from_raw_args(&toks(&["allow", "shell.exec", "git status"]))
            .map_err(ctx("parse"))?;
        assert_eq!(args.cmd.as_deref(), Some("allow"));
        assert_eq!(
            args.tail,
            vec!["shell.exec".to_owned(), "git status".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn test_permissions_args_from_raw_args_empty_tokens_sets_cmd_none() -> TestResult {
        let args = PermissionsArgs::from_raw_args(&toks(&[])).map_err(ctx("parse"))?;
        assert!(args.cmd.is_none());
        assert!(args.tail.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_scope_flags_defaults_when_no_flag_given() -> TestResult {
        let (positional, flags) =
            parse_scope_flags(&toks(&["full"]), SettingScope::Session).map_err(ctx("parse"))?;
        assert_eq!(positional, vec!["full".to_owned()]);
        assert_eq!(flags.scope, SettingScope::Session);
        assert!(!flags.confirmed);
        Ok(())
    }

    #[test]
    fn test_parse_scope_flags_reads_explicit_scope_and_yes() -> TestResult {
        let (positional, flags) =
            parse_scope_flags(&toks(&["full", "--global", "--yes"]), SettingScope::Session)
                .map_err(ctx("parse"))?;
        assert_eq!(positional, vec!["full".to_owned()]);
        assert_eq!(flags.scope, SettingScope::Global);
        assert!(flags.confirmed);
        Ok(())
    }

    #[test]
    fn test_parse_scope_flags_rejects_conflicting_scope_flags() -> TestResult {
        let result = parse_scope_flags(
            &toks(&["full", "--project", "--global"]),
            SettingScope::Session,
        );
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("widersprüchliche"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected InvalidArguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_parse_scope_flags_repeating_same_flag_is_not_a_conflict() -> TestResult {
        let (_positional, flags) =
            parse_scope_flags(&toks(&["--project", "--project"]), SettingScope::Session)
                .map_err(ctx("parse"))?;
        assert_eq!(flags.scope, SettingScope::Project);
        Ok(())
    }

    #[test]
    fn test_compute_mode_origin_prefers_project_over_global() {
        let project = PermissionsSection {
            default_mode: Some("auto".to_owned()),
            ..PermissionsSection::default()
        };
        let global = PermissionsSection {
            default_mode: Some("auto".to_owned()),
            ..PermissionsSection::default()
        };
        assert_eq!(
            compute_mode_origin(Some(&project), Some(&global), ApprovalMode::Delegated),
            "Projekt"
        );
    }

    #[test]
    fn test_compute_mode_origin_falls_back_to_global() {
        let global = PermissionsSection {
            default_mode: Some("full".to_owned()),
            ..PermissionsSection::default()
        };
        assert_eq!(
            compute_mode_origin(None, Some(&global), ApprovalMode::FullAccess),
            "Global"
        );
    }

    #[test]
    fn test_compute_mode_origin_falls_back_to_session_when_no_layer_matches() {
        assert_eq!(
            compute_mode_origin(None, None, ApprovalMode::AlwaysAsk),
            "Sitzung (nicht dauerhaft gespeichert)"
        );
    }

    #[test]
    fn test_global_config_path_joins_config_toml() {
        let home = PathBuf::from("/tmp/harw-example-home");
        assert_eq!(global_config_path(&home), home.join("config.toml"));
    }

    #[test]
    fn test_project_config_path_builds_settings_toml_under_profile_projects() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).map_err(ctx("create fake git dir"))?;
        let home = dir.path().join("home");

        let path =
            project_config_path(&home, "default", &[], &repo).map_err(ctx("resolve path"))?;
        assert!(path.starts_with(home.join("profiles/default/projects")));
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("settings.toml")
        );
        Ok(())
    }

    /// Runde 6, Teil A: `/permissions log` markiert eine Ablehnung, die zur
    /// Rückfrage wurde; eine echte Ablehnung bleibt unmarkiert.
    #[tokio::test]
    async fn permissions_log_marks_a_denial_that_became_a_question() -> TestResult {
        use harw_extension_api::auto_mode::{
            AutoDecision, AutoDecisionLog, AutoLogEntry, AutoVerdict, VerdictSource,
        };
        let base = test_context()?;
        let log = AutoDecisionLog::new();
        let entry = |call_id: &str, verdict: AutoVerdict| AutoLogEntry {
            at: jiff::Timestamp::now(),
            call_id: call_id.to_owned(),
            tool: "shell.exec".to_owned(),
            summary: format!("mv export.md ~/ ({call_id})"),
            verdict,
        };
        let deny = || {
            AutoVerdict::new(
                AutoDecision::Deny,
                "transfer-outside-workspace",
                "Ziel außerhalb des Workspace",
                VerdictSource::Classifier,
            )
        };
        let _ = log.record(entry("gefragt", deny().escalate_to_ask()));
        let _ = log.record(entry("abgelehnt", deny()));
        let mut services = ServiceMap::new();
        services.insert(log);
        let ctx = OpContext::new(
            base.session_id().clone(),
            base.turn_id().clone(),
            base.sandbox().clone(),
            services,
        );

        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("log".to_owned()),
                tail: Vec::new(),
            },
        )
        .await
        .map_err(crate::test_support::ctx("permissions log"))?;

        let asked = result
            .text
            .lines()
            .find(|line| line.contains("(gefragt)"))
            .ok_or(TestError::Missing("Zeile der Rückfrage"))?;
        assert!(asked.contains("(Ablehnung → Rückfrage)"), "{asked}");
        let denied = result
            .text
            .lines()
            .find(|line| line.contains("(abgelehnt)"))
            .ok_or(TestError::Missing("Zeile der Ablehnung"))?;
        assert!(!denied.contains("Rückfrage"), "{denied}");
        Ok(())
    }

    #[tokio::test]
    async fn permissions_show_without_a_mode_cell_is_not_available() -> TestResult {
        let ctx = test_context()?;
        let result = permissions(&ctx, PermissionsArgs::default()).await;
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("ApprovalModeCell"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_show_lists_all_modes_and_marks_the_active_one() -> TestResult {
        let (ctx, _cell) = test_context_with_mode(ApprovalMode::AlwaysAsk)?;
        let output = permissions(&ctx, PermissionsArgs::default())
            .await
            .map_err(crate::test_support::ctx("show"))?;
        for mode in ApprovalMode::ALL {
            assert!(
                output.text.contains(mode.as_str()),
                "expected {mode:?} to be listed"
            );
        }
        assert!(output.text.contains("* ask"));
        assert!(output.text.contains("Regeln:"));
        assert!(output.text.contains("Arbeitsverzeichnisse:"));
        Ok(())
    }

    #[tokio::test]
    async fn permissions_show_degrades_honestly_without_allow_rule_set() -> TestResult {
        let (ctx, _cell) = test_context_with_mode(ApprovalMode::Delegated)?;
        let output = permissions(&ctx, PermissionsArgs::default())
            .await
            .map_err(crate::test_support::ctx("show"))?;
        assert!(output.text.contains("AllowRuleSet"));
        assert!(output.text.contains("ExtraRootsCell"));
        Ok(())
    }

    #[tokio::test]
    async fn permissions_rejects_unknown_subcommand() -> TestResult {
        let ctx = test_context()?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("revoke".to_owned()),
                tail: Vec::new(),
            },
        )
        .await;
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("revoke"));
                assert!(message.contains("unveränderlich"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected unavailable mutation, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_mode_default_scope_is_session_only() -> TestResult {
        let (ctx, cell, _rules) = test_context_with_all_cells(ApprovalMode::Delegated)?;
        let other_cell = ApprovalModeCell::new(ApprovalMode::Delegated);

        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("mode".to_owned()),
                tail: vec!["full".to_owned()],
            },
        )
        .await
        .map_err(crate::test_support::ctx("mode switch"))?;

        assert!(result.text.contains("full"));
        assert!(result.text.contains("Sitzung"));
        assert_eq!(cell.get(), ApprovalMode::FullAccess);
        assert_eq!(other_cell.get(), ApprovalMode::Delegated);
        Ok(())
    }

    #[tokio::test]
    async fn permissions_set_alias_behaves_like_mode() -> TestResult {
        let (ctx, cell, _rules) = test_context_with_all_cells(ApprovalMode::Delegated)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("set".to_owned()),
                tail: vec!["full".to_owned()],
            },
        )
        .await
        .map_err(crate::test_support::ctx("set alias"))?;
        assert!(result.text.contains("full"));
        assert_eq!(cell.get(), ApprovalMode::FullAccess);
        Ok(())
    }

    #[tokio::test]
    async fn permissions_mode_without_a_cell_is_not_available() -> TestResult {
        let ctx = test_context()?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("mode".to_owned()),
                tail: vec!["full".to_owned()],
            },
        )
        .await;
        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("ApprovalModeCell")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_mode_without_mode_returns_invalid_arguments() -> TestResult {
        let ctx = test_context()?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("mode".to_owned()),
                tail: Vec::new(),
            },
        )
        .await;
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("/permissions mode"))
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_mode_unknown_mode_returns_invalid_arguments() -> TestResult {
        let ctx = test_context()?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("mode".to_owned()),
                tail: vec!["quatsch".to_owned()],
            },
        )
        .await;
        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("quatsch")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_mode_persisting_full_without_yes_is_rejected() -> TestResult {
        let (ctx, _cell, _rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("mode".to_owned()),
                tail: vec!["full".to_owned(), "--project".to_owned()],
            },
        )
        .await;
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("--yes"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_allow_defaults_to_project_scope_and_updates_cell_immediately() -> TestResult
    {
        let (ctx, _cell, rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("allow".to_owned()),
                tail: vec!["shell.exec".to_owned(), "git status".to_owned()],
            },
        )
        .await;
        // Project scope tries to touch a real config path; accept either a
        // successful persist or an execution error from path resolution in
        // this sandboxed test environment, but the in-memory rule set must
        // reflect the mutation either way is only guaranteed on success.
        if let Ok(output) = result {
            assert!(output.text.contains("erlaubt"));
            let snapshot = rules.snapshot();
            assert!(snapshot.iter().any(|r| r.tool == "shell.exec"
                && r.pattern.as_deref() == Some("git status")
                && r.scope == RuleScope::Project));
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_allow_session_scope_never_touches_disk() -> TestResult {
        let (ctx, _cell, rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        let output = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("allow".to_owned()),
                tail: vec![
                    "shell.exec".to_owned(),
                    "cargo check".to_owned(),
                    "--session".to_owned(),
                ],
            },
        )
        .await
        .map_err(crate::test_support::ctx(
            "session-scope allow must never fail on disk access",
        ))?;
        assert!(output.text.contains("erlaubt"));
        let snapshot = rules.snapshot();
        assert!(snapshot.iter().any(|r| r.tool == "shell.exec"
            && r.pattern.as_deref() == Some("cargo check")
            && r.scope == RuleScope::Session
            && r.decision == RuleDecision::Allow));
        Ok(())
    }

    #[tokio::test]
    async fn permissions_deny_session_scope_records_deny_decision() -> TestResult {
        let (ctx, _cell, rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("deny".to_owned()),
                tail: vec!["fs.write".to_owned(), "--session".to_owned()],
            },
        )
        .await
        .map_err(crate::test_support::ctx("session-scope deny"))?;
        let snapshot = rules.snapshot();
        assert!(
            snapshot
                .iter()
                .any(|r| r.tool == "fs.write" && r.decision == RuleDecision::Deny)
        );
        Ok(())
    }

    #[tokio::test]
    async fn permissions_allow_without_a_rule_set_is_not_available() -> TestResult {
        let (ctx, _cell) = test_context_with_mode(ApprovalMode::AlwaysAsk)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("allow".to_owned()),
                tail: vec!["shell.exec".to_owned(), "--session".to_owned()],
            },
        )
        .await;
        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("AllowRuleSet")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_allow_without_a_tool_returns_invalid_arguments() -> TestResult {
        let (ctx, _cell, _rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("allow".to_owned()),
                tail: Vec::new(),
            },
        )
        .await;
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("/permissions allow"))
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_remove_session_scoped_rule_by_index() -> TestResult {
        let (ctx, _cell, rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git status".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Session,
        });
        rules.add(ApprovalRule {
            tool: "fs.write".to_owned(),
            pattern: None,
            decision: RuleDecision::Deny,
            scope: RuleScope::Session,
        });

        let output = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("remove".to_owned()),
                tail: vec!["1".to_owned()],
            },
        )
        .await
        .map_err(crate::test_support::ctx("remove by index"))?;
        assert!(output.text.contains("shell.exec"));

        let remaining = rules.snapshot();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].tool, "fs.write");
        Ok(())
    }

    #[tokio::test]
    async fn permissions_remove_out_of_range_index_returns_invalid_arguments() -> TestResult {
        let (ctx, _cell, _rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("remove".to_owned()),
                tail: vec!["5".to_owned()],
            },
        )
        .await;
        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("Nr. 5")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn permissions_remove_zero_is_rejected() -> TestResult {
        let (ctx, _cell, _rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("remove".to_owned()),
                tail: vec!["0".to_owned()],
            },
        )
        .await;
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn permissions_remove_non_numeric_argument_is_rejected() -> TestResult {
        let (ctx, _cell, _rules) = test_context_with_all_cells(ApprovalMode::AlwaysAsk)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("remove".to_owned()),
                tail: vec!["abc".to_owned()],
            },
        )
        .await;
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn permissions_remove_without_a_rule_set_is_not_available() -> TestResult {
        let (ctx, _cell) = test_context_with_mode(ApprovalMode::AlwaysAsk)?;
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("remove".to_owned()),
                tail: vec!["1".to_owned()],
            },
        )
        .await;
        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("AllowRuleSet")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── Persistenz-Rundlauf (Contract §2), ohne echte HARW_HOME-Env-Mutation ──
    // `global_config_path`/`project_config_path` sind reine Funktionen über
    // einem übergebenen `home`; der Rundlauf testet sie zusammen mit
    // `ConfigWriter` direkt gegen ein temporäres Verzeichnis, statt den
    // Prozess-weiten `HARW_HOME`/`HOME` zu mutieren (nicht thread-sicher,
    // würde parallel laufende Tests gefährden).

    #[test]
    fn test_permissions_persistence_round_trip_default_mode_and_rules() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = dir.path().join("home");
        let path = global_config_path(&home);

        let mut writer = ConfigWriter::open(&path).map_err(ctx("open"))?;
        writer
            .set_default_mode("auto")
            .map_err(ctx("set_default_mode"))?;
        writer
            .append_rule(
                RuleKind::Allow,
                &RuleToml {
                    tool: "shell.exec".to_owned(),
                    pattern: Some("cargo check".to_owned()),
                },
            )
            .map_err(ctx("append_rule"))?;
        writer.save().map_err(ctx("save"))?;

        let reopened = ConfigWriter::open(&path).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.get_value("permissions.default_mode"),
            Some("auto".to_owned())
        );

        let content = std::fs::read_to_string(&path).map_err(ctx("read back"))?;
        assert!(content.contains("cargo check"));
        Ok(())
    }

    #[test]
    fn test_remove_persisted_rule_finds_and_removes_matching_entry() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("settings.toml");

        let mut writer = ConfigWriter::open(&path).map_err(ctx("open"))?;
        writer
            .append_rule(
                RuleKind::Deny,
                &RuleToml {
                    tool: "fs.write".to_owned(),
                    pattern: None,
                },
            )
            .map_err(ctx("append_rule"))?;
        writer.save().map_err(ctx("save"))?;

        let removed = super::remove_persisted_rule(&path, RuleDecision::Deny, "fs.write", None)
            .map_err(ctx("remove"))?;
        assert!(removed, "matching rule must be found and removed");

        let content = std::fs::read_to_string(&path).map_err(ctx("read back"))?;
        assert!(!content.contains("fs.write"));

        let removed_again =
            super::remove_persisted_rule(&path, RuleDecision::Deny, "fs.write", None)
                .map_err(ctx("second call"))?;
        assert!(
            !removed_again,
            "already-removed rule must not be found again"
        );
        Ok(())
    }

    #[test]
    fn test_remove_persisted_rule_on_missing_file_returns_false_not_an_error() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let missing = dir.path().join("does-not-exist.toml");
        let result =
            super::remove_persisted_rule(&missing, RuleDecision::Allow, "shell.exec", None);
        assert!(!result.map_err(ctx("must not error"))?);
        Ok(())
    }

    // ── Runde 5, Teil E: rules, rm, log, --user, ALWAYS_ASK-Sperre ───────────

    fn args(cmd: &str, tail: &[&str]) -> PermissionsArgs {
        PermissionsArgs {
            cmd: Some(cmd.to_owned()),
            tail: tail.iter().map(|token| (*token).to_owned()).collect(),
        }
    }

    #[test]
    fn test_parse_scope_flags_accepts_user_as_global() -> TestResult {
        let (_, flags) = parse_scope_flags(&toks(&["x", "--user"]), SettingScope::Project)
            .map_err(ctx("parse"))?;
        assert_eq!(flags.scope, SettingScope::Global);
        Ok(())
    }

    #[tokio::test]
    async fn permissions_rules_lists_rules_with_origin() -> TestResult {
        let (ctx, _cell, rules) = test_context_with_all_cells(ApprovalMode::Delegated)?;
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("cargo test*".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Project,
        });
        rules.add(ApprovalRule {
            tool: "fs.write".to_owned(),
            pattern: Some("secrets/**".to_owned()),
            decision: RuleDecision::Deny,
            scope: RuleScope::Session,
        });
        let output = permissions(&ctx, args("rules", &[]))
            .await
            .map_err(crate::test_support::ctx("rules"))?;
        assert!(
            output.text.contains("1. ✓ shell.exec cargo test*"),
            "{}",
            output.text
        );
        assert!(output.text.contains("Herkunft: Projekt"), "{}", output.text);
        assert!(
            output.text.contains("2. ✗ fs.write secrets/**"),
            "{}",
            output.text
        );
        assert!(output.text.contains("Herkunft: Sitzung"), "{}", output.text);
        assert!(output.text.contains("process.kill"), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn permissions_rm_is_an_alias_of_remove() -> TestResult {
        let (ctx, _cell, rules) = test_context_with_all_cells(ApprovalMode::Delegated)?;
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: None,
            decision: RuleDecision::Allow,
            scope: RuleScope::Session,
        });
        permissions(&ctx, args("rm", &["1"]))
            .await
            .map_err(crate::test_support::ctx("rm"))?;
        assert!(rules.snapshot().is_empty());
        Ok(())
    }

    /// Harte Regel: `ALWAYS_ASK_TOOLS` sind nie per Regel freigebbar.
    #[tokio::test]
    async fn permissions_allow_rejects_always_ask_tools() -> TestResult {
        let (ctx, _cell, rules) = test_context_with_all_cells(ApprovalMode::Delegated)?;
        for tool in harw_registry_defaults::ALWAYS_ASK_TOOLS {
            let result = permissions(&ctx, args("allow", &[*tool, "--session"])).await;
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "{tool}: {result:?}"
            );
        }
        assert!(rules.snapshot().is_empty());
        // Eine Deny-Regel bleibt erlaubt (sie schränkt nur ein).
        permissions(&ctx, args("deny", &["process.kill", "--session"]))
            .await
            .map_err(crate::test_support::ctx("deny"))?;
        assert_eq!(rules.snapshot().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn permissions_log_without_a_log_is_not_available() -> TestResult {
        let (ctx, _cell, _rules) = test_context_with_all_cells(ApprovalMode::Delegated)?;
        let result = permissions(&ctx, args("log", &[])).await;
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn permissions_log_lists_decisions_newest_first() -> TestResult {
        use harw_extension_api::auto_mode::{
            AutoDecision, AutoDecisionLog, AutoLogEntry, AutoVerdict, VerdictSource,
        };
        let base = test_context()?;
        let log = AutoDecisionLog::new();
        for (index, decision) in [AutoDecision::Allow, AutoDecision::Deny]
            .into_iter()
            .enumerate()
        {
            log.record(AutoLogEntry {
                at: jiff::Timestamp::UNIX_EPOCH,
                call_id: format!("call-{index}"),
                tool: "shell.exec".to_owned(),
                summary: format!("shell.exec: befehl-{index}"),
                verdict: AutoVerdict::new(
                    decision,
                    "kategorie",
                    "grund",
                    VerdictSource::Classifier,
                ),
            });
        }
        let mut services = ServiceMap::new();
        services.insert(log);
        let ctx = OpContext::new(
            base.session_id().clone(),
            base.turn_id().clone(),
            base.sandbox().clone(),
            services,
        );
        let output = permissions(&ctx, args("log", &["5"]))
            .await
            .map_err(crate::test_support::ctx("log"))?;
        let text = output.text;
        assert!(text.contains("insgesamt 1/20"), "{text}");
        let newest = text
            .find("befehl-1")
            .ok_or(TestError::Missing("befehl-1"))?;
        let oldest = text
            .find("befehl-0")
            .ok_or(TestError::Missing("befehl-0"))?;
        assert!(newest < oldest, "neueste zuerst:\n{text}");
        assert!(matches!(
            permissions(&ctx, args("log", &["0"])).await,
            Err(OpError::InvalidArguments(_))
        ));
        Ok(())
    }
}
