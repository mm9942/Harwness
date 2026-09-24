//! Hilfe-Overlay mit den Reitern „Befehle“, „Tasten“ und „Präfixe“.
//!
//! Spec-Quelle: `tui_contract.md` (T9, `HelpOverlay::new`).
//!
//! # Verantwortung
//! Zeigt eine statische Übersicht aller Slash-Befehle (gruppiert nach
//! [`CommandDomain`], mit Kurzbeschreibung, Aufruf und Unterbefehlen; lokale
//! TUI-Befehle als „lokal“, geplante aus
//! [`crate::command_catalog::PLANNED_COMMANDS`] als „geplant“), der aktiven
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
use crate::command_catalog::PLANNED_COMMANDS;
use crate::keybindings::KeyBindings;
use crate::overlay_view::{
    OverlayOutcome, OverlayView, is_down, is_up, plain, render_panel, scroll_offset_for,
};
use crate::registry::CommandRegistry;
use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Seitensprung für `Bild↑`/`Bild↓`.
const PAGE: isize = 10;

/// Fußzeile des Reiters „Befehle“.
const FOOTER_COMMANDS: &str = "Tab Reiter · j/k wählen · Enter vorbelegen · Esc schließen";

/// Fußzeile der Reiter „Tasten“/„Präfixe“.
const FOOTER_SCROLL: &str = "Tab Reiter · j/k scrollen · Esc schließen";

/// Erklärungen der Eingabe-Präfixe (Präfix, Erklärung).
const PREFIXES: &[(&str, &str)] = &[
    (
        "/befehl",
        "Slash-Befehl ausführen; das Popup vervollständigt Namen (siehe Reiter „Befehle“).",
    ),
    (
        "!befehl",
        "Shell-Befehl im Projektverzeichnis ausführen (unterliegt der Freigabe).",
    ),
    ("!!", "Den zuletzt ausgeführten Shell-Befehl wiederholen."),
    (
        "#text",
        "Notiz festhalten: ins Tagebuch (/diary note), sonst als Erinnerung (/memory record).",
    ),
    (
        "@rolle text",
        "Nachricht mit Routing-Hinweis an eine Agentenrolle schicken.",
    ),
    (
        "@pfad",
        "Datei aus dem Projekt an die Nachricht anhängen (Größe gedeckelt).",
    ),
    (
        "\\/text",
        "Präfix maskieren: sendet „/text“ wörtlich als Chat (gilt auch für \\! \\# \\@ \\$).",
    ),
    ("$", "Reserviert für spätere Verwendung."),
];

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

    /// Deutsche Reiterbeschriftung.
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Commands => "Befehle",
            Self::Keys => "Tasten",
            Self::Prefixes => "Präfixe",
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
fn domain_label(domain: CommandDomain) -> (&'static str, u8) {
    match domain {
        CommandDomain::SessionLifecycle => ("Sitzung", 0),
        CommandDomain::AgentTopology => ("Agenten", 1),
        CommandDomain::WorkGovernance => ("Arbeit & Freigaben", 2),
        CommandDomain::Execution => ("Ausführung", 3),
        CommandDomain::CatalogConfig => ("Modelle & Konfiguration", 4),
        CommandDomain::Knowledge => ("Wissen", 5),
        CommandDomain::Channels => ("Kanäle", 6),
        CommandDomain::Misc => ("Sonstiges", 7),
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
            group: "Geplant",
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
        for (name, summary) in PLANNED_COMMANDS {
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

        let keys = keys
            .describe()
            .into_iter()
            .map(|(action, chords)| {
                let chords = if chords.is_empty() {
                    "— (nicht belegt)".to_owned()
                } else {
                    chords.join(", ")
                };
                (action.label_de().to_owned(), chords)
            })
            .collect();

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
                    spans.push(Span::styled(" [lokal]", style::dim_style(theme)));
                }
                EntryKind::Planned => {
                    spans.push(Span::styled(" [geplant]", style::warning_style(theme)));
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
                        format!("      Aufruf: {}", sanitize_inline(&entry.usage)),
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
                        format!("      Alias: {}", aliases.join(", ")),
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
                        "      Noch nicht verfügbar.".to_owned(),
                        style::dim_style(theme),
                    ));
                }
                focus = lines.len().saturating_sub(1);
            }
        }
        if lines.is_empty() {
            lines.push(Line::styled(
                "Keine Befehle registriert.".to_owned(),
                style::dim_style(theme),
            ));
        }
        (lines, focus)
    }

    /// Zeilen des Reiters „Tasten“.
    fn key_lines(&self, theme: Theme) -> Vec<Line<'static>> {
        if self.keys.is_empty() {
            return vec![Line::styled(
                "Keine Tastenbelegung bekannt.".to_owned(),
                style::dim_style(theme),
            )];
        }
        let width = self
            .keys
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        self.keys
            .iter()
            .map(|(label, chords)| {
                let pad = width.saturating_sub(label.chars().count());
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
        PREFIXES
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
                self.prefixes_scroll = apply(self.prefixes_scroll, PREFIXES.len());
            }
        }
    }
}

impl OverlayView for HelpOverlay {
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
                    "Hilfe",
                    header,
                    &lines,
                    scroll,
                    FOOTER_COMMANDS,
                );
            }
            HelpTab::Keys => {
                let lines = self.key_lines(theme);
                render_panel(
                    area,
                    buf,
                    theme,
                    "Hilfe",
                    header,
                    &lines,
                    self.keys_scroll,
                    FOOTER_SCROLL,
                );
            }
            HelpTab::Prefixes => {
                let lines = Self::prefix_lines(theme);
                render_panel(
                    area,
                    buf,
                    theme,
                    "Hilfe",
                    header,
                    &lines,
                    self.prefixes_scroll,
                    FOOTER_SCROLL,
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
        for (name, _) in PLANNED_COMMANDS {
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
        assert!(text.contains("Befehle"));
        assert!(text.contains("/mode"));
        assert!(text.contains("Aufruf: /mode [modus]"));
        assert!(text.contains("default <modus>"));
        assert!(text.contains("[lokal]"));

        help.on_key(key(KeyCode::Tab));
        help.on_key(key(KeyCode::Tab));
        let mut buf = Buffer::empty(area);
        help.render(area, &mut buf, Theme::Dark);
        let text = buffer_text(&buf);
        assert!(text.contains("!!"));
        assert!(text.contains("@rolle"));
        Ok(())
    }
}
