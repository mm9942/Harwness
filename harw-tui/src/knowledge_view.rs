//! Wissens-Overlay für Gedächtnispalast, Traumberichte und Tagebuch.
//!
//! [`KnowledgeBrowser`] implementiert [`OverlayView`] und zeigt eine Liste
//! plus Detailansicht. Alle Daten kommen strukturiert aus `OpOutput.data`
//! der jeweiligen Operation (D0: kein Rückparsen von Markdown); Details
//! (Palast, Träume) werden per [`OverlayOutcome::Fetch`] nachgeladen.
//! [`OverlayView::apply_data`] unterscheidet die Nutzlasten am Schlüssel.
//! Die TUI öffnet keinen KnowledgeStore selbst, und die Ansicht führt nie
//! selbst etwas aus: Schreibpfade gehen als Slash-Zeile an `app.rs`
//! (`Run`) oder in die Eingabezeile (`Prefill`).
//!
//! # Erwartetes JSON
//! - `/palace list` → `{"nodes":[{"id","title","status","links","updated"}]}`,
//!   `/palace show <id>` → `{"node":{…,"tags","backlinks","body"}}`,
//!   `/palace search <text>` → `{"query","truncated","hits":[{"id","title",
//!   "status","score","hop_path"}]}`,
//!   `/memory topics` → `{"topics":[{"slug","title","status","origin"}]}`
//!   (vorläufige Themen, Runde 5 Teil C)
//! - `/dream list` → `{"reports":[{"id","date","proposals","suggestions"?}]}`,
//!   `/dream show <id>` → `{"report":{"id","date","body","proposals",
//!   "suggestions"?:[{"id","kind","target","text","status"}]}}`
//! - `/diary today` bzw. `/diary show [agent] --date=…` →
//!   `{"agent","date","entries":[{"time","trigger","text"}]}`,
//!   `/diary show [agent] --from=… --to=…` →
//!   `{"agent","from","to","days":[{"date","entries":[…]}]}`,
//!   `/diary search <text>` → `{"query","truncated","hits":[{"agent","date",
//!   "time","trigger","text"}]}`, `/diary agents` → `{"agents":[…]}`
//!
//! # Bedienung
//! - Tagebuch: `←`/`→` (`h`/`l`) Tag bzw. Woche vor/zurück, `w`
//!   Bereichsansicht (7 Tage), `t` heute, `a` Agent eingeben, `Tab` nächster
//!   bekannter Agent (die Agentenliste kommt aus `/diary agents`, ergänzt um
//!   Agenten aus Antworten), `/` Suche, `o` springt vom Treffer in den Tag,
//!   `n` neue Notiz.
//! - Palast: `/` filtert die Liste live und sucht mit `Enter` über
//!   `/palace search` (Treffer mit Score und Hop-Pfad; `Esc` kehrt zur
//!   Liste zurück), `Tab` wählt einen Link/Backlink der
//!   Detailansicht, `Enter` folgt ihm, `Esc` geht den Pfad zurück. `s`
//!   markiert den Knoten zum Ersetzen, ein zweites `s` auf dem Nachfolger
//!   fragt nach und belegt dann `/palace supersede <alt> <neu> --confirm`
//!   vor; `p` belegt `/palace promote` vor (auf einem `topic/…`-Link nach
//!   Rückfrage mit dem Thema), `e` belegt `/palace edit <id> ` vor.
//!   `T` listet die vorläufigen Themen (`/memory topics`); dort belegt `p`
//!   auf dem gewählten Thema nach Rückfrage `/palace promote topic/<slug>`
//!   vor, `/` filtert lokal, `T`/`Esc` kehrt zur Knotenliste zurück.
//! - Träume: `Tab` wählt einen Vorschlag, `a` nimmt ihn nach Rückfrage an
//!   (`/dream review <id> accept <p-id>`), `r` lehnt ihn ab
//!   (`/dream review <id> reject <p-id>`), `n` belegt `/dream run` vor.
//! - Überall: `R` lädt die offene Ansicht neu (Liste oder Detail).
//!
//! # Live-Update
//! [`OverlayView::refresh_command`] liefert immer den Befehl der gerade
//! offenen Ansicht (Liste, Detail, Tag, Bereich oder Suche). Ein
//! Knowledge-Event lädt so genau diese Ansicht nach; Auswahl (per Id),
//! Scroll-Position und die Auswahl in der Detailansicht bleiben erhalten.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};
use serde_json::Value;

use crate::overlay_view::{OverlayOutcome, OverlayView};
use crate::sanitize::{sanitize_display, sanitize_inline};
use crate::style::{self, Theme};

/// Listet die Knoten des Gedächtnispalasts.
pub(crate) const PALACE_LIST_COMMAND: &str = "/palace list";
/// Lädt einen Palast-Knoten (`<cmd> <id>`).
pub(crate) const PALACE_SHOW_COMMAND: &str = "/palace show";
/// Sucht im Gedächtnispalast (`<cmd> <text>`).
pub(crate) const PALACE_SEARCH_COMMAND: &str = "/palace search";
/// Vorbelegung für eine Themen-Promotion.
pub(crate) const PALACE_PROMOTE_PREFILL: &str = "/palace promote ";
/// Listet die vorläufigen Themen (Promote-Kandidaten, Runde 5 Teil C).
pub(crate) const MEMORY_TOPICS_COMMAND: &str = "/memory topics";
/// Listet die Traumberichte.
pub(crate) const DREAM_LIST_COMMAND: &str = "/dream list";
/// Lädt einen Traumbericht (`<cmd> <id>`).
pub(crate) const DREAM_SHOW_COMMAND: &str = "/dream show";
/// Review eines Traumvorschlags (`<cmd> <id> accept|reject <p-id>`).
pub(crate) const DREAM_REVIEW_COMMAND: &str = "/dream review";
/// Vorbelegung für einen neuen Traumlauf.
pub(crate) const DREAM_RUN_PREFILL: &str = "/dream run";
/// Lädt die heutigen Tagebucheinträge.
pub(crate) const DIARY_TODAY_COMMAND: &str = "/diary today";
/// Tag bzw. Bereich eines Tagebuchs (`<cmd> [agent] --date=…|--from=… --to=…`).
pub(crate) const DIARY_SHOW_COMMAND: &str = "/diary show";
/// Volltextsuche im Tagebuch (`<cmd> <text>`).
pub(crate) const DIARY_SEARCH_COMMAND: &str = "/diary search";
/// Vorbelegung für einen neuen Tagebucheintrag.
pub(crate) const DIARY_NOTE_PREFILL: &str = "/diary note ";
/// Listet die Agenten mit Tagebuch.
pub(crate) const DIARY_AGENTS_COMMAND: &str = "/diary agents";
/// Tage der Bereichsansicht (inklusive Ankertag).
const DIARY_RANGE_DAYS: i64 = 7;
/// Seitensprung für PageUp/PageDown.
const PAGE_STEP: usize = 10;

/// Welche Wissensquelle das Overlay zeigt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnowledgeKind {
    /// Gedächtnispalast (`/palace`).
    Palace,
    /// Traumberichte (`/dream`).
    Dream,
    /// Tagebuch (`/diary`).
    Diary,
}

impl KnowledgeKind {
    /// Deutscher Titel des Overlays.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Palace => "Gedächtnispalast",
            Self::Dream => "Traumberichte",
            Self::Diary => "Tagebuch",
        }
    }

    /// Befehl, der die Ausgangsliste lädt.
    pub(crate) fn list_command(self) -> &'static str {
        match self {
            Self::Palace => PALACE_LIST_COMMAND,
            Self::Dream => DREAM_LIST_COMMAND,
            Self::Diary => DIARY_TODAY_COMMAND,
        }
    }

    /// Befehl, der ein Detail lädt (`None`: Detail kommt aus der Liste).
    pub(crate) fn show_command(self) -> Option<&'static str> {
        match self {
            Self::Palace => Some(PALACE_SHOW_COMMAND),
            Self::Dream => Some(DREAM_SHOW_COMMAND),
            Self::Diary => None,
        }
    }

    fn detail_key(self) -> Option<&'static str> {
        match self {
            Self::Palace => Some("node"),
            Self::Dream => Some("report"),
            Self::Diary => None,
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            Self::Palace => "Keine Knoten vorhanden.",
            Self::Dream => "Keine Traumberichte vorhanden. n belegt /dream run vor.",
            Self::Diary => "Keine Einträge.",
        }
    }
}

/// Ein Listeneintrag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct KnowledgeItem {
    /// Kennung für den Detailbefehl (Tagebuch: leer).
    pub id: String,
    /// Hauptzeile.
    pub title: String,
    /// Nebeninformation (Datum, Anzahl, Zeit · Auslöser …).
    pub meta: String,
    /// Status-Abzeichen (Palast: `provisional`/`established`/`superseded`).
    pub status: Option<String>,
    /// Volltext, falls ohne Nachladen verfügbar (Tagebuch).
    pub body: Option<String>,
    /// Agent eines Tagebuch-Suchtreffers.
    pub agent: Option<String>,
    /// Tag eines Tagebucheintrags bzw. Suchtreffers.
    pub date: Option<String>,
}

/// Ein Link der Palast-Detailansicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetailLink {
    /// Ziel-Id (`palace/<slug>`, `topic/<slug>`, …).
    pub target: String,
    /// `true` für einen Backlink (eingehend).
    pub backlink: bool,
}

/// Ein strukturierter Traumvorschlag (`DreamSuggestion`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SuggestionView {
    pub id: String,
    pub kind: String,
    pub target: Option<String>,
    pub text: String,
    /// `pending`, `accepted` oder `rejected`.
    pub status: String,
}

/// Geladene Detailansicht.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct KnowledgeDetail {
    /// Kennung für Nachladen und Befehle (Tagebuch: leer).
    pub id: String,
    pub title: String,
    /// Status-Abzeichen (Palast).
    pub status: Option<String>,
    /// Schlüssel/Wert-Zeilen über dem Text.
    pub fields: Vec<(String, String)>,
    /// Links und Backlinks (Palast).
    pub links: Vec<DetailLink>,
    /// Vorschläge (Träume).
    pub suggestions: Vec<SuggestionView>,
    pub body: String,
}

impl KnowledgeDetail {
    /// Anzahl der auswählbaren Einträge (Links bzw. Vorschläge).
    fn pick_count(&self) -> usize {
        self.links.len() + self.suggestions.len()
    }
}

/// Offene Rückfrage: `y` liefert `outcome`, `n`/`Esc` bricht ab.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Confirm {
    question: String,
    outcome: OverlayOutcome,
}

/// Art einer Texteingabe in der Statuszeile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    /// Tagebuch-Volltextsuche.
    Search,
    /// Tagebuch-Agent.
    Agent,
    /// Palast-Listenfilter (live).
    Filter,
}

/// Laufende Texteingabe.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Input {
    kind: InputKind,
    buffer: String,
}

/// Navigationszustand des Tagebuchs (Ziel der nächsten Abfrage).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DiaryNav {
    /// Agent (`None`: eigenes Tagebuch, bis die erste Antwort ihn nennt).
    agent: Option<String>,
    /// Ankertag (`None`: heute, bis die erste Antwort ihn nennt).
    date: Option<String>,
    /// Bereichsansicht der letzten [`DIARY_RANGE_DAYS`] Tage bis `date`.
    range: bool,
    /// Aktive Suche (hat Vorrang vor Tag/Bereich).
    query: Option<String>,
    /// Bekannte Agenten (aus `/diary agents` und Antworten, sortiert).
    agents: Vec<String>,
    /// `/diary agents` wurde bereits angefordert.
    agents_requested: bool,
    /// Beschreibung der geladenen Ansicht für den Titel.
    header: String,
    /// Suchtreffer wurden gekürzt.
    truncated: bool,
}

impl DiaryNav {
    /// Befehl, der die aktuelle Tagebuchansicht lädt.
    fn command(&self) -> String {
        if let Some(query) = &self.query {
            return format!("{DIARY_SEARCH_COMMAND} {query}");
        }
        let agent = self
            .agent
            .as_deref()
            .map(|agent| format!(" {agent}"))
            .unwrap_or_default();
        match (&self.date, self.range) {
            (Some(date), true) => {
                let from = self.range_from().unwrap_or_else(|| date.clone());
                format!("{DIARY_SHOW_COMMAND}{agent} --from={from} --to={date}")
            }
            (Some(date), false) => format!("{DIARY_SHOW_COMMAND}{agent} --date={date}"),
            (None, _) if self.agent.is_some() => format!("{DIARY_SHOW_COMMAND}{agent}"),
            (None, _) => DIARY_TODAY_COMMAND.to_owned(),
        }
    }

    /// Erster Tag der Bereichsansicht.
    fn range_from(&self) -> Option<String> {
        self.date
            .as_deref()
            .and_then(|date| shift_date(date, 1 - DIARY_RANGE_DAYS))
    }

    /// Merkt einen Agenten für `Tab`/Vervollständigung.
    fn remember_agent(&mut self, agent: &str) {
        if is_token(agent) && !self.agents.iter().any(|known| known == agent) {
            self.agents.push(agent.to_owned());
            self.agents.sort();
        }
    }

    /// Nächster bekannter Agent nach `current` (zyklisch).
    fn next_agent(&self, current: Option<&str>) -> Option<String> {
        if self.agents.is_empty() {
            return None;
        }
        let next = match current.and_then(|agent| self.agents.iter().position(|a| a == agent)) {
            Some(index) => (index + 1) % self.agents.len(),
            None => 0,
        };
        self.agents.get(next).cloned()
    }
}

/// Wissens-Overlay.
#[derive(Debug, Clone)]
pub(crate) struct KnowledgeBrowser {
    kind: KnowledgeKind,
    items: Vec<KnowledgeItem>,
    /// Auswahl innerhalb der sichtbaren (gefilterten) Liste.
    selected: usize,
    detail: Option<KnowledgeDetail>,
    detail_pending: bool,
    /// Auswahl in der Detailansicht (Link bzw. Vorschlag).
    pick: usize,
    scroll: usize,
    loaded: bool,
    error: Option<String>,
    /// Kurzer Hinweis in der Statuszeile (verfällt mit der nächsten Taste).
    notice: Option<String>,
    confirm: Option<Confirm>,
    input: Option<Input>,
    /// Palast-Listenfilter (Id/Titel/Status, ohne Groß-/Kleinschreibung).
    filter: Option<String>,
    /// Palast: aktive Serversuche (`/palace search`), Vorrang vor der Liste.
    palace_query: Option<String>,
    /// Palast: Suchtreffer wurden gekürzt.
    palace_truncated: bool,
    /// Palast: die nächsten Treffer gehören zu einer frisch gestarteten
    /// Suche — die Auswahl springt dann auf den besten Treffer, statt den
    /// zuvor markierten Listeneintrag zu behalten.
    palace_hits_fresh: bool,
    /// Palast: zum Ersetzen markierter Knoten.
    supersede_from: Option<String>,
    /// Palast: Themenmodus (`T`) — die Liste zeigt die vorläufigen Themen
    /// aus `/memory topics` statt der Knoten (Runde 5, Teil C).
    palace_topics: bool,
    /// Palast: zuvor besuchte Knoten (Esc geht zurück).
    trail: Vec<String>,
    /// Palast: das nächste Detail kommt aus einem gefolgten Link.
    following: bool,
    /// Liste nach Rückkehr aus dem Detail neu laden.
    list_stale: bool,
    diary: DiaryNav,
}

impl KnowledgeBrowser {
    /// Leeres Overlay für `kind`; die Liste folgt über `refresh_command`.
    pub(crate) fn new(kind: KnowledgeKind) -> Self {
        Self {
            kind,
            items: Vec::new(),
            selected: 0,
            detail: None,
            detail_pending: false,
            pick: 0,
            scroll: 0,
            loaded: false,
            error: None,
            notice: None,
            confirm: None,
            input: None,
            filter: None,
            palace_query: None,
            palace_truncated: false,
            palace_hits_fresh: false,
            supersede_from: None,
            palace_topics: false,
            trail: Vec::new(),
            following: false,
            list_stale: false,
            diary: DiaryNav::default(),
        }
    }

    /// Angezeigte Quelle.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn kind(&self) -> KnowledgeKind {
        self.kind
    }

    /// Listeneinträge (für Tests).
    #[cfg(test)]
    pub(crate) fn items(&self) -> &[KnowledgeItem] {
        &self.items
    }

    /// Geöffnetes Detail (für Tests).
    #[cfg(test)]
    pub(crate) fn detail(&self) -> Option<&KnowledgeDetail> {
        self.detail.as_ref()
    }

    // ── Liste ───────────────────────────────────────────────────────────

    /// Indizes der sichtbaren Einträge (Palast-Filter angewandt).
    fn visible(&self) -> Vec<usize> {
        let needle = self
            .filter
            .as_deref()
            .map(str::to_lowercase)
            .filter(|needle| !needle.trim().is_empty());
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                needle.as_deref().is_none_or(|needle| {
                    [
                        item.id.as_str(),
                        item.title.as_str(),
                        item.status.as_deref().unwrap_or_default(),
                    ]
                    .iter()
                    .any(|text| text.to_lowercase().contains(needle))
                })
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_item(&self) -> Option<&KnowledgeItem> {
        self.visible()
            .get(self.selected)
            .and_then(|index| self.items.get(*index))
    }

    fn clamp_selection(&mut self) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    /// Übernimmt eine neue Liste und hält die Auswahl per Id fest.
    fn replace_items(&mut self, items: Vec<KnowledgeItem>) {
        let previous = self
            .selected_item()
            .map(|item| item.id.clone())
            .filter(|id| !id.is_empty());
        self.items = items;
        self.loaded = true;
        self.error = None;
        if let Some(id) = previous
            && let Some(position) = self
                .visible()
                .iter()
                .position(|index| self.items.get(*index).is_some_and(|item| item.id == id))
        {
            self.selected = position;
        }
        self.clamp_selection();
    }

    fn parse_palace_item(value: &Value) -> Option<KnowledgeItem> {
        let id = str_field(value, "id");
        if id.is_empty() {
            return None;
        }
        let mut meta = Vec::new();
        let links = list_len(value.get("links"));
        if links > 0 {
            meta.push(format!("{links} Links"));
        }
        if let Some(updated) = opt_str_field(value, "updated") {
            meta.push(updated);
        }
        Some(KnowledgeItem {
            title: opt_str_field(value, "title").unwrap_or_else(|| id.clone()),
            id,
            meta: meta.join(" · "),
            status: opt_str_field(value, "status"),
            ..KnowledgeItem::default()
        })
    }

    /// Ein Treffer von `/palace search` (`{"id","title","status","score","hop_path"}`).
    fn parse_palace_hit(value: &Value) -> Option<KnowledgeItem> {
        let id = str_field(value, "id");
        if id.is_empty() {
            return None;
        }
        let mut meta = Vec::new();
        if let Some(score) = value.get("score").and_then(Value::as_f64) {
            meta.push(format!("Score {score:.2}"));
        }
        let path = list_texts(value.get("hop_path"));
        if !path.is_empty() {
            meta.push(format!("via {}", path.join(" → ")));
        }
        Some(KnowledgeItem {
            title: opt_str_field(value, "title").unwrap_or_else(|| id.clone()),
            id,
            meta: meta.join(" · "),
            status: opt_str_field(value, "status"),
            ..KnowledgeItem::default()
        })
    }

    /// Ein vorläufiges Thema aus `/memory topics`
    /// (`{"slug","title","status","origin"}`); Id ist `topic/<slug>`. Der
    /// Volltext (Titel, Herkunft) entsteht lokal für die Detailansicht.
    fn parse_topic_item(value: &Value) -> Option<KnowledgeItem> {
        let slug = str_field(value, "slug");
        if !is_token(&slug) || slug.contains('/') {
            return None;
        }
        let id = format!("topic/{slug}");
        let title = opt_str_field(value, "title").unwrap_or_else(|| slug.clone());
        let origin = value.get("origin").filter(|origin| origin.is_object());
        let meta = origin
            .map(|origin| {
                let kind = str_field(origin, "kind");
                let source = str_field(origin, "id");
                match (kind.is_empty(), source.is_empty()) {
                    (false, false) => format!("aus {kind} {source}"),
                    (false, true) => format!("aus {kind}"),
                    (true, false) => format!("aus {source}"),
                    (true, true) => String::new(),
                }
            })
            .unwrap_or_default();
        let mut body = format!("{title}\n\nId: {id}");
        if let Some(origin) = origin {
            let detail = str_field(origin, "detail");
            let at = str_field(origin, "at");
            if !meta.is_empty() {
                body.push_str(&format!("\nHerkunft: {meta}"));
            }
            if !detail.is_empty() {
                body.push_str(&format!("\nBereich: {detail}"));
            }
            if !at.is_empty() {
                body.push_str(&format!("\nErfasst: {at}"));
            }
        }
        body.push_str("\n\np übernimmt das Thema nach Rückfrage in den Palast.");
        Some(KnowledgeItem {
            id,
            title,
            meta,
            status: Some(
                opt_str_field(value, "status").unwrap_or_else(|| "provisional".to_owned()),
            ),
            body: Some(body),
            ..KnowledgeItem::default()
        })
    }

    /// Übernimmt Treffer von `/palace search`, sofern sie zur aktiven Suche
    /// passen (verspätete Antworten werden verworfen).
    fn apply_palace_hits(&mut self, data: &Value) {
        let Some(query) = self.palace_query.clone() else {
            return;
        };
        if normalize(&str_field(data, "query")) != query {
            return;
        }
        let items = array_field(data, "hits")
            .iter()
            .filter(|value| value.is_object())
            .filter_map(Self::parse_palace_hit)
            .collect();
        self.palace_truncated = data.get("truncated").and_then(Value::as_bool) == Some(true);
        self.replace_items(items);
        // Neue Suche: Auswahl auf den besten Treffer; ein Nachladen derselben
        // Suche behält die Auswahl (siehe `replace_items`).
        if std::mem::take(&mut self.palace_hits_fresh) {
            self.selected = 0;
            self.clamp_selection();
        }
    }

    fn parse_dream_item(value: &Value) -> Option<KnowledgeItem> {
        let id = str_field(value, "id");
        if id.is_empty() {
            return None;
        }
        let suggestions = parse_suggestions(value.get("suggestions"));
        let meta = if value.get("suggestions").is_some_and(Value::is_array) {
            let open = suggestions
                .iter()
                .filter(|suggestion| suggestion.status == "pending")
                .count();
            format!("{} Vorschläge · {open} offen", suggestions.len())
        } else {
            format!("{} Vorschläge", list_len(value.get("proposals")))
        };
        Some(KnowledgeItem {
            title: opt_str_field(value, "date").unwrap_or_else(|| id.clone()),
            id,
            meta,
            ..KnowledgeItem::default()
        })
    }

    /// Ein Tagebucheintrag (`{"time","trigger","text"}`), optional mit Tag/Agent.
    fn diary_item(value: &Value, date: Option<&str>, agent: Option<&str>) -> Option<KnowledgeItem> {
        if !value.is_object() {
            return None;
        }
        let text = str_field(value, "text");
        let date = opt_str_field(value, "date").or_else(|| date.map(str::to_owned));
        let agent = opt_str_field(value, "agent").or_else(|| agent.map(str::to_owned));
        let mut meta = Vec::new();
        if let Some(agent) = &agent {
            meta.push(agent.clone());
        }
        let stamp = [date.clone(), opt_str_field(value, "time")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        if !stamp.is_empty() {
            meta.push(stamp);
        }
        if let Some(trigger) = opt_str_field(value, "trigger") {
            meta.push(trigger);
        }
        Some(KnowledgeItem {
            id: String::new(),
            title: text.lines().next().unwrap_or_default().to_owned(),
            meta: meta.join(" · "),
            status: None,
            body: Some(text),
            agent,
            date,
        })
    }

    fn apply_list(&mut self, data: &Value) {
        // Runde 5, Teil C: Themenmodus. Themen gelten nur im Themenmodus,
        // verspätete Knoten-/Trefferlisten überschreiben ihn nicht.
        if self.kind == KnowledgeKind::Palace {
            if let Some(topics) = data.get("topics").and_then(Value::as_array) {
                if self.palace_topics {
                    let items = topics
                        .iter()
                        .filter(|value| value.is_object())
                        .filter_map(Self::parse_topic_item)
                        .collect();
                    self.replace_items(items);
                }
                return;
            }
            if self.palace_topics {
                return;
            }
        }
        if self.kind == KnowledgeKind::Palace && data.get("hits").is_some_and(Value::is_array) {
            return self.apply_palace_hits(data);
        }
        if self.kind == KnowledgeKind::Palace && self.palace_query.is_some() {
            // Eine späte Listenantwort überschreibt die Suche nicht.
            return;
        }
        let key = match self.kind {
            KnowledgeKind::Palace => "nodes",
            KnowledgeKind::Dream => "reports",
            KnowledgeKind::Diary => return self.apply_diary(data),
        };
        let items = array_field(data, key)
            .iter()
            .filter(|value| value.is_object())
            .filter_map(|value| match self.kind {
                KnowledgeKind::Palace => Self::parse_palace_item(value),
                _ => Self::parse_dream_item(value),
            })
            .collect();
        self.replace_items(items);
    }

    /// Übernimmt eine Tagebuch-Nutzlast, sofern sie zur Zielansicht passt
    /// (verspätete Antworten einer verlassenen Ansicht werden verworfen).
    fn apply_diary(&mut self, data: &Value) {
        if let Some(agents) = data.get("agents").and_then(Value::as_array)
            && data.get("entries").is_none()
            && data.get("days").is_none()
            && data.get("hits").is_none()
        {
            for agent in agents.iter().filter_map(Value::as_str) {
                self.diary.remember_agent(agent);
            }
            self.notice = Some(if self.diary.agents.is_empty() {
                "Keine Tagebücher vorhanden.".to_owned()
            } else {
                format!(
                    "{} Agenten: {} (Tab wählt)",
                    self.diary.agents.len(),
                    self.diary.agents.join(", ")
                )
            });
            return;
        }
        if let Some(hits) = data.get("hits").and_then(Value::as_array) {
            let Some(query) = self.diary.query.clone() else {
                return;
            };
            if normalize(&str_field(data, "query")) != query {
                return;
            }
            let items: Vec<KnowledgeItem> = hits
                .iter()
                .filter_map(|hit| Self::diary_item(hit, None, None))
                .collect();
            for item in &items {
                if let Some(agent) = &item.agent {
                    self.diary.remember_agent(agent);
                }
            }
            self.diary.truncated = data.get("truncated").and_then(Value::as_bool) == Some(true);
            self.diary.header = format!("Suche „{query}“");
            return self.replace_items(items);
        }
        let agent = opt_str_field(data, "agent");
        if let Some(agent) = &agent {
            self.diary.remember_agent(agent);
        }
        if self.diary.query.is_some() {
            return;
        }
        if let Some(agent) = &agent
            && self
                .diary
                .agent
                .as_ref()
                .is_some_and(|wanted| wanted != agent)
        {
            return;
        }
        if let Some(days) = data.get("days").and_then(Value::as_array) {
            let (from, to) = (str_field(data, "from"), str_field(data, "to"));
            if !self.diary.range
                || self.diary.date.as_deref() != Some(to.as_str())
                || self.diary.range_from().as_deref() != Some(from.as_str())
            {
                return;
            }
            let mut items = Vec::new();
            for day in days {
                let date = opt_str_field(day, "date");
                items.extend(
                    array_field(day, "entries")
                        .iter()
                        .filter_map(|entry| Self::diary_item(entry, date.as_deref(), None)),
                );
            }
            self.diary.agent = agent.clone();
            self.diary.header =
                format!("{} · {from} – {to}", agent.as_deref().unwrap_or("eigenes"));
            return self.replace_items(items);
        }
        if data.get("entries").is_some() {
            let date = opt_str_field(data, "date");
            if self.diary.range {
                return;
            }
            if let (Some(wanted), Some(date)) = (&self.diary.date, &date)
                && wanted != date
            {
                return;
            }
            if self.diary.date.is_none() {
                self.diary.date = date.clone();
            }
            if self.diary.agent.is_none() {
                self.diary.agent = agent.clone();
            }
            let items = array_field(data, "entries")
                .iter()
                .filter_map(|entry| Self::diary_item(entry, None, None))
                .collect();
            self.diary.header = [agent, date]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            return self.replace_items(items);
        }
        // Unbekannte Nutzlast: leere Liste statt „Lade …“.
        self.replace_items(Vec::new());
    }

    // ── Detail ──────────────────────────────────────────────────────────

    fn parse_detail(&self, value: &Value) -> KnowledgeDetail {
        let mut detail = KnowledgeDetail {
            id: str_field(value, "id"),
            body: str_field(value, "body"),
            ..KnowledgeDetail::default()
        };
        match self.kind {
            KnowledgeKind::Palace => {
                detail.status = opt_str_field(value, "status");
                for (key, label) in [("id", "Kennung"), ("updated", "Aktualisiert")] {
                    if let Some(text) = opt_str_field(value, key) {
                        detail.fields.push((label.to_owned(), text));
                    }
                }
                let tags = list_texts(value.get("tags"));
                if !tags.is_empty() {
                    detail.fields.push(("Tags".to_owned(), tags.join(", ")));
                }
                detail.links = list_texts(value.get("links"))
                    .into_iter()
                    .map(|target| DetailLink {
                        target,
                        backlink: false,
                    })
                    .chain(
                        list_texts(value.get("backlinks"))
                            .into_iter()
                            .map(|target| DetailLink {
                                target,
                                backlink: true,
                            }),
                    )
                    .collect();
                detail.title = opt_str_field(value, "title").unwrap_or_else(|| detail.id.clone());
            }
            KnowledgeKind::Dream => {
                if let Some(id) = opt_str_field(value, "id") {
                    detail.fields.push(("Kennung".to_owned(), id));
                }
                detail.suggestions = parse_suggestions(value.get("suggestions"));
                let legacy = list_len(value.get("proposals"));
                if detail.suggestions.is_empty() && legacy > 0 {
                    detail
                        .fields
                        .push(("Vorschläge (ohne Review)".to_owned(), legacy.to_string()));
                }
                let date = str_field(value, "date");
                detail.title = if date.is_empty() {
                    "Traumbericht".to_owned()
                } else {
                    format!("Traumbericht {date}")
                };
            }
            KnowledgeKind::Diary => detail.title = str_field(value, "time"),
        }
        detail
    }

    fn apply_detail(&mut self, value: &Value) {
        if self.detail.is_none() && !self.detail_pending {
            // Verspätete Antwort eines bereits verlassenen Details.
            return;
        }
        let detail = self.parse_detail(value);
        let same = self
            .detail
            .as_ref()
            .is_some_and(|current| current.id == detail.id);
        if same {
            // Nachladen derselben Ansicht: Scroll und Auswahl bleiben.
            self.list_stale = true;
            self.pick = self.pick.min(detail.pick_count().saturating_sub(1));
        } else {
            if self.following
                && let Some(current) = &self.detail
            {
                self.trail.push(current.id.clone());
            }
            self.pick = 0;
            self.scroll = 0;
        }
        self.following = false;
        self.detail = Some(detail);
        self.detail_pending = false;
        self.error = None;
    }

    fn open_selected(&mut self) -> OverlayOutcome {
        let Some(item) = self.selected_item().cloned() else {
            return OverlayOutcome::Stay;
        };
        self.scroll = 0;
        // Runde 5, Teil C: ein Thema hat (noch) keinen Palast-Knoten — das
        // Detail entsteht lokal.
        let show_command = self.kind.show_command().filter(|_| !self.palace_topics);
        match (show_command, item.body) {
            (Some(command), _) if is_token(&item.id) => {
                self.detail_pending = true;
                self.trail.clear();
                OverlayOutcome::Fetch(format!("{command} {}", item.id))
            }
            (_, Some(body)) => {
                self.pick = 0;
                self.detail = Some(KnowledgeDetail {
                    // Runde 5, Teil C: ein Thema zeigt seinen Titel.
                    title: if self.palace_topics {
                        item.title
                    } else {
                        item.meta
                    },
                    status: item.status.filter(|_| self.palace_topics),
                    body,
                    ..KnowledgeDetail::default()
                });
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
    }

    /// Schließt das Detail bzw. geht im Palast-Pfad einen Schritt zurück.
    fn leave_detail(&mut self) -> OverlayOutcome {
        if self.detail_pending && self.detail.is_none() {
            self.detail_pending = false;
            return OverlayOutcome::Stay;
        }
        self.detail_pending = false;
        self.following = false;
        if let Some(previous) = self.trail.pop()
            && let Some(command) = self.kind.show_command()
        {
            self.detail_pending = true;
            self.pick = 0;
            self.scroll = 0;
            // Ohne `following`: der Rückweg wird nicht erneut vermerkt.
            return OverlayOutcome::Fetch(format!("{command} {previous}"));
        }
        self.detail = None;
        self.scroll = 0;
        self.pick = 0;
        if std::mem::take(&mut self.list_stale) {
            return OverlayOutcome::Fetch(self.list_refresh());
        }
        OverlayOutcome::Stay
    }

    /// Befehl der Listenansicht (Tagebuch: aktuelle Navigation).
    fn list_refresh(&self) -> String {
        match self.kind {
            KnowledgeKind::Diary => self.diary.command(),
            KnowledgeKind::Palace if self.palace_topics => MEMORY_TOPICS_COMMAND.to_owned(),
            KnowledgeKind::Palace => match &self.palace_query {
                Some(query) => format!("{PALACE_SEARCH_COMMAND} {query}"),
                None => PALACE_LIST_COMMAND.to_owned(),
            },
            kind => kind.list_command().to_owned(),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.detail.is_some() {
            self.scroll = self.scroll.saturating_add_signed(delta);
            return;
        }
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.saturating_add_signed(delta).min(len - 1);
    }

    fn move_pick(&mut self, forward: bool) {
        let count = self.detail.as_ref().map_or(0, KnowledgeDetail::pick_count);
        if count == 0 {
            return;
        }
        self.pick = if forward {
            (self.pick + 1) % count
        } else {
            (self.pick + count - 1) % count
        };
    }

    // ── Palast-Aktionen ─────────────────────────────────────────────────

    /// Id des Knotens, auf den sich eine Aktion bezieht (Detail oder Auswahl).
    fn current_node(&self) -> Option<String> {
        match &self.detail {
            Some(detail) => Some(detail.id.clone()),
            None => self.selected_item().map(|item| item.id.clone()),
        }
        .filter(|id| is_token(id))
    }

    fn selected_link(&self) -> Option<&DetailLink> {
        self.detail
            .as_ref()
            .and_then(|detail| detail.links.get(self.pick))
    }

    fn follow_link(&mut self) -> OverlayOutcome {
        let Some(link) = self.selected_link().cloned() else {
            return OverlayOutcome::Stay;
        };
        if !is_palace_ref(&link.target) {
            self.notice = Some(format!(
                "{} ist kein Palast-Knoten",
                sanitize_inline(&link.target)
            ));
            return OverlayOutcome::Stay;
        }
        self.following = true;
        self.detail_pending = true;
        OverlayOutcome::Fetch(format!("{PALACE_SHOW_COMMAND} {}", link.target))
    }

    fn supersede(&mut self) -> OverlayOutcome {
        let Some(node) = self.current_node() else {
            return OverlayOutcome::Stay;
        };
        match self.supersede_from.take() {
            Some(old) if old == node => {
                self.notice = Some("Markierung aufgehoben.".to_owned());
            }
            Some(old) => {
                self.confirm = Some(Confirm {
                    question: format!("{old} durch {node} ersetzen (superseded)?"),
                    outcome: OverlayOutcome::Prefill(format!(
                        "/palace supersede {old} {node} --confirm"
                    )),
                });
            }
            None => {
                self.notice = Some(format!(
                    "{node} zum Ersetzen markiert – Nachfolger wählen, dann s"
                ));
                self.supersede_from = Some(node);
            }
        }
        OverlayOutcome::Stay
    }

    fn promote(&mut self) -> OverlayOutcome {
        // Runde 5, Teil C: im Themenmodus das gewählte vorläufige Thema.
        if self.palace_topics {
            let Some(topic) = self
                .selected_item()
                .map(|item| item.id.clone())
                .filter(|id| id.strip_prefix("topic/").is_some_and(is_token))
            else {
                self.notice = Some("Kein Thema gewählt.".to_owned());
                return OverlayOutcome::Stay;
            };
            self.confirm = Some(Confirm {
                question: format!("Thema {topic} in den Palast promoten (established)?"),
                outcome: OverlayOutcome::Prefill(format!("{PALACE_PROMOTE_PREFILL}{topic}")),
            });
            return OverlayOutcome::Stay;
        }
        if let Some(link) = self.selected_link()
            && let Some(slug) = link.target.strip_prefix("topic/")
            && is_token(slug)
        {
            let topic = link.target.clone();
            self.confirm = Some(Confirm {
                question: format!("Thema {topic} in den Palast promoten (established)?"),
                outcome: OverlayOutcome::Prefill(format!("{PALACE_PROMOTE_PREFILL}{topic}")),
            });
            return OverlayOutcome::Stay;
        }
        OverlayOutcome::Prefill(PALACE_PROMOTE_PREFILL.to_owned())
    }

    /// Wechselt zwischen Knotenliste und vorläufigen Themen (`T`, Runde 5
    /// Teil C) und lädt die neue Liste.
    fn toggle_topics(&mut self) -> OverlayOutcome {
        self.palace_topics = !self.palace_topics;
        self.palace_query = None;
        self.palace_truncated = false;
        self.palace_hits_fresh = false;
        self.supersede_from = None;
        self.filter = None;
        self.items.clear();
        self.loaded = false;
        self.selected = 0;
        OverlayOutcome::Fetch(self.list_refresh())
    }

    // ── Traum-Aktionen ──────────────────────────────────────────────────

    fn review(&mut self, accept: bool) -> OverlayOutcome {
        let Some(detail) = &self.detail else {
            return OverlayOutcome::Stay;
        };
        let Some(suggestion) = detail.suggestions.get(self.pick) else {
            self.notice = Some("Kein Vorschlag gewählt (Tab).".to_owned());
            return OverlayOutcome::Stay;
        };
        if !is_token(&detail.id) || !is_token(&suggestion.id) {
            return OverlayOutcome::Stay;
        }
        if suggestion.status != "pending" {
            self.notice = Some(format!(
                "{} ist bereits entschieden ({}).",
                sanitize_inline(&suggestion.id),
                sanitize_inline(&suggestion.status)
            ));
            return OverlayOutcome::Stay;
        }
        let verb = if accept { "accept" } else { "reject" };
        let command = format!(
            "{DREAM_REVIEW_COMMAND} {} {verb} {}",
            detail.id, suggestion.id
        );
        self.list_stale = true;
        if accept {
            self.confirm = Some(Confirm {
                question: format!(
                    "Vorschlag {} annehmen (führt den Schreibpfad aus)?",
                    sanitize_inline(&suggestion.id)
                ),
                outcome: OverlayOutcome::Run(command),
            });
            return OverlayOutcome::Stay;
        }
        OverlayOutcome::Run(command)
    }

    // ── Tagebuch-Navigation ─────────────────────────────────────────────

    /// Lädt die Tagebuchansicht nach einer Navigationsänderung.
    fn diary_reload(&mut self) -> OverlayOutcome {
        self.selected = 0;
        self.detail = None;
        self.scroll = 0;
        OverlayOutcome::Fetch(self.diary.command())
    }

    fn diary_shift(&mut self, forward: bool) -> OverlayOutcome {
        let Some(date) = self.diary.date.clone() else {
            self.notice = Some("Datum noch nicht geladen.".to_owned());
            return OverlayOutcome::Stay;
        };
        let step = if self.diary.range {
            DIARY_RANGE_DAYS
        } else {
            1
        };
        let Some(next) = shift_date(&date, if forward { step } else { -step }) else {
            return OverlayOutcome::Stay;
        };
        self.diary.query = None;
        self.diary.date = Some(next);
        self.diary_reload()
    }

    /// Fordert `/diary agents` einmal je Overlay an.
    fn request_diary_agents(&mut self) -> OverlayOutcome {
        if self.diary.agents_requested {
            return OverlayOutcome::Stay;
        }
        self.diary.agents_requested = true;
        OverlayOutcome::Fetch(DIARY_AGENTS_COMMAND.to_owned())
    }

    fn diary_key(&mut self, key: KeyEvent) -> Option<OverlayOutcome> {
        let outcome = match key.code {
            KeyCode::Left | KeyCode::Char('h') => self.diary_shift(false),
            KeyCode::Right | KeyCode::Char('l') => self.diary_shift(true),
            KeyCode::Char('w') => {
                if self.diary.date.is_none() {
                    self.notice = Some("Datum noch nicht geladen.".to_owned());
                    return Some(OverlayOutcome::Stay);
                }
                self.diary.query = None;
                self.diary.range = !self.diary.range;
                self.diary_reload()
            }
            KeyCode::Char('t') => {
                self.diary.query = None;
                self.diary.range = false;
                self.diary.date = None;
                self.diary_reload()
            }
            KeyCode::Char('/') => {
                self.input = Some(Input {
                    kind: InputKind::Search,
                    buffer: self.diary.query.clone().unwrap_or_default(),
                });
                OverlayOutcome::Stay
            }
            KeyCode::Char('a') => {
                self.input = Some(Input {
                    kind: InputKind::Agent,
                    buffer: self.diary.agent.clone().unwrap_or_default(),
                });
                // Die Vervollständigung braucht die echte Agentenliste.
                self.request_diary_agents()
            }
            KeyCode::Tab => match self.diary.next_agent(self.diary.agent.as_deref()) {
                Some(agent) => {
                    self.diary.query = None;
                    self.diary.agent = Some(agent);
                    self.diary_reload()
                }
                None if !self.diary.agents_requested => {
                    self.notice = Some("Lade Agenten …".to_owned());
                    self.request_diary_agents()
                }
                None => {
                    self.notice = Some("Keine weiteren Agenten bekannt (a: eingeben).".to_owned());
                    OverlayOutcome::Stay
                }
            },
            KeyCode::Char('o') => {
                let Some(item) = self.selected_item().cloned() else {
                    return Some(OverlayOutcome::Stay);
                };
                let Some(date) = item.date.filter(|date| is_token(date)) else {
                    return Some(OverlayOutcome::Stay);
                };
                if let Some(agent) = item.agent.filter(|agent| is_token(agent)) {
                    self.diary.agent = Some(agent);
                }
                self.diary.query = None;
                self.diary.range = false;
                self.diary.date = Some(date);
                self.diary_reload()
            }
            KeyCode::Char('n') => OverlayOutcome::Prefill(DIARY_NOTE_PREFILL.to_owned()),
            KeyCode::Esc | KeyCode::Char('q') if self.diary.query.is_some() => {
                self.diary.query = None;
                self.diary_reload()
            }
            _ => return None,
        };
        Some(outcome)
    }

    // ── Eingabe und Rückfrage ───────────────────────────────────────────

    fn input_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        let Some(mut input) = self.input.take() else {
            return OverlayOutcome::Stay;
        };
        match key.code {
            KeyCode::Esc => {
                if input.kind == InputKind::Filter {
                    self.filter = None;
                    self.clamp_selection();
                }
                return OverlayOutcome::Stay;
            }
            KeyCode::Enter => return self.submit_input(input),
            KeyCode::Backspace => {
                input.buffer.pop();
            }
            KeyCode::Tab if input.kind == InputKind::Agent => {
                let current = Some(input.buffer.as_str()).filter(|text| !text.is_empty());
                if let Some(agent) = self.diary.next_agent(current) {
                    input.buffer = agent;
                }
            }
            KeyCode::Char(c) if !c.is_control() => input.buffer.push(c),
            _ => {}
        }
        if input.kind == InputKind::Filter {
            self.filter = Some(input.buffer.clone());
            self.selected = 0;
        }
        self.input = Some(input);
        OverlayOutcome::Stay
    }

    fn submit_input(&mut self, input: Input) -> OverlayOutcome {
        let text = normalize(&input.buffer);
        match input.kind {
            InputKind::Filter if self.palace_topics => {
                // Runde 5, Teil C: im Themenmodus bleibt der Filter lokal.
                self.filter = Some(text).filter(|text| !text.is_empty());
                self.clamp_selection();
                OverlayOutcome::Stay
            }
            InputKind::Filter => {
                // Enter sucht über `/palace search`; der Live-Filter war nur
                // die Vorschau über die geladene Liste.
                self.filter = None;
                self.selected = 0;
                if text.is_empty() {
                    self.clamp_selection();
                    return match self.palace_query.take() {
                        Some(_) => OverlayOutcome::Fetch(PALACE_LIST_COMMAND.to_owned()),
                        None => OverlayOutcome::Stay,
                    };
                }
                self.palace_query = Some(text);
                self.palace_truncated = false;
                self.palace_hits_fresh = true;
                OverlayOutcome::Fetch(self.list_refresh())
            }
            InputKind::Search => {
                if text.is_empty() {
                    return OverlayOutcome::Stay;
                }
                self.diary.query = Some(text);
                self.diary_reload()
            }
            InputKind::Agent => {
                if !text.is_empty() && !is_token(&text) {
                    self.notice = Some("Agent-Id ohne Leerzeichen angeben.".to_owned());
                    return OverlayOutcome::Stay;
                }
                self.diary.query = None;
                self.diary.agent = Some(text).filter(|text| !text.is_empty());
                self.diary_reload()
            }
        }
    }

    fn confirm_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        match key.code {
            KeyCode::Char('y' | 'Y' | 'j' | 'J') | KeyCode::Enter => self
                .confirm
                .take()
                .map_or(OverlayOutcome::Stay, |confirm| confirm.outcome),
            KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => {
                self.confirm = None;
                self.notice = Some("Abgebrochen.".to_owned());
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
    }

    // ── Zeichnen ────────────────────────────────────────────────────────

    fn title_text(&self) -> String {
        let mut title = format!(" {}", self.kind.title());
        if self.kind == KnowledgeKind::Diary && !self.diary.header.is_empty() {
            title.push_str(&format!(" · {}", sanitize_inline(&self.diary.header)));
        }
        if self.kind == KnowledgeKind::Palace && self.detail.is_none() && self.palace_topics {
            title.push_str(" · vorläufige Themen");
        }
        if self.kind == KnowledgeKind::Palace
            && self.detail.is_none()
            && let Some(query) = &self.palace_query
        {
            title.push_str(&format!(" · Suche „{}“", sanitize_inline(query)));
        }
        if self.loaded && self.detail.is_none() {
            let visible = self.visible().len();
            if visible == self.items.len() {
                title.push_str(&format!(" ({visible})"));
            } else {
                title.push_str(&format!(" ({visible}/{})", self.items.len()));
            }
            if (self.kind == KnowledgeKind::Diary
                && self.diary.query.is_some()
                && self.diary.truncated)
                || (self.kind == KnowledgeKind::Palace
                    && self.palace_query.is_some()
                    && self.palace_truncated)
            {
                title.push_str(" gekürzt");
            }
        }
        title.push(' ');
        title
    }

    fn hint(&self) -> &'static str {
        if self.confirm.is_some() {
            return "y bestätigen · n abbrechen";
        }
        if let Some(input) = &self.input {
            return match input.kind {
                InputKind::Agent => "Enter übernehmen · Tab bekannter Agent · Esc abbrechen",
                InputKind::Filter if self.palace_topics => "Enter filtern · Esc Filter löschen",
                InputKind::Filter => "Enter /palace search · Esc Filter löschen",
                InputKind::Search => "Enter suchen · Esc abbrechen",
            };
        }
        match (self.kind, self.detail.is_some()) {
            (KnowledgeKind::Palace, true) if self.palace_topics => {
                "p promoten · j/k scrollen · Esc zurück"
            }
            (KnowledgeKind::Palace, false) if self.palace_topics => {
                "j/k · Enter anzeigen · p promoten · / filtern · T/Esc Knoten · R neu"
            }
            (KnowledgeKind::Palace, true) => {
                "Tab Link · Enter folgen · s ersetzen · p promoten · e bearbeiten · Esc zurück"
            }
            (KnowledgeKind::Palace, false) => {
                "j/k · Enter öffnen · / suchen · T Themen · s ersetzen · p promoten · R neu · Esc"
            }
            (KnowledgeKind::Dream, true) => {
                "Tab Vorschlag · a annehmen · r ablehnen · j/k scrollen · R neu · Esc zurück"
            }
            (KnowledgeKind::Dream, false) => {
                "j/k wählen · Enter öffnen · n /dream run · R neu laden · Esc schließen"
            }
            (KnowledgeKind::Diary, true) => "j/k scrollen · Esc zurück",
            (KnowledgeKind::Diary, false) if self.diary.query.is_some() => {
                "j/k · Enter anzeigen · o Tag öffnen · / neue Suche · Esc zurück"
            }
            (KnowledgeKind::Diary, false) => {
                "j/k · Enter · ←/→ Tag · w Woche · t heute · / Suche · a/Tab Agent · n Notiz"
            }
        }
    }

    fn render_status(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let dim = style::dim_style(theme);
        let line = if let Some(confirm) = &self.confirm {
            Line::styled(
                format!("{} (y/n)", sanitize_inline(&confirm.question)),
                style::warning_style(theme),
            )
        } else if let Some(input) = &self.input {
            let label = match input.kind {
                InputKind::Search => "Suche",
                InputKind::Agent => "Agent",
                InputKind::Filter => "Filter",
            };
            Line::from(vec![
                Span::styled(format!("{label}: "), dim),
                Span::raw(sanitize_inline(&input.buffer)),
                Span::styled("▏", dim),
            ])
        } else if let Some(error) = &self.error {
            Line::styled(
                format!("Fehler: {}", sanitize_inline(error)),
                style::error_style(theme),
            )
        } else if self.detail_pending {
            Line::styled("Lade Details …", dim)
        } else if let Some(notice) = &self.notice {
            Line::styled(sanitize_inline(notice), dim)
        } else if let Some(filter) = &self.filter {
            Line::styled(format!("Filter: {}", sanitize_inline(filter)), dim)
        } else {
            return;
        };
        line.render(area, buf);
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let dim = style::dim_style(theme);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let visible = self.visible();
        if visible.is_empty() {
            let text = if self.items.is_empty() && self.palace_topics {
                "Keine vorläufigen Themen."
            } else if self.items.is_empty() {
                self.kind.empty_text()
            } else {
                "Kein Eintrag passt zum Filter."
            };
            lines.push(Line::styled(text, dim));
        }
        for (position, index) in visible.iter().enumerate() {
            let Some(item) = self.items.get(*index) else {
                continue;
            };
            let selected = position == self.selected;
            let marker = if selected { "▸ " } else { "  " };
            let title_style = if selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::raw(marker)];
            if self.kind == KnowledgeKind::Diary && !item.meta.is_empty() {
                spans.push(Span::styled(
                    format!("{} ", sanitize_inline(&item.meta)),
                    dim,
                ));
                spans.push(Span::styled(sanitize_inline(&item.title), title_style));
            } else {
                spans.push(Span::styled(sanitize_inline(&item.title), title_style));
                if let Some(status) = &item.status {
                    spans.push(Span::raw(" "));
                    spans.push(status_badge(status, theme));
                }
                if !item.meta.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", sanitize_inline(&item.meta)),
                        dim,
                    ));
                }
            }
            if self.supersede_from.as_deref() == Some(item.id.as_str()) && !item.id.is_empty() {
                spans.push(Span::styled("  ⇢ ersetzen", style::warning_style(theme)));
            }
            lines.push(Line::from(spans));
        }
        let height = usize::from(area.height);
        let scroll = (self.selected + 1).saturating_sub(height);
        Paragraph::new(lines)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }

    fn render_detail(&self, detail: &KnowledgeDetail, area: Rect, buf: &mut Buffer, theme: Theme) {
        let dim = style::dim_style(theme);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut pick_line = None;
        if !detail.title.is_empty() || detail.status.is_some() {
            let mut spans = vec![Span::styled(
                sanitize_inline(&detail.title),
                Style::default().add_modifier(Modifier::BOLD),
            )];
            if let Some(status) = &detail.status {
                spans.push(Span::raw(" "));
                spans.push(status_badge(status, theme));
            }
            if self.supersede_from.as_deref() == Some(detail.id.as_str()) {
                spans.push(Span::styled("  ⇢ ersetzen", style::warning_style(theme)));
            }
            lines.push(Line::from(spans));
        }
        for (label, value) in &detail.fields {
            lines.push(Line::from(vec![
                Span::styled(format!("{label}: "), dim),
                Span::raw(sanitize_inline(value)),
            ]));
        }
        if !detail.links.is_empty() {
            lines.push(Line::raw(""));
            for (index, link) in detail.links.iter().enumerate() {
                let selected = index == self.pick;
                if selected {
                    pick_line = Some(lines.len());
                }
                let arrow = if link.backlink { "← " } else { "→ " };
                let text_style = if selected {
                    style::selected_style(theme)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::raw(if selected { "▸ " } else { "  " }),
                    Span::styled(arrow, dim),
                    Span::styled(sanitize_inline(&link.target), text_style),
                    Span::styled(if link.backlink { "  (Backlink)" } else { "" }, dim),
                ]));
            }
        }
        if !detail.suggestions.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled("Vorschläge:", dim));
            for (index, suggestion) in detail.suggestions.iter().enumerate() {
                let selected = index == self.pick;
                if selected {
                    pick_line = Some(lines.len());
                }
                let mut head = format!(
                    "{} {}",
                    sanitize_inline(&suggestion.id),
                    sanitize_inline(&suggestion.kind)
                );
                if let Some(target) = &suggestion.target {
                    head.push_str(&format!(" → {}", sanitize_inline(target)));
                }
                let head_style = if selected {
                    style::selected_style(theme)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::raw(if selected { "▸ " } else { "  " }),
                    Span::styled(head, head_style),
                    Span::raw(" "),
                    status_badge(&suggestion.status, theme),
                ]));
                for text_line in sanitize_display(&suggestion.text).lines() {
                    lines.push(Line::raw(format!("    {text_line}")));
                }
            }
        }
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        if detail.body.trim().is_empty() {
            lines.push(Line::styled("(kein Text)", dim));
        } else {
            for line in sanitize_display(&detail.body).lines() {
                lines.push(Line::raw(line.to_owned()));
            }
        }
        // Die gewählte Zeile bleibt sichtbar (ohne Umbruch gerechnet).
        let height = usize::from(area.height).max(1);
        let scroll = match pick_line {
            Some(line) if line < self.scroll => line,
            Some(line) if line >= self.scroll + height => line + 1 - height,
            _ => self.scroll,
        };
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }
}

impl OverlayView for KnowledgeBrowser {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        Clear.render(area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(style::accent_color(theme)))
            .title(self.title_text());
        let inner = block.inner(area);
        block.render(area, buf);
        let [body, status, footer] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        let dim = style::dim_style(theme);
        Line::styled(self.hint(), dim).render(footer, buf);
        self.render_status(status, buf, theme);

        if let Some(detail) = &self.detail {
            self.render_detail(detail, body, buf, theme);
        } else if !self.loaded {
            if self.error.is_none() {
                Line::styled("Lade …", dim).render(body, buf);
            }
        } else {
            self.render_list(body, buf, theme);
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if key.kind == KeyEventKind::Release {
            return OverlayOutcome::Stay;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return OverlayOutcome::Stay;
        }
        self.notice = None;
        if self.confirm.is_some() {
            return self.confirm_key(key);
        }
        if self.input.is_some() {
            return self.input_key(key);
        }
        let in_detail = self.detail.is_some();
        if self.kind == KnowledgeKind::Diary
            && !in_detail
            && let Some(outcome) = self.diary_key(key)
        {
            return outcome;
        }
        match (self.kind, key.code) {
            (_, KeyCode::Esc | KeyCode::Char('q')) => {
                if in_detail || self.detail_pending {
                    self.leave_detail()
                } else if self.supersede_from.take().is_some() {
                    self.notice = Some("Markierung aufgehoben.".to_owned());
                    OverlayOutcome::Stay
                } else if self.filter.take().is_some() {
                    self.clamp_selection();
                    OverlayOutcome::Stay
                } else if self.palace_query.take().is_some() {
                    self.palace_truncated = false;
                    self.selected = 0;
                    OverlayOutcome::Fetch(PALACE_LIST_COMMAND.to_owned())
                } else if self.palace_topics {
                    // Runde 5, Teil C: Themenmodus verlassen.
                    self.toggle_topics()
                } else {
                    OverlayOutcome::Close
                }
            }
            (_, KeyCode::Char('j') | KeyCode::Down) => {
                self.move_selection(1);
                OverlayOutcome::Stay
            }
            (_, KeyCode::Char('k') | KeyCode::Up) => {
                self.move_selection(-1);
                OverlayOutcome::Stay
            }
            (_, KeyCode::PageDown) => {
                self.move_selection(PAGE_STEP as isize);
                OverlayOutcome::Stay
            }
            (_, KeyCode::PageUp) => {
                self.move_selection(-(PAGE_STEP as isize));
                OverlayOutcome::Stay
            }
            (_, KeyCode::Tab) if in_detail => {
                self.move_pick(true);
                OverlayOutcome::Stay
            }
            (_, KeyCode::BackTab) if in_detail => {
                self.move_pick(false);
                OverlayOutcome::Stay
            }
            (KnowledgeKind::Palace, KeyCode::Enter) if in_detail => self.follow_link(),
            (_, KeyCode::Enter) if !in_detail => self.open_selected(),
            (KnowledgeKind::Palace, KeyCode::Char('/')) if !in_detail => {
                self.input = Some(Input {
                    kind: InputKind::Filter,
                    buffer: self
                        .filter
                        .clone()
                        .or_else(|| self.palace_query.clone())
                        .unwrap_or_default(),
                });
                OverlayOutcome::Stay
            }
            (KnowledgeKind::Palace, KeyCode::Char('T')) if !in_detail => self.toggle_topics(),
            (KnowledgeKind::Palace, KeyCode::Char('s' | 'e')) if self.palace_topics => {
                self.notice = Some("Themen: nur p (promoten).".to_owned());
                OverlayOutcome::Stay
            }
            (KnowledgeKind::Palace, KeyCode::Char('s')) => self.supersede(),
            (KnowledgeKind::Palace, KeyCode::Char('p')) => self.promote(),
            (KnowledgeKind::Palace, KeyCode::Char('e')) => match self.current_node() {
                Some(node) => OverlayOutcome::Prefill(format!("/palace edit {node} ")),
                None => OverlayOutcome::Stay,
            },
            (KnowledgeKind::Dream, KeyCode::Char('a')) if in_detail => self.review(true),
            (KnowledgeKind::Dream, KeyCode::Char('r')) if in_detail => self.review(false),
            (KnowledgeKind::Dream, KeyCode::Char('n')) => {
                OverlayOutcome::Prefill(DREAM_RUN_PREFILL.to_owned())
            }
            (_, KeyCode::Char('R')) => match self.refresh_command() {
                Some(command) => {
                    if in_detail && self.kind.show_command().is_some() && !self.palace_topics {
                        self.detail_pending = true;
                    }
                    OverlayOutcome::Fetch(command)
                }
                None => OverlayOutcome::Stay,
            },
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        if let (Some(command), Some(detail)) = (self.kind.show_command(), &self.detail)
            && is_token(&detail.id)
        {
            return Some(format!("{command} {}", detail.id));
        }
        Some(self.list_refresh())
    }

    fn apply_data(&mut self, data: &Value) {
        if let Some(key) = self.kind.detail_key()
            && let Some(detail) = data.get(key).filter(|value| value.is_object())
        {
            self.apply_detail(detail);
            return;
        }
        self.apply_list(data);
    }

    fn apply_error(&mut self, text: &str) {
        self.error = Some(text.to_owned());
        self.detail_pending = false;
        self.following = false;
    }
}

/// Farbiges Status-Abzeichen (`[established]` usw.).
fn status_badge(status: &str, theme: Theme) -> Span<'static> {
    let style = match status {
        "established" | "accepted" => style::success_style(theme),
        "provisional" | "pending" => style::warning_style(theme),
        "rejected" => style::error_style(theme),
        "superseded" => style::dim_style(theme).add_modifier(Modifier::CROSSED_OUT),
        _ => style::dim_style(theme),
    };
    Span::styled(format!("[{}]", sanitize_inline(status)), style)
}

/// Strukturierte Traumvorschläge (`DreamSuggestion`); Einträge ohne Id entfallen.
fn parse_suggestions(value: Option<&Value>) -> Vec<SuggestionView> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| item.is_object())
        .filter_map(|item| {
            let id = str_field(item, "id");
            (!id.is_empty()).then(|| SuggestionView {
                id,
                kind: str_field(item, "kind"),
                target: opt_str_field(item, "target"),
                text: str_field(item, "text"),
                status: opt_str_field(item, "status").unwrap_or_else(|| "pending".to_owned()),
            })
        })
        .collect()
}

/// `true` für einen Palast-Verweis, dem `/palace show` folgen kann.
fn is_palace_ref(target: &str) -> bool {
    is_token(target) && (target.starts_with("palace/") || !target.contains('/'))
}

/// `true`, wenn `text` ein einzelnes, nicht leeres Befehls-Token ist.
fn is_token(text: &str) -> bool {
    !text.is_empty()
        && !text
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\'')
}

/// Fasst Leerraum zusammen und entfernt Steuerzeichen (eine Befehlszeile).
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .map(|word| word.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Verschiebt ein `YYYY-MM-DD`-Datum um `days` Tage.
fn shift_date(date: &str, days: i64) -> Option<String> {
    let parsed: jiff::civil::Date = date.parse().ok()?;
    let span = jiff::Span::new().try_days(days).ok()?;
    parsed.checked_add(span).ok().map(|date| date.to_string())
}

/// Anzahl bei Array-Feldern, Zahl bei numerischen Feldern, sonst 0.
fn list_len(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(items)) => items.len(),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Texte eines Link-Felds: Strings direkt, Objekte über `id`/`target`/`title`.
fn list_texts(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Number(number) => Some(number.to_string()),
                Value::Object(_) => ["id", "target", "title"]
                    .iter()
                    .find_map(|key| opt_str_field(item, key)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn str_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

fn opt_str_field(value: &Value, key: &str) -> Option<String> {
    let text = str_field(value, key);
    (!text.trim().is_empty()).then_some(text)
}

fn array_field<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn fetch_text(outcome: OverlayOutcome) -> Option<String> {
        match outcome {
            OverlayOutcome::Fetch(text) => Some(text),
            _ => None,
        }
    }

    fn type_text(view: &mut KnowledgeBrowser, text: &str) {
        for c in text.chars() {
            view.on_key(key(KeyCode::Char(c)));
        }
    }

    fn render_to_string(view: &KnowledgeBrowser) -> TestResult<String> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 20))?;
        terminal.draw(|frame| {
            let area = frame.area();
            view.render(area, frame.buffer_mut(), Theme::Dark);
        })?;
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        Ok(out)
    }

    fn palace_nodes() -> Value {
        json!({"nodes": [
            {"id": "palace/arch", "title": "Architektur", "status": "established", "links": ["palace/speicher", "topic/idee"], "updated": "2026-09-01"},
            {"id": "palace/speicher", "title": "Speicher", "status": "provisional", "links": []},
            {"id": "palace/alt", "title": "Alte Notiz", "status": "superseded"},
            {"title": "ohne id"},
            "kaputt"
        ]})
    }

    fn palace_node(id: &str) -> Value {
        json!({"node": {
            "id": id, "title": format!("Titel {id}"), "status": "established",
            "links": ["palace/speicher", "topic/idee"], "backlinks": ["palace/alt"],
            "tags": ["kern"], "updated": "2026-09-01", "body": "Zeile 1\nZeile 2"
        }})
    }

    fn dream_report() -> Value {
        json!({"report": {
            "id": "dream/2026-09-23/w1", "date": "2026-09-23", "body": "Bericht",
            "proposals": [],
            "suggestions": [
                {"id": "p1", "kind": "topic", "target": "topic/rust", "text": "Rust-Thema anlegen", "status": "pending"},
                {"id": "p2", "kind": "follow_up", "target": null, "text": "Nachfassen", "status": "accepted"},
                {"id": "p3", "kind": "maintenance", "text": "Aufräumen", "status": "pending"}
            ]
        }})
    }

    #[test]
    fn refresh_commands_per_kind() {
        assert_eq!(
            KnowledgeBrowser::new(KnowledgeKind::Palace)
                .refresh_command()
                .as_deref(),
            Some("/palace list")
        );
        assert_eq!(
            KnowledgeBrowser::new(KnowledgeKind::Dream)
                .refresh_command()
                .as_deref(),
            Some("/dream list")
        );
        let diary = KnowledgeBrowser::new(KnowledgeKind::Diary);
        assert_eq!(diary.refresh_command().as_deref(), Some("/diary today"));
        assert_eq!(diary.kind(), KnowledgeKind::Diary);
    }

    #[test]
    fn palace_list_badges_and_detail_with_links() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        view.apply_data(&palace_nodes());
        assert_eq!(view.items().len(), 3);
        assert_eq!(view.items()[0].meta, "2 Links · 2026-09-01");
        assert_eq!(view.items()[0].status.as_deref(), Some("established"));
        let out = render_to_string(&view)?;
        assert!(out.contains("Architektur [established]"));
        assert!(out.contains("Speicher [provisional]"));
        assert!(out.contains("[superseded]"));

        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/palace show palace/arch")
        );
        assert!(render_to_string(&view)?.contains("Lade Details"));
        view.apply_data(&palace_node("palace/arch"));
        let detail = view.detail().ok_or("kein Detail")?;
        assert_eq!(detail.title, "Titel palace/arch");
        assert_eq!(detail.links.len(), 3);
        assert!(detail.links[2].backlink);
        assert!(
            detail
                .fields
                .contains(&("Tags".to_owned(), "kern".to_owned()))
        );
        // Die Liste bleibt bei Detail-Nutzlast erhalten.
        assert_eq!(view.items().len(), 3);
        let out = render_to_string(&view)?;
        assert!(out.contains("▸ → palace/speicher"));
        assert!(out.contains("← palace/alt  (Backlink)"));
        assert!(out.contains("Zeile 2"));
        assert_eq!(
            view.refresh_command().as_deref(),
            Some("/palace show palace/arch")
        );
        Ok(())
    }

    #[test]
    fn palace_follows_links_and_walks_back() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        view.apply_data(&palace_nodes());
        view.on_key(key(KeyCode::Enter));
        view.apply_data(&palace_node("palace/arch"));
        // Erster Link: palace/speicher.
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/palace show palace/speicher")
        );
        view.apply_data(&palace_node("palace/speicher"));
        assert_eq!(view.trail, vec!["palace/arch".to_owned()]);
        // topic/… ist kein Palast-Knoten.
        view.on_key(key(KeyCode::Tab));
        assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        assert!(render_to_string(&view)?.contains("kein Palast-Knoten"));
        // Shift+Tab zurück, Tab zweimal zum Backlink.
        view.on_key(key(KeyCode::BackTab));
        assert_eq!(view.pick, 0);
        view.on_key(key(KeyCode::Tab));
        view.on_key(key(KeyCode::Tab));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/palace show palace/alt")
        );
        view.apply_error("kein sichtbarer Palace-Knoten");
        assert!(!view.following);
        // Esc geht den Pfad zurück, dann zur Liste, dann schließt.
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Esc))).as_deref(),
            Some("/palace show palace/arch")
        );
        view.apply_data(&palace_node("palace/arch"));
        assert!(view.trail.is_empty());
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Stay);
        assert!(view.detail().is_none());
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
        Ok(())
    }

    #[test]
    fn palace_supersede_needs_two_presses_and_confirmation() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        view.apply_data(&palace_nodes());
        view.on_key(key(KeyCode::Down));
        assert_eq!(view.on_key(key(KeyCode::Char('s'))), OverlayOutcome::Stay);
        assert_eq!(view.supersede_from.as_deref(), Some("palace/speicher"));
        assert!(render_to_string(&view)?.contains("⇢ ersetzen"));
        view.on_key(key(KeyCode::Up));
        assert_eq!(view.on_key(key(KeyCode::Char('s'))), OverlayOutcome::Stay);
        assert!(render_to_string(&view)?.contains("palace/speicher durch palace/arch ersetzen"));
        // Abbrechen lässt nichts zurück.
        assert_eq!(view.on_key(key(KeyCode::Char('n'))), OverlayOutcome::Stay);
        assert!(view.confirm.is_none());
        // Erneut markieren und bestätigen → Vorbelegung mit --confirm.
        view.on_key(key(KeyCode::Down));
        view.on_key(key(KeyCode::Char('s')));
        view.on_key(key(KeyCode::Up));
        view.on_key(key(KeyCode::Char('s')));
        assert_eq!(
            view.on_key(key(KeyCode::Char('y'))),
            OverlayOutcome::Prefill(
                "/palace supersede palace/speicher palace/arch --confirm".to_owned()
            )
        );
        // Markieren und Esc hebt die Markierung auf statt zu schließen.
        view.on_key(key(KeyCode::Char('s')));
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Stay);
        assert!(view.supersede_from.is_none());
        Ok(())
    }

    #[test]
    fn palace_promote_edit_and_filter() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        view.apply_data(&palace_nodes());
        assert_eq!(
            view.on_key(key(KeyCode::Char('p'))),
            OverlayOutcome::Prefill("/palace promote ".to_owned())
        );
        assert_eq!(
            view.on_key(key(KeyCode::Char('e'))),
            OverlayOutcome::Prefill("/palace edit palace/arch ".to_owned())
        );
        // Filter live als Vorschau, Enter sucht über /palace search.
        view.on_key(key(KeyCode::Char('/')));
        type_text(&mut view, "speich");
        assert_eq!(view.visible().len(), 1);
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/palace search speich")
        );
        assert!(view.filter.is_none());
        assert_eq!(
            view.refresh_command().as_deref(),
            Some("/palace search speich")
        );
        // Eine verspätete Listenantwort überschreibt die Suche nicht.
        view.apply_data(&palace_nodes());
        // Treffer einer fremden Suche werden verworfen.
        view.apply_data(&json!({"query": "anders", "truncated": false, "hits": [
            {"id": "palace/arch", "title": "Architektur", "status": "established", "score": 1.0, "hop_path": []}
        ]}));
        view.apply_data(&json!({"query": "speich", "truncated": true, "hits": [
            {"id": "palace/speicher", "title": "Speicher", "status": "established", "score": 2.5, "hop_path": []},
            {"id": "palace/arch", "title": "Architektur", "status": "established", "score": 0.5,
             "hop_path": ["palace/speicher", "palace/arch"]}
        ]}));
        assert_eq!(view.items().len(), 2);
        assert_eq!(view.items()[0].meta, "Score 2.50");
        assert_eq!(
            view.items()[1].meta,
            "Score 0.50 · via palace/speicher → palace/arch"
        );
        let out = render_to_string(&view)?;
        assert!(out.contains("Suche „speich“ (2) gekürzt"), "{out}");
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/palace show palace/speicher")
        );
        view.apply_data(&palace_node("palace/speicher"));
        // Auf dem topic-Link fragt p nach und belegt dann das Thema vor.
        view.on_key(key(KeyCode::Tab));
        assert_eq!(view.on_key(key(KeyCode::Char('p'))), OverlayOutcome::Stay);
        assert!(render_to_string(&view)?.contains("topic/idee in den Palast promoten"));
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::Prefill("/palace promote topic/idee".to_owned())
        );
        view.on_key(key(KeyCode::Esc));
        // Esc verlässt die Suche und lädt die Liste neu.
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Esc))).as_deref(),
            Some("/palace list")
        );
        assert!(view.palace_query.is_none());
        view.apply_data(&palace_nodes());
        assert_eq!(view.visible().len(), 3);
        // Ein reiner Live-Filter ohne Enter wird mit Esc gelöscht.
        view.on_key(key(KeyCode::Char('/')));
        type_text(&mut view, "arch");
        assert_eq!(view.visible().len(), 1);
        view.on_key(key(KeyCode::Esc));
        assert!(view.filter.is_none());
        assert_eq!(view.visible().len(), 3);
        Ok(())
    }

    fn memory_topics() -> Value {
        json!({"topics": [
            {"slug": "canary-regel", "title": "Canary-Regel", "status": "provisional",
             "origin": {"kind": "fact", "id": "f-12", "detail": "project", "at": "2026-09-20T10:00:00Z"}},
            {"slug": "deploy", "title": "Deploy", "status": "provisional", "origin": null},
            {"slug": "böse id", "title": "Leerzeichen"},
            {"title": "ohne slug"}
        ]})
    }

    /// Runde 5, Teil C: `T` listet die vorläufigen Themen, `p` belegt nach
    /// Rückfrage `/palace promote topic/<slug>` vor, verspätete Knotenlisten
    /// überschreiben den Themenmodus nicht, `T`/`Esc` kehren zurück.
    #[test]
    fn palace_topics_list_and_promote_after_confirmation() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        view.apply_data(&palace_nodes());
        // Themen ohne Themenmodus werden ignoriert.
        view.apply_data(&memory_topics());
        assert_eq!(view.items().len(), 3);
        assert!(render_to_string(&view)?.contains("T Themen"));

        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('T')))).as_deref(),
            Some(MEMORY_TOPICS_COMMAND)
        );
        assert_eq!(
            view.refresh_command().as_deref(),
            Some(MEMORY_TOPICS_COMMAND)
        );
        assert!(view.items().is_empty());
        // Eine verspätete Knotenliste überschreibt den Themenmodus nicht.
        view.apply_data(&palace_nodes());
        assert!(view.items().is_empty());
        view.apply_data(&memory_topics());
        let ids: Vec<&str> = view.items().iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["topic/canary-regel", "topic/deploy"]);
        assert_eq!(view.items()[0].meta, "aus fact f-12");
        let out = render_to_string(&view)?;
        assert!(out.contains("vorläufige Themen (2)"), "{out}");
        assert!(out.contains("p promoten"), "{out}");

        // s/e gelten nicht für Themen.
        assert_eq!(view.on_key(key(KeyCode::Char('s'))), OverlayOutcome::Stay);
        assert!(view.supersede_from.is_none());
        assert_eq!(view.on_key(key(KeyCode::Char('e'))), OverlayOutcome::Stay);

        // Enter zeigt das Thema lokal, ohne Nachladen.
        assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        let detail = view.detail().ok_or("topic detail")?;
        assert_eq!(detail.title, "Canary-Regel");
        assert!(
            detail.body.contains("Erfasst: 2026-09-20T10:00:00Z"),
            "{}",
            detail.body
        );
        view.on_key(key(KeyCode::Esc));
        assert!(view.detail().is_none());

        // p fragt nach; n bricht ab, y belegt vor.
        view.on_key(key(KeyCode::Char('j')));
        assert_eq!(view.on_key(key(KeyCode::Char('p'))), OverlayOutcome::Stay);
        assert!(render_to_string(&view)?.contains("topic/deploy in den Palast promoten"));
        assert_eq!(view.on_key(key(KeyCode::Char('n'))), OverlayOutcome::Stay);
        view.on_key(key(KeyCode::Char('p')));
        assert_eq!(
            view.on_key(key(KeyCode::Char('y'))),
            OverlayOutcome::Prefill("/palace promote topic/deploy".to_owned())
        );

        // Der Filter bleibt im Themenmodus lokal.
        view.on_key(key(KeyCode::Char('/')));
        type_text(&mut view, "canary");
        assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        assert_eq!(view.visible().len(), 1);
        assert!(view.palace_query.is_none());
        view.on_key(key(KeyCode::Esc));
        assert!(view.filter.is_none());

        // Esc verlässt den Themenmodus und lädt die Knoten.
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Esc))).as_deref(),
            Some(PALACE_LIST_COMMAND)
        );
        assert!(!view.palace_topics);
        view.apply_data(&palace_nodes());
        assert_eq!(view.items().len(), 3);
        // T hin und zurück.
        view.on_key(key(KeyCode::Char('T')));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('T')))).as_deref(),
            Some(PALACE_LIST_COMMAND)
        );
        Ok(())
    }

    /// Ohne Themen zeigt der Themenmodus einen eigenen Leertext.
    #[test]
    fn palace_topics_empty_state() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        view.on_key(key(KeyCode::Char('T')));
        view.apply_data(&json!({"topics": []}));
        assert!(render_to_string(&view)?.contains("Keine vorläufigen Themen."));
        assert_eq!(view.on_key(key(KeyCode::Char('p'))), OverlayOutcome::Stay);
        Ok(())
    }

    #[test]
    fn live_reload_keeps_selection_and_detail_state() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        view.apply_data(&palace_nodes());
        view.on_key(key(KeyCode::Down));
        // Neuer Knoten vorn: Auswahl bleibt auf palace/speicher.
        view.apply_data(&json!({"nodes": [
            {"id": "palace/neu"}, {"id": "palace/arch"}, {"id": "palace/speicher"}
        ]}));
        assert_eq!(
            view.selected_item().map(|item| item.id.as_str()),
            Some("palace/speicher")
        );
        view.on_key(key(KeyCode::Enter));
        view.apply_data(&palace_node("palace/speicher"));
        view.on_key(key(KeyCode::Tab));
        view.on_key(key(KeyCode::Char('j')));
        // Reload desselben Knotens (Knowledge-Event): pick/scroll bleiben.
        view.apply_data(&palace_node("palace/speicher"));
        assert_eq!((view.pick, view.scroll), (1, 1));
        // Zurück zur Liste lädt sie nach.
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Esc))).as_deref(),
            Some("/palace list")
        );
        Ok(())
    }

    #[test]
    fn dream_list_detail_and_review() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Dream);
        view.apply_data(&json!({"reports": [
            {"id": "dream/2026-09-23/w1", "date": "2026-09-23", "proposals": [],
             "suggestions": [{"id": "p1", "status": "pending"}, {"id": "p2", "status": "rejected"}]},
            {"id": "dream/2026-09-20/w0", "proposals": 4}
        ]}));
        assert_eq!(view.items()[0].title, "2026-09-23");
        assert_eq!(view.items()[0].meta, "2 Vorschläge · 1 offen");
        assert_eq!(view.items()[1].meta, "4 Vorschläge");
        assert_eq!(
            view.on_key(key(KeyCode::Char('n'))),
            OverlayOutcome::Prefill("/dream run".to_owned())
        );
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/dream show dream/2026-09-23/w1")
        );
        view.apply_data(&dream_report());
        let detail = view.detail().ok_or("kein Detail")?;
        assert_eq!(detail.title, "Traumbericht 2026-09-23");
        assert_eq!(detail.suggestions.len(), 3);
        assert_eq!(detail.suggestions[1].target, None);
        let out = render_to_string(&view)?;
        assert!(out.contains("▸ p1 topic → topic/rust [pending]"));
        assert!(out.contains("p2 follow_up [accepted]"));
        assert!(out.contains("Rust-Thema anlegen"));

        // Annehmen fragt nach und führt dann das Review aus.
        assert_eq!(view.on_key(key(KeyCode::Char('a'))), OverlayOutcome::Stay);
        assert!(render_to_string(&view)?.contains("Vorschlag p1 annehmen"));
        assert_eq!(
            view.on_key(key(KeyCode::Char('y'))),
            OverlayOutcome::Run("/dream review dream/2026-09-23/w1 accept p1".to_owned())
        );
        // Entschiedene Vorschläge lassen sich nicht erneut entscheiden.
        view.on_key(key(KeyCode::Tab));
        assert_eq!(view.on_key(key(KeyCode::Char('r'))), OverlayOutcome::Stay);
        assert!(render_to_string(&view)?.contains("bereits entschieden"));
        // Ablehnen ohne Rückfrage.
        view.on_key(key(KeyCode::Tab));
        assert_eq!(
            view.on_key(key(KeyCode::Char('r'))),
            OverlayOutcome::Run("/dream review dream/2026-09-23/w1 reject p3".to_owned())
        );
        // Nach dem Review lädt die Ansicht das Detail nach, Auswahl bleibt.
        assert_eq!(
            view.refresh_command().as_deref(),
            Some("/dream show dream/2026-09-23/w1")
        );
        view.apply_data(&dream_report());
        assert_eq!(view.pick, 2);
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('R')))).as_deref(),
            Some("/dream show dream/2026-09-23/w1")
        );
        view.apply_data(&dream_report());
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Esc))).as_deref(),
            Some("/dream list")
        );
        Ok(())
    }

    #[test]
    fn dream_legacy_report_without_suggestions() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Dream);
        view.apply_data(&json!({"reports": [{"id": "d1"}]}));
        view.on_key(key(KeyCode::Enter));
        view.apply_data(&json!({"report": {"id": "d1", "body": "Text", "proposals": [{}, {}]}}));
        let detail = view.detail().ok_or("kein Detail")?;
        assert_eq!(detail.title, "Traumbericht");
        assert!(
            detail
                .fields
                .contains(&("Vorschläge (ohne Review)".to_owned(), "2".to_owned()))
        );
        assert_eq!(view.on_key(key(KeyCode::Char('a'))), OverlayOutcome::Stay);
        assert!(view.confirm.is_none());
        Ok(())
    }

    fn diary_day(agent: &str, date: &str) -> Value {
        json!({"agent": agent, "date": date, "entries": [
            {"time": "09:15:00", "trigger": "manual", "text": "Erster Gedanke\nmehr"},
            {"time": "18:00:00", "trigger": "end-of-session", "text": "Abschluss"}
        ]})
    }

    #[test]
    fn diary_day_navigation_and_range() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Diary);
        view.apply_data(&diary_day("root", "2026-09-24"));
        assert_eq!(view.items().len(), 2);
        assert_eq!(view.items()[0].title, "Erster Gedanke");
        let out = render_to_string(&view)?;
        assert!(out.contains("Tagebuch · root · 2026-09-24 (2)"));
        assert!(out.contains("09:15:00 · manual Erster Gedanke"));
        assert_eq!(
            view.refresh_command().as_deref(),
            Some("/diary show root --date=2026-09-24")
        );
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Left))).as_deref(),
            Some("/diary show root --date=2026-09-23")
        );
        // Verspätete Antwort des alten Tages wird verworfen.
        view.apply_data(&json!({"agent": "root", "date": "2026-09-24", "entries": []}));
        assert_eq!(view.items().len(), 2);
        view.apply_data(&json!({"agent": "root", "date": "2026-09-23", "entries": []}));
        assert!(view.items().is_empty());
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('l')))).as_deref(),
            Some("/diary show root --date=2026-09-24")
        );
        view.apply_data(&diary_day("root", "2026-09-24"));
        // Bereichsansicht (7 Tage) und Wochensprung.
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('w')))).as_deref(),
            Some("/diary show root --from=2026-09-18 --to=2026-09-24")
        );
        view.apply_data(&json!({"agent": "root", "from": "2026-09-18", "to": "2026-09-24", "days": [
            {"date": "2026-09-20", "entries": [{"time": "10:00:00", "trigger": "compaction", "text": "Verdichtet"}]},
            {"date": "2026-09-24", "entries": [{"time": "11:00:00", "trigger": "manual", "text": "Heute"}]}
        ]}));
        assert_eq!(view.items().len(), 2);
        assert_eq!(view.items()[0].meta, "2026-09-20 10:00:00 · compaction");
        assert!(render_to_string(&view)?.contains("root · 2026-09-18 – 2026-09-24"));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Left))).as_deref(),
            Some("/diary show root --from=2026-09-11 --to=2026-09-17")
        );
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('t')))).as_deref(),
            Some("/diary show root")
        );
        Ok(())
    }

    #[test]
    fn diary_search_agent_switch_and_jump() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Diary);
        view.apply_data(&diary_day("root", "2026-09-24"));
        view.on_key(key(KeyCode::Char('/')));
        type_text(&mut view, "lock  fehler");
        assert!(render_to_string(&view)?.contains("Suche: lock"));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/diary search lock fehler")
        );
        view.apply_data(&json!({"query": "lock fehler", "truncated": true, "hits": [
            {"agent": "coder", "date": "2026-09-10", "time": "08:00:00", "trigger": "manual", "text": "lock fehler gefunden"},
            {"agent": "root", "date": "2026-09-01", "time": "07:00:00", "trigger": "manual", "text": "noch ein lock fehler"}
        ]}));
        assert_eq!(view.items().len(), 2);
        assert_eq!(view.items()[0].meta, "coder · 2026-09-10 08:00:00 · manual");
        let out = render_to_string(&view)?;
        assert!(out.contains("Suche „lock fehler“ (2) gekürzt"));
        assert_eq!(
            view.diary.agents,
            vec!["coder".to_owned(), "root".to_owned()]
        );
        assert_eq!(
            view.refresh_command().as_deref(),
            Some("/diary search lock fehler")
        );
        // o springt in den Tag des Treffers (mit dessen Agent).
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('o')))).as_deref(),
            Some("/diary show coder --date=2026-09-10")
        );
        // Verspätete Suchantwort wird verworfen.
        view.apply_data(&json!({"query": "lock fehler", "hits": []}));
        view.apply_data(&diary_day("coder", "2026-09-10"));
        assert_eq!(view.items().len(), 2);
        // Tab wechselt zum nächsten bekannten Agenten.
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Tab))).as_deref(),
            Some("/diary show root --date=2026-09-10")
        );
        // a: Agent eingeben, Tab vervollständigt.
        view.on_key(key(KeyCode::Char('a')));
        for _ in 0.."root".len() {
            view.on_key(key(KeyCode::Backspace));
        }
        view.on_key(key(KeyCode::Tab));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/diary show coder --date=2026-09-10")
        );
        // Esc in einer Suche kehrt zur Tagesansicht zurück.
        view.on_key(key(KeyCode::Char('/')));
        type_text(&mut view, "x");
        view.on_key(key(KeyCode::Enter));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Esc))).as_deref(),
            Some("/diary show coder --date=2026-09-10")
        );
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
        Ok(())
    }

    #[test]
    fn diary_agents_come_from_diary_agents_once() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Diary);
        view.apply_data(&json!({"agent": "root", "date": "2026-09-24", "entries": []}));
        // Ohne bekannte Agenten lädt Tab die Liste genau einmal.
        let mut empty = KnowledgeBrowser::new(KnowledgeKind::Diary);
        assert_eq!(
            fetch_text(empty.on_key(key(KeyCode::Tab))).as_deref(),
            Some("/diary agents")
        );
        assert_eq!(empty.on_key(key(KeyCode::Tab)), OverlayOutcome::Stay);
        // a öffnet die Eingabe und fordert die Agentenliste an.
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('a')))).as_deref(),
            Some("/diary agents")
        );
        view.apply_data(&json!({"agents": ["coder", "explorer", "root"]}));
        assert_eq!(
            view.diary.agents,
            vec!["coder".to_owned(), "explorer".to_owned(), "root".to_owned()]
        );
        // Die Agentenliste ersetzt die Einträge nicht.
        assert_eq!(view.diary.agent.as_deref(), Some("root"));
        view.on_key(key(KeyCode::Esc));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Tab))).as_deref(),
            Some("/diary show coder --date=2026-09-24")
        );
        Ok(())
    }

    #[test]
    fn diary_entries_open_locally_and_prefill_note() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Diary);
        // Ohne geladenes Datum bleibt die Navigation stehen.
        assert_eq!(view.on_key(key(KeyCode::Left)), OverlayOutcome::Stay);
        view.apply_data(&json!({"agent": "root", "date": "2026-09-24", "entries": [
            {"time": "09:15", "trigger": "manual", "text": "Erster Gedanke\nmehr"},
            {"text": ""}
        ]}));
        assert_eq!(view.items().len(), 2);
        assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        assert_eq!(
            view.detail().map(|d| d.body.as_str()),
            Some("Erster Gedanke\nmehr")
        );
        // Im Detail navigiert ← nicht.
        assert_eq!(view.on_key(key(KeyCode::Left)), OverlayOutcome::Stay);
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Stay);
        assert!(view.detail().is_none());
        assert_eq!(
            view.on_key(key(KeyCode::Char('n'))),
            OverlayOutcome::Prefill("/diary note ".to_owned())
        );
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('R')))).as_deref(),
            Some("/diary show root --date=2026-09-24")
        );
        Ok(())
    }

    #[test]
    fn keys_are_scoped_and_garbage_is_tolerated() {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        assert_eq!(view.on_key(key(KeyCode::Char('n'))), OverlayOutcome::Stay);
        view.apply_data(&json!({"nodes": "kaputt", "node": 3}));
        assert!(view.items().is_empty());
        assert!(view.detail().is_none());
        assert_eq!(view.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        assert_eq!(view.on_key(key(KeyCode::Char('s'))), OverlayOutcome::Stay);
        view.apply_error("nicht verfügbar");
        assert_eq!(view.error.as_deref(), Some("nicht verfügbar"));

        let mut dream = KnowledgeBrowser::new(KnowledgeKind::Dream);
        dream.apply_data(&json!({"reports": [{"id": "a b"}]}));
        // Ids mit Leerraum werden nie zu Befehlen.
        assert_eq!(dream.on_key(key(KeyCode::Enter)), OverlayOutcome::Stay);
        assert_eq!(dream.on_key(key(KeyCode::Char('a'))), OverlayOutcome::Stay);

        let mut diary = KnowledgeBrowser::new(KnowledgeKind::Diary);
        diary.apply_data(&json!({"unbekannt": true}));
        assert!(diary.items().is_empty());
    }

    #[test]
    fn selection_clamps_after_shorter_list() {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Dream);
        view.apply_data(&json!({"reports": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}));
        view.on_key(key(KeyCode::PageDown));
        assert_eq!(view.selected, 2);
        view.apply_data(&json!({"reports": [{"id": "a"}]}));
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn shift_date_handles_month_edges() {
        assert_eq!(shift_date("2026-03-01", -1).as_deref(), Some("2026-02-28"));
        assert_eq!(shift_date("2026-12-31", 1).as_deref(), Some("2027-01-01"));
        assert_eq!(shift_date("kaputt", 1), None);
    }
}
