//! Profile interface preferences. Every mutation returns a regular slash command.
//!
//! PL-100 WP 1.9: the interface tab also carries the DoD watch mode
//! (`dod.watch.mode`, a choice like the other keys) and four read-only DoD
//! rows from `data.dod` of `/settings show`: the same-TTY approval mode
//! (`[dod.approve] tui_tty`, labelled "schwächere Bindung, nur Freeze/Thaw"),
//! the triage model, which only names `/dod model set <provider/model>`, and
//! the triage `effort` and `max_tokens`. No read-only row edits, resets or
//! runs anything. `/settings` is TUI-only and its text is replaced by
//! `present_output`, so these rows are also the only place the TUI prints
//! the DoD values. A value that is delivered but invalid (a model that is no
//! `<provider>/<model>` reference, an unknown effort, `max_tokens` outside
//! `1..=MAX_TRIAGE_TOKENS`) is never echoed: the row reads "invalid (see
//! /dod model show)". An unset model reads "acts as notify" only while the
//! watch mode is `triage`.
use crate::i18n::tr;
use crate::overlay_view::{
    OverlayOutcome, OverlayView, content_height, is_down, is_up, render_panel, scroll_offset_for,
};
use crate::style::{self, Theme};
use crossterm::event::{KeyCode, KeyEvent};
use harw_config::{DodWatchMode, UiLanguage};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Widget, Wrap},
};

const KEYS: [&str; 6] = [
    "tui.language",
    "tui.theme",
    "tui.reasoning",
    "tui.child_stream",
    "tui.status_expiry",
    "dod.watch.mode",
];
/// Read-only DoD rows after [`KEYS`] on the interface tab: `[dod.approve]
/// tui_tty` and `[dod.triage]` model, effort and max_tokens.
const READ_ONLY_ROWS: usize = 4;
const LINKS: [&[(&str, &str)]; 3] = [
    &[
        ("models", "/models"),
        ("model", "/model"),
        ("effort", "/effort"),
    ],
    &[("mode", "/mode"), ("permissions", "/permissions show")],
    &[("providers", "/provider list")],
];

/// A read-only DoD value from `data.dod.triage` of `/settings show`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum DodValue<T> {
    /// Not delivered (no `data.dod.triage` object yet).
    #[default]
    Loading,
    /// Delivered and unset (the key is absent or `null`).
    Unset,
    /// Delivered and valid.
    Set(T),
    /// Delivered but invalid; the payload is never displayed.
    Invalid,
}

#[derive(Debug, Default)]
pub(crate) struct SettingsView {
    category: usize,
    selected: usize,
    editing: Option<usize>,
    values: [Option<String>; 6],
    /// `[dod.approve] tui_tty` from `data.dod`; `None` until loaded.
    dod_tui_tty: Option<bool>,
    /// `[dod.triage] model` from `data.dod` (a valid `<provider>/<model>`).
    dod_model: DodValue<String>,
    /// `[dod.triage] effort` from `data.dod` (a canonical effort name).
    dod_effort: DodValue<String>,
    /// `[dod.triage] max_tokens` from `data.dod` (`1..=MAX_TRIAGE_TOKENS`).
    dod_max_tokens: DodValue<u64>,
    error: Option<String>,
    mutation_feedback: Option<String>,
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
        4 => vec!["consolidate", "hide"],
        5 => DodWatchMode::ALL
            .into_iter()
            .map(DodWatchMode::as_str)
            .collect(),
        // Read-only rows and anything else have no choices.
        _ => Vec::new(),
    }
}
fn label(key: &str) -> &'static str {
    match key {
        "tui.language" => tr("settings.language", "Language"),
        "tui.theme" => tr("settings.theme", "Theme"),
        "tui.reasoning" => tr("settings.reasoning", "Reasoning"),
        "tui.child_stream" => tr("settings.child_stream", "Child agent stream"),
        "tui.status_expiry" => tr("settings.status_expiry", "Expired status"),
        "dod.watch.mode" => tr("settings.dod_watch_mode", "DoD watch"),
        "dod.approve.tui_tty" => tr(
            "settings.dod_tui_tty",
            "DoD approval in this terminal (weaker binding, Freeze/Thaw only)",
        ),
        "dod.triage.model" => tr("settings.dod_triage_model", "DoD triage model"),
        "dod.triage.effort" => tr("settings.dod_triage_effort", "DoD triage effort"),
        "dod.triage.max_tokens" => tr("settings.dod_triage_max_tokens", "DoD triage max_tokens"),
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
        "off" => tr("settings.value_off", "Off").to_owned(),
        "notify" => tr("settings.value_notify", "Notify only").to_owned(),
        "triage" => tr("settings.value_triage", "Notify and triage").to_owned(),
        _ => value.to_owned(),
    }
}
// Reads `key` of the `data.dod.triage` object: `Loading` without that path,
// `Unset` for an absent or `null` key, `Set` when `parse` accepts the value,
// `Invalid` otherwise (also when `triage` is not an object).
fn triage_value<T>(
    data: &serde_json::Value,
    key: &str,
    parse: impl Fn(&serde_json::Value) -> Option<T>,
) -> DodValue<T> {
    match data.pointer("/dod/triage") {
        None => DodValue::Loading,
        Some(serde_json::Value::Object(triage)) => match triage.get(key) {
            None | Some(serde_json::Value::Null) => DodValue::Unset,
            Some(value) => parse(value).map_or(DodValue::Invalid, DodValue::Set),
        },
        Some(_) => DodValue::Invalid,
    }
}
// Display text of a read-only DoD value: `…` while loading, `unset` when not
// set, a fixed hint (never the payload) when invalid.
fn dod_value_text<T>(value: &DodValue<T>, unset: &str, show: impl Fn(&T) -> String) -> String {
    match value {
        DodValue::Loading => "…".to_owned(),
        DodValue::Unset => unset.to_owned(),
        DodValue::Set(value) => show(value),
        DodValue::Invalid => tr(
            "settings.dod_value_invalid",
            "invalid (see /dod model show)",
        )
        .to_owned(),
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
            KEYS.len().saturating_add(READ_ONLY_ROWS)
        } else {
            self.links().len()
        }
    }
    // Whether `/settings show` delivered any DoD value.
    fn dod_delivered(&self) -> bool {
        self.dod_tui_tty.is_some()
            || !matches!(self.dod_model, DodValue::Loading)
            || !matches!(self.dod_effort, DodValue::Loading)
            || !matches!(self.dod_max_tokens, DodValue::Loading)
    }
    /// The read-only DoD rows (same-TTY approval, triage model, effort and
    /// max_tokens); `…` until `/settings show` delivered them.
    fn read_only_rows(&self) -> [String; READ_ONLY_ROWS] {
        let tui_tty = match self.dod_tui_tty {
            Some(true) => tr("settings.value_on", "On"),
            Some(false) => tr("settings.value_off", "Off"),
            None => "…",
        };
        // Without a model only `triage` falls back to notify; `off` and
        // `notify` are not affected.
        let watch_triage = KEYS
            .iter()
            .zip(self.values.iter())
            .any(|(key, value)| *key == "dod.watch.mode" && value.as_deref() == Some("triage"));
        let unset_model = if watch_triage {
            tr(
                "settings.dod_model_unset",
                "not set (the watch acts as notify)",
            )
        } else {
            tr("settings.dod_model_not_set", "not set")
        };
        let model = dod_value_text(&self.dod_model, unset_model, |model| {
            crate::sanitize::sanitize_inline(model)
        });
        let effort = dod_value_text(
            &self.dod_effort,
            tr("settings.dod_effort_default", "model default"),
            String::clone,
        );
        let max_tokens = dod_value_text(
            &self.dod_max_tokens,
            tr("settings.dod_max_tokens_default", "dispatcher default"),
            u64::to_string,
        );
        let read_only = tr("settings.read_only", "read-only");
        [
            format!("{}: {tui_tty} · {read_only}", label("dod.approve.tui_tty")),
            format!(
                "{}: {model} · {}",
                label("dod.triage.model"),
                tr(
                    "settings.dod_model_hint",
                    "change with /dod model set <provider/model>"
                )
            ),
            format!("{}: {effort} · {read_only}", label("dod.triage.effort")),
            format!(
                "{}: {max_tokens} · {read_only}",
                label("dod.triage.max_tokens")
            ),
        ]
    }
}
impl OverlayView for SettingsView {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        // Keep mutation feedback outside the scrolling rows, including while editing.
        let area = if let Some(feedback) = &self.mutation_feedback {
            let paragraph = Paragraph::new(crate::sanitize::sanitize_inline(feedback))
                .wrap(Wrap { trim: false });
            let height = u16::try_from(paragraph.line_count(area.width))
                .unwrap_or(u16::MAX)
                .min(area.height.saturating_sub(4));
            let feedback_area = Rect::new(
                area.x,
                area.bottom().saturating_sub(height),
                area.width,
                height,
            );
            paragraph.render(feedback_area, buf);
            Rect::new(
                area.x,
                area.y,
                area.width,
                area.height.saturating_sub(height),
            )
        } else {
            area
        };
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
            lines.push(Line::from(
                KEYS.get(self.selected).copied().map(label).unwrap_or(""),
            ));
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
                    .zip(self.values.iter())
                    .map(|(key, value)| {
                        format!(
                            "{}: {}",
                            label(key),
                            value
                                .as_deref()
                                .map(value_label)
                                .unwrap_or_else(|| "…".to_owned())
                        )
                    })
                    .chain(self.read_only_rows())
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
            if values.is_empty() {
                self.editing = None;
                return OverlayOutcome::Stay;
            }
            if is_down(&key) {
                *choice = (*choice + 1) % values.len();
            } else if is_up(&key) {
                *choice = (*choice + values.len() - 1) % values.len();
            } else if key.code == KeyCode::Esc {
                self.editing = None;
            } else if key.code == KeyCode::Enter {
                let command = KEYS
                    .get(self.selected)
                    .zip(values.get(*choice))
                    .map(|(name, value)| format!("/settings set {name} {value}"));
                self.editing = None;
                if let Some(command) = command {
                    return OverlayOutcome::Run(command);
                }
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
                // Read-only rows (past `KEYS`) have no value and never reset.
                KeyCode::Char('x')
                    if self.category == 0
                        && self.values.get(self.selected).is_some_and(Option::is_some) =>
                {
                    if let Some(name) = KEYS.get(self.selected) {
                        return OverlayOutcome::Run(format!("/settings reset {name}"));
                    }
                }
                KeyCode::Enter if self.category == 0 => {
                    if let Some(Some(value)) = self.values.get(self.selected) {
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
        self.dod_tui_tty = data
            .pointer("/dod/approve/tui_tty")
            .and_then(serde_json::Value::as_bool);
        self.dod_model = triage_value(data, "model", |value| {
            value
                .as_str()
                .filter(|model| harw_config::parse_triage_model(model).is_ok())
                .map(str::to_owned)
        });
        self.dod_effort = triage_value(data, "effort", |value| {
            value
                .as_str()
                .filter(|effort| {
                    effort
                        .parse::<harw_types::ReasoningEffort>()
                        .is_ok_and(|level| level.to_string() == **effort)
                })
                .map(str::to_owned)
        });
        self.dod_max_tokens = triage_value(data, "max_tokens", |value| {
            value.as_u64().filter(|tokens| {
                (1..=u64::from(harw_config::dod_toml::MAX_TRIAGE_TOKENS)).contains(tokens)
            })
        });
    }
    fn apply_command_result(
        &mut self,
        language: UiLanguage,
        result: Result<&harw_operations::OpOutput, &str>,
    ) {
        self.mutation_feedback = Some(match result {
            Err(error) => error.to_owned(),
            Ok(output)
                if output
                    .data
                    .as_ref()
                    .is_some_and(|data| data["warning"] == "durability_uncertain") =>
            {
                crate::i18n::tr_for(
                    language,
                    "settings.durability",
                    "Settings were saved and applied, but durability across a crash is uncertain.",
                )
                .to_owned()
            }
            Ok(_) => crate::i18n::tr_for(language, "settings.saved", "Settings saved").to_owned(),
        });
    }
    fn apply_error(&mut self, text: &str) {
        self.error = Some(text.to_owned());
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
        for (key, value) in KEYS.iter().zip(view.values.iter()) {
            if let Some(value) = value {
                lines.push(format!("{}: {}", label(key), value_label(value)));
            }
        }
        if view.dod_delivered() {
            lines.extend(view.read_only_rows());
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

    fn dod_loaded(model: serde_json::Value) -> SettingsView {
        let mut view = SettingsView::default();
        view.apply_data(&serde_json::json!({
            "rows": [
                {"key": "tui.language", "value": "en"},
                {"key": "dod.watch.mode", "value": "triage"}
            ],
            "dod": {
                "triage": {"model": model},
                "approve": {"tui_tty": true}
            }
        }));
        view
    }

    fn rendered(view: &SettingsView, language: UiLanguage) -> String {
        let area = Rect::new(0, 0, 200, 20);
        let mut buffer = Buffer::empty(area);
        crate::i18n::with_language(language, || view.render(area, &mut buffer, Theme::Dark));
        crate::test_support::display_buffer_text(&buffer)
    }

    /// PL-100 WP 1.9: the watch mode is a regular choice; the read-only DoD
    /// rows after it never edit, reset or run anything.
    #[test]
    fn dod_watch_mode_is_a_choice_and_dod_rows_are_read_only() {
        let mut view = dod_loaded(serde_json::json!("local/qwen2.5"));
        assert_eq!(view.row_count(), KEYS.len().saturating_add(READ_ONLY_ROWS));
        for _ in 0..5 {
            view.on_key(key(KeyCode::Down));
        }
        assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        assert_eq!(view.editing, Some(2), "triage is the third choice");
        view.on_key(key(KeyCode::Down));
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::Run("/settings set dod.watch.mode off".to_owned())
        );
        assert_eq!(
            view.on_key(key(KeyCode::Char('x'))),
            OverlayOutcome::Run("/settings reset dod.watch.mode".to_owned())
        );
        for _ in 0..READ_ONLY_ROWS {
            view.on_key(key(KeyCode::Down));
            assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
            assert!(view.editing.is_none());
            assert_eq!(view.on_key(key(KeyCode::Char('x'))), OverlayOutcome::Stay);
        }
        view.on_key(key(KeyCode::Down));
        assert_eq!(view.selected, 0, "selection wraps after the read-only rows");
    }

    #[test]
    fn dod_rows_render_the_weaker_binding_label_and_the_model_hint() {
        let view = dod_loaded(serde_json::json!("local/qwen2.5"));
        let german = rendered(&view, UiLanguage::German);
        assert!(
            german.contains("schwächere Bindung, nur Freeze/Thaw"),
            "{german}"
        );
        assert!(german.contains("local/qwen2.5"), "{german}");
        assert!(
            german.contains("/dod model set <provider/model>"),
            "{german}"
        );
        let english = rendered(&view, UiLanguage::English);
        assert!(
            english.contains("weaker binding, Freeze/Thaw only"),
            "{english}"
        );
        assert!(english.contains("Notify and triage"), "{english}");

        let unset = rendered(&dod_loaded(serde_json::Value::Null), UiLanguage::English);
        assert!(
            unset.contains("not set (the watch acts as notify)"),
            "{unset}"
        );
    }

    /// An unset model says "acts as notify" only while the watch mode is
    /// `triage`; with `off` or `notify` it is plainly "not set".
    #[test]
    fn unset_model_row_names_the_notify_fallback_only_for_triage() {
        for (mode, fallback) in [("triage", true), ("notify", false), ("off", false)] {
            let mut view = SettingsView::default();
            view.apply_data(&serde_json::json!({
                "rows": [{"key": "dod.watch.mode", "value": mode}],
                "dod": {"triage": {"model": null}, "approve": {"tui_tty": true}}
            }));
            for language in [UiLanguage::English, UiLanguage::German] {
                let text = rendered(&view, language);
                let notify = if language == UiLanguage::English {
                    "acts as notify"
                } else {
                    "wirkt als notify"
                };
                assert_eq!(text.contains(notify), fallback, "{mode}: {text}");
                let unset = if language == UiLanguage::English {
                    "not set"
                } else {
                    "nicht gesetzt"
                };
                assert!(text.contains(unset), "{mode}: {text}");
            }
        }
    }

    /// A delivered but invalid model is neither shown nor mistaken for the
    /// loading placeholder: the row says "invalid (see /dod model show)".
    #[test]
    fn dod_model_payload_must_be_a_valid_reference() {
        for model in [
            serde_json::json!("noslash"),
            serde_json::json!("local/qwen\u{202E}evil"),
            serde_json::json!("/dod model clear"),
            serde_json::json!(42),
        ] {
            let view = dod_loaded(model.clone());
            assert_eq!(view.dod_model, DodValue::Invalid, "{model}");
            let text = rendered(&view, UiLanguage::English);
            assert!(!text.contains("noslash"), "{text}");
            assert!(!text.contains("evil"), "{text}");
            assert!(!text.contains("/dod model clear"), "{text}");
            assert!(
                text.contains("DoD triage model: invalid (see /dod model show)"),
                "{text}"
            );
            let german = rendered(&view, UiLanguage::German);
            assert!(
                german.contains("ungültig (siehe /dod model show)"),
                "{german}"
            );
        }
        let mut view = SettingsView::default();
        view.apply_data(&serde_json::json!({"rows": [], "dod": "not an object"}));
        assert_eq!(view.dod_model, DodValue::Loading);
        assert_eq!(view.dod_effort, DodValue::Loading);
        assert_eq!(view.dod_max_tokens, DodValue::Loading);
        assert_eq!(view.dod_tui_tty, None);
        assert!(!view.dod_delivered());
        view.apply_data(&serde_json::json!({"rows": [], "dod": {"triage": "not an object"}}));
        assert_eq!(view.dod_model, DodValue::Invalid);
        assert_eq!(view.dod_effort, DodValue::Invalid);
        assert_eq!(view.dod_max_tokens, DodValue::Invalid);
    }

    /// `effort` is shown only as a canonical effort name, `max_tokens` only
    /// within `1..=MAX_TRIAGE_TOKENS`; unset values name the default, invalid
    /// ones are never echoed.
    #[test]
    fn dod_effort_and_max_tokens_rows_validate_the_payload() {
        let with = |effort: serde_json::Value, max_tokens: serde_json::Value| {
            let mut view = SettingsView::default();
            view.apply_data(&serde_json::json!({
                "rows": [{"key": "dod.watch.mode", "value": "notify"}],
                "dod": {
                    "triage": {"model": "local/qwen2.5", "effort": effort, "max_tokens": max_tokens},
                    "approve": {"tui_tty": true}
                }
            }));
            view
        };
        let valid = with(serde_json::json!("xhigh"), serde_json::json!(4096));
        assert_eq!(valid.dod_effort, DodValue::Set("xhigh".to_owned()));
        assert_eq!(valid.dod_max_tokens, DodValue::Set(4096));
        let text = rendered(&valid, UiLanguage::English);
        assert!(
            text.contains("DoD triage effort: xhigh · read-only"),
            "{text}"
        );
        assert!(
            text.contains("DoD triage max_tokens: 4096 · read-only"),
            "{text}"
        );
        let edge = with(serde_json::json!("minimal"), serde_json::json!(32_768));
        assert_eq!(edge.dod_max_tokens, DodValue::Set(32_768));

        let unset = with(serde_json::Value::Null, serde_json::Value::Null);
        assert_eq!(unset.dod_effort, DodValue::Unset);
        assert_eq!(unset.dod_max_tokens, DodValue::Unset);
        let text = rendered(&unset, UiLanguage::English);
        assert!(text.contains("DoD triage effort: model default"), "{text}");
        assert!(
            text.contains("DoD triage max_tokens: dispatcher default"),
            "{text}"
        );

        for (effort, max_tokens) in [
            (serde_json::json!("HIGH"), serde_json::json!(0)),
            (serde_json::json!("ultra"), serde_json::json!(32_769)),
            (serde_json::json!("high\u{202E}"), serde_json::json!(-5)),
            (serde_json::json!(3), serde_json::json!("4096")),
            (serde_json::json!(["high"]), serde_json::json!(4096.5)),
        ] {
            let view = with(effort.clone(), max_tokens.clone());
            assert_eq!(view.dod_effort, DodValue::Invalid, "{effort}");
            assert_eq!(view.dod_max_tokens, DodValue::Invalid, "{max_tokens}");
            let text = rendered(&view, UiLanguage::English);
            assert!(
                text.contains("DoD triage effort: invalid (see /dod model show)"),
                "{text}"
            );
            assert!(
                text.contains("DoD triage max_tokens: invalid (see /dod model show)"),
                "{text}"
            );
            for echoed in ["HIGH", "ultra", "32769", "-5", "4096"] {
                assert!(!text.contains(echoed), "{echoed}: {text}");
            }
        }
    }

    /// `/settings` is TUI-only and its text is replaced by `present_output`
    /// (`command_data.rs`): the DoD model, effort and max_tokens must be in
    /// it, invalid payloads must not.
    #[test]
    fn present_output_lists_the_read_only_dod_values() {
        let output = harw_operations::OpOutput {
            text: "payload must not be used".to_owned(),
            data: Some(serde_json::json!({
                "rows": [{"key": "dod.watch.mode", "value": "notify"}],
                "dod": {"triage": {"model": null}, "approve": {"tui_tty": false}}
            })),
        };
        let german = present_output(UiLanguage::German, &output);
        assert!(german.contains("Nur Hinweise"), "{german}");
        assert!(german.contains("nicht gesetzt"), "{german}");
        assert!(german.contains("Modellvorgabe"), "{german}");
        assert!(german.contains("Dispatcher-Vorgabe"), "{german}");
        assert!(!german.contains("payload"), "{german}");

        let set = harw_operations::OpOutput {
            text: "payload must not be used".to_owned(),
            data: Some(serde_json::json!({
                "rows": [{"key": "dod.watch.mode", "value": "triage"}],
                "dod": {
                    "triage": {"model": "local/qwen2.5", "effort": "high", "max_tokens": 4096},
                    "approve": {"tui_tty": true}
                }
            })),
        };
        let english = present_output(UiLanguage::English, &set);
        for line in [
            "DoD triage model: local/qwen2.5",
            "DoD triage effort: high · read-only",
            "DoD triage max_tokens: 4096 · read-only",
            "weaker binding, Freeze/Thaw only",
        ] {
            assert!(english.contains(line), "{line}: {english}");
        }

        let invalid = harw_operations::OpOutput {
            text: "payload must not be used".to_owned(),
            data: Some(serde_json::json!({
                "rows": [],
                "dod": {"triage": {"model": "local/qwen2.5", "effort": "turbo", "max_tokens": 99_999}}
            })),
        };
        let english = present_output(UiLanguage::English, &invalid);
        assert!(!english.contains("turbo"), "{english}");
        assert!(!english.contains("99999"), "{english}");
        assert!(
            english.contains("DoD triage effort: invalid (see /dod model show)"),
            "{english}"
        );

        let without_dod = harw_operations::OpOutput {
            text: String::new(),
            data: Some(serde_json::json!({"rows": [{"key": "tui.language", "value": "en"}]})),
        };
        let english = present_output(UiLanguage::English, &without_dod);
        assert!(!english.contains("DoD"), "{english}");
    }
}
