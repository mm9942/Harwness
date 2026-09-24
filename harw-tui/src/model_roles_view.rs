//! Übersicht „Modelle je Rolle“ (`/models`, F8).
//!
//! Spec-Quelle: `tui_contract.md` (T10, `ModelRolesView::from_config`).
//!
//! # Verantwortung
//! Zeigt in einer Tabelle das live aktive Sitzungsmodell sowie für jede der
//! zwölf [`ModelRole`]s Provider, Modell, Herkunft (Quelle), Reasoning-
//! Effort und ab wann eine Änderung wirkt. Die Ansicht schreibt nichts
//! selbst: `Enter` öffnet über `/models pick <rolle>` den Modell-Picker
//! (bzw. `/model` für die Live-Zeile), `r` setzt über `/models reset <rolle>`
//! zurück. Frische Daten kommen über [`OverlayView::refresh_command`]
//! (`/models show`) und [`OverlayView::apply_data`].
//!
//! # Nebenläufigkeit
//! Keine; der Aufrufer hält die Ansicht exklusiv.
//!
//! # Fehlertypen
//! Keine — ein fehlgeschlagenes Nachladen wird als Hinweiszeile angezeigt.

use crossterm::event::{KeyCode, KeyEvent};
use harw_config::{ModelRole, ResolvedConfig, resolve_role_models};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::overlay_view::{
    OverlayOutcome, OverlayView, content_height, is_down, is_up, json_str, plain, render_panel,
    scroll_offset_for,
};
use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Befehl, der die Tabellendaten liefert.
pub(crate) const REFRESH_COMMAND: &str = "/models show";

/// Beschriftung der Live-Zeile.
const LIVE_LABEL: &str = "Aktive Sitzung (live)";

/// Wirkungs-Spalte der Live-Zeile.
const LIVE_EFFECT: &str = "sofort (/model)";

/// Wirkungs-Spalte der Rollen-Zeilen.
const ROLE_EFFECT: &str = "ab nächster Sitzung";

/// Fußzeile.
const FOOTER: &str = "j/k wählen · Enter Modell wählen · r zurücksetzen · Esc schließen";

/// Spaltenbreiten (Zeichen): Rolle, Provider, Modell, Quelle, Effort.
const WIDTHS: [usize; 5] = [24, 14, 30, 22, 8];

/// Eine Tabellenzeile (Rolle oder Live-Sitzung).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RoleRow {
    /// Rollen-Schlüssel für `/models …` (`None` für die Live-Zeile).
    key: Option<String>,
    /// Anzeigename.
    label: String,
    /// Provider, falls aufgelöst.
    provider: Option<String>,
    /// Modell, falls aufgelöst.
    model: Option<String>,
    /// Herkunft (bereits als Anzeigetext).
    source: String,
    /// Reasoning-Effort, falls gesetzt.
    effort: Option<String>,
}

impl RoleRow {
    /// Live-Zeile aus aktivem Provider/Modell.
    fn live(provider: Option<&str>, model: Option<&str>) -> Self {
        Self {
            key: None,
            label: LIVE_LABEL.to_owned(),
            provider: provider.map(str::to_owned),
            model: model.map(str::to_owned),
            source: "live".to_owned(),
            effort: None,
        }
    }

    /// Wirkungs-Spalte.
    fn effect(&self) -> &'static str {
        if self.key.is_some() {
            ROLE_EFFECT
        } else {
            LIVE_EFFECT
        }
    }
}

/// Ansicht „Modelle je Rolle“ (implementiert [`OverlayView`]).
#[derive(Debug, Clone)]
pub(crate) struct ModelRolesView {
    /// Zeile 0 ist die Live-Sitzung, danach die Rollen.
    rows: Vec<RoleRow>,
    /// Ausgewählte Zeile.
    selected: usize,
    /// Letzter Fehler beim Nachladen.
    error: Option<String>,
}

impl ModelRolesView {
    /// Baut die Ansicht aus der aufgelösten Konfiguration.
    ///
    /// # Argumente
    /// - `config`: aufgelöste Konfiguration (für [`resolve_role_models`]).
    /// - `live_provider`/`live_model`: aktives Sitzungsmodell, falls bekannt.
    #[must_use]
    pub(crate) fn from_config(
        config: &ResolvedConfig,
        live_provider: Option<&str>,
        live_model: Option<&str>,
    ) -> Self {
        let mut rows = vec![RoleRow::live(live_provider, live_model)];
        rows.extend(resolve_role_models(config).into_iter().map(|row| RoleRow {
            key: Some(row.role.key().to_owned()),
            label: row.role.label().to_owned(),
            provider: row.provider,
            model: row.model,
            source: row.source.label().to_owned(),
            effort: row.reasoning_effort,
        }));
        Self {
            rows,
            selected: 0,
            error: None,
        }
    }

    /// Leere Ansicht nur mit Live-Zeile und allen Rollen ohne Werte (etwa
    /// wenn keine Konfiguration geladen ist; `apply_data` füllt nach).
    #[must_use]
    pub(crate) fn empty(live_provider: Option<&str>, live_model: Option<&str>) -> Self {
        let mut rows = vec![RoleRow::live(live_provider, live_model)];
        rows.extend(ModelRole::ALL.into_iter().map(|role| RoleRow {
            key: Some(role.key().to_owned()),
            label: role.label().to_owned(),
            provider: None,
            model: None,
            source: "—".to_owned(),
            effort: None,
        }));
        Self {
            rows,
            selected: 0,
            error: None,
        }
    }

    /// Schlüssel der ausgewählten Rolle (`None` auf der Live-Zeile).
    fn selected_key(&self) -> Option<&str> {
        self.rows
            .get(self.selected)
            .and_then(|row| row.key.as_deref())
    }

    /// Kopfzeile der Tabelle.
    fn header_line(theme: Theme) -> Line<'static> {
        let text = format!(
            "  {}{}{}{}{}{}",
            cell("Rolle", WIDTHS[0]),
            cell("Provider", WIDTHS[1]),
            cell("Modell", WIDTHS[2]),
            cell("Quelle", WIDTHS[3]),
            cell("Effort", WIDTHS[4]),
            "Wirkung"
        );
        Line::styled(
            text,
            Style::default()
                .fg(style::accent_color(theme))
                .add_modifier(Modifier::BOLD),
        )
    }

    /// Tabellenzeilen (inkl. evtl. Fehlerzeile am Ende).
    fn body_lines(&self, theme: Theme) -> Vec<Line<'static>> {
        let dash = "—";
        let mut lines: Vec<Line<'static>> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let is_selected = index == self.selected;
                let marker = if is_selected { "❯ " } else { "  " };
                let row_style = if is_selected {
                    style::selected_style(theme)
                } else {
                    Style::default()
                };
                let text = format!(
                    "{marker}{}{}{}{}{}",
                    cell(&row.label, WIDTHS[0]),
                    cell(row.provider.as_deref().unwrap_or(dash), WIDTHS[1]),
                    cell(row.model.as_deref().unwrap_or(dash), WIDTHS[2]),
                    cell(&row.source, WIDTHS[3]),
                    cell(row.effort.as_deref().unwrap_or(dash), WIDTHS[4]),
                );
                Line::from(vec![
                    Span::styled(text, row_style),
                    Span::styled(row.effect().to_owned(), style::dim_style(theme)),
                ])
            })
            .collect();
        if let Some(error) = &self.error {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                format!("Aktualisieren fehlgeschlagen: {}", sanitize_inline(error)),
                style::error_style(theme),
            ));
        }
        lines
    }
}

/// Bereinigt, kürzt (mit `…`) und füllt `text` auf `width` Zeichen plus ein
/// Leerzeichen Abstand auf.
fn cell(text: &str, width: usize) -> String {
    let clean = sanitize_inline(text);
    let count = clean.chars().count();
    let body = if count > width {
        let mut short: String = clean.chars().take(width.saturating_sub(1)).collect();
        short.push('…');
        short
    } else {
        format!("{clean}{}", " ".repeat(width - count))
    };
    format!("{body} ")
}

/// Liest eine Tabellenzeile aus dem `/models show`-JSON (tolerant).
fn row_from_json(value: &serde_json::Value) -> Option<RoleRow> {
    let key = json_str(value, "role")?;
    let label = json_str(value, "label")
        .or_else(|| ModelRole::parse(&key).map(|role| role.label().to_owned()))
        .unwrap_or_else(|| key.clone());
    Some(RoleRow {
        key: Some(key),
        label,
        provider: json_str(value, "provider"),
        model: json_str(value, "model"),
        source: json_str(value, "source").unwrap_or_else(|| "—".to_owned()),
        effort: json_str(value, "effort"),
    })
}

impl OverlayView for ModelRolesView {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let lines = self.body_lines(theme);
        let scroll = scroll_offset_for(self.selected, content_height(area, true));
        render_panel(
            area,
            buf,
            theme,
            "Modelle je Rolle",
            Some(Self::header_line(theme)),
            &lines,
            scroll,
            FOOTER,
        );
    }

    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if is_down(&key) {
            self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
            return OverlayOutcome::Stay;
        }
        if is_up(&key) {
            self.selected = self.selected.saturating_sub(1);
            return OverlayOutcome::Stay;
        }
        match key.code {
            KeyCode::Esc => OverlayOutcome::Close,
            KeyCode::Char('q') if plain(&key) => OverlayOutcome::Close,
            KeyCode::Enter => match self.selected_key() {
                Some(role) => OverlayOutcome::RunAndClose(format!("/models pick {role}")),
                None => OverlayOutcome::RunAndClose("/model".to_owned()),
            },
            KeyCode::Char('r') if plain(&key) => match self.selected_key() {
                Some(role) => OverlayOutcome::Run(format!("/models reset {role}")),
                None => OverlayOutcome::Stay,
            },
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        Some(REFRESH_COMMAND.to_owned())
    }

    fn apply_data(&mut self, data: &serde_json::Value) {
        let live = data.get("live");
        let live_row = match live {
            Some(live) => RoleRow::live(
                json_str(live, "provider").as_deref(),
                json_str(live, "model").as_deref(),
            ),
            None => self
                .rows
                .first()
                .cloned()
                .unwrap_or_else(|| RoleRow::live(None, None)),
        };
        let roles: Vec<RoleRow> = data
            .get("roles")
            .and_then(serde_json::Value::as_array)
            .map(|items| items.iter().filter_map(row_from_json).collect())
            .unwrap_or_default();
        if roles.is_empty() && live.is_none() {
            // Unbrauchbare Antwort: bestehende Tabelle behalten.
            return;
        }
        let mut rows = vec![live_row];
        if roles.is_empty() {
            rows.extend(self.rows.iter().skip(1).cloned());
        } else {
            rows.extend(roles);
        }
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.error = None;
    }

    fn apply_error(&mut self, text: &str) {
        self.error = Some(text.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::overlay_view::buffer_text;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn view() -> ModelRolesView {
        ModelRolesView::empty(Some("anthropic"), Some("claude-sonnet"))
    }

    #[test]
    fn test_has_live_row_plus_twelve_roles() {
        let view = view();
        assert_eq!(view.rows.len(), 13);
        assert_eq!(view.rows[0].key, None);
        assert_eq!(view.rows[0].label, LIVE_LABEL);
    }

    #[test]
    fn test_enter_on_live_row_opens_model_picker() {
        let mut view = view();
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose("/model".to_owned())
        );
    }

    #[test]
    fn test_enter_on_role_row_runs_models_pick() {
        let mut view = view();
        view.on_key(key(KeyCode::Char('j')));
        let first = ModelRole::ALL[0].key();
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose(format!("/models pick {first}"))
        );
    }

    #[test]
    fn test_r_resets_role_but_not_live_row() {
        let mut view = view();
        assert_eq!(view.on_key(key(KeyCode::Char('r'))), OverlayOutcome::Stay);
        view.on_key(key(KeyCode::Down));
        view.on_key(key(KeyCode::Down));
        let second = ModelRole::ALL[1].key();
        assert_eq!(
            view.on_key(key(KeyCode::Char('r'))),
            OverlayOutcome::Run(format!("/models reset {second}"))
        );
    }

    #[test]
    fn test_navigation_stays_in_bounds_and_esc_closes() {
        let mut view = view();
        for _ in 0..50 {
            view.on_key(key(KeyCode::Down));
        }
        assert_eq!(view.selected, 12);
        for _ in 0..50 {
            view.on_key(key(KeyCode::Char('k')));
        }
        assert_eq!(view.selected, 0);
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
    }

    #[test]
    fn test_refresh_command_is_models_show() {
        assert_eq!(view().refresh_command().as_deref(), Some("/models show"));
    }

    #[test]
    fn test_apply_data_replaces_rows_and_live() {
        let mut view = view();
        view.apply_error("kaputt");
        view.apply_data(&serde_json::json!({
            "roles": [
                {"role": "uia", "label": "UIA", "provider": "openai", "model": "gpt-5",
                 "source": "explizit", "effort": "high"},
                {"role": "explorer", "provider": null, "model": null, "source": "nicht gesetzt"},
                {"label": "ohne Schlüssel"}
            ],
            "live": {"provider": "openrouter", "model": "x/y"}
        }));
        assert_eq!(view.rows.len(), 3);
        assert_eq!(view.rows[0].provider.as_deref(), Some("openrouter"));
        assert_eq!(view.rows[0].model.as_deref(), Some("x/y"));
        assert_eq!(view.rows[1].key.as_deref(), Some("uia"));
        assert_eq!(view.rows[1].effort.as_deref(), Some("high"));
        assert_eq!(view.rows[2].model, None);
        assert_eq!(view.error, None);
    }

    #[test]
    fn test_apply_data_ignores_garbage() {
        let mut view = view();
        view.apply_data(&serde_json::json!({"foo": 1}));
        assert_eq!(view.rows.len(), 13);
    }

    #[test]
    fn test_render_shows_columns_and_effect() {
        let mut view = view();
        view.apply_error("kein Zugriff");
        let area = Rect::new(0, 0, 140, 22);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, Theme::Dark);
        let text = buffer_text(&buf);
        assert!(text.contains("Modelle je Rolle"));
        assert!(text.contains(LIVE_LABEL));
        assert!(text.contains("claude-sonnet"));
        assert!(text.contains(ROLE_EFFECT));
        assert!(text.contains("kein Zugriff"));
    }

    #[test]
    fn test_cell_truncates_and_pads() {
        assert_eq!(cell("abc", 5), "abc   ");
        assert_eq!(cell("abcdefgh", 5), "abcd… ");
    }
}
