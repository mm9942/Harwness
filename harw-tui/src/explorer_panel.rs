//! Explorer-Seitenpanel der TUI: ein einklappbarer Baum über den
//! [`harw_explorer::ExplorerIndex`] des Arbeitsverzeichnisses.
//!
//! [`ExplorerPanel`] ist reiner Zustand plus Rendering. Der Index wird auf
//! einem eigenen Thread gebaut ([`ExplorerPanel::start_indexing`]) und über
//! einen `mpsc`-Kanal abgeholt ([`ExplorerPanel::poll`]); die UI blockiert
//! dabei nie. Tasten liefern eine [`ExplorerAction`], die die App umsetzt
//! (z. B. `@pfad` in den Composer einfügen).
//!
//! ```text
//! ┌ Explorer · 120 Einträge · 3 Projekte ┐
//! │▾ harw-tui [cargo-crate]              │
//! │  ▸ src                               │
//! │    Cargo.toml config                 │
//! │──────── Cargo.toml ──────────────────│
//! │[package]                             │
//! │/ such▏                               │
//! └──────────────────────────────────────┘
//! ```


use std::cell::Cell;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harw_explorer::{ExplorerIndex, ExplorerOptions, FileKind, Node};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Höchstzahl an Treffern im Filtermodus.
const FILTER_LIMIT: usize = 500;
/// Obergrenze der gelesenen Bytes für die Vorschau.
const PREVIEW_BYTES: u64 = 64 * 1024;
/// Höchstzahl angezeigter Vorschauzeilen.
const PREVIEW_LINES: usize = 40;
/// Schutz gegen pathologisch tiefe Bäume beim Aufklappen.
const MAX_TREE_DEPTH: usize = 128;

/// Ergebnis einer Tastenbehandlung im Explorer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExplorerAction {
    /// Taste nicht verbraucht bzw. nichts geändert.
    None,
    /// Zustand geändert; neu zeichnen.
    Redraw,
    /// Relativen Pfad (mit `/` getrennt, ohne führendes `@`) als
    /// `@pfad`-Referenz in den Composer einfügen.
    InsertPath(String),
    /// Neuaufbau des Index wurde gestartet; die App sollte [`ExplorerPanel::poll`]
    /// regelmäßig aufrufen, bis er fertig ist.
    Rebuild,
}

/// Welche Ansicht das Panel zeigt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// Der einklappbare Dateibaum.
    Tree,
    /// Liste der erkannten Projekte.
    Projects,
}

/// Inhalt des Vorschaubereichs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreviewBody {
    /// Bereits bereinigte Textzeilen.
    Text(Vec<String>),
    /// Einzeilige Beschreibung (PDF, Binärdatei, Verzeichnis, Fehler).
    Info(String),
}

/// Vorschau der ausgewählten Datei.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Preview {
    path: PathBuf,
    body: PreviewBody,
}

/// Eine sichtbare Zeile des Baums bzw. der Trefferliste.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    path: PathBuf,
    kind: FileKind,
    size: u64,
    ignored: bool,
    depth: usize,
    expanded: bool,
    /// `true` im Filtermodus: vollständiger Pfad statt Dateiname.
    flat: bool,
}

impl Row {
    fn from_node(node: &Node, depth: usize, expanded: bool, flat: bool) -> Self {
        Self {
            path: node.path.clone(),
            kind: node.kind.clone(),
            size: node.size,
            ignored: node.ignored,
            depth,
            expanded,
            flat,
        }
    }

    fn is_dir(&self) -> bool {
        self.kind == FileKind::Dir
    }
}

/// Ergebnis eines Hintergrund-Indexlaufs.
type IndexOutcome = Result<ExplorerIndex, String>;

/// Zustand des Explorer-Panels.
#[derive(Debug)]
pub(crate) struct ExplorerPanel {
    /// Absolute Wurzel des Explorers.
    root: PathBuf,
    /// Arbeitsverzeichnis beim Start; Projekte, die es enthalten, starten
    /// aufgeklappt.
    cwd: Option<PathBuf>,
    index: Option<ExplorerIndex>,
    /// Kanal zum laufenden Indexlauf.
    pending: Option<Receiver<IndexOutcome>>,
    error: Option<String>,
    include_ignored: bool,
    /// Aufgeklappte Verzeichnisse (relativ zur Wurzel).
    expanded: HashSet<PathBuf>,
    /// Wurde die Anfangs-Aufklappung schon angewandt?
    initialized: bool,
    view: View,
    /// Auswahl in den Baumzeilen.
    selected: usize,
    /// Auswahl in der Projektliste.
    project_selected: usize,
    /// Filtertext; `Some` heißt Filtermodus aktiv.
    filter: Option<String>,
    /// Tippt der Nutzer gerade in den Filter?
    filter_editing: bool,
    preview: Option<Preview>,
    /// Erste sichtbare Zeile des Baums (wird beim Rendern nachgeführt).
    scroll: Cell<usize>,
    /// Erste sichtbare Zeile der Projektliste.
    project_scroll: Cell<usize>,
    /// Zuletzt gerenderte Listenhöhe, für Bild-auf/-ab.
    page: Cell<usize>,
}

impl ExplorerPanel {
    /// Legt ein leeres Panel für `root` an. Führt keine Datei-E/A aus; der
    /// Index entsteht erst mit [`Self::start_indexing`].
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            cwd: std::env::current_dir().ok(),
            index: None,
            pending: None,
            error: None,
            include_ignored: false,
            expanded: HashSet::new(),
            initialized: false,
            view: View::Tree,
            selected: 0,
            project_selected: 0,
            filter: None,
            filter_editing: false,
            preview: None,
            scroll: Cell::new(0),
            project_scroll: Cell::new(0),
            page: Cell::new(10),
        }
    }

    /// Testkonstruktor mit bereits fertigem Index (ohne Thread, ohne `cwd`).
    #[cfg(test)]
    fn with_index(root: PathBuf, index: ExplorerIndex) -> Self {
        let mut panel = Self::new(root);
        panel.cwd = None;
        panel.apply_index(index);
        panel
    }

    /// Startet den Indexaufbau auf einem Hintergrund-Thread. Ein noch
    /// laufender älterer Lauf wird verworfen (sein Ergebnis kommt nie an).
    pub(crate) fn start_indexing(&mut self) {
        let (tx, rx) = mpsc::channel::<IndexOutcome>();
        let root = self.root.clone();
        let opts = ExplorerOptions {
            include_ignored: self.include_ignored,
            ..ExplorerOptions::default()
        };
        let spawned = std::thread::Builder::new()
            .name("harw-explorer-index".to_owned())
            .spawn(move || {
                let outcome = ExplorerIndex::build(&root, &opts).map_err(|e| e.to_string());
                // Empfänger kann bereits verworfen sein (neuer Lauf); dann egal.
                let _ = tx.send(outcome);
            });
        match spawned {
            Ok(_) => {
                self.pending = Some(rx);
                self.error = None;
            }
            Err(error) => {
                self.pending = None;
                self.error = Some(format!("Indexierung nicht startbar: {error}"));
            }
        }
    }

    /// `true`, solange ein Indexlauf aussteht.
    pub(crate) fn is_indexing(&self) -> bool {
        self.pending.is_some()
    }

    /// Holt einen fertigen Index ab, ohne zu blockieren. Liefert `true`, wenn
    /// sich dadurch Sichtbares geändert hat.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(index)) => {
                self.pending = None;
                self.apply_index(index);
                true
            }
            Ok(Err(error)) => {
                self.pending = None;
                self.error = Some(error);
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.error = Some("Indexierung abgebrochen".to_owned());
                true
            }
        }
    }

    /// Übernimmt einen neuen Index; die Auswahl bleibt (per Pfad) erhalten.
    fn apply_index(&mut self, index: ExplorerIndex) {
        let keep = self.selected_row().map(|row| row.path);
        if !self.initialized {
            self.initial_expansion(&index);
            self.initialized = true;
        }
        self.index = Some(index);
        self.error = None;
        if let Some(path) = keep
            && let Some(pos) = self.rows().iter().position(|row| row.path == path)
        {
            self.selected = pos;
        }
        self.clamp_selection();
    }

    /// Klappt Projektwurzeln auf, die das Arbeitsverzeichnis enthalten
    /// (samt ihrer Vorfahren); die oberste Ebene ist ohnehin sichtbar.
    fn initial_expansion(&mut self, index: &ExplorerIndex) {
        let Some(cwd) = &self.cwd else {
            return;
        };
        let canonical = std::fs::canonicalize(cwd).ok();
        for project in &index.projects {
            if project.root.as_os_str().is_empty() {
                continue;
            }
            let abs = index.root.join(&project.root);
            let alt = self.root.join(&project.root);
            let contains = |dir: &Path| dir.starts_with(&abs) || dir.starts_with(&alt);
            if contains(cwd) || canonical.as_deref().is_some_and(contains) {
                expand_with_ancestors(&mut self.expanded, &project.root);
            }
        }
    }

    // ---------------------------------------------------------------
    // Zeilenmodell
    // ---------------------------------------------------------------

    /// Aktiver, nicht-leerer Filtertext.
    fn active_filter(&self) -> Option<&str> {
        self.filter.as_deref().filter(|q| !q.trim().is_empty())
    }

    /// Alle sichtbaren Zeilen in Anzeigereihenfolge.
    fn rows(&self) -> Vec<Row> {
        let Some(index) = &self.index else {
            return Vec::new();
        };
        if let Some(query) = self.active_filter() {
            return index
                .find(query, FILTER_LIMIT)
                .into_iter()
                .map(|node| Row::from_node(node, 0, false, true))
                .collect();
        }
        let mut out = Vec::new();
        self.push_children(index, Path::new(""), 0, &mut out);
        out
    }

    fn push_children(&self, index: &ExplorerIndex, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        for node in index.children(dir) {
            let expanded = node.kind == FileKind::Dir && self.expanded.contains(&node.path);
            out.push(Row::from_node(node, depth, expanded, false));
            if expanded && depth < MAX_TREE_DEPTH {
                self.push_children(index, &node.path, depth + 1, out);
            }
        }
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows().into_iter().nth(self.selected)
    }

    fn clamp_selection(&mut self) {
        let len = self.rows().len();
        self.selected = self.selected.min(len.saturating_sub(1));
        let projects = self.index.as_ref().map_or(0, |i| i.projects.len());
        self.project_selected = self.project_selected.min(projects.saturating_sub(1));
    }

    /// Wählt `path` aus: Filter aus, Vorfahren aufklappen, Zeile markieren.
    fn reveal(&mut self, path: &Path) {
        self.filter = None;
        self.filter_editing = false;
        self.view = View::Tree;
        if let Some(parent) = path.parent() {
            expand_with_ancestors(&mut self.expanded, parent);
        }
        if let Some(pos) = self.rows().iter().position(|row| row.path == path) {
            self.selected = pos;
        }
        self.clamp_selection();
    }

    // ---------------------------------------------------------------
    // Tasten
    // ---------------------------------------------------------------

    /// Verarbeitet eine Taste, wenn das Panel den Fokus hat.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ExplorerAction {
        if key.kind == KeyEventKind::Release
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ExplorerAction::None;
        }
        if self.filter_editing {
            return self.handle_filter_key(key);
        }
        if self.view == View::Projects {
            return self.handle_projects_key(key);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-self.page_step()),
            KeyCode::PageDown => self.move_by(self.page_step()),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX / 2),
            KeyCode::Right | KeyCode::Char('l') => self.expand_selected(),
            KeyCode::Left | KeyCode::Char('h') => self.collapse_or_parent(),
            KeyCode::Enter => self.activate(),
            KeyCode::Char('@') => self.insert_selected(),
            KeyCode::Char('/') => {
                self.filter = Some(String::new());
                self.filter_editing = true;
                self.selected = 0;
                ExplorerAction::Redraw
            }
            KeyCode::Esc => {
                if self.filter.is_some() {
                    self.clear_filter();
                    ExplorerAction::Redraw
                } else if self.preview.take().is_some() {
                    ExplorerAction::Redraw
                } else {
                    ExplorerAction::None
                }
            }
            KeyCode::Char('p') => {
                self.view = View::Projects;
                ExplorerAction::Redraw
            }
            KeyCode::Char('r') => self.rebuild(),
            KeyCode::Char('i') => {
                self.include_ignored = !self.include_ignored;
                self.rebuild()
            }
            _ => ExplorerAction::None,
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent) -> ExplorerAction {
        match key.code {
            KeyCode::Esc => {
                self.clear_filter();
                ExplorerAction::Redraw
            }
            KeyCode::Enter => {
                self.filter_editing = false;
                if self.active_filter().is_none() {
                    self.clear_filter();
                }
                ExplorerAction::Redraw
            }
            KeyCode::Backspace => {
                if let Some(filter) = &mut self.filter {
                    filter.pop();
                }
                self.selected = 0;
                ExplorerAction::Redraw
            }
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Char(c) => {
                if let Some(filter) = &mut self.filter {
                    filter.push(c);
                }
                self.selected = 0;
                ExplorerAction::Redraw
            }
            _ => ExplorerAction::None,
        }
    }

    fn handle_projects_key(&mut self, key: KeyEvent) -> ExplorerAction {
        let count = self.index.as_ref().map_or(0, |i| i.projects.len());
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.project_selected = self.project_selected.saturating_sub(1);
                ExplorerAction::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.project_selected = (self.project_selected + 1).min(count.saturating_sub(1));
                ExplorerAction::Redraw
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let root = self
                    .index
                    .as_ref()
                    .and_then(|i| i.projects.get(self.project_selected))
                    .map(|p| p.root.clone());
                match root {
                    Some(root) if root.as_os_str().is_empty() => {
                        self.view = View::Tree;
                        self.selected = 0;
                    }
                    Some(root) => {
                        self.expanded.insert(root.clone());
                        self.reveal(&root);
                    }
                    None => self.view = View::Tree,
                }
                ExplorerAction::Redraw
            }
            KeyCode::Char('@') => {
                let root = self
                    .index
                    .as_ref()
                    .and_then(|i| i.projects.get(self.project_selected))
                    .map(|p| p.root.clone());
                match root {
                    Some(root) if root.as_os_str().is_empty() => {
                        ExplorerAction::InsertPath(".".to_owned())
                    }
                    Some(root) => ExplorerAction::InsertPath(slash_path(&root)),
                    None => ExplorerAction::None,
                }
            }
            KeyCode::Char('p') | KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                self.view = View::Tree;
                ExplorerAction::Redraw
            }
            KeyCode::Char('r') => self.rebuild(),
            KeyCode::Char('i') => {
                self.include_ignored = !self.include_ignored;
                self.rebuild()
            }
            _ => ExplorerAction::None,
        }
    }

    fn page_step(&self) -> isize {
        isize::try_from(self.page.get().max(1)).unwrap_or(10)
    }

    fn clear_filter(&mut self) {
        self.filter = None;
        self.filter_editing = false;
        self.selected = 0;
    }

    fn rebuild(&mut self) -> ExplorerAction {
        self.start_indexing();
        ExplorerAction::Rebuild
    }

    fn move_by(&mut self, delta: isize) -> ExplorerAction {
        let len = self.rows().len();
        if len == 0 {
            return ExplorerAction::None;
        }
        let target = self.selected.saturating_add_signed(delta).min(len - 1);
        if target == self.selected {
            return ExplorerAction::None;
        }
        self.selected = target;
        self.refresh_preview();
        ExplorerAction::Redraw
    }

    fn expand_selected(&mut self) -> ExplorerAction {
        let Some(row) = self.selected_row() else {
            return ExplorerAction::None;
        };
        if !row.is_dir() {
            return ExplorerAction::None;
        }
        if row.flat {
            self.expanded.insert(row.path.clone());
            self.reveal(&row.path);
            return ExplorerAction::Redraw;
        }
        if row.expanded {
            // Schon offen: zum ersten Kind springen, falls vorhanden.
            return self.move_by(1);
        }
        self.expanded.insert(row.path);
        ExplorerAction::Redraw
    }

    fn collapse_or_parent(&mut self) -> ExplorerAction {
        let Some(row) = self.selected_row() else {
            return ExplorerAction::None;
        };
        if row.flat {
            return ExplorerAction::None;
        }
        if row.expanded {
            self.expanded.remove(&row.path);
            return ExplorerAction::Redraw;
        }
        let Some(parent) = row.path.parent().filter(|p| !p.as_os_str().is_empty()) else {
            return ExplorerAction::None;
        };
        if let Some(pos) = self.rows().iter().position(|r| r.path == parent) {
            self.selected = pos;
            self.refresh_preview();
            return ExplorerAction::Redraw;
        }
        ExplorerAction::None
    }

    /// `Enter`: Verzeichnis auf-/zuklappen, Datei-Vorschau umschalten; im
    /// Filtermodus zuerst den Treffer im Baum aufdecken.
    fn activate(&mut self) -> ExplorerAction {
        let Some(row) = self.selected_row() else {
            return ExplorerAction::None;
        };
        if row.flat {
            if row.is_dir() {
                self.expanded.insert(row.path.clone());
            }
            self.reveal(&row.path);
            if !row.is_dir() {
                self.preview = Some(self.load_preview(&row));
            }
            return ExplorerAction::Redraw;
        }
        if row.is_dir() {
            if !self.expanded.remove(&row.path) {
                self.expanded.insert(row.path);
            }
            return ExplorerAction::Redraw;
        }
        if self.preview.as_ref().is_some_and(|p| p.path == row.path) {
            self.preview = None;
        } else {
            self.preview = Some(self.load_preview(&row));
        }
        ExplorerAction::Redraw
    }

    fn insert_selected(&self) -> ExplorerAction {
        match self.selected_row() {
            Some(row) => ExplorerAction::InsertPath(slash_path(&row.path)),
            None => ExplorerAction::None,
        }
    }

    // ---------------------------------------------------------------
    // Vorschau
    // ---------------------------------------------------------------

    /// Folgt bei offener Vorschau der Auswahl.
    fn refresh_preview(&mut self) {
        if self.preview.is_none() {
            return;
        }
        if let Some(row) = self.selected_row() {
            self.preview = Some(self.load_preview(&row));
        }
    }

    /// Lädt die Vorschau für `row` (liest höchstens [`PREVIEW_BYTES`]).
    fn load_preview(&self, row: &Row) -> Preview {
        let body = match &row.kind {
            FileKind::Dir => {
                let count = self
                    .index
                    .as_ref()
                    .map_or(0, |i| i.children(&row.path).len());
                PreviewBody::Info(format!("Verzeichnis · {count} Einträge"))
            }
            FileKind::Pdf => PreviewBody::Info(format!(
                "PDF · {} KiB — Inhalt per doc.read_pdf",
                row.size.div_ceil(1024)
            )),
            FileKind::Image | FileKind::Archive | FileKind::Binary => {
                PreviewBody::Info(format!("{} · {}", row.kind.label(), human_size(row.size)))
            }
            FileKind::Source { .. }
            | FileKind::Markdown
            | FileKind::Config
            | FileKind::Data
            | FileKind::Text => {
                let base = self.index.as_ref().map_or(&self.root, |i| &i.root);
                read_text_preview(&base.join(&row.path), row)
            }
        };
        Preview {
            path: row.path.clone(),
            body,
        }
    }

    // ---------------------------------------------------------------
    // Rendering
    // ---------------------------------------------------------------

    /// Titelzeile des Panels.
    fn title(&self) -> String {
        if self.is_indexing() {
            return " Explorer · indiziert… ".to_owned();
        }
        if let Some(index) = &self.index {
            let mut title = format!(
                " Explorer · {} Einträge · {} Projekte",
                index.nodes.len(),
                index.projects.len()
            );
            if index.truncated {
                title.push_str(" (gekürzt)");
            }
            if self.include_ignored {
                title.push_str(" · +ignoriert");
            }
            title.push(' ');
            return title;
        }
        if self.error.is_some() {
            return " Explorer · Fehler ".to_owned();
        }
        " Explorer ".to_owned()
    }

    /// Zeichnet das Panel in `area`.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme, focused: bool) {
        let border = if focused {
            Style::default().fg(style::accent_color(theme))
        } else {
            Style::default().fg(style::border_color(theme))
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(sanitize_inline(&self.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let mut list = inner;
        if let Some(filter) = &self.filter
            && list.height >= 2
        {
            list.height -= 1;
            let line_area = Rect {
                y: list.y + list.height,
                height: 1,
                ..list
            };
            let cursor = if self.filter_editing { "▏" } else { "" };
            Paragraph::new(Line::from(vec![
                Span::styled("/ ", Style::default().fg(style::accent_color(theme))),
                Span::raw(sanitize_inline(filter)),
                Span::styled(cursor, Style::default().fg(style::accent_color(theme))),
            ]))
            .render(line_area, buf);
        }
        if let Some(preview) = &self.preview
            && self.view == View::Tree
            && list.height >= 6
        {
            let height = list.height / 3;
            list.height -= height;
            let preview_area = Rect {
                y: list.y + list.height,
                height,
                ..list
            };
            render_preview(preview, preview_area, buf, theme);
        }
        match self.view {
            View::Tree => self.render_tree(list, buf, theme, focused),
            View::Projects => self.render_projects(list, buf, theme, focused),
        }
    }

    fn render_tree(&self, area: Rect, buf: &mut Buffer, theme: Theme, focused: bool) {
        let Some(index) = &self.index else {
            let line = if let Some(error) = &self.error {
                Line::styled(
                    format!("Fehler: {}", sanitize_inline(error)),
                    style::error_style(theme),
                )
            } else if self.is_indexing() {
                Line::styled("Indiziere …", style::dim_style(theme))
            } else {
                Line::styled("Noch kein Index (r startet).", style::dim_style(theme))
            };
            Paragraph::new(line).render(area, buf);
            return;
        };
        let rows = self.rows();
        let height = usize::from(area.height);
        self.page.set(height);
        if rows.is_empty() {
            let text = if self.active_filter().is_some() {
                "Keine Treffer."
            } else {
                "Leeres Verzeichnis."
            };
            Paragraph::new(Line::styled(text, style::dim_style(theme))).render(area, buf);
            return;
        }
        let offset = visible_offset(self.scroll.get(), self.selected, height, rows.len());
        self.scroll.set(offset);
        let lines: Vec<Line<'static>> = rows
            .iter()
            .enumerate()
            .skip(offset)
            .take(height)
            .map(|(pos, row)| row_line(index, row, pos == self.selected, focused, theme))
            .collect();
        Paragraph::new(lines).render(area, buf);
    }

    fn render_projects(&self, area: Rect, buf: &mut Buffer, theme: Theme, focused: bool) {
        let Some(index) = &self.index else {
            Paragraph::new(Line::styled("Noch kein Index.", style::dim_style(theme)))
                .render(area, buf);
            return;
        };
        let mut lines: Vec<Line<'static>> = vec![Line::styled(
            format!(
                "Projekte {} · Relationen {}",
                index.projects.len(),
                index.relations.len()
            ),
            style::dim_style(theme).add_modifier(Modifier::BOLD),
        )];
        let height = usize::from(area.height.saturating_sub(1));
        if index.projects.is_empty() {
            lines.push(Line::styled(
                "Keine Projekte erkannt.",
                style::dim_style(theme),
            ));
        }
        let offset = visible_offset(
            self.project_scroll.get(),
            self.project_selected,
            height,
            index.projects.len(),
        );
        self.project_scroll.set(offset);
        for (pos, project) in index.projects.iter().enumerate().skip(offset).take(height) {
            let root = if project.root.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                slash_path(&project.root)
            };
            let mut spans = vec![
                Span::styled(
                    format!("[{}] ", project.kind.label()),
                    Style::default().fg(style::accent_color(theme)),
                ),
                Span::styled(
                    sanitize_inline(&project.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", sanitize_inline(&root)),
                    style::dim_style(theme),
                ),
            ];
            if !project.members.is_empty() {
                spans.push(Span::styled(
                    format!(" · {} Mitglieder", project.members.len()),
                    style::dim_style(theme),
                ));
            }
            lines.push(highlight(
                Line::from(spans),
                pos == self.project_selected,
                focused,
            ));
        }
        Paragraph::new(lines).render(area, buf);
    }
}

/// Fügt `path` und alle nicht-leeren Vorfahren in `expanded` ein.
fn expand_with_ancestors(expanded: &mut HashSet<PathBuf>, path: &Path) {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        expanded.insert(ancestor.to_path_buf());
    }
}

/// Relativer Pfad mit `/` als Trenner.
fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Kompakte Größenangabe (`512 B`, `3 KiB`, `1.5 MiB`).
fn human_size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{} KiB", bytes.div_ceil(1024)),
        _ => format!("{:.1} MiB", bytes as f64 / 1_048_576.0),
    }
}

/// Erste sichtbare Zeile, sodass `selected` im Fenster `height` liegt.
fn visible_offset(previous: usize, selected: usize, height: usize, len: usize) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let mut offset = previous.min(len.saturating_sub(height));
    if selected < offset {
        offset = selected;
    } else if selected >= offset + height {
        offset = selected + 1 - height;
    }
    offset
}

/// Liest bis zu [`PREVIEW_BYTES`] und liefert höchstens [`PREVIEW_LINES`]
/// bereinigte Zeilen; Dateien mit NUL-Bytes gelten als binär.
fn read_text_preview(path: &Path, row: &Row) -> PreviewBody {
    let mut bytes = Vec::new();
    let read =
        std::fs::File::open(path).and_then(|file| file.take(PREVIEW_BYTES).read_to_end(&mut bytes));
    if let Err(error) = read {
        return PreviewBody::Info(format!("Nicht lesbar: {error}"));
    }
    if bytes.contains(&0) {
        return PreviewBody::Info(format!("binär · {}", human_size(row.size)));
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text
        .lines()
        .take(PREVIEW_LINES)
        .map(|line| sanitize_inline(&line.replace('\t', "    ")))
        .collect();
    if text.lines().nth(PREVIEW_LINES).is_some() || row.size > PREVIEW_BYTES {
        lines.push("…".to_owned());
    }
    PreviewBody::Text(lines)
}

/// Zeichnet den Vorschaubereich mit Trennlinie und Dateiname.
fn render_preview(preview: &Preview, area: Rect, buf: &mut Buffer, theme: Theme) {
    let name = preview
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(style::border_color(theme)))
        .title(format!(" {} ", sanitize_inline(&name)));
    let lines: Vec<Line<'static>> = match &preview.body {
        PreviewBody::Text(lines) if lines.is_empty() => {
            vec![Line::styled("(leer)", style::dim_style(theme))]
        }
        PreviewBody::Text(lines) => lines.iter().map(|l| Line::raw(l.clone())).collect(),
        PreviewBody::Info(info) => {
            vec![Line::styled(sanitize_inline(info), style::dim_style(theme))]
        }
    };
    Paragraph::new(lines).block(block).render(area, buf);
}

/// Hebt eine Zeile als Auswahl hervor.
fn highlight(line: Line<'static>, selected: bool, focused: bool) -> Line<'static> {
    if !selected {
        return line;
    }
    let modifier = if focused {
        Modifier::REVERSED
    } else {
        Modifier::BOLD
    };
    line.patch_style(Style::default().add_modifier(modifier))
}

/// Baut die Anzeigezeile für einen Baumeintrag.
fn row_line(
    index: &ExplorerIndex,
    row: &Row,
    selected: bool,
    focused: bool,
    theme: Theme,
) -> Line<'static> {
    let dim = style::dim_style(theme);
    let mut spans = vec![Span::raw("  ".repeat(row.depth))];
    if row.is_dir() {
        let marker = if row.expanded { "▾ " } else { "▸ " };
        spans.push(Span::styled(
            marker,
            Style::default().fg(style::accent_color(theme)),
        ));
    } else {
        spans.push(Span::raw("  "));
    }
    let name = if row.flat {
        slash_path(&row.path)
    } else {
        row.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let name_style = match (row.ignored, row.is_dir()) {
        (true, _) => dim,
        (false, true) => Style::default().add_modifier(Modifier::BOLD),
        (false, false) => Style::default(),
    };
    spans.push(Span::styled(sanitize_inline(&name), name_style));
    let badges: Vec<&str> = index
        .projects
        .iter()
        .filter(|p| p.root == row.path)
        .map(|p| p.kind.label())
        .collect();
    if !badges.is_empty() {
        spans.push(Span::styled(
            format!(" [{}]", badges.join(",")),
            Style::default().fg(style::accent_color(theme)),
        ));
    }
    if !row.is_dir() {
        spans.push(Span::styled(
            format!(" {}", sanitize_inline(row.kind.label())),
            dim,
        ));
    }
    highlight(Line::from(spans), selected, focused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;
    use harw_explorer::{Project, ProjectKind};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn node(path: &str, kind: FileKind, depth: u16) -> Node {
        Node {
            path: PathBuf::from(path),
            kind,
            size: 12,
            depth,
            ignored: false,
        }
    }

    fn rust() -> FileKind {
        FileKind::Source {
            lang: "rust".to_owned(),
        }
    }

    /// `a/{x.rs}`, `b/{sub/, y.md}`, `readme.md`, Projekt in `b`.
    fn sample_index(root: &Path) -> ExplorerIndex {
        ExplorerIndex {
            root: root.to_path_buf(),
            nodes: vec![
                node("a", FileKind::Dir, 1),
                node("a/x.rs", rust(), 2),
                node("b", FileKind::Dir, 1),
                node("b/sub", FileKind::Dir, 2),
                node("b/y.md", FileKind::Markdown, 2),
                node("readme.md", FileKind::Markdown, 1),
            ],
            projects: vec![Project {
                root: PathBuf::from("b"),
                kind: ProjectKind::CargoCrate,
                name: "bee".to_owned(),
                members: Vec::new(),
                manifest: None,
            }],
            relations: Vec::new(),
            truncated: false,
        }
    }

    fn panel() -> ExplorerPanel {
        let root = PathBuf::from("/nicht/vorhanden");
        ExplorerPanel::with_index(root.clone(), sample_index(&root))
    }

    fn paths(panel: &ExplorerPanel) -> Vec<String> {
        panel.rows().iter().map(|r| slash_path(&r.path)).collect()
    }

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn starts_collapsed_and_navigates() {
        let mut p = panel();
        assert_eq!(paths(&p), ["a", "b", "readme.md"]);
        assert_eq!(p.handle_key(key(KeyCode::Down)), ExplorerAction::Redraw);
        assert_eq!(
            p.handle_key(key(KeyCode::Char('j'))),
            ExplorerAction::Redraw
        );
        assert_eq!(p.selected, 2);
        assert_eq!(
            p.handle_key(key(KeyCode::Down)),
            ExplorerAction::None,
            "clamped at end"
        );
        p.handle_key(key(KeyCode::Char('k')));
        assert_eq!(p.selected, 1);
        p.handle_key(key(KeyCode::Home));
        assert_eq!(p.selected, 0);
        p.handle_key(key(KeyCode::End));
        assert_eq!(p.selected, 2);
    }

    #[test]
    fn expand_collapse_and_parent() {
        let mut p = panel();
        assert_eq!(p.handle_key(key(KeyCode::Right)), ExplorerAction::Redraw);
        assert_eq!(paths(&p), ["a", "a/x.rs", "b", "readme.md"]);
        // Rechts auf offenem Ordner springt zum ersten Kind.
        p.handle_key(key(KeyCode::Char('l')));
        assert_eq!(p.selected, 1);
        // Links auf Datei: zum Elternordner.
        p.handle_key(key(KeyCode::Left));
        assert_eq!(p.selected, 0);
        // Links auf offenem Ordner: zuklappen.
        p.handle_key(key(KeyCode::Char('h')));
        assert_eq!(paths(&p), ["a", "b", "readme.md"]);
        // Enter klappt Ordner um.
        p.handle_key(key(KeyCode::Enter));
        assert_eq!(paths(&p).len(), 4);
        p.handle_key(key(KeyCode::Enter));
        assert_eq!(paths(&p).len(), 3);
        // Kinder: Ordner vor Dateien.
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Right));
        assert_eq!(paths(&p), ["a", "b", "b/sub", "b/y.md", "readme.md"]);
    }

    #[test]
    fn project_containing_cwd_starts_expanded() {
        let root = PathBuf::from("/nicht/vorhanden");
        let mut p = ExplorerPanel::new(root.clone());
        p.cwd = Some(root.join("b/sub"));
        p.apply_index(sample_index(&root));
        assert_eq!(paths(&p), ["a", "b", "b/sub", "b/y.md", "readme.md"]);
    }

    #[test]
    fn filter_edits_and_clears() {
        let mut p = panel();
        p.handle_key(key(KeyCode::Char('/')));
        assert!(p.filter_editing);
        for c in "y.m".chars() {
            p.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(paths(&p), ["b/y.md"]);
        // Buchstaben wie `j`/`k` gehören beim Tippen dem Filter.
        p.handle_key(key(KeyCode::Backspace));
        assert_eq!(p.filter.as_deref(), Some("y."));
        p.handle_key(key(KeyCode::Enter));
        assert!(!p.filter_editing);
        assert_eq!(p.filter.as_deref(), Some("y."));
        assert_eq!(
            p.handle_key(key(KeyCode::Char('@'))),
            ExplorerAction::InsertPath("b/y.md".into())
        );
        p.handle_key(key(KeyCode::Esc));
        assert!(p.filter.is_none());
        assert_eq!(paths(&p), ["a", "b", "readme.md"]);
    }

    #[test]
    fn enter_on_filter_hit_reveals_it_in_tree() {
        let mut p = panel();
        p.handle_key(key(KeyCode::Char('/')));
        for c in "x.rs".chars() {
            p.handle_key(key(KeyCode::Char(c)));
        }
        p.handle_key(key(KeyCode::Enter)); // Tippen beenden
        p.handle_key(key(KeyCode::Enter)); // Treffer aufdecken
        assert!(p.filter.is_none());
        assert_eq!(
            p.selected_row().map(|r| slash_path(&r.path)).as_deref(),
            Some("a/x.rs")
        );
        assert!(p.preview.is_some(), "file hit opens preview");
    }

    #[test]
    fn insert_path_uses_relative_slash_path() {
        let mut p = panel();
        p.handle_key(key(KeyCode::Right));
        p.handle_key(key(KeyCode::Down));
        assert_eq!(
            p.handle_key(key(KeyCode::Char('@'))),
            ExplorerAction::InsertPath("a/x.rs".to_owned())
        );
    }

    #[test]
    fn projects_view_toggles_and_jumps() {
        let mut p = panel();
        p.handle_key(key(KeyCode::Char('p')));
        assert_eq!(p.view, View::Projects);
        assert_eq!(
            p.handle_key(key(KeyCode::Char('@'))),
            ExplorerAction::InsertPath("b".into())
        );
        p.handle_key(key(KeyCode::Enter));
        assert_eq!(p.view, View::Tree);
        assert_eq!(
            p.selected_row().map(|r| slash_path(&r.path)).as_deref(),
            Some("b")
        );
        assert!(p.expanded.contains(Path::new("b")));
    }

    #[test]
    fn preview_toggles_and_classifies() -> TestResult {
        let dir = tempfile::tempdir()?;
        let body: String = (0..60).map(|i| format!("zeile\t{i}\n")).collect();
        std::fs::write(dir.path().join("readme.md"), &body)?;
        let mut index = sample_index(dir.path());
        index.nodes.push(Node {
            path: PathBuf::from("z.pdf"),
            kind: FileKind::Pdf,
            size: 4096,
            depth: 1,
            ignored: false,
        });
        let mut p = ExplorerPanel::with_index(dir.path().to_path_buf(), index);
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Enter));
        let Some(Preview {
            body: PreviewBody::Text(lines),
            ..
        }) = &p.preview
        else {
            return Err("expected text preview".into());
        };
        assert_eq!(lines.len(), PREVIEW_LINES + 1);
        assert_eq!(lines[0], "zeile    0");
        // Vorschau folgt der Auswahl.
        p.handle_key(key(KeyCode::Down));
        assert_eq!(
            p.preview.as_ref().map(|pr| pr.body.clone()),
            Some(PreviewBody::Info(
                "PDF · 4 KiB — Inhalt per doc.read_pdf".into()
            ))
        );
        p.handle_key(key(KeyCode::Enter));
        assert!(p.preview.is_none(), "enter closes preview again");
        Ok(())
    }

    #[test]
    fn render_smoke() {
        let mut p = panel();
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Right));
        let area = Rect::new(0, 0, 44, 12);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf, Theme::Dark, true);
        let text = buffer_text(&buf);
        assert!(
            text.contains("Explorer · 6 Einträge · 1 Projekte"),
            "{text}"
        );
        assert!(text.contains("▾ b [cargo-crate]"), "{text}");
        assert!(text.contains("x.rs") || text.contains("y.md"), "{text}");
        assert!(text.contains("readme.md"), "{text}");

        p.handle_key(key(KeyCode::Char('p')));
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf, Theme::Light, false);
        let text = buffer_text(&buf);
        assert!(text.contains("Projekte 1 · Relationen 0"), "{text}");
        assert!(text.contains("[cargo-crate] bee"), "{text}");

        // Winzige Flächen dürfen nicht paniken.
        let tiny = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::empty(tiny);
        p.render(tiny, &mut buf, Theme::Dark, true);
    }

    #[test]
    fn scroll_keeps_selection_visible() {
        let mut p = panel();
        p.handle_key(key(KeyCode::Right));
        p.handle_key(key(KeyCode::End));
        let area = Rect::new(0, 0, 30, 4); // zwei Listenzeilen
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf, Theme::Dark, true);
        assert_eq!(p.scroll.get(), 2);
        assert!(buffer_text(&buf).contains("readme.md"));
        assert_eq!(visible_offset(5, 1, 3, 10), 1);
        assert_eq!(visible_offset(0, 9, 3, 10), 7);
    }

    #[test]
    fn background_indexing_delivers_via_poll() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("notes.txt"), "hallo")?;
        let mut p = ExplorerPanel::new(dir.path().to_path_buf());
        p.start_indexing();
        assert!(p.is_indexing());
        assert!(p.title().contains("indiziert"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !p.poll() {
            if std::time::Instant::now() > deadline {
                return Err("index did not arrive".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!p.is_indexing());
        assert!(paths(&p).contains(&"notes.txt".to_owned()));
        Ok(())
    }
}
