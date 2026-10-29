//! Hilfe-Overlay mit den Reitern „Befehle“, „Tasten“ und „Präfixe“.
//!
//! Spec-Quelle: `tui_contract.md` (T9, `HelpOverlay::new`).
//!
//! # Verantwortung
//! Zeigt eine statische Übersicht aller Slash-Befehle (gruppiert nach
//! [`CommandDomain`], mit Kurzbeschreibung, Aufruf und Unterbefehlen; lokale
//! TUI-Befehle als „lokal“, geplante aus
//! [`crate::command_catalog::planned_commands`] als „geplant“), der aktiven
//! Tastenbelegung ([`KeyBindings::describe`]) und der Eingabe-Präfixe.
//! Die Daten werden beim Öffnen kopiert; die Ansicht lädt nichts nach.
//!
//! # Bedienung
//! `Tab`/`Shift+Tab` wechselt den Reiter, `j`/`k` bzw. `↑`/`↓` bewegt die
//! Auswahl (Befehle) bzw. scrollt (Tasten, Präfixe), `Bild↑`/`Bild↓`
//! springen seitenweise, `Enter` auf einem Befehl belegt die Eingabezeile mit
//! `/name ` vor, `Esc`/`q` schließt.
//!
//! # Nebenläufigkeit
//! Keine; der Aufrufer hält die Ansicht exklusiv.
//!
//! # Fehlertypen
//! Keine.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::command::{CommandDomain, CommandOrigin, CommandSpec};
use crate::command_catalog::planned_commands;
use crate::i18n::{tr, tr_args};
use crate::keybindings::KeyBindings;
use crate::overlay_view::{
    OverlayOutcome, OverlayView, is_down, is_up, plain, render_panel, scroll_offset_for,
};
use crate::registry::CommandRegistry;
use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};
use unicode_width::UnicodeWidthStr;

/// Seitensprung für `Bild↑`/`Bild↓`.
const PAGE: isize = 10;

/// Hilfetext des TUI-lokalen Unterbefehls `/dod view` (PL-97 D5).
const DOD_VIEW_SUMMARY: &str = "Harw DoD view: units, sensors, recent findings (local, read-only)";

/// Fußzeile des Reiters „Befehle“.
const FOOTER_COMMANDS: &str = "Tab: tab · j/k: select · Enter: prefill · Esc: close";

/// Fußzeile der Reiter „Tasten“/„Präfixe“.
const FOOTER_SCROLL: &str = "Tab: tab · j/k: scroll · Esc: close";

/// Feste (nicht umbelegbare) Tasten, die der Reiter „Tasten“ nach den
/// umbelegbaren Aktionen zeigt.
fn fixed_keys() -> [(&'static str, &'static str); 19] {
    [
        (
            tr("help.fixed.agent_select", "Agent panel (focused): select"),
            "↑/↓, k/j, w/s",
        ),
        (
            tr(
                "help.fixed.agent_page",
                "Agent panel (focused): page / jump",
            ),
            "PgUp/PgDown, Home/End",
        ),
        (
            tr(
                "help.fixed.agent_details",
                "Agent panel (focused): open details / aggregate row",
            ),
            "Enter",
        ),
        (
            tr(
                "help.fixed.agent_finished",
                "Agent panel (focused): expand/collapse finished agents",
            ),
            "f",
        ),
        (
            tr(
                "help.fixed.agent_failures",
                "Agent panel (focused): acknowledge failures",
            ),
            "c",
        ),
        (
            tr(
                "help.fixed.agent_pin",
                "Agent panel (focused): pin/unpin agent in LIVE",
            ),
            "p",
        ),
        (
            tr(
                "help.fixed.jobs_select",
                "Jobs window (focused): select / page",
            ),
            "↑/↓, k/j, PgUp/PgDown, Home/End",
        ),
        (
            tr(
                "help.fixed.jobs_details",
                "Jobs window (focused): open job details",
            ),
            "Enter",
        ),
        (
            tr(
                "help.fixed.jobs_stop",
                "Jobs window (focused): stop selected job (confirm, then /jobs stop)",
            ),
            "x",
        ),
        (
            tr(
                "help.fixed.finished_toggle",
                "Finished/Failed (focused): expand/collapse categories",
            ),
            tr("help.fixed.finished_toggle_chords", "f (Enter expands)"),
        ),
        (
            tr(
                "help.fixed.finished_runs",
                "Finished/Failed (expanded/landscape): open and acknowledge individual runs",
            ),
            "Enter",
        ),
        (
            tr(
                "help.fixed.finished_ack",
                "Finished/Failed (focused): acknowledge selected category (collapsed: all)",
            ),
            "a",
        ),
        (
            tr(
                "help.fixed.finished_failures",
                "Finished/Failed (focused): acknowledge failures",
            ),
            "c",
        ),
        (
            tr(
                "help.fixed.live_strip",
                "LIVE strip (portrait, shallow landscape)",
            ),
            tr(
                "help.fixed.live_strip_desc",
                "Display only: never focused or scrolled, accepts no keys",
            ),
        ),
        (
            tr(
                "help.fixed.live_window",
                "LIVE window (landscape, focused): scroll / details",
            ),
            tr(
                "help.fixed.live_chords",
                "↑/↓, k/j, PgUp/PgDown, Home/g, End/G, Enter (details), r (reasoning)",
            ),
        ),
        (
            tr("help.fixed.click", "Left click"),
            tr(
                "help.fixed.click_desc",
                "Focuses the agent, Jobs, Finished or LIVE window (landscape) under the pointer",
            ),
        ),
        (
            tr(
                "help.fixed.composer",
                "Composer with text: previous / next word",
            ),
            tr(
                "help.fixed.composer_chords",
                "Alt+B / Alt+F (takes priority over focus_finished)",
            ),
        ),
        (
            tr(
                "help.fixed.dialog",
                "Dialog: scroll text (options remain visible)",
            ),
            tr(
                "help.fixed.dialog_chords",
                "Ctrl+↑/↓ (scroll_panel_up/down), mouse wheel",
            ),
        ),
        (
            tr("help.fixed.wheel", "Mouse wheel"),
            tr(
                "help.fixed.wheel_desc",
                "Scrolls the dialog, agent, Jobs, Finished, LIVE window (landscape) or history under the pointer",
            ),
        ),
    ]
}

/// Erklärungen der Eingabe-Präfixe (Präfix, Erklärung).
fn prefixes() -> [(&'static str, &'static str); 8] {
    [
        (
            "/command",
            tr(
                "help.prefix.slash",
                "Run a slash command; the popup completes names (see Commands tab).",
            ),
        ),
        (
            "!command",
            tr(
                "help.prefix.shell",
                "Run a shell command in the project directory (subject to approval).",
            ),
        ),
        (
            "!!",
            tr(
                "help.prefix.repeat",
                "Repeat the last executed shell command.",
            ),
        ),
        (
            "#text",
            tr(
                "help.prefix.note",
                "Record a note: in the diary (/diary note), otherwise as memory (/memory record).",
            ),
        ),
        (
            "@role text",
            tr(
                "help.prefix.role",
                "Send a message with a routing hint to an agent role.",
            ),
        ),
        (
            "@path",
            tr(
                "help.prefix.file",
                "Attach a project file to the message (size limited).",
            ),
        ),
        (
            "\\/text",
            tr(
                "help.prefix.escape",
                "Escape a prefix: sends “/text” as literal chat (also applies to \\! \\# \\@ \\$).",
            ),
        ),
        ("$", tr("help.prefix.reserved", "Reserved for future use.")),
    ]
}

/// Reiter des Hilfe-Overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum HelpTab {
    /// Slash-Befehle.
    #[default]
    Commands,
    /// Tastenbelegung.
    Keys,
    /// Eingabe-Präfixe.
    Prefixes,
}

impl HelpTab {
    /// Alle Reiter in Anzeigereihenfolge.
    const ALL: [Self; 3] = [Self::Commands, Self::Keys, Self::Prefixes];

    /// Localized tab label.
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Commands => tr("help.commands", "Commands"),
            Self::Keys => tr("help.keys", "Keys"),
            Self::Prefixes => tr("help.prefixes", "Prefixes"),
        }
    }

    /// Nächster Reiter (zyklisch).
    fn next(self) -> Self {
        match self {
            Self::Commands => Self::Keys,
            Self::Keys => Self::Prefixes,
            Self::Prefixes => Self::Commands,
        }
    }

    /// Vorheriger Reiter (zyklisch).
    fn prev(self) -> Self {
        match self {
            Self::Commands => Self::Prefixes,
            Self::Keys => Self::Commands,
            Self::Prefixes => Self::Keys,
        }
    }
}

/// Art eines Befehlseintrags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    /// Registrierte Operation.
    Operation,
    /// Lokal in der TUI behandelter Befehl.
    Local,
    /// Geplanter, noch nicht verfügbarer Befehl.
    Planned,
}

/// Kopierte Anzeige-Daten eines Befehls.
#[derive(Debug, Clone)]
struct CommandEntry {
    /// Name ohne `/`.
    name: String,
    /// Aliasse ohne `/`.
    aliases: Vec<String>,
    /// Kurzbeschreibung (evtl. leer).
    summary: String,
    /// Aufrufzeile (evtl. leer).
    usage: String,
    /// Unterbefehle: (Name, Argumente, Kurzbeschreibung).
    subcommands: Vec<(String, String, String)>,
    /// Gruppenüberschrift.
    group: &'static str,
    /// Sortierrang der Gruppe.
    group_rank: u8,
    /// Herkunft.
    kind: EntryKind,
}

/// Deutsche Bezeichnung und Sortierrang einer [`CommandDomain`].
pub(crate) fn domain_label(domain: CommandDomain) -> (&'static str, u8) {
    match domain {
        CommandDomain::SessionLifecycle => (tr("help.session", "Session"), 0),
        CommandDomain::AgentTopology => (tr("help.agents", "Agents"), 1),
        CommandDomain::WorkGovernance => (tr("help.work", "Work & approvals"), 2),
        CommandDomain::Execution => (tr("help.execution", "Execution"), 3),
        CommandDomain::CatalogConfig => (tr("help.config", "Models & configuration"), 4),
        CommandDomain::Knowledge => (tr("help.knowledge", "Knowledge"), 5),
        CommandDomain::Channels => (tr("help.channels", "Channels"), 6),
        CommandDomain::Identity => (tr("help.identity", "Identity & devices"), 7),
        CommandDomain::Network => (tr("help.network", "Network"), 8),
        CommandDomain::Security => (tr("help.security", "Security"), 9),
        CommandDomain::Crypto => (tr("help.crypto", "Cryptography & keys"), 10),
        CommandDomain::Misc => (tr("help.misc", "Miscellaneous"), 11),
    }
}

impl CommandEntry {
    /// Übernimmt die Anzeige-Daten aus einer [`CommandSpec`].
    fn from_spec(spec: &CommandSpec) -> Self {
        let (group, group_rank) = domain_label(spec.domain);
        Self {
            name: spec.name.as_str().to_owned(),
            aliases: spec.aliases.clone(),
            summary: spec.summary.clone(),
            usage: spec.usage.clone(),
            subcommands: spec
                .subcommands
                .iter()
                .map(|hint| {
                    (
                        hint.name.to_owned(),
                        hint.args.to_owned(),
                        hint.summary.to_owned(),
                    )
                })
                .collect(),
            group,
            group_rank,
            kind: match spec.origin {
                CommandOrigin::Operation => EntryKind::Operation,
                CommandOrigin::TuiLocal => EntryKind::Local,
            },
        }
    }

    /// Eintrag für einen geplanten Befehl.
    fn planned(name: &str, summary: &str) -> Self {
        Self {
            name: name.trim_start_matches('/').to_owned(),
            aliases: Vec::new(),
            summary: summary.to_owned(),
            usage: String::new(),
            subcommands: Vec::new(),
            group: tr("help.planned", "Planned"),
            group_rank: u8::MAX,
            kind: EntryKind::Planned,
        }
    }
}

/// Hilfe-Overlay (implementiert [`OverlayView`]).
#[derive(Debug, Clone)]
pub(crate) struct HelpOverlay {
    /// Aktiver Reiter.
    tab: HelpTab,
    /// Befehle, sortiert nach Gruppe und Name.
    commands: Vec<CommandEntry>,
    /// Tastenbelegung: (Beschriftung, Chords).
    keys: Vec<(String, String)>,
    /// Ausgewählter Befehl (Index in `commands`).
    selected: usize,
    /// Scroll-Offset des Reiters „Tasten“.
    keys_scroll: usize,
    /// Scroll-Offset des Reiters „Präfixe“.
    prefixes_scroll: usize,
}

impl HelpOverlay {
    /// Baut das Overlay aus Registry und Tastenbelegung.
    ///
    /// # Argumente
    /// - `registry`: alle bekannten Befehle (Operationen und lokale).
    /// - `keys`: aktive Tastenbelegung.
    /// - `tab`: Startreiter.
    #[must_use]
    pub(crate) fn new(registry: &CommandRegistry, keys: &KeyBindings, tab: HelpTab) -> Self {
        let mut commands: Vec<CommandEntry> = registry
            .specs()
            .iter()
            .map(CommandEntry::from_spec)
            .collect();
        for (name, summary) in planned_commands() {
            let bare = name.trim_start_matches('/');
            if commands.iter().any(|entry| entry.name == bare) {
                continue;
            }
            commands.push(CommandEntry::planned(name, summary));
        }
        commands.sort_by(|left, right| {
            left.group_rank
                .cmp(&right.group_rank)
                .then_with(|| left.name.cmp(&right.name))
        });
        commands.dedup_by(|left, right| left.name == right.name);
        // PL-97 D5: `/dod view` ist TUI-lokal (Harw-DoD-Ansicht) und steht
        // deshalb nicht in den Unterbefehlen der `dod`-Operation.
        if let Some(dod) = commands.iter_mut().find(|entry| entry.name == "dod")
            && !dod.subcommands.iter().any(|(name, _, _)| name == "view")
        {
            dod.subcommands.push((
                "view".to_owned(),
                String::new(),
                tr("help.dod_view", DOD_VIEW_SUMMARY).to_owned(),
            ));
        }

        let mut keys: Vec<(String, String)> = keys
            .describe()
            .into_iter()
            .map(|(action, chords)| {
                let chords = if chords.is_empty() {
                    tr("help.unbound", "— (unbound)").to_owned()
                } else {
                    chords.join(", ")
                };
                (action.label().to_owned(), chords)
            })
            .collect();
        keys.extend(
            fixed_keys()
                .iter()
                .map(|(label, chords)| ((*label).to_owned(), (*chords).to_owned())),
        );

        Self {
            tab,
            commands,
            keys,
            selected: 0,
            keys_scroll: 0,
            prefixes_scroll: 0,
        }
    }

    /// Aktiver Reiter.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn tab(&self) -> HelpTab {
        self.tab
    }

    /// Anzahl der Zeilen im Reiter „Tasten“.
    fn keys_len(&self) -> usize {
        self.keys.len()
    }

    /// Reiterleiste als feste Kopfzeile.
    fn tab_bar(&self, theme: Theme) -> Line<'static> {
        let mut spans = Vec::new();
        for (index, tab) in HelpTab::ALL.into_iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(" · ", style::dim_style(theme)));
            }
            let text = format!(" {} ", tab.label());
            if tab == self.tab {
                spans.push(Span::styled(text, style::selected_style(theme)));
            } else {
                spans.push(Span::styled(text, style::dim_style(theme)));
            }
        }
        Line::from(spans)
    }

    /// Zeilen des Reiters „Befehle“ und Index der letzten Zeile des
    /// ausgewählten Eintrags (für das Scrollen).
    fn command_lines(&self, theme: Theme) -> (Vec<Line<'static>>, usize) {
        let mut lines = Vec::new();
        let mut focus = 0;
        let mut last_group: Option<&str> = None;
        for (index, entry) in self.commands.iter().enumerate() {
            if last_group != Some(entry.group) {
                if last_group.is_some() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::styled(
                    entry.group.to_owned(),
                    Style::default()
                        .fg(style::accent_color(theme))
                        .add_modifier(Modifier::BOLD),
                ));
                last_group = Some(entry.group);
            }
            let is_selected = index == self.selected;
            let marker = if is_selected { "❯ " } else { "  " };
            let name_style = if is_selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::styled(
                format!("{marker}/{}", sanitize_inline(&entry.name)),
                name_style,
            )];
            match entry.kind {
                EntryKind::Operation => {}
                EntryKind::Local => {
                    spans.push(Span::styled(
                        tr("help.local", " [local]"),
                        style::dim_style(theme),
                    ));
                }
                EntryKind::Planned => {
                    spans.push(Span::styled(
                        tr("help.planned_tag", " [planned]"),
                        style::warning_style(theme),
                    ));
                }
            }
            let summary = if entry.summary.trim().is_empty() {
                "—".to_owned()
            } else {
                sanitize_inline(&entry.summary)
            };
            spans.push(Span::raw(format!("  {summary}")));
            lines.push(Line::from(spans));

            if is_selected {
                if !entry.usage.trim().is_empty() {
                    lines.push(Line::styled(
                        tr_args(
                            "help.usage",
                            "      Usage: {usage}",
                            &[("usage", &sanitize_inline(&entry.usage))],
                        ),
                        style::dim_style(theme),
                    ));
                }
                if !entry.aliases.is_empty() {
                    let aliases: Vec<String> = entry
                        .aliases
                        .iter()
                        .map(|alias| format!("/{}", sanitize_inline(alias)))
                        .collect();
                    lines.push(Line::styled(
                        tr_args(
                            "help.aliases",
                            "      Aliases: {aliases}",
                            &[("aliases", &aliases.join(", "))],
                        ),
                        style::dim_style(theme),
                    ));
                }
                for (name, args, summary) in &entry.subcommands {
                    let head = if args.trim().is_empty() {
                        sanitize_inline(name)
                    } else {
                        format!("{} {}", sanitize_inline(name), sanitize_inline(args))
                    };
                    lines.push(Line::styled(
                        format!("      {head} — {}", sanitize_inline(summary)),
                        style::dim_style(theme),
                    ));
                }
                if entry.kind == EntryKind::Planned {
                    lines.push(Line::styled(
                        tr("help.unavailable", "      Not available yet.").to_owned(),
                        style::dim_style(theme),
                    ));
                }
                focus = lines.len().saturating_sub(1);
            }
        }
        if lines.is_empty() {
            lines.push(Line::styled(
                tr("help.no_commands", "No commands registered.").to_owned(),
                style::dim_style(theme),
            ));
        }
        (lines, focus)
    }

    /// Zeilen des Reiters „Tasten“.
    fn key_lines(&self, theme: Theme) -> Vec<Line<'static>> {
        if self.keys.is_empty() {
            return vec![Line::styled(
                tr("help.no_keys", "No key bindings available.").to_owned(),
                style::dim_style(theme),
            )];
        }
        let width = self
            .keys
            .iter()
            .map(|(label, _)| label.width())
            .max()
            .unwrap_or(0);
        self.keys
            .iter()
            .map(|(label, chords)| {
                let pad = width.saturating_sub(label.width());
                Line::from(vec![
                    Span::raw(format!("  {label}{}  ", " ".repeat(pad))),
                    Span::styled(
                        chords.clone(),
                        Style::default().fg(style::accent_color(theme)),
                    ),
                ])
            })
            .collect()
    }

    /// Zeilen des Reiters „Präfixe“.
    fn prefix_lines(theme: Theme) -> Vec<Line<'static>> {
        prefixes()
            .iter()
            .map(|(prefix, text)| {
                Line::from(vec![
                    Span::styled(
                        format!("  {prefix:<12}"),
                        Style::default().fg(style::accent_color(theme)),
                    ),
                    Span::raw(format!(" {text}")),
                ])
            })
            .collect()
    }

    /// Bewegt die Auswahl bzw. den Scroll-Offset um `delta` Zeilen.
    fn move_by(&mut self, delta: isize) {
        let apply = |value: usize, len: usize| -> usize {
            let max = len.saturating_sub(1);
            value.saturating_add_signed(delta).min(max)
        };
        match self.tab {
            HelpTab::Commands => self.selected = apply(self.selected, self.commands.len()),
            HelpTab::Keys => self.keys_scroll = apply(self.keys_scroll, self.keys_len()),
            HelpTab::Prefixes => {
                self.prefixes_scroll = apply(self.prefixes_scroll, prefixes().len());
            }
        }
    }
}

impl OverlayView for HelpOverlay {
    fn refresh_presentation(&mut self, registry: &CommandRegistry, keys: &KeyBindings) {
        let selected = self
            .commands
            .get(self.selected)
            .map(|entry| entry.name.clone());
        let mut refreshed = Self::new(registry, keys, self.tab);
        refreshed.selected = selected
            .and_then(|name| {
                refreshed
                    .commands
                    .iter()
                    .position(|entry| entry.name == name)
            })
            .unwrap_or(0);
        refreshed.keys_scroll = self.keys_scroll;
        refreshed.prefixes_scroll = self.prefixes_scroll;
        *self = refreshed;
    }

    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let header = Some(self.tab_bar(theme));
        match self.tab {
            HelpTab::Commands => {
                let (lines, focus) = self.command_lines(theme);
                let height = crate::overlay_view::content_height(area, true);
                let scroll = scroll_offset_for(focus, height);
                render_panel(
                    area,
                    buf,
                    theme,
                    tr("help.title", "Help"),
                    header,
                    &lines,
                    scroll,
                    tr("help.footer_commands", FOOTER_COMMANDS),
                );
            }
            HelpTab::Keys => {
                let lines = self.key_lines(theme);
                render_panel(
                    area,
                    buf,
                    theme,
                    tr("help.title", "Help"),
                    header,
                    &lines,
                    self.keys_scroll,
                    tr("help.footer_scroll", FOOTER_SCROLL),
                );
            }
            HelpTab::Prefixes => {
                let lines = Self::prefix_lines(theme);
                render_panel(
                    area,
                    buf,
                    theme,
                    tr("help.title", "Help"),
                    header,
                    &lines,
                    self.prefixes_scroll,
                    tr("help.footer_scroll", FOOTER_SCROLL),
                );
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if is_down(&key) {
            self.move_by(1);
            return OverlayOutcome::Stay;
        }
        if is_up(&key) {
            self.move_by(-1);
            return OverlayOutcome::Stay;
        }
        match key.code {
            KeyCode::Esc => OverlayOutcome::Close,
            KeyCode::Char('q') if plain(&key) => OverlayOutcome::Close,
            KeyCode::Tab => {
                self.tab = self.tab.next();
                OverlayOutcome::Stay
            }
            KeyCode::BackTab => {
                self.tab = self.tab.prev();
                OverlayOutcome::Stay
            }
            KeyCode::PageDown => {
                self.move_by(PAGE);
                OverlayOutcome::Stay
            }
            KeyCode::PageUp => {
                self.move_by(-PAGE);
                OverlayOutcome::Stay
            }
            KeyCode::Home => {
                match self.tab {
                    HelpTab::Commands => self.selected = 0,
                    HelpTab::Keys => self.keys_scroll = 0,
                    HelpTab::Prefixes => self.prefixes_scroll = 0,
                }
                OverlayOutcome::Stay
            }
            KeyCode::Enter => {
                if self.tab != HelpTab::Commands {
                    return OverlayOutcome::Stay;
                }
                match self.commands.get(self.selected) {
                    Some(entry) if entry.kind != EntryKind::Planned => {
                        OverlayOutcome::Prefill(format!("/{} ", entry.name))
                    }
                    _ => OverlayOutcome::Stay,
                }
            }
            _ => OverlayOutcome::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::command::{CommandScope, OutputSurface, PermissionTier, SubcommandHint};
    use crate::overlay_view::buffer_text;
    use crate::test_support::{TestResult, ctx};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    const MODE_SUBS: &[SubcommandHint] = &[SubcommandHint {
        name: "default",
        args: "<modus>",
        summary: "Standardmodus speichern",
    }];

    fn registry() -> TestResult<CommandRegistry> {
        let mode = CommandSpec::new(
            "mode",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::SessionLifecycle,
        )
        .map_err(ctx("mode spec"))?
        .with_help("Interaktionsmodus wechseln", "/mode [modus]", MODE_SUBS);
        let model = CommandSpec::new(
            "model",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::CatalogConfig,
        )
        .map_err(ctx("model spec"))?
        .with_help("Sitzungsmodell wählen", "/model", &[])
        .local();
        Ok(CommandRegistry::new(vec![mode, model]))
    }

    fn overlay(tab: HelpTab) -> TestResult<HelpOverlay> {
        Ok(HelpOverlay::new(&registry()?, &KeyBindings::default(), tab))
    }

    /// PL-97 D5: die Hilfe nennt `/dod view` unter `/dod`.
    #[test]
    fn dod_help_lists_the_local_view_subcommand() -> TestResult {
        let dod = CommandSpec::new(
            "dod",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::Security,
        )
        .map_err(ctx("dod spec"))?
        .with_help("Harw-DoD", "/dod [status]", &[]);
        let help = HelpOverlay::new(
            &CommandRegistry::new(vec![dod]),
            &KeyBindings::default(),
            HelpTab::Commands,
        );
        let entry = help
            .commands
            .iter()
            .find(|entry| entry.name == "dod")
            .ok_or(crate::test_support::TestError::Missing("dod entry"))?;
        assert!(
            entry
                .subcommands
                .iter()
                .any(|(name, _, summary)| name == "view" && summary == DOD_VIEW_SUMMARY)
        );
        Ok(())
    }

    #[test]
    fn test_enter_prefills_selected_command() -> TestResult {
        let mut help = overlay(HelpTab::Commands)?;
        // Sortierung: Sitzung (/mode) vor Modelle (/model).
        assert_eq!(
            help.on_key(key(KeyCode::Enter)),
            OverlayOutcome::Prefill("/mode ".to_owned())
        );
        assert_eq!(help.on_key(key(KeyCode::Char('j'))), OverlayOutcome::Stay);
        assert_eq!(
            help.on_key(key(KeyCode::Enter)),
            OverlayOutcome::Prefill("/model ".to_owned())
        );
        Ok(())
    }

    #[test]
    fn test_selection_stays_in_bounds() -> TestResult {
        let mut help = overlay(HelpTab::Commands)?;
        for _ in 0..500 {
            help.on_key(key(KeyCode::Down));
        }
        assert!(help.selected < help.commands.len());
        for _ in 0..500 {
            help.on_key(key(KeyCode::Char('k')));
        }
        assert_eq!(help.selected, 0);
        Ok(())
    }

    #[test]
    fn test_tab_cycles_through_tabs() -> TestResult {
        let mut help = overlay(HelpTab::Commands)?;
        help.on_key(key(KeyCode::Tab));
        assert_eq!(help.tab(), HelpTab::Keys);
        help.on_key(key(KeyCode::Tab));
        assert_eq!(help.tab(), HelpTab::Prefixes);
        help.on_key(key(KeyCode::Tab));
        assert_eq!(help.tab(), HelpTab::Commands);
        help.on_key(key(KeyCode::BackTab));
        assert_eq!(help.tab(), HelpTab::Prefixes);
        Ok(())
    }

    #[test]
    fn test_enter_outside_commands_tab_stays() -> TestResult {
        let mut help = overlay(HelpTab::Keys)?;
        assert_eq!(help.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        Ok(())
    }

    #[test]
    fn test_esc_and_q_close() -> TestResult {
        let mut help = overlay(HelpTab::Prefixes)?;
        assert_eq!(help.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
        assert_eq!(help.on_key(key(KeyCode::Char('q'))), OverlayOutcome::Close);
        Ok(())
    }

    #[test]
    fn test_planned_commands_are_listed_and_not_prefilled() -> TestResult {
        let mut help = overlay(HelpTab::Commands)?;
        let planned = help
            .commands
            .iter()
            .position(|entry| entry.kind == EntryKind::Planned);
        if let Some(index) = planned {
            help.selected = index;
            assert_eq!(help.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        }
        for (name, _) in planned_commands() {
            let bare = name.trim_start_matches('/');
            assert!(help.commands.iter().any(|entry| entry.name == bare));
        }
        Ok(())
    }

    #[test]
    fn test_render_marks_local_and_shows_details() -> TestResult {
        let mut help = overlay(HelpTab::Commands)?;
        let area = Rect::new(0, 0, 90, 20);
        let mut buf = Buffer::empty(area);
        help.render(area, &mut buf, Theme::Dark);
        let text = buffer_text(&buf);
        assert!(text.contains("Commands"));
        assert!(text.contains("/mode"));
        assert!(text.contains("Usage: /mode [modus]"));
        assert!(text.contains("default <modus>"));
        assert!(text.contains("[local]"));

        help.on_key(key(KeyCode::Tab));
        help.on_key(key(KeyCode::Tab));
        let mut buf = Buffer::empty(area);
        help.render(area, &mut buf, Theme::Dark);
        let text = buffer_text(&buf);
        assert!(text.contains("!!"));
        assert!(text.contains("@role"));
        Ok(())
    }

    /// Die Scroll-Aktionen (umbelegbar) und die festen Panel-Tasten
    /// (inkl. `w`/`s`) erscheinen im Reiter „Tasten“ — mit der aktuellen
    /// Belegung.
    #[test]
    fn test_keys_tab_lists_scroll_actions_and_panel_keys() -> TestResult {
        let rebound = KeyBindings::from_toml_str(
            "scroll_panel_up = \"alt+k\"\n",
            std::path::Path::new("kb.toml"),
        )
        .map_err(ctx("keybindings"))?;
        let help = HelpOverlay::new(&registry()?, &rebound, HelpTab::Keys);
        let text: String = help
            .key_lines(Theme::Dark)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Scroll agent panel/dialog text up"), "{text}");
        assert!(text.contains("alt+k"), "{text}");
        assert!(text.contains("ctrl+down"), "{text}");
        assert!(text.contains("w/s"), "{text}");
        assert!(text.contains("Mouse wheel"), "{text}");
        // PL-97 C2: neue Aktionen samt Belegung und feste Fenstertasten.
        assert!(text.contains("Focus Jobs window"), "{text}");
        assert!(text.contains("alt+j"), "{text}");
        assert!(text.contains("alt+f"), "{text}");
        assert!(text.contains("alt+l"), "{text}");
        assert!(text.contains("pin/unpin agent in LIVE"), "{text}");
        assert!(text.contains("Finished/Failed (focused)"), "{text}");
        // PL-97 C4: Stopp im Jobs-Fenster und Quittieren in Finished/Failed.
        assert!(text.contains("stop selected job"), "{text}");
        assert!(
            text.contains("open and acknowledge individual runs"),
            "{text}"
        );
        // PL-97 C3: LIVE-Fenster des Querformats und Klick-Fokus.
        assert!(text.contains("LIVE window (landscape, focused)"), "{text}");
        assert!(text.contains("Home/g, End/G, Enter (details)"), "{text}");
        assert!(text.contains("LIVE strip (portrait"), "{text}");
        assert!(text.contains("Left click"), "{text}");
        assert!(text.contains("Finished, LIVE window (landscape)"), "{text}");
        assert!(text.contains("Composer with text"), "{text}");
        Ok(())
    }
    #[test]
    fn locale_refresh_preserves_identity_tab_and_scrolls() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        let registry = CommandRegistry::new(
            registry()?
                .specs()
                .iter()
                .filter(|spec| spec.name.as_str() != "mode")
                .cloned()
                .collect(),
        );
        let keys = KeyBindings::default();
        let mut help = HelpOverlay::new(&registry, &keys, HelpTab::Commands);
        let selected = help.commands[help.selected].name.clone();
        assert_eq!(HelpTab::Commands.label(), "Commands");
        assert_eq!(HelpTab::Keys.label(), "Keys");
        assert_eq!(HelpTab::Prefixes.label(), "Prefixes");
        assert_eq!(help.commands[help.selected].group, "Models & configuration");
        assert!(help.commands.iter().all(|entry| entry.name != "mode"));
        help.keys_scroll = 3;
        help.prefixes_scroll = 2;
        with_language(UiLanguage::Japanese, || {
            help.refresh_presentation(&registry, &keys);
            assert_eq!(HelpTab::Prefixes.label(), "プレフィックス");
            assert_eq!(help.commands[help.selected].name, selected);
            assert_eq!(help.commands[help.selected].group, "モデルと設定");
            assert_eq!(
                help.commands[help.selected].summary,
                "Sitzungsmodell wählen"
            );
            assert_eq!(help.keys_scroll, 3);
            assert_eq!(help.prefixes_scroll, 2);
            assert_eq!(help.tab(), HelpTab::Commands);
            let area = Rect::new(0, 0, 80, 16);
            let mut buf = Buffer::empty(area);
            help.render(area, &mut buf, Theme::Dark);
            let text = crate::test_support::display_buffer_text(&buf);
            assert!(text.contains("ヘルプ"), "{text}");
            assert!(text.contains("使い方: /model"), "{text}");
            assert_eq!(
                help.on_key(key(KeyCode::Enter)),
                OverlayOutcome::Prefill("/model ".to_owned())
            );
            help.on_key(key(KeyCode::Tab));
            assert_eq!(help.tab(), HelpTab::Keys);
            help.refresh_presentation(&registry, &keys);
            assert_eq!(help.tab(), HelpTab::Keys);
            assert_eq!(help.keys_scroll, 3);
        });
        Ok(())
    }

    #[test]
    fn all_locales_render_all_tabs_at_narrow_widths_and_keep_chords() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        let registry = registry()?;
        for language in UiLanguage::ALL {
            with_language(language, || {
                let keys = KeyBindings::default();
                for tab in HelpTab::ALL {
                    let mut help = HelpOverlay::new(&registry, &keys, tab);
                    for width in [1, 8, 20, 80] {
                        let area = Rect::new(0, 0, width, 12);
                        let mut buf = Buffer::empty(area);
                        help.render(area, &mut buf, Theme::Dark);
                        help.on_key(key(KeyCode::PageDown));
                        help.render(area, &mut buf, Theme::Dark);
                    }
                }
                let labels: Vec<_> = fixed_keys().into_iter().collect();
                assert_eq!(labels[0].1, "↑/↓, k/j, w/s");
                assert!(labels[8].0.contains("/jobs stop"));
                assert_eq!(prefixes()[0].0, "/command");
                assert!(prefixes()[3].1.contains("/memory record"));
            });
        }
        Ok(())
    }

    #[test]
    fn cjk_key_columns_align_by_display_width() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        let mut help = overlay(HelpTab::Keys)?;
        help.keys = vec![
            ("日本語".to_owned(), "Enter".to_owned()),
            ("ab".to_owned(), "Tab".to_owned()),
        ];
        with_language(UiLanguage::Japanese, || {
            let area = Rect::new(0, 0, 30, 2);
            let mut buf = Buffer::empty(area);
            for (index, line) in help.key_lines(Theme::Dark).into_iter().enumerate() {
                ratatui::widgets::Widget::render(line, Rect::new(0, index as u16, 30, 1), &mut buf);
            }
            assert_eq!(buf[(10, 0)].symbol(), "E");
            assert_eq!(buf[(10, 1)].symbol(), "T");
        });
        Ok(())
    }
}
