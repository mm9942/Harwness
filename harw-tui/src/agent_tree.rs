//! Interactive projection of the controller's agent tree. No execution authority lives here.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::{
    sanitize::{sanitize_display, sanitize_inline},
    style::{self, Theme},
};

/// A presentation snapshot; missing telemetry remains unknown rather than zero.
#[derive(Debug, Clone)]
pub(crate) struct AgentRow {
    pub id: String,
    pub parent: Option<String>,
    pub role: String,
    pub depth: usize,
    pub status: String,
    pub task: Option<String>,
    pub tokens: Option<u64>,
    pub tool_calls: Option<u32>,
    pub duration_ms: Option<u64>,
    pub budget: String,
    pub result: Option<String>,
    pub can_stop: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AgentTreeAction {
    Stay,
    Close,
    Stop(String),
}

#[derive(Debug, Default)]
pub(crate) struct AgentTree {
    selected: Option<String>,
    collapsed: HashSet<String>,
    details: bool,
    detail_scroll: u16,
    requested: HashSet<String>,
}

impl AgentTree {
    fn visible<'a>(&self, rows: &'a [AgentRow]) -> Vec<&'a AgentRow> {
        let mut hidden_depth = None;
        rows.iter()
            .filter(|row| {
                if hidden_depth.is_some_and(|depth| row.depth > depth) {
                    return false;
                }
                hidden_depth = self.collapsed.contains(&row.id).then_some(row.depth);
                true
            })
            .collect()
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent, rows: &[AgentRow]) -> AgentTreeAction {
        let visible = self.visible(rows);
        let index = visible
            .iter()
            .position(|row| Some(&row.id) == self.selected.as_ref())
            .unwrap_or(0);
        let Some(row) = visible.get(index).copied() else {
            return if key.code == KeyCode::Esc {
                AgentTreeAction::Close
            } else {
                AgentTreeAction::Stay
            };
        };
        self.selected = Some(row.id.clone());
        match key.code {
            KeyCode::Esc if self.details => {
                self.details = false;
                self.detail_scroll = 0;
            }
            KeyCode::Esc => return AgentTreeAction::Close,
            KeyCode::Down if self.details => {
                self.detail_scroll = self.detail_scroll.saturating_add(1)
            }
            KeyCode::Up if self.details => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Down => {
                self.selected = Some(visible[(index + 1).min(visible.len() - 1)].id.clone())
            }
            KeyCode::Up => self.selected = Some(visible[index.saturating_sub(1)].id.clone()),
            KeyCode::Left => {
                if !rows
                    .iter()
                    .any(|child| child.parent.as_ref() == Some(&row.id))
                    || !self.collapsed.insert(row.id.clone())
                {
                    if let Some(parent) = &row.parent {
                        self.selected = Some(parent.clone());
                    }
                }
            }
            KeyCode::Right => {
                if !self.collapsed.remove(&row.id) {
                    if let Some(child) = rows
                        .iter()
                        .find(|child| child.parent.as_ref() == Some(&row.id))
                    {
                        self.selected = Some(child.id.clone());
                    }
                }
            }
            KeyCode::Enter => {
                self.details = true;
                self.detail_scroll = 0;
            }
            KeyCode::Char('s') if row.can_stop && !self.requested.contains(&row.id) => {
                self.requested.insert(row.id.clone());
                return AgentTreeAction::Stop(row.id.clone());
            }
            _ => {}
        }
        AgentTreeAction::Stay
    }

    pub(crate) fn render(&self, area: Rect, buffer: &mut Buffer, theme: Theme, rows: &[AgentRow]) {
        let visible = self.visible(rows);
        let selected = visible
            .iter()
            .position(|row| Some(&row.id) == self.selected.as_ref())
            .unwrap_or(0);
        let row = visible.get(selected).copied();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Agenten · ↑↓ wählen · ←→ Baum · Enter Details · s stoppen · Esc zurück ");
        let inner = block.inner(area);
        block.render(area, buffer);
        let lines: Vec<Line<'static>> = if self.details {
            row.map(|row| {
                vec![
                    format!("{} · {}", row.role, row.status),
                    format!("ID: {}", row.id),
                    format!("Eltern: {}", row.parent.as_deref().unwrap_or("—")),
                    format!("Auftrag: {}", row.task.as_deref().unwrap_or("—")),
                    format!(
                        "Tokens: {} · Tools: {} · Dauer: {} ms",
                        known(row.tokens),
                        known(row.tool_calls),
                        known(row.duration_ms)
                    ),
                    format!("Budget: {}", row.budget),
                    String::new(),
                    row.result
                        .clone()
                        .unwrap_or_else(|| "Noch kein Ergebnis.".to_owned()),
                ]
            })
            .unwrap_or_default()
            .into_iter()
            .flat_map(|text| {
                sanitize_display(&text)
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .map(Line::from)
            .collect()
        } else {
            visible
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let status = if self.requested.contains(&row.id) && row.can_stop {
                        "Abbruch angefordert"
                    } else {
                        &row.status
                    };
                    let marker = if index == selected { "›" } else { " " };
                    let text = format!(
                        "{marker} {}{} {} · {} · Tokens {} · Tools {}",
                        "  ".repeat(row.depth.min(24)),
                        if self.collapsed.contains(&row.id) {
                            "▸"
                        } else {
                            "▾"
                        },
                        row.role,
                        status,
                        known(row.tokens),
                        known(row.tool_calls)
                    );
                    let line = Line::from(sanitize_inline(&text));
                    if index == selected {
                        line.style(style::tool_style(theme))
                    } else {
                        line
                    }
                })
                .collect()
        };
        let scroll = if self.details {
            self.detail_scroll
        } else {
            selected
                .saturating_sub(inner.height.saturating_sub(1) as usize)
                .min(u16::MAX as usize) as u16
        };
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .render(inner, buffer);
    }
}

fn known<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn row(id: &str, parent: Option<&str>, depth: usize) -> AgentRow {
        AgentRow {
            id: id.to_owned(),
            parent: parent.map(str::to_owned),
            role: id.to_owned(),
            depth,
            status: "running".to_owned(),
            task: None,
            tokens: None,
            tool_calls: None,
            duration_ms: None,
            budget: "—".to_owned(),
            result: None,
            can_stop: parent.is_some(),
        }
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn navigation_collapse_details_and_scoped_stop() {
        let rows = vec![
            row("uia", None, 0),
            row("root", Some("uia"), 1),
            row("worker", Some("root"), 2),
        ];
        let mut tree = AgentTree::default();
        assert_eq!(
            tree.handle_key(key(KeyCode::Char('s')), &rows),
            AgentTreeAction::Stay
        );
        tree.handle_key(key(KeyCode::Right), &rows);
        tree.handle_key(key(KeyCode::Left), &rows);
        assert_eq!(tree.visible(&rows).len(), 2);
        tree.handle_key(key(KeyCode::Right), &rows);
        assert_eq!(tree.visible(&rows).len(), 3);
        tree.handle_key(key(KeyCode::Enter), &rows);
        assert_eq!(
            tree.handle_key(key(KeyCode::Esc), &rows),
            AgentTreeAction::Stay
        );
        assert_eq!(
            tree.handle_key(key(KeyCode::Char('s')), &rows),
            AgentTreeAction::Stop("root".to_owned())
        );
        assert_eq!(
            tree.handle_key(key(KeyCode::Char('s')), &rows),
            AgentTreeAction::Stay
        );
        assert_eq!(
            tree.handle_key(key(KeyCode::Esc), &rows),
            AgentTreeAction::Close
        );
    }

    #[test]
    fn unknown_usage_is_not_zero_and_terminal_nodes_cannot_stop() {
        assert_eq!(known::<u64>(None), "—");
        let mut node = row("done", Some("uia"), 1);
        node.can_stop = false;
        assert_eq!(
            AgentTree::default().handle_key(key(KeyCode::Char('s')), &[node]),
            AgentTreeAction::Stay
        );
    }
}
