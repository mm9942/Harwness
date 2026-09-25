//! TUI-lokale Befehle: Abfangen vor dem regulären `/command`-Dispatch.
//!
//! # Beschreibung
//! Bündelt alle Eingaben, die die TUI selbst beantwortet, statt sie an eine
//! Operation weiterzureichen: interaktive Projektionen bare aufgerufener
//! Befehle (`/model`, `/effort`, `/mode`, `/models`, `/help`, …), rein lokale
//! Befehle (`/clear`, `/verbose`, `/whoami`, `/rename`, …) sowie die
//! Eingabe-Präfixe `#` (Notiz) und `@` (Rollen-Erwähnung).
//!
//! [`intercept`] ist eine **reine** Funktion: Sie liest nur den
//! [`LocalCommandContext`] und liefert ein [`LocalIntercept`], das `app.rs`
//! anschließend anwendet (Overlay öffnen, Zeile umschreiben, …). `None`
//! bedeutet: nicht lokal — die Zeile läuft unverändert durch den regulären
//! Dispatch bzw. den Chat-Pfad.
//!
//! Bewusst **nicht** abgefangen werden `/tools` und `/compact` (bleiben in
//! `app.rs`, weil sie Sitzungszustand direkt verändern), `/exit`, `/new` und
//! `/resume <selektor>` (erzeugen ein `TuiRunOutcome` in `app.rs`).
//!
//! # Nebenläufigkeit
//! Keine; reine Funktionen ohne geteilten Zustand.
//!
//! # Fehlertypen
//! Keine — ungültige Eingaben werden als [`LocalIntercept::System`] mit
//! deutscher Meldung zurückgegeben.

use std::path::Path;

use harw_config::{ModelRole, ResolvedConfig};
use harw_core::InteractionMode;
use harw_extension_api::approval_mode::ApprovalMode;
use harw_types::SessionId;

use crate::app::EffortTarget;
use crate::command::{CommandOrigin, PermissionTier};
use crate::help_overlay::{HelpOverlay, HelpTab};
use crate::kanban_board::KanbanBoard;
use crate::keybindings::KeyBindings;
use crate::knowledge_view::{KnowledgeBrowser, KnowledgeKind};
use crate::matrix_view::MatrixView;
use crate::mention::role_mention_text;
use crate::mode_picker::ModePicker;
use crate::model_roles_view::ModelRolesView;
use crate::model_switch_picker::PickerTarget;
use crate::overlay_view::OverlayView;
use crate::registry::CommandRegistry;

/// Lesender Kontext für [`intercept`].
///
/// Alle Felder sind Momentaufnahmen aus `ChatApp`; [`intercept`] verändert
/// nichts.
pub(crate) struct LocalCommandContext<'a> {
    /// Aktiver Befehlskatalog (Operationen + lokale Spezifikationen).
    pub registry: &'a CommandRegistry,
    /// Aufgelöste Konfiguration, falls geladen.
    pub config: Option<&'a ResolvedConfig>,
    /// Aktive Tastenbelegung (für die Hilfe).
    pub key_bindings: &'a KeyBindings,
    /// Aktiver Interaktionsmodus.
    pub active_mode: InteractionMode,
    /// Aktiver Freigabemodus, falls bekannt.
    pub approval: Option<ApprovalMode>,
    /// Provider des Live-Sitzungsmodells, falls bekannt.
    pub live_provider: Option<&'a str>,
    /// Live-Sitzungsmodell, falls bekannt.
    pub live_model: Option<&'a str>,
    /// Projektwurzel (für spätere Dateiprüfungen; `@pfad` expandiert der
    /// Submit-Pfad).
    #[allow(dead_code)]
    pub project_root: &'a Path,
    /// ID der laufenden Sitzung.
    pub session_id: &'a SessionId,
    /// Berechtigungsstufe des TUI-Nutzers.
    pub tier: PermissionTier,
    /// Bekannte Agentenrollen für `@rolle`.
    pub known_roles: &'a [&'a str],
    /// R10 Welle 3B: kanonische Namen der Befehle, die diese Sitzung
    /// (kompilierter Agent, [`crate::fixed_agent`]) versteckt — kleingeschrieben,
    /// leer für die normale TUI. Geprüft **vor** jedem anderen Abfang, damit
    /// weder eine bare Projektion (`/agent`, `/model`, …) noch ein Fall, den
    /// `intercept` sonst gar nicht kennt (fällt an die Operation-Dispatch
    /// durch), einen versteckten Befehl je erreicht.
    pub hidden_commands: &'a [String],
    /// R10 Welle 3B: erlaubte `/model switch`-Ziele (`provider`, `model`),
    /// wenn `Some` — jedes andere Ziel wird abgelehnt. `None` lässt
    /// `/model switch` uneingeschränkt.
    pub model_switch_allowlist: Option<&'a [(String, String)]>,
}

/// Umschaltbare Seitenpanels.
// `Agents`/`Explorer` sind für Tasten-/Befehlsvarianten reserviert, die
// `app.rs` (T21) verdrahtet; [`intercept`] erzeugt derzeit nur `Workbench`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelToggle {
    /// Agenten-Panel.
    Agents,
    /// Workbench-Panel.
    Workbench,
    /// Datei-Explorer.
    Explorer,
}

/// Ergebnis eines lokalen Abfangs; `app.rs` wendet es an.
#[derive(Debug)]
pub(crate) enum LocalIntercept {
    /// Generisches Overlay öffnen (`Overlay::View`).
    OpenOverlay(Box<dyn OverlayView>),
    /// Provider/Modell-Picker für das Ziel öffnen.
    OpenModelPicker(PickerTarget),
    /// UIA-Worker-Picker öffnen; den festen Provider löst `app.rs` auf
    /// (UIA-Pin, sonst aktiver/Standard-Provider).
    OpenUiaWorkerPicker,
    /// Runde 5, Teil G: UIA-Modellwahl öffnen, danach direkt den Bereich
    /// „UIA-Worker-Modelle“ (`/models pick uia`).
    OpenUiaPickerThenWorkers,
    /// Effort-Auswahl für das Ziel öffnen.
    OpenEffortChoice(EffortTarget),
    /// Agentenbaum öffnen.
    OpenAgentTree,
    /// Sitzungsauswahl öffnen (bare `/resume`).
    OpenSessionPicker,
    /// Seitenpanel umschalten.
    TogglePanel(PanelToggle),
    /// Ausführliche Anzeige umschalten.
    ToggleVerbose,
    /// Transkript leeren (nur Anzeige).
    ClearTranscript,
    /// Sitzung umbenennen.
    RenameSession(String),
    /// Zeile durch diese Befehlszeile ersetzen und erneut dispatchen.
    Rewrite(String),
    /// Diesen Text als Chat-Nachricht an die UIA senden.
    Chat(String),
    /// Systemzeile anzeigen; sonst nichts.
    System(String),
    /// Runde 5, Teil F: lokaler `/plan`-Befehl (`/plan`, `/plan
    /// show|edit|list|open`); `app/plan_mode.rs` führt ihn aus.
    Plan(crate::plan_dialog::PlanCommand),
    /// Runde 5, Teil I: `/agent stream <orchestrators|all|none>` — schaltet
    /// den Live-Stream der Kind-Agenten für die Sitzung um. Trägt die rohen
    /// Argumente hinter `stream`; geparst wird in [`crate::child_stream`].
    ChildStream(String),
    /// Runde 5, Teil K: `/agent bg` (Hintergrund-Agenten auflisten) und
    /// `/agent cancel <id>` (eigenen Hintergrund-Agenten abbrechen); trägt
    /// die rohen Argumente, ausgeführt in `app/background_agents.rs`.
    BackgroundAgents(String),
    /// Runde 5, Teil L: `/btw <frage>` — flüchtige Nebenfrage ohne
    /// Werkzeuge; trägt die (nicht leere) Frage. `crate::app::btw` führt sie
    /// aus.
    Btw(String),
}

/// Runde 5, Teil I: Hinweis auf den entfallenen Befehl `/agents`.
pub(crate) const AGENTS_REMOVED_HINT: &str = "/agents gibt es nicht mehr – nutze /agent";

/// Erkennt die bare Form eines Befehls **oder** dessen argloses `switch`
/// (Welle 4a/7b: `/model`, `/model switch`, `/uia-effort switch`, …).
///
/// # Beschreibung
/// `raw.trim() == command` oder `raw.trim() == format!("{command} switch")`
/// — jeweils exakt, kein zusätzliches Argument. `/model switch <id>` bleibt
/// unberührt (bleibt Text-Dispatch); nur die beiden argumentlosen Formen
/// öffnen den jeweiligen Picker.
///
/// # Argumente
/// - `raw` (`&str`): die unveränderte Befehlszeile.
/// - `command` (`&str`): der zu erkennende Befehl, z. B. `"/model"`.
///
/// # Rückgabe
/// `true` für die bare Form oder das arglose `switch`, sonst `false`.
pub(crate) fn is_bare_or_argless_switch(raw: &str, command: &str) -> bool {
    let trimmed = raw.trim();
    trimmed == command || trimmed == format!("{command} switch")
}

/// Whether a `/model switch <target>` argument names one of `allowed`'s
/// `(provider, model)` pairs (R10 wave 3B).
///
/// # Beschreibung
/// Matches the same two forms `/model switch` itself accepts (identical
/// logic to [`crate::fixed_agent::is_model_switch_target_allowed`], restated
/// here over a plain slice so this module does not need a
/// [`crate::fixed_agent::FixedAgentOptions`] just to check a target):
/// case-insensitively either a bare model id (`"gpt-5"`) or a qualified
/// `provider/model` pair (`"openai/gpt-5"`).
fn model_switch_target_allowed(target: &str, allowed: &[(String, String)]) -> bool {
    allowed.iter().any(|(provider, model)| {
        target.eq_ignore_ascii_case(model)
            || target.eq_ignore_ascii_case(&format!("{provider}/{model}"))
    })
}

/// Fängt TUI-lokale Befehle und Präfixe ab.
///
/// # Beschreibung
/// Reihenfolge: `#`-Notiz, `@`-Erwähnung, dann `/`-Befehle. Befehle mit
/// Argumenten, die eine Operation bedient (`/model switch x`, `/kanban move …`,
/// `/mode plan`, …), liefern `None`.
///
/// # Argumente
/// - `raw`: unveränderte Eingabezeile.
/// - `ctx`: lesender Kontext.
///
/// # Rückgabe
/// `Some(LocalIntercept)`, wenn lokal behandelt, sonst `None`.
pub(crate) fn intercept(raw: &str, ctx: &LocalCommandContext<'_>) -> Option<LocalIntercept> {
    let trimmed = raw.trim();
    if let Some(note) = trimmed.strip_prefix('#') {
        return Some(note_intercept(note, ctx.registry));
    }
    if let Some(mention) = trimmed.strip_prefix('@') {
        return mention_intercept(trimmed, mention, ctx.known_roles);
    }
    let rest = trimmed.strip_prefix('/')?;
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest, ""),
    };
    let bare = args.is_empty();

    // R10 Welle 3B: versteckte Befehle (fixed-agent-Beschränkung) zuerst
    // abfangen — vor jeder bare-Projektion (`/agent`, `/model`, …), die
    // sonst unbedingt öffnen würde, und vor jedem Fall, den diese Funktion
    // gar nicht kennt (fiele sonst an die Operation-Dispatch durch, die den
    // Befehl ohnehin nicht mehr kennt, sobald er aus `command_registry`
    // gefiltert ist — hier aber mit einer eigenen, kurzen Meldung statt
    // „Unbekannter Command").
    if ctx
        .hidden_commands
        .iter()
        .any(|hidden| hidden.eq_ignore_ascii_case(name))
    {
        return Some(LocalIntercept::System(format!(
            "/{name} ist in diesem Agenten nicht verfügbar."
        )));
    }
    // R10 Welle 3B: `/model switch <ziel>` gegen die erlaubten Modelle des
    // kompilierten Agenten prüfen, bevor die Operation dispatcht. Die bare
    // und die argloses-`switch`-Form laufen weiter über den Picker unten
    // (unverändert); nur ein konkretes Ziel wird hier geprüft.
    if name == "model"
        && let Some(allowlist) = ctx.model_switch_allowlist
    {
        let mut parts = args.splitn(2, char::is_whitespace);
        let sub = parts.next().unwrap_or("");
        let target = parts.next().map(str::trim).filter(|t| !t.is_empty());
        if sub.eq_ignore_ascii_case("switch")
            && let Some(target) = target
            && !model_switch_target_allowed(target, allowlist)
        {
            return Some(LocalIntercept::System(format!(
                "/model switch {target}: Modell ist für diesen Agenten nicht zugelassen."
            )));
        }
    }

    // Picker-Projektionen (bare oder argloses `switch`).
    if is_bare_or_argless_switch(trimmed, "/model") {
        return Some(LocalIntercept::OpenModelPicker(PickerTarget::Orchestrator));
    }
    if is_bare_or_argless_switch(trimmed, "/uia-model") {
        return Some(LocalIntercept::OpenModelPicker(PickerTarget::Uia));
    }
    if is_bare_or_argless_switch(trimmed, "/uia-worker-model") {
        return Some(LocalIntercept::OpenUiaWorkerPicker);
    }
    if is_bare_or_argless_switch(trimmed, "/effort") {
        return Some(LocalIntercept::OpenEffortChoice(EffortTarget::Session));
    }
    if is_bare_or_argless_switch(trimmed, "/uia-effort") {
        return Some(LocalIntercept::OpenEffortChoice(EffortTarget::Uia));
    }

    match name {
        "resume" if bare => Some(LocalIntercept::OpenSessionPicker),
        "agent" if bare => Some(LocalIntercept::OpenAgentTree),
        // Runde 5, Teil K: Hintergrund-Agenten — Liste und Abbruch, vor der
        // Operation `/agent` (deren `stop` bleibt unverändert erreichbar).
        "agent" if args == "bg" || args == "cancel" || args.starts_with("cancel ") => {
            Some(LocalIntercept::BackgroundAgents(args.to_owned()))
        }
        // Runde 5, Teil I: `/agent stream …` vor der Operation `/agent`.
        "agent" if args == "stream" || args.starts_with("stream ") => {
            Some(LocalIntercept::ChildStream(
                args.strip_prefix("stream")
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            ))
        }
        // Runde 5, Teil I: `/agents` entfällt; keine stille Weiterleitung.
        "agents" => Some(LocalIntercept::System(AGENTS_REMOVED_HINT.to_owned())),
        "models" => models_intercept(args, ctx),
        "mode" if bare => Some(LocalIntercept::OpenOverlay(Box::new(ModePicker::new(
            ctx.active_mode,
            ctx.config
                .map(|config| config.harness.mode.default.as_str()),
            ctx.approval,
        )))),
        "help" if bare => Some(help(ctx, HelpTab::Commands)),
        "keys" => Some(help(ctx, HelpTab::Keys)),
        "kanban" if bare => Some(LocalIntercept::OpenOverlay(Box::new(KanbanBoard::new()))),
        "matrix" if bare => Some(LocalIntercept::OpenOverlay(Box::new(MatrixView::new()))),
        "workbench" if bare => Some(LocalIntercept::TogglePanel(PanelToggle::Workbench)),
        "palace" if bare => Some(knowledge(KnowledgeKind::Palace)),
        "dream" if bare => Some(knowledge(KnowledgeKind::Dream)),
        "diary" if bare => Some(knowledge(KnowledgeKind::Diary)),
        "clear" => Some(LocalIntercept::ClearTranscript),
        "verbose" => Some(LocalIntercept::ToggleVerbose),
        "whoami" => Some(LocalIntercept::System(whoami_text(ctx))),
        "rename" if bare => Some(LocalIntercept::System(
            "Bitte einen Namen angeben: /rename <titel>".to_owned(),
        )),
        "rename" => Some(LocalIntercept::RenameSession(args.to_owned())),
        // Runde 5, Teil L: `/btw` ohne Frage → Nutzungshinweis.
        "btw" if bare => Some(LocalIntercept::System(
            crate::app::btw::BTW_USAGE_HINT.to_owned(),
        )),
        "btw" => Some(LocalIntercept::Btw(args.to_owned())),
        "sessions" if bare => Some(LocalIntercept::Rewrite("/resume".to_owned())),
        "sessions" => Some(LocalIntercept::Rewrite(format!("/resume {args}"))),
        // Runde 5, Teil F: `/plan` schaltet den Plan-Modus ein,
        // `/plan show|edit|list|open` bedienen die Plan-Dateien. Alle übrigen
        // Unterbefehle (`/plan inspect`, `/plan add …`) gehören weiter der
        // `plan`-Operation.
        "plan" if crate::plan_dialog::parse_plan_command(args).is_some() => {
            crate::plan_dialog::parse_plan_command(args).map(LocalIntercept::Plan)
        }
        "plan" | "goal" if ctx.registry.find(name).is_none() => {
            Some(LocalIntercept::System(format!(
                "/{name} ist nicht verfügbar: Die Plan-Werkzeuge sind deaktiviert \
                 ([tools.plan] enabled = false)."
            )))
        }
        _ => None,
    }
}

/// `#text` → Tagebuchnotiz (wenn die `diary`-Operation registriert ist),
/// sonst Gedächtnis-Eintrag.
fn note_intercept(note: &str, registry: &CommandRegistry) -> LocalIntercept {
    let note = note.trim();
    if note.is_empty() {
        return LocalIntercept::System("Leere Notiz: bitte Text nach „#“ angeben.".to_owned());
    }
    let diary_op = registry
        .find("diary")
        .is_some_and(|spec| spec.origin == CommandOrigin::Operation);
    if diary_op {
        LocalIntercept::Rewrite(format!("/diary note {note}"))
    } else {
        LocalIntercept::Rewrite(format!("/memory record {note}"))
    }
}

/// `@rolle text` → Delegationsbitte an die UIA; sonst unveränderter Chat
/// (Dateianhänge expandiert der Submit-Pfad).
fn mention_intercept(trimmed: &str, mention: &str, roles: &[&str]) -> Option<LocalIntercept> {
    let (token, body) = match mention.split_once(char::is_whitespace) {
        Some((token, body)) => (token, body),
        None => (mention, ""),
    };
    if token.is_empty() {
        return None;
    }
    match roles.iter().find(|role| role.eq_ignore_ascii_case(token)) {
        Some(role) => Some(LocalIntercept::Chat(role_mention_text(role, body))),
        None => Some(LocalIntercept::Chat(trimmed.to_owned())),
    }
}

/// `/models` bare/`show` → Rollenansicht; `/models pick <rolle>` → Picker.
fn models_intercept(args: &str, ctx: &LocalCommandContext<'_>) -> Option<LocalIntercept> {
    let mut words = args.split_whitespace();
    match words.next() {
        None | Some("show") if words.clone().next().is_none() => {
            let view = match ctx.config {
                Some(config) => {
                    ModelRolesView::from_config(config, ctx.live_provider, ctx.live_model)
                }
                None => ModelRolesView::empty(ctx.live_provider, ctx.live_model),
            };
            Some(LocalIntercept::OpenOverlay(Box::new(view)))
        }
        Some("pick") => {
            let (Some(role), None) = (words.next(), words.next()) else {
                return Some(LocalIntercept::System(format!(
                    "Nutzung: /models pick <rolle> — bekannte Rollen: {}",
                    role_keys()
                )));
            };
            // Runde 5, Teil G: jede UIA-Worker-Rolle (auch `uia-worker`)
            // bekommt eine eigene, vom UIA-Provider unabhängige Wahl.
            if let Some(worker) = uia_worker_role(role) {
                return Some(LocalIntercept::OpenModelPicker(
                    PickerTarget::UiaWorkerRole {
                        role: worker.to_owned(),
                    },
                ));
            }
            match ModelRole::parse(role) {
                Some(ModelRole::Uia) => Some(LocalIntercept::OpenUiaPickerThenWorkers),
                Some(role) => Some(LocalIntercept::OpenModelPicker(PickerTarget::Role { role })),
                None => Some(LocalIntercept::System(format!(
                    "Unbekannte Rolle „{role}“ — bekannte Rollen: {}",
                    role_keys()
                ))),
            }
        }
        // Runde 5, Teil G: `/models worker` ohne Argumente → Worker-Bereich.
        Some("worker" | "workers") if words.clone().next().is_none() => Some(
            LocalIntercept::OpenOverlay(Box::new(ModelRolesView::uia_workers(ctx.config))),
        ),
        _ => None,
    }
}

/// Runde 5, Teil G: kanonischer Name einer UIA-Worker-Rolle
/// ([`harw_config::UIA_WORKER_ROLES`]), Groß-/Kleinschreibung und `_`/`-`
/// egal.
fn uia_worker_role(raw: &str) -> Option<&'static str> {
    let normalized = raw.trim().to_ascii_lowercase().replace('_', "-");
    harw_config::UIA_WORKER_ROLES
        .into_iter()
        .find(|role| *role == normalized)
}

/// Kommagetrennte Liste aller Rollen-Schlüssel.
fn role_keys() -> String {
    ModelRole::ALL
        .iter()
        .map(|role| role.key())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Hilfe-Overlay auf dem angegebenen Reiter.
fn help(ctx: &LocalCommandContext<'_>, tab: HelpTab) -> LocalIntercept {
    LocalIntercept::OpenOverlay(Box::new(HelpOverlay::new(
        ctx.registry,
        ctx.key_bindings,
        tab,
    )))
}

/// Wissensbrowser für die angegebene Art.
fn knowledge(kind: KnowledgeKind) -> LocalIntercept {
    LocalIntercept::OpenOverlay(Box::new(KnowledgeBrowser::new(kind)))
}

/// Mehrzeiliger `/whoami`-Text.
fn whoami_text(ctx: &LocalCommandContext<'_>) -> String {
    let approval = ctx
        .approval
        .map_or("unbekannt", |approval| approval.as_str());
    let model = match (ctx.live_provider, ctx.live_model) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        (None, Some(model)) => model.to_owned(),
        (Some(provider), None) => format!("{provider}/—"),
        (None, None) => "unbekannt".to_owned(),
    };
    format!(
        "Sitzung: {}\nBerechtigungsstufe: {:?}\nModus: {} — {}\nFreigabe: {approval}\nModell: {model}",
        ctx.session_id.as_str(),
        ctx.tier,
        ctx.active_mode.as_str(),
        ctx.active_mode.summary_de(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::command::{CommandDomain, CommandScope, CommandSpec, OutputSurface};
    use crate::test_support::{TestError, TestResult, ctx as err_ctx};

    struct Fixture {
        registry: CommandRegistry,
        config: ResolvedConfig,
        keys: KeyBindings,
        root: PathBuf,
        session: SessionId,
        with_config: bool,
        hidden_commands: Vec<String>,
        model_switch_allowlist: Option<Vec<(String, String)>>,
    }

    const ROLES: &[&str] = &["explorer", "worker-simple"];

    impl Fixture {
        fn new(registry: CommandRegistry) -> Self {
            Self {
                registry,
                config: ResolvedConfig::default(),
                keys: KeyBindings::default(),
                root: PathBuf::from("."),
                session: SessionId::new(),
                with_config: true,
                hidden_commands: Vec::new(),
                model_switch_allowlist: None,
            }
        }

        fn with_hidden(mut self, hidden: &[&str]) -> Self {
            self.hidden_commands = hidden.iter().map(|name| (*name).to_owned()).collect();
            self
        }

        fn with_model_switch_allowlist(mut self, allowed: &[(&str, &str)]) -> Self {
            self.model_switch_allowlist = Some(
                allowed
                    .iter()
                    .map(|(provider, model)| ((*provider).to_owned(), (*model).to_owned()))
                    .collect(),
            );
            self
        }

        fn ctx(&self) -> LocalCommandContext<'_> {
            LocalCommandContext {
                registry: &self.registry,
                config: self.with_config.then_some(&self.config),
                key_bindings: &self.keys,
                active_mode: InteractionMode::default(),
                approval: Some(ApprovalMode::ALL[0]),
                live_provider: Some("prov"),
                live_model: Some("mod-1"),
                project_root: &self.root,
                session_id: &self.session,
                tier: PermissionTier::Operator,
                known_roles: ROLES,
                hidden_commands: &self.hidden_commands,
                model_switch_allowlist: self.model_switch_allowlist.as_deref(),
            }
        }

        fn run(&self, raw: &str) -> Option<LocalIntercept> {
            intercept(raw, &self.ctx())
        }
    }

    fn spec(name: &str) -> TestResult<CommandSpec> {
        CommandSpec::new(
            name,
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::Knowledge,
        )
        .map_err(err_ctx("spec"))
    }

    fn empty() -> Fixture {
        Fixture::new(CommandRegistry::new(Vec::new()))
    }

    fn built_in() -> TestResult<Fixture> {
        Ok(Fixture::new(
            CommandRegistry::built_in().map_err(err_ctx("registry"))?,
        ))
    }

    fn overlay_debug(result: Option<LocalIntercept>) -> TestResult<String> {
        match result {
            Some(LocalIntercept::OpenOverlay(view)) => Ok(format!("{view:?}")),
            other => Err(TestError::Unexpected(format!("overlay, got {other:?}"))),
        }
    }

    fn rewrite(result: Option<LocalIntercept>) -> TestResult<String> {
        match result {
            Some(LocalIntercept::Rewrite(line)) => Ok(line),
            other => Err(TestError::Unexpected(format!("rewrite, got {other:?}"))),
        }
    }

    fn system(result: Option<LocalIntercept>) -> TestResult<String> {
        match result {
            Some(LocalIntercept::System(text)) => Ok(text),
            other => Err(TestError::Unexpected(format!("system, got {other:?}"))),
        }
    }

    /// Bare Form und argloses `switch` öffnen den Picker; `switch <id>` mit
    /// Argument bleibt Text-Dispatch (kein Picker).
    #[test]
    fn is_bare_or_argless_switch_matches_bare_and_argless_switch_only() -> TestResult {
        assert!(is_bare_or_argless_switch("/model", "/model"));
        assert!(is_bare_or_argless_switch("  /model  ", "/model"));
        assert!(is_bare_or_argless_switch("/model switch", "/model"));
        assert!(is_bare_or_argless_switch(
            "/uia-effort switch",
            "/uia-effort"
        ));

        assert!(!is_bare_or_argless_switch("/model switch x", "/model"));
        assert!(!is_bare_or_argless_switch("/model list", "/model"));
        assert!(!is_bare_or_argless_switch("/provider", "/model"));
        // Bare `/provider` öffnet seit der Konsolidierung (Welle 4a) keinen
        // Picker mehr; das Prädikat darf `/provider` nicht fälschlich matchen.
        assert!(!is_bare_or_argless_switch("/provider switch", "/model"));
        Ok(())
    }

    #[test]
    fn model_pickers_and_effort_open_for_bare_and_argless_switch() {
        let fx = empty();
        assert!(matches!(
            fx.run("/model"),
            Some(LocalIntercept::OpenModelPicker(PickerTarget::Orchestrator))
        ));
        assert!(matches!(
            fx.run("/model switch"),
            Some(LocalIntercept::OpenModelPicker(PickerTarget::Orchestrator))
        ));
        assert!(fx.run("/model switch gpt-x").is_none());
        assert!(fx.run("/model list").is_none());
        assert!(matches!(
            fx.run("/uia-model"),
            Some(LocalIntercept::OpenModelPicker(PickerTarget::Uia))
        ));
        assert!(matches!(
            fx.run("/uia-worker-model switch"),
            Some(LocalIntercept::OpenUiaWorkerPicker)
        ));
        assert!(matches!(
            fx.run("/effort"),
            Some(LocalIntercept::OpenEffortChoice(EffortTarget::Session))
        ));
        assert!(matches!(
            fx.run("/uia-effort switch"),
            Some(LocalIntercept::OpenEffortChoice(EffortTarget::Uia))
        ));
        assert!(fx.run("/effort high").is_none());
    }

    #[test]
    fn tools_compact_provider_and_unknown_pass_through() {
        let fx = empty();
        for raw in [
            "/tools",
            "/tools on x",
            "/compact",
            "/provider",
            "/exit",
            "/new",
            "/resume abc",
            "hallo",
        ] {
            assert!(fx.run(raw).is_none(), "{raw} must not be intercepted");
        }
    }

    #[test]
    fn resume_agent_and_agents() {
        let fx = empty();
        assert!(matches!(
            fx.run("/resume"),
            Some(LocalIntercept::OpenSessionPicker)
        ));
        assert!(matches!(
            fx.run("/agent"),
            Some(LocalIntercept::OpenAgentTree)
        ));
        assert!(fx.run("/agent list").is_none());
        // Runde 5, Teil K: Hintergrund-Agenten auflisten bzw. abbrechen.
        assert!(matches!(
            fx.run("/agent bg"),
            Some(LocalIntercept::BackgroundAgents(ref args)) if args == "bg"
        ));
        assert!(matches!(
            fx.run("/agent cancel abc"),
            Some(LocalIntercept::BackgroundAgents(ref args)) if args == "cancel abc"
        ));
        assert!(fx.run("/agent stop abc").is_none());
        // Runde 5, Teil I: `/agents` gibt es nicht mehr — nur ein Hinweis.
        for raw in ["/agents", "/agents stream all"] {
            assert!(matches!(
                fx.run(raw),
                Some(LocalIntercept::System(ref text)) if text == AGENTS_REMOVED_HINT
            ));
        }
        // `/agent stream <modus>` schaltet den Live-Stream.
        assert!(matches!(
            fx.run("/agent stream all"),
            Some(LocalIntercept::ChildStream(ref mode)) if mode == "all"
        ));
        assert!(matches!(
            fx.run("/agent stream"),
            Some(LocalIntercept::ChildStream(ref mode)) if mode.is_empty()
        ));
        assert!(fx.run("/agent streamx").is_none());
    }

    #[test]
    fn models_show_opens_roles_view_with_and_without_config() -> TestResult {
        let mut fx = empty();
        assert!(overlay_debug(fx.run("/models"))?.contains("ModelRolesView"));
        assert!(overlay_debug(fx.run("/models show"))?.contains("ModelRolesView"));
        fx.with_config = false;
        assert!(overlay_debug(fx.run("/models"))?.contains("ModelRolesView"));
        assert!(fx.run("/models set explorer p/m").is_none());
        assert!(fx.run("/models show extra").is_none());
        // Runde 5, Teil G: `/models worker` ohne Argumente öffnet den
        // Worker-Bereich; mit Argumenten geht es an die Operation.
        assert!(overlay_debug(fx.run("/models worker"))?.contains("UiaWorkers"));
        assert!(fx.run("/models worker uia-writer uia").is_none());
        Ok(())
    }

    #[test]
    fn models_pick_maps_roles_to_picker_targets() -> TestResult {
        let fx = empty();
        // Runde 5, Teil G: die UIA-Wahl öffnet danach den Worker-Bereich,
        // jede UIA-Worker-Rolle hat einen eigenen, freien Picker.
        assert!(matches!(
            fx.run("/models pick uia"),
            Some(LocalIntercept::OpenUiaPickerThenWorkers)
        ));
        for role in harw_config::UIA_WORKER_ROLES {
            match fx.run(&format!("/models pick {role}")) {
                Some(LocalIntercept::OpenModelPicker(PickerTarget::UiaWorkerRole {
                    role: picked,
                })) => assert_eq!(picked, role),
                other => {
                    return Err(TestError::Unexpected(format!("{role}: {other:?}")));
                }
            }
        }
        assert!(matches!(
            fx.run("/models pick orchestrator"),
            Some(LocalIntercept::OpenModelPicker(PickerTarget::Role {
                role: ModelRole::Orchestrator
            }))
        ));
        assert!(matches!(
            fx.run("/models pick explorer"),
            Some(LocalIntercept::OpenModelPicker(PickerTarget::Role {
                role: ModelRole::Explorer
            }))
        ));
        assert!(system(fx.run("/models pick quatsch"))?.contains("Unbekannte Rolle"));
        assert!(system(fx.run("/models pick"))?.contains("Nutzung"));
        Ok(())
    }

    #[test]
    fn mode_bare_opens_picker_with_args_passes_through() -> TestResult {
        let fx = empty();
        assert!(overlay_debug(fx.run("/mode"))?.contains("ModePicker"));
        assert!(fx.run("/mode plan").is_none());
        assert!(fx.run("/mode show").is_none());
        Ok(())
    }

    #[test]
    fn help_and_keys_open_help_overlay() -> TestResult {
        let fx = built_in()?;
        let commands = overlay_debug(fx.run("/help"))?;
        assert!(commands.contains("HelpOverlay") && commands.contains("tab: Commands"));
        let keys = overlay_debug(fx.run("/keys"))?;
        assert!(keys.contains("HelpOverlay") && keys.contains("tab: Keys"));
        assert!(fx.run("/help model").is_none());
        Ok(())
    }

    #[test]
    fn knowledge_views_bare_only() -> TestResult {
        let fx = empty();
        assert!(overlay_debug(fx.run("/kanban"))?.contains("KanbanBoard"));
        assert!(fx.run("/kanban move c1 done").is_none());
        assert!(overlay_debug(fx.run("/matrix"))?.contains("MatrixView"));
        assert!(fx.run("/matrix step").is_none());
        for (raw, kind) in [
            ("/palace", "Palace"),
            ("/dream", "Dream"),
            ("/diary", "Diary"),
        ] {
            let debug = overlay_debug(fx.run(raw))?;
            assert!(debug.contains("KnowledgeBrowser") && debug.contains(&format!("kind: {kind}")));
        }
        assert!(fx.run("/diary note hallo").is_none());
        assert!(fx.run("/palace show x").is_none());
        assert!(matches!(
            fx.run("/workbench"),
            Some(LocalIntercept::TogglePanel(PanelToggle::Workbench))
        ));
        assert!(fx.run("/workbench note x").is_none());
        Ok(())
    }

    #[test]
    fn simple_local_commands() -> TestResult {
        let fx = empty();
        assert!(matches!(
            fx.run("/clear"),
            Some(LocalIntercept::ClearTranscript)
        ));
        assert!(matches!(
            fx.run("/verbose"),
            Some(LocalIntercept::ToggleVerbose)
        ));
        let who = system(fx.run("/whoami"))?;
        assert!(who.contains(fx.session.as_str()));
        assert!(who.contains("Operator"));
        assert!(who.contains("prov/mod-1"));
        assert!(who.contains(InteractionMode::default().as_str()));
        match fx.run("/rename  Mein Titel ") {
            Some(LocalIntercept::RenameSession(title)) => assert_eq!(title, "Mein Titel"),
            other => return Err(TestError::Unexpected(format!("rename, got {other:?}"))),
        }
        assert!(system(fx.run("/rename"))?.contains("/rename"));
        assert_eq!(rewrite(fx.run("/sessions"))?, "/resume");
        Ok(())
    }

    #[test]
    fn hash_note_prefers_diary_operation() -> TestResult {
        let with_op = Fixture::new(CommandRegistry::new(vec![spec("diary")?]));
        assert_eq!(
            rewrite(with_op.run("#  Idee festhalten"))?,
            "/diary note Idee festhalten"
        );
        let local_only = Fixture::new(CommandRegistry::new(vec![spec("diary")?.local()]));
        assert_eq!(rewrite(local_only.run("#Idee"))?, "/memory record Idee");
        assert_eq!(rewrite(empty().run("#Idee"))?, "/memory record Idee");
        assert!(system(empty().run("#   "))?.contains("Leere Notiz"));
        Ok(())
    }

    #[test]
    fn at_mention_routes_known_roles_else_plain_chat() -> TestResult {
        let fx = empty();
        match fx.run("@Explorer finde die Konfig") {
            Some(LocalIntercept::Chat(text)) => {
                assert_eq!(text, role_mention_text("explorer", "finde die Konfig"));
            }
            other => return Err(TestError::Unexpected(format!("chat, got {other:?}"))),
        }
        match fx.run("@src/main.rs erkläre") {
            Some(LocalIntercept::Chat(text)) => assert_eq!(text, "@src/main.rs erkläre"),
            other => return Err(TestError::Unexpected(format!("chat, got {other:?}"))),
        }
        assert!(fx.run("@").is_none());
        Ok(())
    }

    #[test]
    fn plan_and_goal_hint_only_when_unregistered() -> TestResult {
        let fx = empty();
        // Runde 5, Teil F: bare `/plan` schaltet den Plan-Modus ein; der
        // Hinweis gilt für die Unterbefehle der `plan`-Operation.
        assert!(system(fx.run("/plan inspect"))?.contains("[tools.plan]"));
        assert!(system(fx.run("/goal check"))?.contains("[tools.plan]"));
        let registered = Fixture::new(CommandRegistry::new(vec![spec("plan")?, spec("goal")?]));
        assert!(registered.run("/plan inspect").is_none());
        assert!(registered.run("/goal check").is_none());
        Ok(())
    }

    /// Runde 5, Teil F: `/plan`, `/plan show|edit|list|open` sind lokal —
    /// mit und ohne registrierte `plan`-Operation.
    #[test]
    fn plan_mode_subcommands_are_local() -> TestResult {
        use crate::plan_dialog::PlanCommand;

        let registered = Fixture::new(CommandRegistry::new(vec![spec("plan")?]));
        for fx in [empty(), registered] {
            for (line, expected) in [
                ("/plan", PlanCommand::Enter),
                ("/plan show", PlanCommand::Show),
                ("/plan edit", PlanCommand::Edit),
                ("/plan list", PlanCommand::List),
                ("/plan open auth", PlanCommand::Open("auth".to_owned())),
            ] {
                match fx.run(line) {
                    Some(LocalIntercept::Plan(command)) => assert_eq!(command, expected, "{line}"),
                    other => {
                        return Err(TestError::Unexpected(format!("{line}: {other:?}")));
                    }
                }
            }
        }
        Ok(())
    }
}
