# Track A — harw-memory Promotion/Decay-Runtime

**Owner:** Anastasya 🖤
**Reference:** philosophy.md §4 („Memory-Konsolidierung ist ein langlebiger Workflow")
**Status:** Design für M3-Vollausbau (M3-Detection ist bereits ausgeliefert)

## Invarianten (aus philosophy.md)

1. **Never infer from silence.** Nur explizite Signale (Correction, Reflection, PatternHint) sind zulässige Trigger.
2. **Promotion nach 3× in 7 Tagen** (ClawHub-Regel).
3. **Decay 30d unused → COLD**, **90d unused → warnen** — nie stumm löschen.
4. **Watermark = Buchhaltung, Workspace-Dirtiness = Arbeitsentscheidung.** Der reale Signal-Log ist die Wahrheit, nicht `state.json`.
5. **Idempotent + State-Machine.** Wiederanlauf beginnt am letzten committeten Schritt.
6. **Local-first**, keine Netz-Requests.

## Aktueller Stand (M1/M2/M3-Detection fertig)

- `FileMemoryStore::maintain()` läuft die 5-Schritte-State-Machine durch, aktualisiert `signals_seen`, schreibt aber **keine** WARM/COLD-Dateien.
- `detect_correction()` (M3-Detection) erkennt Korrekturen aus User-Text.
- `Signal::PatternHint { key, note }` ist definiert, wird aber nirgends aggregiert.

## Was gebaut werden muss

### 1. `src/promote.rs` — neues Modul

```rust
/// Ergebnis eines einzelnen Promotion-Passes.
pub struct PromotionOutcome {
    pub warm_created: usize,
    pub demoted_to_cold: usize,
    pub archived_warnings: Vec<String>,
}

/// Wertet den Signal-Log seit `since` aus und liefert Kandidaten.
pub fn evaluate_signals(
    signals: &[Signal],
    now: OffsetDateTime,
    window: Duration,
    threshold: usize,
) -> Vec<PromotionCandidate>;

pub struct PromotionCandidate {
    pub namespace: String,   // z. B. "correction/das-ist-falsch"
    pub content: String,     // aggregierte Zeilen
    pub hit_count: usize,
}
```

### 2. Änderung an `FileMemoryStore::maintain()`

Im Schritt `WorkspaceSynced → AgentCompleted`:

1. Signal-Log seit `state.last_maintenance` einlesen (nicht alles, nur Diff).
2. `evaluate_signals(...)` aufrufen mit Fenster `Duration::days(7)` und Threshold `3`.
3. Für jeden Kandidaten: `warm/<namespace>.md` schreiben oder erweitern (append, mit Zeitstempel-Zeile).
4. `warm_created` und `demoted_to_cold` in den `MaintenanceReport` einfließen.

Im Schritt `AgentCompleted → BaselineCommitted`:

1. Alle WARM-Dateien inspizieren, `last_used` in `state.json` vergleichen.
2. Datei ohne Nutzung seit `Duration::days(30)`: nach `cold/` verschieben (atomic rename).
3. Datei in `cold/` ohne Nutzung seit `Duration::days(90)`: `archived_warnings.push(...)` — aber niemals löschen.

### 3. `INDEX.md`-Writer

Nach jedem `maintain()`-Erfolg neu schreiben:

```markdown
# Memory Index (updated 2026-07-16T00:23Z)

## HOT (`HOT.md`, X Zeilen)

## WARM
| Namespace | Zeilen | Zuletzt genutzt |
|-----------|--------|-----------------|

## COLD
| Namespace | Zeilen |
```

### 4. Neues Feld in `state.json`

```json
{
  "last_maintenance": "…",
  "signals_seen": 42,
  "usage_index": { "namespace/foo": {"count": 3, "last_used": "…"} },
  "promoted_to_hot": 0,
  "demoted_to_cold": 0,
  "warm_created": 0,
  "cold_warnings_last_pass": []
}
```

## Tests (mindestens)

1. `evaluate_signals` mit 3 identischen `PatternHint`-Keys in 7d → 1 Kandidat mit `hit_count=3`.
2. Nur 2 identische Keys → keine Kandidaten (Threshold).
3. 3 Keys, aber verteilt über 8 Tage → keine Kandidaten (Fenster).
4. WARM-Datei existiert bereits, neue Signale hängen an — nur eine neue Zeile pro Signal.
5. WARM-Datei 31 Tage unused → nach `cold/`. Original-Datei ist weg, `cold/` hat Inhalt.
6. COLD-Datei 91 Tage unused → `archived_warnings.push`; Datei bleibt an ihrem Platz.
7. Wiederanlauf nach Crash zwischen `AgentCompleted` und `BaselineCommitted` → gleicher Report, keine Doppel-Writes (INDEX.md ist idempotent).

## Grenzen (bewusst NICHT in diesem Track)

- **Keine LLM-Konsolidierung.** Das ist Track #7 (M4, opt-in, braucht Provider).
- **Kein automatischer HOT-Editor.** WARM→HOT bleibt manuell via `/memory promote <ns>` (das ist Track B/UX).
- **Keine cross-namespace-Merges.** Ein Kandidat entspricht einem Namespace-Slot.

## Akzeptanzkriterien

- `cargo test -p harw-memory` grün mit mindestens 6 neuen Tests.
- `cargo clippy -p harw-memory --all-targets -- -D warnings` sauber.
- Bestehende 16 Tests bleiben grün (keine Regression).
- `maintain()` bleibt idempotent unter Wiederanlauf (Test 7 oben).
