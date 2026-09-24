//! Übersicht „Modelle je Rolle“ (`/models`, F8).
//!
//! Spec-Quelle: `tui_contract.md` (T10, `ModelRolesView::from_config`).
//!
//! # Verantwortung
//! Zeigt in einer Tabelle das live aktive Sitzungsmodell sowie für jede der
//! zwölf [`ModelRole`]s Provider, Modell, Herkunft (Quelle), Reasoning-
//! Effort und ab wann eine Änderung wirkt. Die Ansicht schreibt nichts
//! selbst: `Enter` öffnet über `/models pick <rolle>` den Modell-Picker
//! (bzw. `/model` für die Live-Zeile, bei einer UIA-Wurzel `/uia-model`),
//! `r` setzt über `/models reset <rolle>` zurück. Frische Daten kommen über
//! [`OverlayView::refresh_command`] (`/models show`) und
//! [`OverlayView::apply_data`]; die TUI baut die Ansicht zudem bei jedem
//! Öffnen aus dem Live-Stand der Montage (`ChatApp::model_roles_view`).
//!
//! Runde 5, Teil G: [`ModelRolesView::uia_workers`] zeigt denselben
//! Tabellenrahmen als Bereich „UIA-Worker-Modelle“ — je UIA-Worker-Rolle
//! ihre Wahl („wie UIA“ oder Provider/Modell). `Enter` öffnet über
//! `/models pick <rolle>` die Modellwahl der Rolle, `r` setzt sie auf „wie
//! UIA“, `a` alle Rollen (`/models worker all uia`), `Esc` schließt; Daten
//! kommen über `/models worker`.
//!
//! # Nebenläufigkeit
//! Keine; der Aufrufer hält die Ansicht exklusiv.
//!
//! # Fehlertypen
//! Keine — ein fehlgeschlagenes Nachladen wird als Hinweiszeile angezeigt.

use crossterm::event::{KeyCode, KeyEvent};
use harw_config::{
    ModelRole, ResolvedConfig, UIA_WORKER_ROLES, UiaWorkerModelChoice, resolve_role_models,
    resolve_uia_worker_models,
};
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

/// Standard-Befehl für `Enter` auf der Live-Zeile.
const LIVE_COMMAND: &str = "/model";

/// Wirkungs-Spalte der Rollen-Zeilen.
const ROLE_EFFECT: &str = "ab nächster Sitzung";

/// Wirkungs-Spalte der Kind-Rollen (Live-Modellwechsel: neu gestartete
/// Agenten nehmen die Wahl sofort).
const CHILD_ROLE_EFFECT: &str = "neue Agenten sofort";

/// Fußzeile.
const FOOTER: &str = "j/k wählen · Enter Modell wählen · r zurücksetzen · Esc schließen";

/// Spaltenbreiten (Zeichen): Rolle, Provider, Modell, Quelle, Effort.
const WIDTHS: [usize; 5] = [24, 14, 30, 22, 8];

/// Runde 5, Teil G: Befehl, der die Daten des Worker-Bereichs liefert.
pub(crate) const WORKERS_REFRESH_COMMAND: &str = "/models worker";

/// Runde 5, Teil G: Fußzeile des Worker-Bereichs.
const WORKERS_FOOTER: &str =
    "j/k wählen · Enter Modell wählen · r wie UIA · a alle wie UIA · Esc schließen";

/// Runde 5, Teil G: Wirkungs-Spalte des Worker-Bereichs.
const WORKER_EFFECT: &str = "neue Worker";

/// Welche Tabelle die Ansicht zeigt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    /// Modelle je Rolle (`/models`, F8).
    Roles,
    /// Runde 5, Teil G: Bereich „UIA-Worker-Modelle“ — eine Zeile je
    /// UIA-Worker-Rolle mit ihrer Wahl („wie UIA“ oder Provider/Modell).
    UiaWorkers,
}

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
    fn effect(&self, mode: ViewMode) -> &'static str {
        match (mode, self.key.is_some()) {
            (ViewMode::UiaWorkers, _) => WORKER_EFFECT,
            (ViewMode::Roles, true) => {
                let live = self
                    .key
                    .as_deref()
                    .and_then(harw_config::ModelRole::parse)
                    .is_some_and(harw_config::ModelRole::applies_to_new_agents_live);
                if live { CHILD_ROLE_EFFECT } else { ROLE_EFFECT }
            }
            (ViewMode::Roles, false) => LIVE_EFFECT,
        }
    }

    /// Runde 5, Teil G: Zeile einer UIA-Worker-Rolle.
    fn worker(resolved: &harw_config::ResolvedUiaWorkerModel) -> Self {
        let (provider, model) = match &resolved.choice {
            UiaWorkerModelChoice::Fixed { provider, model } => {
                (Some(provider.clone()), Some(model.clone()))
            }
            UiaWorkerModelChoice::FollowUia => (None, Some(resolved.choice.label())),
        };
        Self {
            key: Some(resolved.role.clone()),
            label: resolved.role.clone(),
            provider,
            model,
            source: resolved.source.label().to_owned(),
            effort: None,
        }
    }
}

/// Ansicht „Modelle je Rolle“ (implementiert [`OverlayView`]).
#[derive(Debug, Clone)]
pub(crate) struct ModelRolesView {
    /// Zeile 0 ist die Live-Sitzung, danach die Rollen (im Worker-Bereich:
    /// nur die UIA-Worker-Rollen, keine Live-Zeile).
    rows: Vec<RoleRow>,
    /// Ausgewählte Zeile.
    selected: usize,
    /// Letzter Fehler beim Nachladen.
    error: Option<String>,
    /// Runde 5, Teil G: gezeigte Tabelle.
    mode: ViewMode,
    /// Runde 5, Teil G: Hinweise (Rückfall auf „wie UIA“), unter der Tabelle.
    notices: Vec<String>,
    /// Die Live-Zeile stammt aus der aufgelösten Wurzel-Route der TUI
    /// (`<provider>/<modell>`, wie die Statuszeile) und wird von
    /// [`OverlayView::apply_data`] nicht durch die rohe Controller-Auswahl
    /// aus `/models show` ersetzt (siehe [`Self::with_resolved_live`]).
    live_resolved: bool,
    /// Befehl, den `Enter` auf der Live-Zeile öffnet (`/model`, bei einer
    /// UIA-Wurzel `/uia-model`).
    live_command: String,
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
            mode: ViewMode::Roles,
            notices: Vec::new(),
            live_resolved: false,
            live_command: LIVE_COMMAND.to_owned(),
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
            mode: ViewMode::Roles,
            notices: Vec::new(),
            live_resolved: false,
            live_command: LIVE_COMMAND.to_owned(),
        }
    }

    /// Runde 5, Teil G: Bereich „UIA-Worker-Modelle“ — je UIA-Worker-Rolle
    /// ihre aktuelle Wahl. Ohne Konfiguration folgen alle Rollen der UIA
    /// (`apply_data` füllt nach).
    #[must_use]
    pub(crate) fn uia_workers(config: Option<&ResolvedConfig>) -> Self {
        let resolved: Vec<harw_config::ResolvedUiaWorkerModel> = match config {
            Some(config) => resolve_uia_worker_models(config),
            None => UIA_WORKER_ROLES
                .iter()
                .map(|role| harw_config::ResolvedUiaWorkerModel {
                    role: (*role).to_owned(),
                    choice: UiaWorkerModelChoice::FollowUia,
                    source: harw_config::UiaWorkerModelSource::Default,
                    notice: None,
                })
                .collect(),
        };
        Self {
            rows: resolved.iter().map(RoleRow::worker).collect(),
            selected: 0,
            error: None,
            mode: ViewMode::UiaWorkers,
            notices: resolved
                .iter()
                .filter_map(|resolved| resolved.notice.clone())
                .collect(),
            live_resolved: false,
            live_command: LIVE_COMMAND.to_owned(),
        }
    }

    /// Markiert die Live-Zeile als von der TUI aufgelöst und legt fest,
    /// welcher Picker sie ändert.
    ///
    /// # Beschreibung
    /// Die TUI baut die Ansicht bei jedem Öffnen aus dem Live-Stand (Wurzel-
    /// Route, Live-Konfiguration). Die Live-Zeile zeigt dann dasselbe wie
    /// die Statuszeile; ein späteres `/models show` meldet nur die rohe
    /// generische Controller-Auswahl und darf sie nicht überschreiben (bei
    /// einer UIA-Wurzel wäre das das falsche Modell).
    ///
    /// # Argumente
    /// - `command`: Befehl für `Enter` auf der Live-Zeile — `/model`, bei
    ///   einer UIA-Wurzel `/uia-model` (nur er ändert, was die Zeile zeigt).
    #[must_use]
    pub(crate) fn with_resolved_live(mut self, command: &str) -> Self {
        self.live_resolved = true;
        command.clone_into(&mut self.live_command);
        self
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
                // Die Live-Zeile nennt den Befehl, der sie tatsächlich ändert.
                let effect = if row.key.is_none() && self.mode == ViewMode::Roles {
                    format!("sofort ({})", self.live_command)
                } else {
                    row.effect(self.mode).to_owned()
                };
                Line::from(vec![
                    Span::styled(text, row_style),
                    Span::styled(effect, style::dim_style(theme)),
                ])
            })
            .collect();
        if !self.notices.is_empty() {
            lines.push(Line::from(""));
            for notice in &self.notices {
                lines.push(Line::styled(
                    sanitize_inline(notice),
                    style::dim_style(theme),
                ));
            }
        }
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

impl ModelRolesView {
    /// Runde 5, Teil G: Tasten des Worker-Bereichs (Navigation erledigt
    /// bereits der Aufrufer).
    fn on_worker_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        match key.code {
            KeyCode::Esc => OverlayOutcome::Close,
            KeyCode::Char('q') if plain(&key) => OverlayOutcome::Close,
            KeyCode::Enter => match self.selected_key() {
                Some(role) => OverlayOutcome::RunAndClose(format!("/models pick {role}")),
                None => OverlayOutcome::Stay,
            },
            KeyCode::Char('r') if plain(&key) => match self.selected_key() {
                Some(role) => OverlayOutcome::Run(format!("/models worker {role} uia")),
                None => OverlayOutcome::Stay,
            },
            KeyCode::Char('a') if plain(&key) => {
                OverlayOutcome::Run("/models worker all uia".to_owned())
            }
            _ => OverlayOutcome::Stay,
        }
    }

    /// Runde 5, Teil G: übernimmt `/models worker`-Daten
    /// (`{"workers":[{"role","choice","source","notice"}]}`); eine
    /// unbrauchbare Antwort lässt die Tabelle stehen.
    fn apply_worker_data(&mut self, data: &serde_json::Value) {
        let Some(items) = data.get("workers").and_then(serde_json::Value::as_array) else {
            return;
        };
        let mut rows = Vec::new();
        let mut notices = Vec::new();
        for item in items {
            let Some(role) = json_str(item, "role") else {
                continue;
            };
            let choice = json_str(item, "choice")
                .and_then(|value| UiaWorkerModelChoice::parse(&value))
                .unwrap_or(UiaWorkerModelChoice::FollowUia);
            let (provider, model) = match &choice {
                UiaWorkerModelChoice::Fixed { provider, model } => {
                    (Some(provider.clone()), Some(model.clone()))
                }
                UiaWorkerModelChoice::FollowUia => (None, Some(choice.label())),
            };
            if let Some(notice) = json_str(item, "notice") {
                notices.push(notice);
            }
            rows.push(RoleRow {
                key: Some(role.clone()),
                label: role,
                provider,
                model,
                source: json_str(item, "source").unwrap_or_else(|| "—".to_owned()),
                effort: None,
            });
        }
        if rows.is_empty() {
            return;
        }
        self.rows = rows;
        self.notices = notices;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.error = None;
    }
}

impl OverlayView for ModelRolesView {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let lines = self.body_lines(theme);
        let scroll = scroll_offset_for(self.selected, content_height(area, true));
        let (title, footer) = match self.mode {
            ViewMode::Roles => ("Modelle je Rolle", FOOTER),
            ViewMode::UiaWorkers => ("UIA-Worker-Modelle", WORKERS_FOOTER),
        };
        render_panel(
            area,
            buf,
            theme,
            title,
            Some(Self::header_line(theme)),
            &lines,
            scroll,
            footer,
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
        if self.mode == ViewMode::UiaWorkers {
            return self.on_worker_key(key);
        }
        match key.code {
            KeyCode::Esc => OverlayOutcome::Close,
            KeyCode::Char('q') if plain(&key) => OverlayOutcome::Close,
            KeyCode::Enter => match self.selected_key() {
                Some(role) => OverlayOutcome::RunAndClose(format!("/models pick {role}")),
                None => OverlayOutcome::RunAndClose(self.live_command.clone()),
            },
            KeyCode::Char('r') if plain(&key) => match self.selected_key() {
                Some(role) => OverlayOutcome::Run(format!("/models reset {role}")),
                None => OverlayOutcome::Stay,
            },
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        Some(
            match self.mode {
                ViewMode::Roles => REFRESH_COMMAND,
                ViewMode::UiaWorkers => WORKERS_REFRESH_COMMAND,
            }
            .to_owned(),
        )
    }

    fn apply_data(&mut self, data: &serde_json::Value) {
        if self.mode == ViewMode::UiaWorkers {
            self.apply_worker_data(data);
            return;
        }
        let live = data.get("live");
        let resolved_live = self
            .rows
            .first()
            .filter(|row| self.live_resolved && row.key.is_none())
            .cloned();
        let live_row = match (resolved_live, live) {
            (Some(row), _) => row,
            (None, Some(live)) => RoleRow::live(
                json_str(live, "provider").as_deref(),
                json_str(live, "model").as_deref(),
            ),
            (None, None) => self
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
    fn test_has_live_row_plus_thirteen_roles() {
        // Runde 5, Teil E: `auto-classifier` ist die dreizehnte Rolle.
        let view = view();
        assert_eq!(view.rows.len(), 14);
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
        assert_eq!(view.selected, 13);
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

    /// Eine von der TUI aufgelöste Live-Zeile bleibt stehen; die Rollen
    /// kommen trotzdem frisch aus `/models show`.
    #[test]
    fn test_apply_data_keeps_a_resolved_live_row() {
        let mut view = view().with_resolved_live("/uia-model");
        view.apply_data(&serde_json::json!({
            "roles": [{"role": "explorer", "provider": "openrouter", "model": "neu"}],
            "live": {"provider": null, "model": "roh"}
        }));
        assert_eq!(view.rows[0].provider.as_deref(), Some("anthropic"));
        assert_eq!(view.rows[0].model.as_deref(), Some("claude-sonnet"));
        assert_eq!(view.rows[1].model.as_deref(), Some("neu"));
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose("/uia-model".to_owned())
        );
    }

    #[test]
    fn test_apply_data_ignores_garbage() {
        let mut view = view();
        view.apply_data(&serde_json::json!({"foo": 1}));
        assert_eq!(view.rows.len(), 14);
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
        assert!(text.contains(CHILD_ROLE_EFFECT));
        assert!(text.contains("kein Zugriff"));
    }

    #[test]
    fn test_cell_truncates_and_pads() {
        assert_eq!(cell("abc", 5), "abc   ");
        assert_eq!(cell("abcdefgh", 5), "abcd… ");
    }

    // ── Runde 5, Teil G: Bereich „UIA-Worker-Modelle“ ────────────────────

    #[test]
    fn test_uia_workers_lists_every_worker_role_following_the_uia() {
        let view = ModelRolesView::uia_workers(None);
        assert_eq!(view.rows.len(), UIA_WORKER_ROLES.len());
        for (row, role) in view.rows.iter().zip(UIA_WORKER_ROLES) {
            assert_eq!(row.key.as_deref(), Some(role));
            assert_eq!(row.model.as_deref(), Some("wie UIA"));
        }
        assert_eq!(
            view.refresh_command().as_deref(),
            Some(WORKERS_REFRESH_COMMAND)
        );
    }

    #[test]
    fn test_uia_workers_keys_pick_reset_all_and_close() {
        let mut view = ModelRolesView::uia_workers(None);
        view.on_key(key(KeyCode::Down));
        let second = UIA_WORKER_ROLES[1];
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::RunAndClose(format!("/models pick {second}"))
        );
        assert_eq!(
            view.on_key(key(KeyCode::Char('r'))),
            OverlayOutcome::Run(format!("/models worker {second} uia"))
        );
        assert_eq!(
            view.on_key(key(KeyCode::Char('a'))),
            OverlayOutcome::Run("/models worker all uia".to_owned())
        );
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
    }

    #[test]
    fn test_uia_workers_apply_data_shows_fixed_choice_and_notice() {
        let mut view = ModelRolesView::uia_workers(None);
        view.apply_data(&serde_json::json!({
            "workers": [
                {"role": "uia-worker", "choice": "uia", "source": "Vorgabe"},
                {"role": "uia-writer", "choice": "openai/gpt-5", "source": "eigene Wahl"},
                {"role": "uia-explorer", "choice": "uia", "source": "Rückfall",
                 "notice": "uia-explorer: Provider „x“ ist nicht angemeldet"}
            ]
        }));
        assert_eq!(view.rows.len(), 3);
        assert_eq!(view.rows[1].provider.as_deref(), Some("openai"));
        assert_eq!(view.rows[1].model.as_deref(), Some("gpt-5"));
        assert_eq!(view.notices.len(), 1);

        let area = Rect::new(0, 0, 140, 16);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, Theme::Dark);
        let text = buffer_text(&buf);
        assert!(text.contains("UIA-Worker-Modelle"));
        assert!(text.contains("wie UIA"));
        assert!(text.contains("nicht angemeldet"));
        assert!(text.contains(WORKER_EFFECT));
    }
}
