//! Live-Werte für die Agentenbaum-Ansicht `/agent` (Runde 5, Teil I).
//!
//! # Verantwortung
//! Die Zeilen des Agentenbaums ([`AgentRow`]) stammen aus dem Spawner
//! (Admission, Status, Budget-Deckel) bzw. aus dauerhaften
//! Orchestrierungs-Ereignissen, die erst am Ende Zahlen tragen. Dieses Modul
//! ergänzt sie um die Live-Werte, die das Agenten-Panel rechts ohnehin hält
//! ([`AgentMonitor`]): Tokens (frisch, Ausgabe, Cache wie im Panel),
//! Werkzeugaufrufe, laufende Dauer seit Start, aktueller Zustand
//! (denkt / Werkzeug xyz / wartet), letzter Schritt bzw. Textausschnitt.
//! Dazu kommen die Budget-Auslastung in Prozent, der Auftrag aus dem
//! Spawn-Ereignis und die Beschriftung der Wurzel nach ihrer tatsächlichen
//! Rolle (`UIA · <name>` bzw. der explizit gestartete Wurzel-Agent).
//!
//! # Aktualisierung
//! Die Zeilen werden bei jedem Zeichnen neu aus Spawner und Monitor
//! projiziert; der Zustand der Ansicht ([`crate::agent_tree::AgentTree`]:
//! Auswahl, eingeklappte Knoten, Detailansicht, Bildlauf) bleibt dabei
//! unberührt — kein Neuaufbau der Ansicht. Im Leerlauf zeichnet `run_loop`
//! bei offener Ansicht alle [`AGENT_TREE_LIVE_INTERVAL`] neu, im Busy-Pfad
//! der Spinner-Takt. Die Dauer wird in ganzen Sekunden gezeigt, damit die
//! Anzeige nicht flackert.
//!
//! # Sicherheit
//! Reine Projektion vorhandener Beobachtungen; keine Ausführungsautorität.
//! Texte laufen beim Rendern durch [`crate::sanitize`] (in `agent_tree.rs`).

use std::time::{Duration, Instant};

use crate::agent_monitor::{AgentLive, AgentMonitor, AgentPhase, TraceEntry, human_tokens};
use crate::agent_tree::AgentRow;
use crate::history_cell::truncate_chars;

/// Takt, in dem eine offene Agentenbaum-Ansicht im Leerlauf neu zeichnet.
pub(crate) const AGENT_TREE_LIVE_INTERVAL: Duration = Duration::from_millis(500);

/// Länge des Auftrags in der Baumzeile (Zeichen).
pub(crate) const TASK_TREE_CHARS: usize = 48;

/// Länge des „letzten Schritts“ (Zeichen).
const LAST_STEP_CHARS: usize = 200;

/// Budget-Deckel eines Kindes aus der Admission (`None` = kein Deckel).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BudgetLimits {
    /// Token-Deckel.
    pub tokens: Option<u64>,
    /// Deckel der Werkzeugaufrufe.
    pub tool_calls: Option<u32>,
    /// Laufzeit-Deckel in Millisekunden.
    pub wall_ms: Option<u64>,
}

impl BudgetLimits {
    /// `true`, wenn kein Deckel gesetzt ist.
    pub(crate) fn is_empty(&self) -> bool {
        self.tokens.is_none() && self.tool_calls.is_none() && self.wall_ms.is_none()
    }
}

/// Live-Werte eines Agenten zum Zeitpunkt der Projektion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentRowLive {
    /// Prompt-Tokens inklusive Cache (wie `↑` im Panel).
    pub prompt_tokens: u64,
    /// Ausgabe-Tokens (`↓`).
    pub output_tokens: u64,
    /// Gelesene Cache-Tokens (`⟳`), falls gemeldet.
    pub cached_tokens: Option<u64>,
    /// Gesamt (für die Budget-Auslastung).
    pub total_tokens: u64,
    /// Abgeschlossene Werkzeugaufrufe.
    pub tool_calls: u32,
    /// Laufzeit seit Start (bzw. bis Ende) in Millisekunden.
    pub elapsed_ms: u64,
    /// Aktueller Zustand, z. B. `denkt`, `Werkzeug fs.read`, `wartet`.
    pub state: String,
    /// Agent läuft noch.
    pub active: bool,
    /// Letzter Schritt aus der Spur (gekürzt).
    pub last_step: Option<String>,
    /// Ende des sichtbaren Antworttexts (gekürzt).
    pub preview: Option<String>,
}

impl AgentRowLive {
    /// Projiziert den Monitor-Zustand eines Agenten.
    ///
    /// # Argumente
    /// - `live` (`&AgentLive`): Monitor-Eintrag.
    /// - `now` (`Instant`): Bezugszeit für die laufende Dauer.
    pub(crate) fn from_live(live: &AgentLive, now: Instant) -> Self {
        let usage = live.usage();
        let end = live.finished.unwrap_or(now);
        let elapsed_ms = u64::try_from(end.saturating_duration_since(live.started).as_millis())
            .unwrap_or(u64::MAX);
        let active = matches!(
            live.phase,
            AgentPhase::Admitted | AgentPhase::Thinking | AgentPhase::Tool | AgentPhase::Waiting
        );
        let state = match (live.phase, live.current_tool.as_deref()) {
            (AgentPhase::Tool, Some(tool)) => format!("Werkzeug {tool}"),
            (phase, _) => phase.label().to_owned(),
        };
        let preview = Some(live.preview.trim())
            .filter(|text| !text.is_empty())
            .map(|text| tail_chars(text, LAST_STEP_CHARS));
        Self {
            prompt_tokens: usage.prompt_tokens(),
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens.filter(|cached| *cached > 0),
            total_tokens: usage.total(),
            tool_calls: live.tool_calls,
            elapsed_ms,
            state,
            active,
            last_step: live.trace.entries().last().map(step_text),
            preview,
        }
    }

    /// Token-Darstellung wie im Agenten-Panel: `↑352.1k ↓11.1k ⟳1.2k`.
    pub(crate) fn tokens_label(&self) -> String {
        let mut text = format!(
            "↑{} ↓{}",
            human_tokens(self.prompt_tokens),
            human_tokens(self.output_tokens)
        );
        if let Some(cached) = self.cached_tokens {
            text.push_str(&format!(" ⟳{}", human_tokens(cached)));
        }
        text
    }

    /// Dauer in ganzen Sekunden (ruhige Anzeige).
    pub(crate) fn elapsed_label(&self) -> String {
        format!("{} s", self.elapsed_ms / 1000)
    }
}

/// Kurzform eines Spur-Eintrags als „letzter Schritt“.
fn step_text(entry: &TraceEntry) -> String {
    let text = match entry {
        TraceEntry::ToolCall { name, args_preview } => format!("→ {name} {args_preview}"),
        TraceEntry::ToolResult { name, ok, preview } => {
            let outcome = if *ok { "ok" } else { "Fehler" };
            format!("← {name} {outcome}: {preview}")
        }
        TraceEntry::Text(text) => return format!("» {}", tail_chars(text.trim(), LAST_STEP_CHARS)),
        TraceEntry::Reasoning(text) => {
            return format!("∴ {}", tail_chars(text.trim(), LAST_STEP_CHARS));
        }
        TraceEntry::Status(text) => text.clone(),
    };
    truncate_chars(text.trim(), LAST_STEP_CHARS)
}

/// Die letzten `max` Zeichen (mit führendem `…` bei Kürzung).
fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count - max.saturating_sub(1)).collect();
    format!("…{tail}")
}

/// Beschriftung der Wurzel nach ihrer tatsächlichen Rolle.
///
/// # Argumente
/// - `explicit_root` (`Option<&str>`): explizit gestarteter Wurzel-Agent
///   (`RuntimeSpec::active_agent`), z. B. `root-orchestrator`.
/// - `active_uia` (`Option<&str>`): aktive UIA-Definition
///   (`harness.active_uia_definition`), z. B. `assistant-ui`.
///
/// # Rückgabe
/// Der explizite Wurzel-Agent, sonst `UIA · <name>`, sonst `UIA`.
pub(crate) fn root_label(explicit_root: Option<&str>, active_uia: Option<&str>) -> String {
    fn clean(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }
    match (clean(explicit_root), clean(active_uia)) {
        (Some(root), _) => root.to_owned(),
        (None, Some(uia)) => format!("UIA · {uia}"),
        (None, None) => "UIA".to_owned(),
    }
}

/// Ergänzt die dauerhaften Zeilen um Live-Werte und Aufträge.
///
/// # Beschreibung
/// - Live-Werte aus dem Monitor überschreiben unbekannte (`None`) oder
///   kleinere dauerhafte Zähler; dauerhafte Endwerte bleiben, wenn sie größer
///   sind.
/// - Ein fehlender Auftrag kommt aus dem Monitor (Orchestrierungs-Ereignis)
///   oder aus dem `ChildSpawned`-Ereignis (`spawn_task`).
///
/// # Argumente
/// - `rows`: die zu ergänzenden Zeilen (in Anzeigereihenfolge).
/// - `monitor`: Live-Zustand des Agenten-Panels.
/// - `spawn_task`: Auftrag eines Kindes aus seinem Spawn-Ereignis.
/// - `now`: Bezugszeit für laufende Dauern.
pub(crate) fn enrich_rows<F>(
    rows: &mut [AgentRow],
    monitor: &AgentMonitor,
    spawn_task: F,
    now: Instant,
) where
    F: Fn(&str) -> Option<String>,
{
    for row in rows.iter_mut() {
        if let Some(live) = monitor.agent(&row.id) {
            let info = AgentRowLive::from_live(live, now);
            row.tokens = Some(row.tokens.unwrap_or(0).max(info.total_tokens));
            row.tool_calls = Some(row.tool_calls.unwrap_or(0).max(info.tool_calls));
            if row.duration_ms.is_none() || info.active {
                row.duration_ms = Some(info.elapsed_ms);
            }
            if row.task.is_none() {
                row.task = live.task.clone();
            }
            row.live = Some(info);
        }
        if row.task.is_none() {
            row.task = spawn_task(&row.id);
        }
    }
}

/// Budget mit Auslastung, z. B. `Tokens 120000 (35 %) · Tools 80 (60 %) ·
/// Dauer 900000 ms (52 %)`; `None` ohne Deckel.
pub(crate) fn budget_text(row: &AgentRow) -> Option<String> {
    let limits = row.budget_limits;
    if limits.is_empty() {
        return None;
    }
    let part = |label: &str, limit: Option<u64>, used: Option<u64>, unit: &str| -> String {
        match limit {
            None => format!("{label} —"),
            Some(limit) => match used.and_then(|used| percent(used, limit)) {
                Some(pct) => format!("{label} {limit}{unit} ({pct} %)"),
                None => format!("{label} {limit}{unit}"),
            },
        }
    };
    Some(format!(
        "{} · {} · {}",
        part("Tokens", limits.tokens, row.tokens, ""),
        part(
            "Tools",
            limits.tool_calls.map(u64::from),
            row.tool_calls.map(u64::from),
            ""
        ),
        part("Dauer", limits.wall_ms, row.duration_ms, " ms"),
    ))
}

/// Ganzzahlige Auslastung in Prozent; `None` bei Deckel `0`.
pub(crate) fn percent(used: u64, limit: u64) -> Option<u64> {
    (limit > 0).then(|| {
        let pct = u128::from(used) * 100 / u128::from(limit);
        u64::try_from(pct).unwrap_or(u64::MAX)
    })
}

/// `true` für abgeschlossene Zustände (dauerhaft oder live).
pub(crate) fn is_finished(row: &AgentRow) -> bool {
    let terminal = matches!(
        row.status.as_str(),
        "completed" | "failed" | "cancelled" | "interrupted" | "done"
    );
    terminal || row.live.as_ref().is_some_and(|live| !live.active)
}

/// Kurzform des Auftrags für die Baumzeile.
pub(crate) fn short_task(task: &str) -> String {
    let single_line = task.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&single_line, TASK_TREE_CHARS)
}

/// Detailzeilen (vor der Bereinigung) für eine Zeile.
pub(crate) fn detail_lines(row: &AgentRow) -> Vec<String> {
    let state = row
        .live
        .as_ref()
        .map(|live| format!(" · {}", live.state))
        .unwrap_or_default();
    let tokens = match &row.live {
        Some(live) => format!("{} (gesamt {})", live.tokens_label(), live.total_tokens),
        None => known(row.tokens),
    };
    let duration = match &row.live {
        Some(live) => live.elapsed_label(),
        None => row
            .duration_ms
            .map_or_else(|| "—".to_owned(), |ms| format!("{} s", ms / 1000)),
    };
    let mut lines = vec![
        format!("{} · {}{state}", row.role, row.status),
        format!("ID: {}", row.id),
        format!("Eltern: {}", row.parent.as_deref().unwrap_or("—")),
        format!("Auftrag: {}", row.task.as_deref().unwrap_or("—")),
        format!(
            "Tokens: {tokens} · Tools: {} · Dauer: {duration}",
            known(row.tool_calls)
        ),
        format!(
            "Budget: {}",
            budget_text(row).unwrap_or_else(|| row.budget.clone())
        ),
        String::new(),
    ];
    if let Some(result) = &row.result {
        lines.push("Ergebnis:".to_owned());
        lines.push(result.clone());
    } else if is_finished(row) {
        match row.live.as_ref().and_then(|live| live.preview.clone()) {
            Some(preview) => {
                lines.push("Ergebnis (Ausschnitt):".to_owned());
                lines.push(preview);
            }
            None => lines.push("Noch kein Ergebnis.".to_owned()),
        }
    } else {
        let live = row.live.as_ref();
        match (
            live.and_then(|live| live.last_step.clone()),
            live.and_then(|live| live.preview.clone()),
        ) {
            (Some(step), preview) => {
                lines.push(format!("Letzter Schritt: {step}"));
                if let Some(preview) = preview {
                    lines.push(format!("Zwischenstand: {preview}"));
                }
            }
            (None, Some(preview)) => lines.push(format!("Zwischenstand: {preview}")),
            (None, None) => lines.push("Läuft …".to_owned()),
        }
    }
    lines
}

/// Baumzeile ohne Marker/Einrückung: Rolle, Status, Zustand, Tokens, Tools,
/// Dauer und gekürzter Auftrag.
pub(crate) fn tree_text(row: &AgentRow, status: &str) -> String {
    let mut text = format!("{} · {status}", row.role);
    match &row.live {
        Some(live) => {
            if live.active {
                text.push_str(&format!(" · {}", live.state));
            }
            text.push_str(&format!(
                " · {} · Tools {} · {}",
                live.tokens_label(),
                live.tool_calls.max(row.tool_calls.unwrap_or(0)),
                live.elapsed_label()
            ));
        }
        None => text.push_str(&format!(
            " · Tokens {} · Tools {}",
            known(row.tokens),
            known(row.tool_calls)
        )),
    }
    if let Some(task) = &row.task {
        text.push_str(&format!(" · „{}“", short_task(task)));
    }
    text
}

/// Unbekannte Werte bleiben `—` statt `0`.
pub(crate) fn known<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_core::{AgentEvent, AgentEventKind};
    use harw_protocol::{AgentOrchestrationEvent, AgentOrchestrationStatus, TurnEvent};
    use harw_types::{SessionId, TokenUsage, ToolCallId, TurnId};

    fn row(id: &str, parent: Option<&str>) -> AgentRow {
        AgentRow {
            id: id.to_owned(),
            parent: parent.map(str::to_owned),
            role: "root-orchestrator".to_owned(),
            depth: 1,
            status: "running".to_owned(),
            budget: "—".to_owned(),
            can_stop: true,
            budget_limits: BudgetLimits {
                tokens: Some(1000),
                tool_calls: Some(10),
                wall_ms: Some(900_000),
            },
            ..AgentRow::default()
        }
    }

    fn turn(agent: &str, event: TurnEvent) -> AgentEvent {
        AgentEvent {
            agent: SessionId::from_str(agent),
            parent: Some(SessionId::from_str("uia")),
            role: "root-orchestrator".to_owned(),
            kind: AgentEventKind::Turn(event),
        }
    }

    fn running_monitor(agent: &str) -> AgentMonitor {
        let mut monitor = AgentMonitor::default();
        let call = ToolCallId::new();
        monitor.apply(&turn(
            agent,
            TurnEvent::ToolCallRequested {
                turn_id: TurnId::new(),
                call_id: call.clone(),
                tool_name: "fs.read".to_owned(),
                arguments: serde_json::json!({"path": "a.rs"}),
            },
        ));
        monitor.apply(&turn(
            agent,
            TurnEvent::ToolCallCompleted {
                turn_id: TurnId::new(),
                call_id: call,
                result: harw_protocol::ToolCallResult::success(serde_json::json!({"content": "x"})),
                duration_ms: 3,
                placement: None,
            },
        ));
        monitor.apply(&turn(
            agent,
            TurnEvent::UsageUpdated {
                turn_id: TurnId::new(),
                round: TokenUsage::default(),
                turn_total: TokenUsage {
                    input_tokens: 400,
                    output_tokens: 100,
                    ..TokenUsage::default()
                },
                final_round: false,
            },
        ));
        monitor
    }

    /// Ein laufendes Kind zeigt Live-Tokens, Tools und Dauer statt `—`.
    #[test]
    fn running_child_shows_live_tokens_tools_and_duration() -> TestResult {
        let monitor = running_monitor("child-1");
        let mut rows = vec![row("child-1", Some("uia"))];
        enrich_rows(&mut rows, &monitor, |_| None, Instant::now());
        let enriched = rows.first().ok_or(TestError::Missing("Zeile"))?;
        let live = enriched.live.as_ref().ok_or(TestError::Missing("Live"))?;
        assert_eq!(live.tool_calls, 1);
        assert!(enriched.tokens.is_some_and(|tokens| tokens > 0));
        assert!(enriched.duration_ms.is_some());
        let tree = tree_text(enriched, &enriched.status);
        assert!(tree.contains("Tools 1"), "{tree}");
        assert!(!tree.contains("Tokens —"), "{tree}");
        let details = detail_lines(enriched).join("\n");
        assert!(details.contains("Tools: 1"), "{details}");
        assert!(details.contains(" s"), "{details}");
        assert!(details.contains("(10 %)"), "Tools-Auslastung: {details}");
        assert!(!details.contains("Noch kein Ergebnis"), "{details}");
        assert!(details.contains("Letzter Schritt"), "{details}");
        Ok(())
    }

    /// Der Auftrag kommt aus dem Spawn-Ereignis; im Baum gekürzt, im Detail
    /// vollständig.
    #[test]
    fn task_from_spawn_event_is_visible() -> TestResult {
        let long = "Analysiere die Konfiguration aller Crates und schreibe einen \
                    ausführlichen Bericht über jede gefundene Unstimmigkeit";
        let mut rows = vec![row("child-2", Some("uia"))];
        enrich_rows(
            &mut rows,
            &AgentMonitor::default(),
            |id| (id == "child-2").then(|| long.to_owned()),
            Instant::now(),
        );
        let enriched = rows.first().ok_or(TestError::Missing("Zeile"))?;
        let tree = tree_text(enriched, &enriched.status);
        assert!(tree.contains("„Analysiere"), "{tree}");
        assert!(!tree.contains("Unstimmigkeit"), "gekürzt: {tree}");
        let details = detail_lines(enriched).join("\n");
        assert!(details.contains(long), "vollständig: {details}");
        Ok(())
    }

    /// Die Wurzel heißt nach ihrer tatsächlichen Rolle.
    #[test]
    fn root_label_follows_uia_or_explicit_root() {
        assert_eq!(root_label(None, Some("assistant-ui")), "UIA · assistant-ui");
        assert_eq!(
            root_label(Some("root-orchestrator"), Some("assistant-ui")),
            "root-orchestrator"
        );
        assert_eq!(root_label(Some("  "), None), "UIA");
    }

    /// Ein fertiges Kind zeigt sein Ergebnis; ohne Ergebnis den Hinweis.
    #[test]
    fn finished_child_shows_result() {
        let mut done = row("child-3", Some("uia"));
        done.status = "completed".to_owned();
        done.result = Some("Bericht: alles konsistent.".to_owned());
        let details = detail_lines(&done).join("\n");
        assert!(details.contains("Ergebnis:"), "{details}");
        assert!(details.contains("alles konsistent"), "{details}");

        done.result = None;
        assert!(
            detail_lines(&done)
                .join("\n")
                .contains("Noch kein Ergebnis.")
        );

        let running = row("child-4", Some("uia"));
        assert!(
            !detail_lines(&running)
                .join("\n")
                .contains("Noch kein Ergebnis.")
        );
    }

    /// Ein Orchestrierungs-Auftrag im Monitor füllt eine fehlende Zeile auf;
    /// Budget-Prozente rechnen gegen die Live-Tokens.
    #[test]
    fn monitor_task_and_budget_percent() -> TestResult {
        let mut monitor = running_monitor("child-5");
        monitor.apply(&AgentEvent {
            agent: SessionId::from_str("child-5"),
            parent: Some(SessionId::from_str("uia")),
            role: "root-orchestrator".to_owned(),
            kind: AgentEventKind::Orchestration(AgentOrchestrationEvent {
                schema_version: AgentOrchestrationEvent::CURRENT_SCHEMA_VERSION,
                event_id: "e1".to_owned(),
                root_session_id: SessionId::from_str("uia"),
                parent_session_id: SessionId::from_str("uia"),
                child_session_id: SessionId::from_str("child-5"),
                turn_id: None,
                role: "root-orchestrator".to_owned(),
                depth: 1,
                task: Some("Baue X".to_owned()),
                status: AgentOrchestrationStatus::Running,
                usage: None,
                duration_ms: None,
                progress: None,
                detail: None,
                tool_calls: None,
                model: None,
                provider: None,
            }),
        });
        let mut rows = vec![row("child-5", Some("uia"))];
        enrich_rows(&mut rows, &monitor, |_| None, Instant::now());
        let enriched = rows.first().ok_or(TestError::Missing("Zeile"))?;
        assert_eq!(enriched.task.as_deref(), Some("Baue X"));
        let budget = budget_text(enriched).ok_or(TestError::Missing("Budget"))?;
        assert!(budget.contains("Tokens 1000 (50 %)"), "{budget}");
        assert_eq!(percent(5, 0), None);
        Ok(())
    }
}
