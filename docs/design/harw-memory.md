# harw-memory — Long-Term Memory & Continuous Improvement

**Status:** Design (2026-07-16)
**Owner:** harw-memory crate

## Zweck

Persistente, tokensparsame Memory-Schicht für Agent-Sessions. Kombiniert das
Beste aus drei Inspirationen:

- **ClawHub `memory` / `self-improving`** — Tiered HOT/WARM/COLD, Indices, "never infer from silence".
- **Codex `memories/`** — Zwei-Phasen-Pipeline: Rollout-Extraktion (parallel) → globale Konsolidierung (seriell, LLM-Agent).
- **Hermes `~/.hermes/memories/MEMORY.md`** — radikale Minimalität als Fallback-Kern.

## Nicht-Ziele

- Keine externe DB (SQLite optional in Phase 2, nicht MVP).
- Kein automatischer LLM-Call auf jedem Turn.
- Kein "Silent Inference" — es wird nur gelernt, was explizit signalisiert wird
  (Korrektur, Reflexion nach Task, ≥3× wiederholte Instruktion).
- Kein Ersatz für Skill-Configs oder Rollen-Definitionen.

## Kernprinzipien

1. **Token-Budget zuerst.** HOT ist per Konstruktion ≤100 Zeilen (~2 KB). WARM/COLD werden nur geladen, wenn ein Match zutrifft.
2. **Explizit vor implizit.** Signale werden angehängt, wenn der User korrigiert oder eine Reflexion nach einem Turn signalisiert.
3. **Append-only + periodische Konsolidierung.** Signale werden append-only geschrieben; Promotion/Demotion läuft in einer separaten `maintain()`-Pass (idempotent).
4. **Local-first.** Alle Daten auf der lokalen Platte, kein Netzwerkzugriff im Standardpfad.
5. **Verhaltenskompatibel mit Hermes.** Wenn kein Setup existiert, verhält sich das Crate wie Hermes: eine `MEMORY.md`, `§`-delimitiert.

## Layout auf Platte

Default-Wurzel: `<HARWNESS_HOME>/memory/` (fällt auf `~/.harwness/memory/` zurück).

```
<home>/memory/
├── HOT.md                # ≤100 Zeilen; immer geladen; von Hand oder Consolidation-Agent geschrieben
├── INDEX.md              # Tier-Übersicht (Zähler, Timestamps)
├── warm/
│   ├── INDEX.md          # <namespace> → Datei-Pfad, size, last_used
│   └── {namespace}.md    # z. B. project/harwness.md, domain/rust.md
├── cold/
│   └── {archived}.md     # Namensschema wie warm/
├── signals/
│   ├── corrections.jsonl # append-only, NDJSON
│   ├── reflections.jsonl # append-only
│   └── patterns.jsonl    # candidates mit Zähler
└── state.json            # last_maintenance, usage_counters, promotion_state
```

## Tiers

| Tier | Ort | Größenlimit | Load-Semantik | Persistenz |
|------|-----|-------------|---------------|------------|
| HOT | `HOT.md` | ≤100 Zeilen | Bei jedem Session-Start | User- oder Agent-editiert |
| WARM | `warm/{ns}.md` | ≤200 Zeilen/Datei | Bei `recall(query)` mit Namespace-/Keyword-Match | Auto (Promotion aus signals) |
| COLD | `cold/{ns}.md` | unbegrenzt | Nur bei expliziter Anfrage | Auto (Demotion aus WARM) |

## Übergänge

- **Signal → WARM:** Ein Muster in `patterns.jsonl` wird bei ≥3 Vorkommen in 7 Tagen zu einer WARM-Zeile (mit User-Bestätigung — nicht stumm).
- **WARM → HOT:** Explizite Nutzer-Bestätigung (`/memory promote <ns>`) oder ≥3 Recall-Hits in 7 Tagen.
- **WARM → COLD:** 30 Tage kein Recall.
- **COLD → gelöscht:** Nie ohne User-Bestätigung.

## Rust-API (Skeleton)

```rust
pub trait Memory: Send + Sync {
    /// HOT tier als kompletter Text (≤100 Zeilen garantiert).
    fn hot(&self) -> Result<String, MemoryError>;

    /// Recall WARM/COLD anhand Namespace + Keyword.
    fn recall<'a>(&self, query: RecallQuery<'a>) -> Result<Vec<Entry>, MemoryError>;

    /// Signal anhängen (append-only).
    fn record(&self, signal: Signal) -> Result<(), MemoryError>;

    /// Wartung: Promotion, Decay, Compaction. Idempotent, seriell (single-lock).
    fn maintain(&self) -> Result<MaintenanceReport, MemoryError>;

    /// Statistik ohne Content-Load — nur Zähler/Timestamps.
    fn stats(&self) -> Result<Stats, MemoryError>;
}

pub enum Signal {
    Correction { text: String, context: Option<String> },
    Reflection { context: String, lesson: String },
    PatternHint { key: String, note: String },
}

pub struct RecallQuery<'a> {
    pub namespace: Option<&'a str>,
    pub keywords: &'a [&'a str],
    pub include_cold: bool,
    pub limit: usize,
}
```

## Integration in harwness

- **OpContext-Service.** `Arc<dyn Memory>` in `ServiceMap`. Ops holen es per `ctx.service::<Arc<dyn Memory>>()`.
- **System-Prompt-Injection.** Der Turn-Loop hängt `memory.hot()` an den System-Prompt an, nach den Rollen-Instructions.
- **Slash-Command `/memory`.** Subcommands: `list`, `recall <keywords>`, `record correction <text>`, `record reflection <lesson>`, `promote <ns>`, `demote <ns>`, `stats`, `maintain`.
- **Tracing.** Jeder `recall` / `record` / `maintain` emittiert Spans mit `namespace`, `tier`, `hit_count`, `bytes_loaded` — direkt in die Wave-3-Token-Statuszeile speisbar.

## Vergleich zu den drei Inspirationen

| Aspekt | Hermes | Codex | ClawHub | **harw-memory** |
|--------|--------|-------|---------|-----------------|
| Tiers | keine | 2 Phasen | 3 (HOT/WARM/COLD) | 3 |
| Trigger | manuell | Session-Start (async) | Auf Signal | Auf Signal + optional Session-Start |
| Konsolidierung | keine | LLM-Agent (Phase 2) | Regel-basiert | Regel-basiert + LLM opt-in |
| Storage | 1 Datei | State-DB + Git-Baseline | Dateien + INDEX | Dateien + JSONL-Signals |
| Learning-Quelle | User schreibt | Rollout-Extraktion | Korrekturen | Korrekturen + Reflexionen |
| Silent Inference | ja | ja (LLM entscheidet) | **nein** | **nein** |
| Token-Kosten | 8 Zeilen | hoch (Phase 1+2) | mittel | **niedrig** (HOT ≤100 Zeilen) |

## Milestones

- **M1** — Crate-Skeleton + `FileMemoryStore` + trait + Tests (kein LLM, keine Consolidation-Pipeline).
- **M2** — OpContext-Integration + `/memory`-Slash-Command + System-Prompt-Injection.
- **M3** — Signal-Auto-Detection (`Correction`-Heuristik auf User-Nachrichten).
- **M4** — Opt-in Consolidation-Agent (Codex-Phase-2-Analog): LLM merged WARM→HOT auf Anfrage.

## Sicherheit

- Keine Secrets in Memory (Redaction-Filter auf `record`).
- Kein Netzwerkzugriff im Standardpfad.
- `boundaries.md`-Regel: nie Credentials, Health-Data, Third-Party-PII.

## Offene Fragen

1. Sollen Memories per-Session sein oder global? (Vorschlag: global per default, per-Session als Namespace).
2. Wie interagiert `harw-memory` mit `harw-session-store`? (Vorschlag: getrennt — Session-Store ist Turn-History, Memory ist verdichtete Erkenntnis).
3. Format `HOT.md` als frei-Text oder strukturiert? (Vorschlag: `§`-delimitiert wie Hermes, plus optionale YAML-Front-Matter je Block).
