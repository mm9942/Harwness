//! Profile interface preferences. Every mutation returns a regular slash command.
use crate::i18n::tr;
use crate::overlay_view::{
    OverlayOutcome, OverlayView, content_height, is_down, is_up, render_panel, scroll_offset_for,
};
use crate::style::{self, Theme};
use crossterm::event::{KeyCode, KeyEvent};
use harw_config::UiLanguage;
use ratatui::{buffer::Buffer, layout::Rect, text::Line};

const KEYS: [&str; 5] = [
    "tui.language",
    "tui.theme",
    "tui.reasoning",
    "tui.child_stream",
    "tui.status_expiry",
];
const LINKS: [&[(&str, &str)]; 3] = [
    &[
        ("models", "/models"),
        ("model", "/model"),
        ("effort", "/effort"),
    ],
    &[("mode", "/mode"), ("permissions", "/permissions show")],
    &[("providers", "/provider list")],
];

#[derive(Debug, Default)]
pub(crate) struct SettingsView {
    category: usize,
    selected: usize,
    editing: Option<usize>,
    values: [Option<String>; 5],
    error: Option<String>,
    hidden: Vec<String>,
}

fn choices(index: usize) -> Vec<&'static str> {
    match index {
        0 => UiLanguage::ALL
            .into_iter()
            .map(UiLanguage::as_str)
            .collect(),
        1 => vec!["auto", "dark", "light"],
        2 => vec!["message", "collapsed", "hidden"],
        3 => vec!["orchestrators", "all", "none"],
        _ => vec!["consolidate", "hide"],
    }
}
fn label(key: &str) -> &'static str {
    match key {
        "tui.language" => tr("settings.language", "Language"),
        "tui.theme" => tr("settings.theme", "Theme"),
        "tui.reasoning" => tr("settings.reasoning", "Reasoning"),
        "tui.child_stream" => tr("settings.child_stream", "Child agent stream"),
        "tui.status_expiry" => tr("settings.status_expiry", "Expired status"),
        "models" => tr("settings.models", "Models by role"),
        "model" => tr("settings.model", "Session model"),
        "effort" => tr("settings.effort", "Reasoning effort"),
        "mode" => tr("settings.mode", "Interaction mode"),
        "permissions" => tr("settings.permissions", "Approvals"),
        _ => tr("settings.providers", "Providers"),
    }
}
fn value_label(value: &str) -> String {
    if let Ok(language) = value.parse::<UiLanguage>() {
        return language.native_name().to_owned();
    }
    match value {
        "auto" => tr("settings.value_auto", "Automatic").to_owned(),
        "dark" => tr("settings.value_dark", "Dark").to_owned(),
        "light" => tr("settings.value_light", "Light").to_owned(),
        "message" => tr("settings.value_message", "Expanded message").to_owned(),
        "collapsed" => tr("settings.value_collapsed", "Collapsed").to_owned(),
        "hidden" => tr("settings.value_hidden", "Hidden").to_owned(),
        "orchestrators" => tr("settings.value_orchestrators", "Orchestrators only").to_owned(),
        "all" => tr("settings.value_all", "All agents").to_owned(),
        "none" => tr("settings.value_none", "No agents").to_owned(),
        "consolidate" => tr("settings.value_consolidate", "Consolidate").to_owned(),
        "hide" => tr("settings.value_hide", "Hide").to_owned(),
        _ => value.to_owned(),
    }
}
impl SettingsView {
    pub(crate) fn new(hidden: &[String]) -> Self {
        Self {
            hidden: hidden.to_vec(),
            ..Self::default()
        }
    }
    fn links(&self) -> Vec<(&'static str, &'static str)> {
        LINKS[self.category - 1]
            .iter()
            .copied()
            .filter(|(_, command)| {
                let name = command
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                !self
                    .hidden
                    .iter()
                    .any(|hidden| hidden.eq_ignore_ascii_case(name))
            })
            .collect()
    }
    fn row_count(&self) -> usize {
        if self.category == 0 {
            KEYS.len()
        } else {
            self.links().len()
        }
    }
}
impl OverlayView for SettingsView {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let tabs = [
            tr("settings.interface", "Interface"),
            tr("settings.models", "Models by role"),
            tr("settings.behavior", "Behavior / Approvals"),
            tr("settings.providers", "Providers"),
        ];
        let header = Line::from(if area.width < 70 {
            format!("[{}] · Tab", tabs[self.category])
        } else {
            tabs.iter()
                .enumerate()
                .map(|(index, name)| {
                    if index == self.category {
                        format!("[{name}]")
                    } else {
                        (*name).to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" · ")
        });
        let mut lines = vec![Line::from(if self.category == 0 {
            tr("settings.scope", "Active profile · applies immediately")
        } else {
            tr(
                "settings.delegated",
                "Existing command rules and scopes apply",
            )
        })];
        let focus;
        if let Some(choice) = self.editing {
            lines.push(Line::from(label(KEYS[self.selected])));
            for (index, value) in choices(self.selected).iter().enumerate() {
                lines.push(Line::styled(
                    format!(
                        "{} {}",
                        if index == choice { "›" } else { " " },
                        value_label(value)
                    ),
                    if index == choice {
                        style::selected_style(theme)
                    } else {
                        style::dim_style(theme)
                    },
                ));
            }
            focus = choice + 2;
        } else {
            let rows: Vec<_> = if self.category == 0 {
                KEYS.iter()
                    .enumerate()
                    .map(|(index, key)| {
                        format!(
                            "{}: {}",
                            label(key),
                            self.values[index]
                                .as_deref()
                                .map(value_label)
                                .unwrap_or_else(|| "…".to_owned())
                        )
                    })
                    .collect()
            } else {
                self.links()
                    .iter()
                    .map(|(key, command)| format!("{}  {command}", label(key)))
                    .collect()
            };
            for (index, row) in rows.into_iter().enumerate() {
                lines.push(Line::styled(
                    format!("{} {row}", if index == self.selected { "›" } else { " " }),
                    if index == self.selected {
                        style::selected_style(theme)
                    } else {
                        style::dim_style(theme)
                    },
                ));
            }
            focus = self.selected + 1;
        }
        if let Some(error) = &self.error {
            lines[0] = Line::from(crate::sanitize::sanitize_inline(error));
        }
        render_panel(
            area,
            buf,
            theme,
            tr("settings.title", "Settings"),
            Some(header),
            &lines,
            scroll_offset_for(focus, content_height(area, true)),
            if area.width < 40 {
                "↑↓ Enter Esc Tab x r"
            } else {
                tr(
                    "settings.controls",
                    "Tab: category · ↑↓: select · Enter: edit/save · Esc: cancel · x: reset · r: refresh",
                )
            },
        );
    }
    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if let Some(choice) = self.editing.as_mut() {
            let values = choices(self.selected);
            if is_down(&key) {
                *choice = (*choice + 1) % values.len();
            } else if is_up(&key) {
                *choice = (*choice + values.len() - 1) % values.len();
            } else if key.code == KeyCode::Esc {
                self.editing = None;
            } else if key.code == KeyCode::Enter {
                let command = format!("/settings set {} {}", KEYS[self.selected], values[*choice]);
                self.editing = None;
                return OverlayOutcome::Run(command);
            }
            return OverlayOutcome::Stay;
        }
        let count = self.row_count();
        if is_down(&key) && count > 0 {
            self.selected = (self.selected + 1) % count;
        } else if is_up(&key) && count > 0 {
            self.selected = (self.selected + count - 1) % count;
        } else {
            match key.code {
                KeyCode::Esc => return OverlayOutcome::Close,
                KeyCode::Tab | KeyCode::BackTab => {
                    self.category =
                        (self.category + if key.code == KeyCode::Tab { 1 } else { 3 }) % 4;
                    self.selected = 0;
                }
                KeyCode::Char('r') => return OverlayOutcome::Fetch("/settings show".to_owned()),
                KeyCode::Char('x')
                    if self.category == 0 && self.values[self.selected].is_some() =>
                {
                    return OverlayOutcome::Run(format!("/settings reset {}", KEYS[self.selected]));
                }
                KeyCode::Enter if self.category == 0 => {
                    if let Some(value) = &self.values[self.selected] {
                        self.editing = Some(
                            choices(self.selected)
                                .iter()
                                .position(|choice| *choice == value)
                                .unwrap_or(0),
                        );
                    }
                }
                KeyCode::Enter if count > 0 => {
                    return OverlayOutcome::RunAndClose(self.links()[self.selected].1.to_owned());
                }
                _ => {}
            }
        }
        OverlayOutcome::Stay
    }
    fn refresh_command(&self) -> Option<String> {
        Some("/settings show".to_owned())
    }
    fn apply_data(&mut self, data: &serde_json::Value) {
        self.error = None;
        self.values = std::array::from_fn(|index| {
            data["rows"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["key"] == KEYS[index]))
                .and_then(|row| row["value"].as_str())
                .filter(|value| choices(index).contains(value))
                .map(str::to_owned)
        });
    }
    fn apply_error(&mut self, text: &str) {
        self.error = Some(text.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }
    fn loaded() -> SettingsView {
        let mut view = SettingsView::default();
        view.apply_data(&serde_json::json!({"rows": [{"key": "tui.language", "value": "en"}, {"key": "tui.theme", "value": "dark"}]}));
        view
    }
    #[test]
    fn navigate_edit_cancel_reset_refresh() {
        let mut view = loaded();
        assert_eq!(view.refresh_command().as_deref(), Some("/settings show"));
        assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        view.on_key(key(KeyCode::Down));
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::Run("/settings set tui.language de".to_owned())
        );
        view.on_key(key(KeyCode::Enter));
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Stay);
        assert_eq!(
            view.on_key(key(KeyCode::Char('x'))),
            OverlayOutcome::Run("/settings reset tui.language".to_owned())
        );
        assert_eq!(
            view.on_key(key(KeyCode::Char('r'))),
            OverlayOutcome::Fetch("/settings show".to_owned())
        );
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
    }
    #[test]
    fn payload_cannot_supply_commands_or_secrets() {
        let mut view = loaded();
        view.apply_data(&serde_json::json!({"secret": "secret-value", "rows": [{"key":"tui.language", "value":"en /provider set secret"}]}));
        assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        assert!(view.editing.is_none());
        assert!(view.values.iter().all(Option::is_none));
    }
    #[test]
    fn delegated_links_respect_hidden_commands() {
        let mut view = SettingsView::new(&["model".to_owned(), "models".to_owned()]);
        view.on_key(key(KeyCode::Tab));
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose("/effort".to_owned())
        );
    }
    #[test]
    fn committed_durability_warning_is_explicit_and_localized() {
        let output = harw_operations::OpOutput {
            text: "payload must not be used for inference".to_owned(),
            data: Some(
                serde_json::json!({"rows": [{"key":"tui.language", "value":"ja"}], "warning":"durability_uncertain", "secret":"never-display"}),
            ),
        };
        let english = present_output(UiLanguage::English, &output);
        assert!(english.contains("saved and applied"));
        assert!(english.contains("日本語"));
        assert!(!english.contains("never-display"));
        let german = present_output(UiLanguage::German, &output);
        assert!(german.contains("gespeichert und angewendet"));
        assert_eq!(
            output.data.as_ref().map(|data| data["warning"].as_str()),
            Some(Some("durability_uncertain"))
        );
    }

    #[test]
    fn rendered_settings_samples() {
        let view = loaded();
        for language in [UiLanguage::English, UiLanguage::Japanese] {
            for width in [80, 24] {
                let area = Rect::new(0, 0, width, 10);
                let mut buffer = Buffer::empty(area);
                crate::i18n::with_language(language, || {
                    view.render(area, &mut buffer, Theme::Dark)
                });
                let rows = (0..area.height)
                    .map(|y| {
                        let mut row = String::new();
                        let mut x = 0;
                        while x < area.width {
                            let symbol = buffer[(x, y)].symbol();
                            row.push_str(symbol);
                            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
                        }
                        row.trim_end().to_owned()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(rows.contains(crate::i18n::tr_for(language, "settings.title", "Settings")));
                assert!(!rows.contains("secret"));
                println!("{} / {} columns\n{}", language.as_str(), width, rows);
            }
        }
    }

    #[test]
    fn narrow_render_all_languages_and_refresh_preserves_selection() {
        let mut view = loaded();
        view.on_key(key(KeyCode::Enter));
        view.on_key(key(KeyCode::Down));
        for language in UiLanguage::ALL {
            for width in [1, 8, 20, 60] {
                let area = Rect::new(0, 0, width, 8);
                let mut buffer = Buffer::empty(area);
                crate::i18n::with_language(language, || {
                    view.render(area, &mut buffer, Theme::Dark)
                });
            }
        }
        assert_eq!(view.editing, Some(1));
    }
}

/// Project only allowlisted operation data; preserve payload and durability status.
pub(crate) fn present_output(language: UiLanguage, output: &harw_operations::OpOutput) -> String {
    crate::i18n::with_language(language, || {
        let mut lines =
            vec![tr("settings.scope", "Active profile · applies immediately").to_owned()];
        let mut view = SettingsView::default();
        if let Some(data) = &output.data {
            view.apply_data(data);
        }
        for (index, key) in KEYS.iter().enumerate() {
            if let Some(value) = &view.values[index] {
                lines.push(format!("{}: {}", label(key), value_label(value)));
            }
        }
        if output
            .data
            .as_ref()
            .is_some_and(|data| data["warning"] == "durability_uncertain")
        {
            lines.push(
                tr(
                    "settings.durability",
                    "Settings were saved and applied, but durability across a crash is uncertain.",
                )
                .to_owned(),
            );
        }
        lines.join("\n")
    })
}
