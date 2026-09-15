# Token-Effizienz: Maßnahmenkatalog

Stand: 2026-09-14. Quelle: lokale Code-Analyse (kein Web-Zugriff aus dieser Rolle);
Provider-Mechaniken sind als bewährtes Wissen eingeordnet, vor Umsetzung gegen
die jeweilige aktuelle API-Doku verifizieren.

## Vorhandene Grundlagen (bereits im Code)

- `harw-core::history::ConversationHistory::tail_within_estimated_bytes` —
  bytebudgetierter Historien-Tail, bricht nie Call/Result-Paare.
- `harw-core::context_budget` — typisierte Assembly-Pipeline
  (Gathered → Admitted → Budgeted → Rendered) mit `ContextBudget`.
- `harw-context` — `DetailMode::{Full, Summary, References}`, `SectionStrength`,
  Trust-Klassen; Verweis- statt Volltext-Rendering (`reference.rs`).
- `harw-context::ContextBudgetSpec::tighten` — monotone Budgetschnitte,
  Kinder erben nie mehr Kontext als Eltern.
- TUI: `/compact`-Command-Support (Commit 9d4a773).
- Subagents: Child-Contract liefert bereits nur strukturierte Rückgaben
  (Finding/Summary), keine rohen Transkripte (`child.returns` summary,
  `child.raw_transcript.*` excluded in `orchestrate.toml`).

## Lücke 1: Provider-seitiges Prompt-Caching

Kein `cache_control`/Äquivalent in `harw-provider-http`. Maßnahmen:

1. **Anthropic**: `cache_control: {"type": "ephemeral"}`-Breakpoints an
   System-Prompt, Tool-Definitionen und stabile History-Präfixe. Cache-Write
   kostet 1,25×, Cache-Read 0,1× des Basispreises — ab dem 2. Hit gewinnt es.
   TTL: 5 min (Standard) bzw. 1 h.
2. **OpenAI**: automatisches Prefix-Caching (≥1024 Tokens, 128er-Inkremente);
   dafür müssen Präfixe **stabil** sein: statische Teile (System, Tools)
   strikt vorne, keine Timestamps/Randoms im Präfix.
3. **Gemeinsame Vorbedingung**: Reihenfolge-Invarianz der Prompt-Front.
   → Prüfpunkt: `harw-core::context_budget::Rendered` darf keine
   nichtdeterministische Serialisierung (BTreeMap ist ok, HashMap nicht).

## Lücke 2: Kompaktierung langer Sessions

`/compact` existiert nur in der TUI, nicht als Turn-Loop-Mechanik. Maßnahmen:

1. **Threshold-Kompaktierung**: wenn `tail_within_estimated_bytes` mehr als
   x % droppen müsste, stattdessen Summarize-Turn: Modell verdichtet
   ältere Items zu einem `SummaryItem`, Tail bleibt unangetastet.
2. **Tool-Result-Verdrängung**: große, alte Tool-Ergebnisse (fs.read, shell)
   durch ihre ersten N Zeilen + Verweis ersetzen; Call/Result-Paarung bleibt
   (Invariante existiert bereits).
3. **Deterministische Offline-Kompaktierung**: Fehleritems und wiederholte
   gleiche Tool-Calls ohne Modellaufruf zusammenfassen.

## Lücke 3: Subagent-Verdichtung

Bereits gut: Kinder liefern nur Findings, keine Transkripte. Verbleibend:

1. **Return-Budget**: `child.returns` in `orchestrate.toml` ist `summary`,
   aber ohne hartes Byte-Limit — über `ContextBudgetSpec.per_section`
   deckeln (Mechanik existiert).
2. **Dedup über parallele Kinder**: mehrere Explorer auf demselben Scope
   liefern überlappende Funde; Dedup beim Join (gleiche `digest` in
   EvidenceRef existiert bereits als Schlüssel).

## Lücke 4: Kontext-Programm-Disziplin

- `DetailMode::References` in mehr Kontextprogrammen nutzen (aktuell nur
  punktuell gesetzt; `orchestrate.toml` begründet bewusst den Verzicht).
- `must-include` nur für wirklich entscheidungsrelevante Sektionen; Rest
  `preferred`, damit das Budget zuerst dort kürzt.

## Lücke 5: Tool-Ausgaben an der Quelle

- `fs.read`/`shell.exec` kappen bereits (64 KiB bzw. Output-Limits); zusätzlich
  `grep`-vor-`read` als Anweisung in den Baseline-Instruktionen verankern
  (betrifft `harw-instructions`), damit Modelle gar nicht erst breit lesen.

## Priorität (Impact/Aufwand)

1. Anthropic `cache_control` + stabile Prompt-Front (hoch/mittel).
2. Threshold-Kompaktierung im Turn-Loop (hoch/mittel).
3. Tool-Result-Verdrängung in History (hoch/gering — Infrastruktur da).
4. Return-Budget per_section (mittel/gering).
5. Baseline-Instruktion „grep vor read" (gering/sehr gering).
