//! Modus- und Freigabe-Auswahl (`/mode`, F7).
//!
//! Spec-Quelle: `tui_contract.md` (T12, Entscheidung 6: „/mode und F7 öffnen
//! ModePicker mit zwei Abschnitten „Modus“/„Freigabe““).
//!
//! # Verantwortung
//! Zeigt zwei Abschnitte:
//! - **Modus** — alle [`InteractionMode`]s mit deutscher Kurzbeschreibung,
//!   dem aktiven und dem konfigurierten Standardmodus. `Enter` führt
//!   `/mode <modus>` aus (wirkt ab der nächsten Turn-Grenze), `d` speichert
//!   ihn per `/mode default <modus>` als Standard.
//! - **Freigabe** — alle [`ApprovalMode`]s. `Enter` führt
//!   `/permissions mode <ask|auto|full>` aus (Default-Scope `--session`,
//!   wirkt sofort; laufende Kinder folgen, höchstens `auto`).
//!
//! Die Ansicht schreibt nichts selbst; sie liefert Slash-Zeilen als
//! [`OverlayOutcome`]. Frische Daten kommen über `/mode show`.
//!
//! # Bedienung
//! `Tab`/`Shift+Tab` wechselt den Abschnitt, `j`/`k` bzw. `↑`/`↓` wählt,
//! `Enter` übernimmt, `d` (nur Modus) setzt den Standard, `Esc`/`q` schließt.
//!
//! # Nebenläufigkeit
//! Keine; der Aufrufer hält die Ansicht exklusiv.
//!
//! # Fehlertypen
//! Keine — ein fehlgeschlagenes Nachladen wird als Hinweiszeile angezeigt.

use crossterm::event::{KeyCode, KeyEvent};
use harw_core::InteractionMode;
use harw_extension_api::approval_mode::ApprovalMode;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
};

use crate::i18n::{tr, tr_args};
use crate::overlay_view::{
    OverlayOutcome, OverlayView, content_height, is_down, is_up, json_str, plain, render_panel,
    scroll_offset_for,
};
use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};
use unicode_width::UnicodeWidthStr;

/// Befehl, der aktiven und Standardmodus liefert.
pub(crate) const REFRESH_COMMAND: &str = "/mode show";

/// Fußzeile des Abschnitts „Modus“.
const FOOTER_MODE: &str = "Tab section · j/k select · Enter set · d set default · Esc";

/// Fußzeile des Abschnitts „Freigabe“.
const FOOTER_APPROVAL: &str = "Tab section · j/k select · Enter set · Esc close";

/// Abschnitt des Pickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ModeSection {
    /// Interaktionsmodus.
    #[default]
    Mode,
    /// Freigabemodus.
    Approval,
}

impl ModeSection {
    /// Deutsche Beschriftung.
    fn label(self) -> &'static str {
        match self {
            Self::Mode => tr("pickers.mode_section", "Mode"),
            Self::Approval => tr("pickers.approval_section", "Approval"),
        }
    }

    /// Anderer Abschnitt.
    fn toggled(self) -> Self {
        match self {
            Self::Mode => Self::Approval,
            Self::Approval => Self::Mode,
        }
    }
}

/// Modus-/Freigabe-Picker (implementiert [`OverlayView`]).
#[derive(Debug, Clone)]
pub(crate) struct ModePicker {
    /// Aktiver Abschnitt.
    section: ModeSection,
    /// Aktiver Interaktionsmodus.
    current: InteractionMode,
    /// Konfigurierter Standardmodus (Rohtext aus der Konfiguration).
    configured_default: Option<String>,
    /// Aktiver Freigabemodus, falls bekannt.
    approval: Option<ApprovalMode>,
    /// Cursor im Abschnitt „Modus“ (Index in [`InteractionMode::ALL`]).
    mode_cursor: usize,
    /// Cursor im Abschnitt „Freigabe“ (Index in [`ApprovalMode::ALL`]).
    approval_cursor: usize,
    /// Letzter Fehler beim Nachladen.
    error: Option<String>,
}

/// Index eines Modus in [`InteractionMode::ALL`] (0, falls unbekannt).
fn mode_index(mode: InteractionMode) -> usize {
    InteractionMode::ALL
        .iter()
        .position(|candidate| *candidate == mode)
        .unwrap_or(0)
}

impl ModePicker {
    /// Erstellt den Picker; der Cursor steht jeweils auf dem aktiven Wert.
    ///
    /// # Argumente
    /// - `current`: aktiver Interaktionsmodus.
    /// - `configured_default`: `[mode] default` aus der Konfiguration.
    /// - `approval`: aktiver Freigabemodus, falls bekannt.
    #[must_use]
    pub(crate) fn new(
        current: InteractionMode,
        configured_default: Option<&str>,
        approval: Option<ApprovalMode>,
    ) -> Self {
        let approval_cursor = approval
            .and_then(|mode| {
                ApprovalMode::ALL
                    .iter()
                    .position(|candidate| *candidate == mode)
            })
            .unwrap_or(0);
        Self {
            section: ModeSection::Mode,
            current,
            configured_default: configured_default
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            approval,
            mode_cursor: mode_index(current),
            approval_cursor,
            error: None,
        }
    }

    /// Öffnet direkt im angegebenen Abschnitt.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_section(mut self, section: ModeSection) -> Self {
        self.section = section;
        self
    }

    /// Aktiver Abschnitt.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn section(&self) -> ModeSection {
        self.section
    }

    /// Ob `mode` der konfigurierte Standard ist.
    fn is_default(&self, mode: InteractionMode) -> bool {
        self.configured_default
            .as_deref()
            .and_then(InteractionMode::parse)
            == Some(mode)
    }

    /// Abschnittsleiste als feste Kopfzeile.
    fn section_bar(&self, theme: Theme) -> Line<'static> {
        let mut spans = Vec::new();
        for (index, section) in [ModeSection::Mode, ModeSection::Approval]
            .into_iter()
            .enumerate()
        {
            if index > 0 {
                spans.push(Span::styled(" · ", style::dim_style(theme)));
            }
            let text = format!(" {} ", section.label());
            let span_style = if section == self.section {
                style::selected_style(theme)
            } else {
                style::dim_style(theme)
            };
            spans.push(Span::styled(text, span_style));
        }
        Line::from(spans)
    }

    /// Eine Auswahlzeile.
    fn option_line(
        theme: Theme,
        selected: bool,
        name: &str,
        tags: &str,
        summary: &str,
    ) -> Line<'static> {
        let marker = if selected { "❯ " } else { "  " };
        let name_style = if selected {
            style::selected_style(theme)
        } else {
            Style::default()
        };
        Line::from(vec![
            Span::styled(
                format!(
                    "{marker}{name}{}",
                    " ".repeat(8usize.saturating_sub(UnicodeWidthStr::width(name)))
                ),
                name_style,
            ),
            Span::styled(
                format!(
                    "{tags}{}",
                    " ".repeat(22usize.saturating_sub(UnicodeWidthStr::width(tags)))
                ),
                Style::default().fg(style::accent_color(theme)),
            ),
            Span::raw(summary.to_owned()),
        ])
    }

    /// Inhaltszeilen und Index der Cursorzeile.
    fn body_lines(&self, theme: Theme) -> (Vec<Line<'static>>, usize) {
        let approval_text = self
            .approval
            .map_or(tr("pickers.unknown", "unknown"), ApprovalMode::as_str);
        let mut lines = vec![
            Line::styled(
                tr_args(
                    "pickers.current",
                    "Current: mode {mode} · approval {approval}",
                    &[("mode", self.current.as_str()), ("approval", approval_text)],
                ),
                style::dim_style(theme),
            ),
            Line::from(""),
        ];
        let first_option = lines.len();
        let focus = match self.section {
            ModeSection::Mode => {
                for (index, mode) in InteractionMode::ALL.into_iter().enumerate() {
                    let mut tags = Vec::new();
                    if mode == self.current {
                        tags.push(tr("pickers.active", "active"));
                    }
                    if self.is_default(mode) {
                        tags.push(tr("pickers.default", "default"));
                    }
                    let tags = if tags.is_empty() {
                        String::new()
                    } else {
                        format!("[{}]", tags.join(", "))
                    };
                    lines.push(Self::option_line(
                        theme,
                        index == self.mode_cursor,
                        mode.as_str(),
                        &tags,
                        mode_summary(mode),
                    ));
                }
                first_option + self.mode_cursor
            }
            ModeSection::Approval => {
                for (index, mode) in ApprovalMode::ALL.into_iter().enumerate() {
                    let tags = if Some(mode) == self.approval {
                        format!("[{}]", tr("pickers.active", "active"))
                    } else {
                        String::new()
                    };
                    lines.push(Self::option_line(
                        theme,
                        index == self.approval_cursor,
                        mode.as_str(),
                        &tags,
                        approval_summary(mode),
                    ));
                }
                first_option + self.approval_cursor
            }
        };
        lines.push(Line::from(""));
        let hint = match self.section {
            ModeSection::Mode => {
                let default = self.configured_default.as_deref().unwrap_or("—");
                tr_args(
                    "pickers.mode_hint",
                    "Applies at the next turn boundary. Default: {default}",
                    &[("default", &sanitize_inline(default))],
                )
            }
            ModeSection::Approval => tr(
                "pickers.approval_hint",
                "Applies immediately; running children follow, capped at auto.",
            )
            .to_owned(),
        };
        lines.push(Line::styled(hint, style::dim_style(theme)));
        if let Some(error) = &self.error {
            lines.push(Line::styled(
                tr_args(
                    "pickers.refresh_error",
                    "Refresh failed: {error}",
                    &[("error", &sanitize_inline(error))],
                ),
                style::error_style(theme),
            ));
        }
        (lines, focus)
    }

    /// Bewegt den Cursor des aktiven Abschnitts.
    fn move_cursor(&mut self, down: bool) {
        let (cursor, len) = match self.section {
            ModeSection::Mode => (&mut self.mode_cursor, InteractionMode::ALL.len()),
            ModeSection::Approval => (&mut self.approval_cursor, ApprovalMode::ALL.len()),
        };
        *cursor = if down {
            (*cursor + 1).min(len.saturating_sub(1))
        } else {
            cursor.saturating_sub(1)
        };
    }

    /// Modus unter dem Cursor.
    fn cursor_mode(&self) -> Option<InteractionMode> {
        InteractionMode::ALL.get(self.mode_cursor).copied()
    }

    /// Freigabemodus unter dem Cursor.
    fn cursor_approval(&self) -> Option<ApprovalMode> {
        ApprovalMode::ALL.get(self.approval_cursor).copied()
    }
}

impl OverlayView for ModePicker {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let (lines, focus) = self.body_lines(theme);
        let scroll = scroll_offset_for(focus, content_height(area, true));
        let footer = match self.section {
            ModeSection::Mode => tr("pickers.mode_footer", FOOTER_MODE),
            ModeSection::Approval => tr("pickers.approval_footer", FOOTER_APPROVAL),
        };
        render_panel(
            area,
            buf,
            theme,
            tr("pickers.mode_title", "Mode & approval"),
            Some(self.section_bar(theme)),
            &lines,
            scroll,
            footer,
        );
    }

    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if is_down(&key) {
            self.move_cursor(true);
            return OverlayOutcome::Stay;
        }
        if is_up(&key) {
            self.move_cursor(false);
            return OverlayOutcome::Stay;
        }
        match key.code {
            KeyCode::Esc => OverlayOutcome::Close,
            KeyCode::Char('q') if plain(&key) => OverlayOutcome::Close,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.section = self.section.toggled();
                OverlayOutcome::Stay
            }
            KeyCode::Enter => match self.section {
                ModeSection::Mode => match self.cursor_mode() {
                    // Runde 5, Teil F: `plan` läuft wie Shift+Tab über die
                    // Stufe `plan` (`/plan`): sofortige Sperre, Freigabe
                    // `ask`, Rückkehr zum vorherigen Modus beim Verlassen.
                    Some(InteractionMode::Plan) => OverlayOutcome::RunAndClose("/plan".to_owned()),
                    Some(mode) => OverlayOutcome::RunAndClose(format!("/mode {}", mode.as_str())),
                    None => OverlayOutcome::Stay,
                },
                ModeSection::Approval => match self.cursor_approval() {
                    Some(mode) => {
                        OverlayOutcome::RunAndClose(format!("/permissions mode {}", mode.as_str()))
                    }
                    None => OverlayOutcome::Stay,
                },
            },
            KeyCode::Char('d') if plain(&key) && self.section == ModeSection::Mode => {
                match self.cursor_mode() {
                    Some(mode) => {
                        // Optimistisch anzeigen; `/mode show` bestätigt danach.
                        self.configured_default = Some(mode.as_str().to_owned());
                        OverlayOutcome::Run(format!("/mode default {}", mode.as_str()))
                    }
                    None => OverlayOutcome::Stay,
                }
            }
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        Some(REFRESH_COMMAND.to_owned())
    }

    fn apply_data(&mut self, data: &serde_json::Value) {
        if let Some(mode) = json_str(data, "mode").and_then(|text| InteractionMode::parse(&text)) {
            self.current = mode;
        }
        if let Some(default) = json_str(data, "default") {
            self.configured_default = Some(default);
        }
        self.error = None;
    }

    fn apply_error(&mut self, text: &str) {
        self.error = Some(text.to_owned());
    }
}

fn mode_summary(mode: InteractionMode) -> &'static str {
    match mode {
        InteractionMode::Chat => tr(
            "pickers.summary_chat",
            "Conversation without tool restrictions",
        ),
        InteractionMode::Plan => tr(
            "pickers.summary_plan",
            "Plan and research without making changes",
        ),
        InteractionMode::Explore => {
            tr("pickers.summary_explore", "Read-only tools for exploration")
        }
        InteractionMode::Work => tr(
            "pickers.summary_work",
            "Execute with all tools, including writing and shell",
        ),
        InteractionMode::Shell => tr(
            "pickers.summary_shell",
            "Like work; host tools require user-approved sandbox access",
        ),
    }
}

fn approval_summary(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::AlwaysAsk => tr(
            "pickers.approval_ask",
            "Confirm every tool call individually",
        ),
        ApprovalMode::Delegated => tr(
            "pickers.approval_auto",
            "harw approves read tools; the auto classifier judges the rest and asks when unsure",
        ),
        ApprovalMode::FullAccess => tr(
            "pickers.approval_full",
            "No tool call requires confirmation",
        ),
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::test_support::display_buffer_text as buffer_text;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn picker() -> ModePicker {
        ModePicker::new(
            InteractionMode::Chat,
            Some("plan"),
            Some(ApprovalMode::Delegated),
        )
    }

    #[test]
    fn locale_changes_preserve_section_and_canonical_commands_at_narrow_widths() {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        let mut picker = picker();
        picker.on_key(key(KeyCode::Tab));
        picker.on_key(key(KeyCode::Down));
        for language in UiLanguage::ALL {
            with_language(language, || {
                for width in [1, 8, 24, 80] {
                    let area = Rect::new(0, 0, width, 12);
                    picker.render(area, &mut Buffer::empty(area), Theme::Dark);
                }
            });
        }
        let area = Rect::new(0, 0, 100, 16);
        let mut buf = Buffer::empty(area);
        with_language(UiLanguage::Japanese, || {
            picker.render(area, &mut buf, Theme::Dark)
        });
        assert!(buffer_text(&buf).contains("モードと承認"));
        assert!(buffer_text(&buf).contains("ツール呼び出しの確認なし"));
        assert_eq!(picker.section(), ModeSection::Approval);
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose("/permissions mode full".into())
        );
    }

    #[test]
    fn test_enter_on_current_mode_runs_mode_command() {
        let mut picker = picker();
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose("/mode chat".to_owned())
        );
    }

    #[test]
    fn test_cursor_moves_and_enter_selects_next_mode() {
        let mut picker = picker();
        picker.on_key(key(KeyCode::Char('j')));
        picker.on_key(key(KeyCode::Char('j')));
        let expected = InteractionMode::ALL[2].as_str();
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose(format!("/mode {expected}"))
        );
    }

    /// Runde 5, Teil F: `plan` im Picker schaltet über `/plan` die Stufe
    /// `plan` (sofortige Sperre) statt über das an der Turn-Grenze
    /// gestaffelte `/mode plan`.
    #[test]
    fn test_picking_plan_runs_the_plan_stage_command() {
        let mut picker = picker();
        picker.on_key(key(KeyCode::Char('j')));
        assert_eq!(InteractionMode::ALL[1], InteractionMode::Plan);
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose("/plan".to_owned())
        );
    }

    #[test]
    fn test_d_sets_default_mode() {
        let mut picker = picker();
        picker.on_key(key(KeyCode::Down));
        let expected = InteractionMode::ALL[1].as_str();
        assert_eq!(
            picker.on_key(key(KeyCode::Char('d'))),
            OverlayOutcome::Run(format!("/mode default {expected}"))
        );
        assert_eq!(picker.configured_default.as_deref(), Some(expected));
    }

    #[test]
    fn test_tab_switches_to_approval_and_enter_runs_permissions() {
        let mut picker = picker();
        picker.on_key(key(KeyCode::Tab));
        assert_eq!(picker.section(), ModeSection::Approval);
        // Cursor steht auf dem aktiven Freigabemodus (auto).
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose("/permissions mode auto".to_owned())
        );
        picker.on_key(key(KeyCode::Down));
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose("/permissions mode full".to_owned())
        );
        // `d` hat im Abschnitt „Freigabe“ keine Wirkung.
        assert_eq!(picker.on_key(key(KeyCode::Char('d'))), OverlayOutcome::Stay);
        picker.on_key(key(KeyCode::BackTab));
        assert_eq!(picker.section(), ModeSection::Mode);
    }

    #[test]
    fn test_cursor_stays_in_bounds_and_esc_closes() {
        let mut picker = picker().with_section(ModeSection::Approval);
        for _ in 0..20 {
            picker.on_key(key(KeyCode::Down));
        }
        assert_eq!(picker.approval_cursor, ApprovalMode::ALL.len() - 1);
        for _ in 0..20 {
            picker.on_key(key(KeyCode::Char('k')));
        }
        assert_eq!(picker.approval_cursor, 0);
        assert_eq!(picker.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
    }

    #[test]
    fn test_apply_data_updates_current_and_default() {
        let mut picker = picker();
        picker.apply_error("weg");
        assert_eq!(picker.refresh_command().as_deref(), Some("/mode show"));
        picker.apply_data(&serde_json::json!({
            "mode": "work", "known": true, "default": "chat",
            "available_modes": [{"name": "chat", "summary": "…"}]
        }));
        assert_eq!(picker.current, InteractionMode::Work);
        assert_eq!(picker.configured_default.as_deref(), Some("chat"));
        assert_eq!(picker.error, None);
        picker.apply_data(&serde_json::json!({"mode": null}));
        assert_eq!(picker.current, InteractionMode::Work);
    }

    #[test]
    fn test_render_shows_sections_and_hints() {
        let mut picker = picker();
        let area = Rect::new(0, 0, 110, 16);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf, Theme::Dark);
        let text = buffer_text(&buf);
        assert!(text.contains("Mode"));
        assert!(text.contains("Approval"));
        assert!(text.contains("[active]"));
        assert!(text.contains("[default]"));
        assert!(text.contains("next turn boundary"));

        picker.on_key(key(KeyCode::Tab));
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf, Theme::Dark);
        let text = buffer_text(&buf);
        assert!(text.contains("capped at auto"));
        assert!(text.contains("full"));
    }
}
