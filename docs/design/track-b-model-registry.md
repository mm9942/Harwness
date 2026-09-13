# Track B — Model Capabilities & Behavior-Profile Registry

**Owner-Split:** Yuehua 🌙 (Datenrecherche) → Alice ✨ (Rust-Implementierung)
**Reference:** philosophy.md §2 („Warum Modellvielfalt eine Runtime-Frage ist"), §6 („Das Modell darf Arbeit vorschlagen, aber niemals Prozesse besitzen")
**Status:** Design; `harw-model-catalog` erweitern, nicht ersetzen

## Kern-Aussage aus philosophy.md §2

> Deshalb reicht eine Provider-Abstraktion wie `async fn respond(messages, tools) -> ModelResponse` **nicht** aus. Ein leistungsfähiges Harness benötigt zwei getrennte Modelleigenschaften:
>
> - **Deklarierte Provider-Fähigkeiten** (`ModelCapabilities`) — die technische API-Oberfläche.
> - **Gemessenes Laufzeitprofil** (`ModelBehaviorProfile`) — wie das Modell unter *diesem* Harness tatsächlich arbeitet.

**Alice's Design-Prinzip:** Trennung strikt durchziehen. `Capabilities` sind kontrahiert (Provider-Doku), `BehaviorProfile` ist empirisch (versioniert, bewertbar, überschreibbar).

## Neue Typen (`harw-model-catalog/src/behavior.rs`)

```rust
/// Deklarierte technische Fähigkeiten (aus Provider-Doku).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub context_limit: usize,
    pub max_output_tokens: Option<usize>,
    pub supports_tools: bool,
    pub supports_parallel_tools: bool,
    pub supports_streaming: bool,
    pub supports_reasoning_effort: bool,
    pub supports_prompt_caching: bool,
    pub supports_images: bool,
    pub supports_audio: bool,
    pub supports_json_mode: bool,
}

/// Empirisches Verhalten unter unserer Runtime.
/// Alle Scores sind Ganzzahlen 0..=100 (kalibrierbar, JSON-freundlich).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBehaviorProfile {
    pub tool_call_reliability: u8,
    pub long_context_retention: u8,
    pub delegation_discipline: u8,
    pub schema_strictness: u8,
    pub compaction_tolerance: u8,
    pub retry_sensitivity: u8,
    pub preferred_task_granularity: TaskGranularity,
    pub safe_parallelism: u8,
    /// Datum der letzten Kalibrierung.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_calibrated: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskGranularity {
    /// Kleine, eng abgegrenzte Aufgaben.
    Focused,
    /// Mittlere Zerlegung.
    Balanced,
    /// Große Kontext-Sweeps, Langhorizont.
    Sweeping,
}

/// Rollen aus philosophy.md §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// Zerlegt, löst Konflikte auf, synthetisiert.
    Orchestrator,
    /// Große Repos, lange Kontext-Akkumulation.
    RepositoryWorker,
    /// Eng abgegrenzte Implementierungen.
    FocusedCoding,
    /// Konservativ, schemafest, prüfungsorientiert.
    Verifier,
    /// Günstig, schnell, read-only, hoch parallelisierbar.
    Scout,
}
```

## Erweiterung `ProviderSpec::models`

Aktuell ist `models: Vec<...>` ein Platzhalter. Neu: `Vec<ModelSpec>` mit:

```rust
pub struct ModelSpec {
    pub id: String,                        // z. B. "claude-opus-4-8"
    pub display_name: String,              // z. B. "Claude Opus 4.8"
    pub family: ModelFamily,               // enum: GptFamily, ClaudeFamily, ...
    pub capabilities: ModelCapabilities,   // deklariert
    pub behavior: Option<ModelBehaviorProfile>, // empirisch, initial None
    pub suggested_roles: Vec<ModelRole>,   // Router-Hinweis
    pub tags: Vec<String>,                 // free-form
}

pub enum ModelFamily {
    Gpt,        // OpenAI GPT-5.x, GPT-4.x
    Claude,     // Anthropic Opus/Sonnet/Haiku/Fable
    Mistral,    // Mistral Large/Medium/Small
    Glm,        // Zhipu GLM-4/5
    Kimi,       // Moonshot Kimi K1/K2
    Qwen,       // Alibaba Qwen 2/3
    Llama,      // Meta Llama 3/4
    Grok,       // xAI Grok
    Local,      // Ollama-hosted
}
```

## Router (`harw-model-catalog/src/router.rs`)

```rust
pub struct ModelRouter { catalog: Arc<Catalog> }

impl ModelRouter {
    pub fn select(&self, role: ModelRole, hints: &RoutingHints) -> Option<ModelSelection>;
}

pub struct RoutingHints {
    pub prefers_families: Vec<ModelFamily>,
    pub min_context: Option<usize>,
    pub needs_tools: bool,
    pub max_cost_tier: CostTier,
}

pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
    pub reason: String,
}
```

**Auswahl-Algorithmus (philosophy.md §2, §16 Prinzip 6 „semantisch verlustfrei"):**

1. Kandidaten = Modelle, die die harten Constraints erfüllen (context, tools).
2. Wenn eine `behavior`-Profile-Zahl den `role` bevorzugt (z. B. `long_context_retention ≥ 80` für `RepositoryWorker`), Prioritäts-Bonus.
3. Falls `prefers_families` gesetzt: Familien-Match als Tiebreaker.
4. Deterministisch: bei Gleichstand alphabetisch nach `model_id`.

## Yuehua's Rechercheauftrag (Datenrohstoff)

**Format:** `docs/design/model-registry-data.md` mit einer Tabelle pro Modellfamilie. Jede Zeile:

| Field | Quelle |
|-------|--------|
| Model ID | Provider-Doku |
| Display Name | Provider-Doku |
| Context Limit | Provider-Doku |
| Max Output Tokens | Provider-Doku |
| Supports Tools | Provider-Doku |
| Supports Parallel Tools | Provider-Doku |
| Supports Streaming | Provider-Doku |
| Supports Reasoning Effort | Provider-Doku |
| Supports Prompt Caching | Provider-Doku |
| Supports Images | Provider-Doku |
| Suggested Role(s) | Yuehua-Urteil auf Basis der offiziellen Positionierung |
| Notes | Kurz, Quelle mit URL |

**Modell-Familien zu recherchieren:**

- **OpenAI GPT**: GPT-5.6, GPT-5, o1/o3 falls noch relevant.
- **Anthropic Claude**: Opus 4.8, Sonnet 5, Haiku 4.5, Fable 5 (bereits im Repo — Yuehua ergänzt Behavior-Profile-Schätzwerte).
- **Mistral**: Large 2, Medium 3, Small 3.
- **Zhipu GLM**: GLM-5.2 (philosophy.md §2 nennt das Modell explizit).
- **Moonshot Kimi**: K2 (philosophy.md §2 nennt es).
- **Alibaba Qwen**: Qwen 2.5, Qwen 3.
- **Meta Llama**: Llama 3.3, Llama 4 (falls verfügbar).
- **xAI Grok**: Grok 4.5 (philosophy.md §2 nennt es).

**Yuehua's Grenzen:**
- Nur öffentliche Provider-Docs oder autoritative Quellen (nie Marketing-Copy ohne Fußnote).
- `BehaviorProfile`-Zahlen sind Initial-Schätzwerte auf Basis der offiziellen Positionierung, **nicht** eigene Benchmarks. Klar so kennzeichnen.
- Bei Unsicherheit: `None` lassen, nicht raten.

## Bindung an philosophy.md

- **§2** — Trennung `Capabilities` (deklariert) vs. `BehaviorProfile` (empirisch): strukturell durchgezogen.
- **§6** — „Das Modell darf Arbeit vorschlagen, aber niemals Prozesse besitzen": Der Router entscheidet server-seitig; ein LLM-Tool-Call darf **nie** `model_id` überschreiben — nur eine `ModelRole` vorschlagen.
- **§12** — Authority-Monotonie: `RoutingHints` können nur einschränken, niemals erweitern (`min_context`, `max_cost_tier`).
- **§16 Prinzip 6** — „Provider-Adapter müssen semantisch verlustfrei sein": `ModelCapabilities` verhindert, dass Modell-spezifische Features (Reasoning-Effort, Prompt-Caching) hinter einem kleinsten gemeinsamen Nenner verschwinden.

## Alice's Bau-Reihenfolge (nach Yuehua)

1. `harw-model-catalog/src/behavior.rs` — Typen (Capabilities, BehaviorProfile, TaskGranularity, ModelRole, ModelFamily).
2. `ModelSpec` in `spec.rs` neu, `models: Vec<ModelSpec>` in `ProviderSpec` real füllen.
3. `harw-model-catalog/src/router.rs` — Auswahl-Algorithmus.
4. Yuehua's `model-registry-data.md` → `embedded.rs` als eingebettete Katalog-Erweiterung.
5. Tests: per Familie eine Round-Trip-Serde-Runde + ein Router-Test pro Rolle.

## Akzeptanzkriterien

- `cargo test -p harw-model-catalog` grün.
- `cargo clippy -p harw-model-catalog --all-targets -- -D warnings` sauber.
- Alle Modellfamilien aus §2 im Katalog vertreten.
- Router liefert für jede Rolle mindestens einen Kandidaten (sonst Test-Fail).
- Kein `unwrap()`/`expect()` in Produktionspfaden.
