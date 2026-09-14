# C-MODEL — Model-/Stop-/Reasoning-/Fehlervertrag (`harw-core/src/{model,history}.rs`)

Welle: W3 (Verträge), Teil B. Rolle: focused-coding-task (opus, laut Plan `eventual-wandering-pebble.md` Teil B,
W3-Tabelle Zeile `C-MODEL`). Build-Policy eingehalten: kein `cargo build/check/test/clippy/run/add`, kein
`make`/`rustc`/`rust-analyzer`, keine Git-Schreibbefehle; erlaubt war nur `cargo metadata --offline --no-deps`
(nicht benötigt). Verifikation ausschließlich durch Lesen (Signaturen, Imports, Macro-Expansion-Logik,
Cargo.toml-Abhängigkeiten).

## Geänderte Dateien

- `harw-core/src/model.rs` — additive Felder auf `ModelRequest`/`ModelResponse`, neuer Typ `StopReason`,
  erweiterter `ModelError`-Vertrag + `is_retryable()`, angepasste Doku, angepasste/neue Tests.
- `harw-core/src/history.rs` — ein neuer Import (`ReasoningItem`, nur in `mod tests`) und ein
  Regressions-/Roundtrip-Test; **keine** Verhaltensänderung an der bestehenden Logik (wie im Auftrag
  „(unverändert)" gefordert).
- **neu** `docs/remediation/ledger/W3/C-MODEL.md` (diese Datei).

Keine weiteren Dateien angefasst. `harw-tui/**`, `harw-cli/**`, `harw-runtime/**`, `harw-core/src/session.rs`
waren gesperrt und wurden nicht gelesen oder geändert.

## Eingefrorene öffentliche Signaturen (exakt wie geschrieben)

```rust
// harw-core/src/model.rs

pub struct ModelRequest {
    pub system_prompt: String,
    pub instruction_fragments: Vec<String>,
    pub context: Vec<ContextFragment>,           // unverändert (Legacy, dead per F-016/G-023)
    pub history: ConversationHistory,
    pub tools: Vec<ToolSpec>,
    pub context_assembly: ContextAssembly,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub model_id: Option<ModelId>,
    pub provider_id: Option<ProviderId>,
    // --- W3/C-MODEL, additiv ---
    pub data_block: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub tool_result_max_bytes: Option<usize>,
}

impl ModelRequest {
    // unverändert: new, with_context_budget, with_context_program (Verhalten s.u.),
    // with_reasoning_effort, with_model_id, with_provider_id
    // --- W3/C-MODEL, additiv ---
    pub fn with_data_block(mut self, data_block: Option<String>) -> Self;
    pub fn with_max_output_tokens(mut self, max_output_tokens: Option<u32>) -> Self;
    pub fn with_tool_result_max_bytes(mut self, tool_result_max_bytes: Option<usize>) -> Self;
}

// --- W3/C-MODEL, neuer Typ, exakt wie im Auftragstext gefordert ---
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    #[default]
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    PauseTurn,
    Refusal { detail: Option<String> },
    ContextWindowExceeded,
    ContentFilter,
    Other(String),
}

pub struct ModelResponse {
    pub message: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    // --- W3/C-MODEL, additiv ---
    pub stop: StopReason,
    pub reasoning: Option<harw_protocol::OpaqueReasoning>,
}
// impl ModelResponse::text/is_final unverändert (text() setzt stop: EndTurn, reasoning: None)

#[derive(Debug, HarwError)]
pub enum ModelError {
    RequestFailed(String),                 // unverändert
    EmptyResponse,                         // unverändert
    RateLimited { retry_after_secs: u64, message: String },  // unverändert (Legacy, s. Offene Annahmen)
    SerdeJson(serde_json::Error),          // unverändert, #[from]
    ContextAssembly(ContextAssemblyError), // unverändert, #[from]
    // --- W3/C-MODEL, additiv ---
    Refusal { detail: Option<String> },
    Truncated { message: String },
    Transient { status: Option<u16>, retry_after_secs: Option<u64>, message: String },
    Auth { message: String },
    QuotaExceeded { message: String },
    ContextLength { message: String },
    Timeout { message: String },
    Cancelled,
}

impl ModelError {
    pub fn is_retryable(&self) -> bool; // true nur für Transient/Timeout
}
```

Deckt sich mit dem Auftragstext und mit `eventual-wandering-pebble.md` Zeile 193-194 („Gefrorene
W3-Signaturen (Auszug)"): `ModelRequest.data_block/.max_output_tokens/.tool_result_max_bytes`;
`StopReason{EndTurn,ToolUse,MaxTokens,StopSequence,PauseTurn,Refusal{..},ContextWindowExceeded,
ContentFilter,Other}`; `ModelResponse.stop/.reasoning`; `ModelError += Refusal|Truncated|
Transient{status,retry_after_secs,..}|Auth|QuotaExceeded|ContextLength|Timeout|Cancelled` + `is_retryable()`.

## Feldwahl der nicht wörtlich vorgegebenen Varianten (hiermit eingefroren)

Der Auftrag gab nur `Transient{status,retry_after_secs,message}` wörtlich vor; die übrigen Varianten
sollten „minimal und sinnvoll" sein. Entscheidung:

| Variante | Felder | Begründung |
|---|---|---|
| `Refusal` | `detail: Option<String>` | spiegelt `StopReason::Refusal` — derselbe Provider-Begriff, hier als harter Aufruf-Fehler statt regulärem Turn-Ende |
| `Truncated` | `message: String` | Diagnosetext; kein `StopReason`-Duplikat, weil `MaxTokens` bereits ein reguläres, vom Provider selbst gemeldetes Limit abdeckt — `Truncated` ist der *unerwartete* Abbruch (z. B. Streamverbindung bricht mitten im Body) |
| `Auth` | `message: String` | 401/403 bzw. ungültiges Credential |
| `QuotaExceeded` | `message: String` | Kontingent/Billing erschöpft — bewusst **kein** `retry_after_secs`, weil Wiederholung laut Auftrag nicht sinnvoll ist |
| `ContextLength` | `message: String` | Provider lehnt den Request wegen Kontextlänge komplett ab (vs. `StopReason::ContextWindowExceeded`, eine reguläre Antwort mit dieser Begründung) |
| `Timeout` | `message: String` | Diagnosetext (z. B. konfiguriertes Zeitlimit) |
| `Cancelled` | — (Unit) | reiner Zustand, kein Zusatzkontext nötig — der Grund lebt in `harw_core::cancel::CancelReason` (W3/C-CANCEL), nicht hier verdoppelt |

`#[msg(...)]`-Texte referenzieren bewusst **keine** `Option<...>`-Felder direkt (z. B. `Refusal.detail`),
weil `harw-macros/src/error.rs`s generierter `Display`-Match jedes referenzierte Feld über `write!`
interpoliert und `Option<String>` kein `Display` implementiert — Verifikation durch Lesen von
`harw-macros/src/error.rs:150-169` (`display_arm`, `Fields::Named`-Zweig: bindet alle Feldnamen, interpoliert
aber nur, was im `#[msg]`-String vorkommt) und Zeile 90 (`#[allow(unused_variables)]` auf `fmt`, deckt
absichtlich nicht referenzierte Felder ab — dieselbe „inhaltsfreie Meldung"-Konvention, die
`RateLimited`/`ContextAssembly` bereits nutzen, s. Kommentar `error.rs:82-89`).

## Semantik (wie umgesetzt)

### `data_block` (F-016/G-023)

`ModelRequest.context: Vec<ContextFragment>` wird laut Befund von keinem der drei Wire-Builder gelesen
(`harw-provider-http/src/lib.rs:858,1013`, `anthropic.rs:376-381`, `x-findings-register-w1-w3.md:95`).
`ModelRequest::with_context_program` legte den AW4-01-Datenblock vor diesem Knoten ausgerechnet dort ab
(`ContextFragment{label:"context.data_block",..}` in `context`) — also im selben toten Feld. Diese Änderung
entfernt diesen Push und legt den Datenblock stattdessen in das neue `data_block: Option<String>` ab
(`None`, wenn der gerenderte Block leer ist). `with_context_budget` (Legacy-Pfad, viele Aufrufer inkl.
`harw-tui`) bleibt **exakt unverändert** — `data_block` ist dort immer `None`; die bytegleiche
Fallback-Garantie (`test_with_context_program_without_program_matches_with_context_budget`) bleibt
unangetastet, weil dieser Test nur den `program=None`-Zweig prüft, der `data_block` nie berührt.

Das tatsächliche *Lesen* von `request.data_block` durch einen Provider (Einfügen nach den
`tool_result`-Blöcken, cache-stabil) ist **nicht** Teil dieses Knotens — das ist laut Plan Zeile 225
(`A-ANTH`) und Zeile 226 (`A-OAI`) Folgearbeit von W4a. Diese Datei liefert nur den Transportvertrag.

### `max_output_tokens` / `tool_result_max_bytes`

Reine additive Provider-Hinweise ohne Logik in diesem Modul; `None` in allen bestehenden Konstruktoren.
Setter `with_max_output_tokens`/`with_tool_result_max_bytes` analog zu `with_reasoning_effort` gebaut
(gleiches Builder-Muster, gleiche Doku-Tiefe wie die Nachbarmethoden in derselben `impl`).

### `StopReason` / `ModelResponse.stop`

Additiv, ersetzt nichts. `ModelResponse::is_final()` bleibt unverändert (`tool_calls.is_empty()`) —
der Auftrag verlangte keine Änderung dieser Methode, und eine Änderung ihrer Semantik wäre ein
Verhaltensbruch für alle bestehenden Aufrufer außerhalb dieser Zuständigkeit. `ModelResponse::text()`
setzt `stop: StopReason::EndTurn, reasoning: None` — konsistent mit „reine Text-Antwort beendet den Turn
regulär". `#[derive(Default)]` auf `ModelResponse` bleibt möglich, weil `StopReason: Default` (via
`#[default] EndTurn`) und `Option<OpaqueReasoning>` immer `Default` ist.

### `ModelResponse.reasoning` / `harw_protocol::OpaqueReasoning` (G-015)

Verwendet exakt den von C-PROTO zugesagten Pfad/Typ `harw_protocol::OpaqueReasoning{provider, model,
blocks: Vec<serde_json::Value>}` (Re-Export auf Crate-Root laut Auftragstext). **Zum Zeitpunkt dieser
Bearbeitung existierte `OpaqueReasoning` noch nicht in `harw-protocol/src/items.rs` bzw. `lib.rs`**
(verifiziert durch Lesen — C-PROTO lief parallel). Das ist ein **stubbed import**, kein Fehler dieser
Arbeit: sobald C-PROTO `pub struct OpaqueReasoning { pub provider: ProviderId /* oder String */, pub
model: ModelId /* oder String */, pub blocks: Vec<serde_json::Value> }` mit mindestens `Debug + Clone`
liefert und es auf Crate-Root re-exportiert, kompiliert `use harw_protocol::OpaqueReasoning;` unverändert.
**Offene Annahme (an Orchestrator zu verifizieren):** `OpaqueReasoning` muss `Debug` und `Clone`
implementieren, weil `ModelResponse` beides ableitet und `Option<OpaqueReasoning>` das transitiv fordert.

Das tatsächliche Befüllen von `ModelResponse.reasoning` (Anthropic `thinking`/`redacted_thinking`-Blöcke
unverändert extrahieren) und das Zurückspielen in den nächsten Request ist **Folgearbeit von W4a**
(A-ANTH: „Thinking/redacted_thinking als OpaqueReasoning unverändert zurückspielen", Plan Zeile 225;
A-LOOP für die History-Verdrahtung). Diese Datei liefert nur den Transporttyp.

### `history.rs` — Reasoning-Blöcke bleiben speicherbar/zurückspielbar (unverändert)

Der Auftrag verlangte ausdrücklich **keine** Verhaltensänderung an `history.rs` für Reasoning — nur den
Nachweis, dass die bestehende Fähigkeit, `TurnItem::Reasoning(ReasoningItem)` zu speichern und
wiederherzustellen, durch die Vertragsänderungen in `model.rs` nicht beschädigt wird. `ReasoningItem`
(`harw-protocol/src/items.rs:97-104`, Felder `id`, `summary_text: Vec<String>`, `raw_content: Vec<String>`)
wurde **nicht** geändert — das ist C-PROTOs Datei, nicht meine Zuständigkeit. `to_model_messages()` lässt
`TurnItem::Reasoning` weiterhin bewusst aus (`history.rs:175`, unverändert) — das ist exakt die Stelle, die
G-015 als Ursache nennt (`harw-provider-http/anthropic.rs:462`; `history.rs:175`); ihre tatsächliche
Behebung (Reasoning-Blöcke aus `ModelResponse.reasoning` in die History aufnehmen und über eine neue
`ModelMessage`-Variante an den Provider zurückspielen) ist laut Plan **W4a (A-LOOP/A-ANTH)**, nicht W3 —
eine neue `pub`-`ModelMessage`-Variante ohne Produktionsaufrufer hätte hier gegen die Harte Regel 4
(„kein `pub`-Item ohne Produktionsaufrufer oder benannte Folgewelle") verstoßen. Der neue Test
`test_conversation_history_roundtrip_preserves_reasoning_item` in `history.rs` sichert die
Serde-Roundtrip-Fähigkeit ab (Push → `serde_json::to_string` → `from_str` → Felder + Reihenfolge + Signatur
identisch, `to_model_messages()` weiterhin ohne Reasoning-Eintrag).

### `is_retryable()`

`matches!(self, Self::Transient { .. } | Self::Timeout { .. })` — ausschließlich diese beiden Varianten,
wie im Auftrag verlangt („nur Transient/Timeout; Quota nicht"). Alle übrigen Varianten (inkl. der
Legacy-`RateLimited`) liefern `false` — siehe „Offene Annahmen" zu `RateLimited`.

## API-Nachweise (Datei:Zeile, verifiziert durch Lesen vor Nutzung)

- `harw-macros/src/error.rs:34-63` (`expand_harw_error`, Attribut-Parsing `#[msg]`/`#[from]`) — Named-Field-
  Varianten mit `#[msg("...")]` sind bereits produktiv genutzt (`RateLimited`, `harw-core/src/model.rs:499-504`
  vor dieser Änderung) → dasselbe Muster für die neuen Varianten zulässig.
- `harw-macros/src/error.rs:150-169` (`display_arm`, `Fields::Named`-Zweig) — bindet alle Feldnamen,
  interpoliert nur referenzierte; kein `Display`-Zwang für nicht referenzierte `Option<...>`-Felder.
- `harw-macros/src/error.rs:90` (`#[allow(unused_variables)]` auf der generierten `fmt`-Funktion) — deckt
  bewusst content-freie Meldungen mit ungenutzten Feldern ab.
- `harw-macros/src/error.rs:213-224` (`result_alias`) — erzeugt `pub type ModelResult<T> = Result<T,
  ModelError>;` unverändert (Enum-Name endet weiter auf `Error`, Präfix `Model` unverändert).
- `harw-types/src/usage.rs:6-13` (`TokenUsage`, `#[derive(..., Default, ...)]`) — Präzedenzfall für
  `Default`-Ableitung auf einem Feldtyp in `ModelResponse`.
- `harw-types/src/reasoning.rs:8,19` (`ReasoningEffort`, `#[derive(..., Serialize, Deserialize)]`,
  `#[serde(rename_all = "snake_case")]`) — Präzedenzfall/Konvention für `StopReason`s Serde-Darstellung.
- `harw-types/src/ids.rs:25-33` (`newtype_id!`-Makro) — `ItemId: Debug + Clone + PartialEq + Eq + Hash +
  Serialize` plus manuelles `Deserialize` (Zeile 93-97) — verifiziert, dass `assert_eq!(item.id,
  reasoning_id)` im neuen History-Test kompiliert (`ItemId: PartialEq`).
- `harw-protocol/src/items.rs:97-104` (`ReasoningItem`, `#[serde(deny_unknown_fields)]`) — exakte
  Feldliste (`id`, `summary_text: Vec<String>`, `raw_content: Vec<String>`) für den neuen History-Test
  übernommen; **nicht geändert**.
- `harw-protocol/src/items.rs:10-16` (`TurnItem`, `#[serde(deny_unknown_fields, tag="type",
  rename_all="snake_case")]`) — `Reasoning(ReasoningItem)`-Variante existiert bereits, unverändert genutzt.
- `harw-core/Cargo.toml:10,19,30-31` — `harw-protocol`, `harw-macros`, `serde` (mit `derive`-Feature) und
  `serde_json` sind bereits Abhängigkeiten von `harw-core`; **kein neuer `dep-request` nötig** für diesen
  Knoten (weder für `harw_protocol::OpaqueReasoning` noch für `StopReason`s Serde-Ableitung).
- `harw-protocol/src/lib.rs:14-18` — Re-Export-Muster (`pub use items::{...}`) bereits etabliert; erwartetes
  Muster, über das C-PROTO `OpaqueReasoning` auf Crate-Root re-exportieren wird (`harw_protocol::
  OpaqueReasoning` statt `harw_protocol::items::OpaqueReasoning`), wie im Auftragstext gefordert.

## Tests (in `harw-core/src/model.rs` bzw. `harw-core/src/history.rs`, `#[cfg(test)] mod tests`)

| Datei | Test | Szenario |
|---|---|---|
| model.rs | `test_model_request_with_data_block_and_provider_hints_builders` | alle drei neuen Builder setzen ihr Feld |
| model.rs | `test_model_request_data_block_and_provider_hints_default_none` | alle drei neuen Felder sind bei `new()` `None` |
| model.rs | `test_model_response_text_defaults_stop_end_turn_and_no_reasoning` | `ModelResponse::text()` → `stop==EndTurn`, `reasoning==None` |
| model.rs | `test_stop_reason_default_is_end_turn` | `StopReason::default() == EndTurn` |
| model.rs | `test_stop_reason_serde_roundtrip_unit_and_named_variants` | alle 9 Varianten (inkl. `Refusal{Some}`/`Refusal{None}`) über `serde_json` roundtrip-stabil |
| model.rs | `test_stop_reason_serde_snake_case_tag` | JSON-Tag ist `"context_window_exceeded"` (snake_case) |
| model.rs | `test_model_error_is_retryable_table` | Tabellentest über 11 Fälle — nur `Transient`(×2)/`Timeout` `true`, `QuotaExceeded` explizit `false` |
| model.rs | `test_model_error_new_variants_display_is_content_free_or_carries_message` | `Refusal`/`Cancelled` content-frei; `Timeout`/`QuotaExceeded` interpolieren `message` |
| model.rs | `test_with_context_program_with_program_and_ceiling_separates_trust_blocks` (angepasst) | Datenblock jetzt in `request.data_block` statt `request.context[0]`; `context` bleibt leer |
| history.rs | `test_conversation_history_roundtrip_preserves_reasoning_item` | Push `Reasoning`-Item → Serde-Roundtrip → Felder/Reihenfolge/Signatur erhalten, `to_model_messages()` lässt es weiter aus |

Alle Tests sind reine In-Process-Assertions (kein Netz, kein Dateisystem, keine externen Prozesse);
konkrete Assertions, keine reinen „doesn't panic"-Tests.

## Match-/Struct-Literal-Inventar (per Auftrag: nur listen, nicht ändern — Folgearbeit)

### `ModelRequest { .. }` / `ModelResponse { .. }` — echte Struct-Literale außerhalb dieser Zuständigkeit

Diese Stellen bauen `ModelRequest`/`ModelResponse` per vollständigem Struct-Literal (kein
`..Default::default()`, keine Konstruktorfunktion) und werden durch die additiven Pflichtfelder **nicht
mehr kompilieren**, bis sie auf einen Konstruktor/Builder migriert oder um die neuen Felder ergänzt werden.
Auftrag: „Konstruktoren/Default nutzen; Literal-Stellen im Workspace per grep im Ledger listen (Folgearbeit,
nicht ändern)" — hier erfüllt, **keine dieser Dateien wurde angefasst**.

**`ModelRequest { .. }`** (fehlt: `data_block`, `max_output_tokens`, `tool_result_max_bytes`):

| Datei:Zeile | Wave/Owner laut gelesenem Planauszug |
|---|---|
| `harw-provider-http/src/routing.rs:111` | A-ANTH/A-OAI (W4a) |
| `harw-provider-http/src/anthropic.rs:712,854,895,914,935,957,1055,1094,1117,1168,1211,1264,1352,1388` | A-ANTH (W4a) |
| `harw-provider-http/src/lib.rs:2202,2481,2525,2646,2668,2801,2826,2861,2895,2918,2974,3024,3092,3139,4072` | A-OAI (W4a) |
| `harw-provider-http/tests/anthropic_effort.rs:21` | A-ANTH (W4a) |
| `harw-cli/src/onboarding.rs:576` | nicht im gelesenen Planauszug (Zeilen 1-317 von 592) benannt — Orchestrator-Triage nötig |

**`ModelResponse { .. }`** (fehlt: `stop`, `reasoning`):

| Datei:Zeile | Wave/Owner laut gelesenem Planauszug |
|---|---|
| `harw-provider-http/src/anthropic.rs:698` | A-ANTH (W4a) |
| `harw-provider-http/src/lib.rs:1762` | A-OAI (W4a) |
| `harw-tui/src/approval.rs:1264` | T-TUI (W4b) |
| `harw-tui/src/app.rs:3805` | T-TUI (W4b) / D5 (falls noch Teil A) |
| `harw-core/src/child_controller.rs:2102` | A-CHILD (W4a) |
| `harw-core/src/turn_loop.rs:1944` (Produktionscode), `:2678-2679` (Test-Helper) | A-LOOP (W4a) — **eigene Zuständigkeit dieser Welle, aber `turn_loop.rs` war für C-MODEL gesperrt (nicht in Owned files)** |
| `harw-core/tests/turn_loop.rs:139,188,375,495,541,623,693,760` | A-LOOP (W4a) (Integrationstest zu `turn_loop.rs`) |

Hinweis: `harw-runtime/src/model.rs:215,224` und `harw-core/src/model.rs:503` (eigene Datei) sind
Funktionssignaturen (`fn … -> ModelRequest`/`-> ModelResponse`), **keine** Struct-Literale — per Lesen
verifiziert, nicht in obiger Tabelle.

### `match`/`matches!` auf `ModelError`/`StopReason`

Per Grep über den gesamten Workspace (`match err { .. }`, `match .*ModelError`, `matches!(.*ModelError`)
existiert **kein einziger erschöpfender `match`-Ausdruck** auf `ModelError` — nur `matches!`-Aufrufe
(nicht erschöpfungspflichtig):

- `harw-provider-http/src/routing.rs:220` — `matches!(error, ModelError::RequestFailed(_))` (Test).
- `harw-core/src/turn_loop.rs:935,2281` — `matches!(error, CoreError::Model(ModelError::RateLimited {..}))`.
- `harw-provider-http/src/lib.rs:1878,3947,4048` — `let ModelError::RequestFailed(message) = error else { .. }`.

Alle vier Stellen bleiben **syntaktisch gültig** nach dieser Änderung (neue Varianten brechen `matches!`/
`let-else` nicht, nur echte `match`-Ausdrücke ohne Wildcard). Kein `match`/`matches!` auf `StopReason`
existiert (neuer Typ, noch kein Konsument). **Kein Folgearbeit-Eintrag nötig für Exhaustiveness** — die
Migration der obigen Struct-Literal-Stellen (Tabelle oben) ist die tatsächliche Folgearbeit.

## Geschlossene Register-IDs

- **F-016** (`x-findings-register-w1-w3.md:52,95`): „`ModelRequest.context` wird von keinem Provider
  gelesen" — der **Transportvertrag** ist behoben (`data_block` ersetzt den toten `context`-Push für den
  AW4-01-Datenblock); das tatsächliche *Lesen* durch einen Provider bleibt Folgearbeit A-ANTH/A-OAI (W4a,
  Plan Zeile 225-226) — daher hier als **Vertrag geliefert, Finding formal offen bis W4a**.
- **G-023** (`x-findings-register-w4.md:94`): identischer Befund aus W4-Perspektive — dieselbe Einordnung.
- **G-015** (`x-findings-register-w4.md:48,86`): „Thinking-/`redacted_thinking`-Blöcke werden verworfen" —
  der **Transporttyp** (`OpaqueReasoning` via `ModelResponse.reasoning`) ist eingefroren; das tatsächliche
  Extrahieren/Zurückspielen bleibt Folgearbeit A-ANTH (Extraktion) und A-LOOP (History-Verdrahtung), beide
  W4a — daher ebenfalls **Vertrag geliefert, Finding formal offen bis W4a**.

Alle drei IDs bleiben bis zur tatsächlichen Verdrahtung in W4a offiziell **offen** — konsistent mit der
Einordnung, die C-CANCEL für F-160/G-017 in derselben Welle gewählt hat (`ledger/W3/C-CANCEL.md`).

## Offene Annahmen

1. **`harw_protocol::OpaqueReasoning` existierte zum Zeitpunkt dieser Bearbeitung noch nicht** (C-PROTO lief
   parallel an derselben Welle). Angenommen: exakt `pub struct OpaqueReasoning { provider: .., model: ..,
   blocks: Vec<serde_json::Value> }`, re-exportiert auf `harw_protocol::` (Crate-Root), mit mindestens
   `Debug + Clone` (gefordert durch `ModelResponse: Debug + Clone`). **Muss vom Orchestrator nach C-PROTOs
   Abschluss gegengeprüft werden** — falls C-PROTO andere Ableitungen oder einen anderen Pfad wählt, muss
   entweder C-PROTO angepasst werden oder diese Datei (Folgeauftrag).
2. `ModelError::RateLimited` (Legacy-Variante, vor diesem Knoten vorhanden) ist **nicht** in
   `is_retryable()` als `true` eingestuft, obwohl sie konzeptionell retryable ist — der Auftrag verlangt
   wörtlich „nur Transient/Timeout". Angenommen: `RateLimited` wird in einer späteren Welle (A-ANTH/A-OAI)
   durch `Transient{status:Some(429),..}` ersetzt bzw. migriert; bis dahin liefert `is_retryable()` für
   `RateLimited` bewusst `false` (kein stillschweigendes Reparieren einer Variante außerhalb des
   Auftragswortlauts).
3. Feldwahl der nicht wörtlich vorgegebenen `ModelError`-Varianten (`Refusal`, `Truncated`, `Auth`,
   `QuotaExceeded`, `ContextLength`, `Timeout`) ist in der Tabelle oben eingefroren — Änderungswünsche
   gehen über einen neuen, benannten Auftrag, nicht durch stillschweigendes Ändern dieser Datei.
4. `harw-cli/src/onboarding.rs:576` (echtes `ModelRequest{..}`-Literal) tauchte in keiner der bisher
   gelesenen Plan-Tabellen (Teil A, Teil B W3/W4a/W4b, Zeilen 1-317 von 592) als Zuständigkeit auf — der
   Orchestrator muss diese Datei einer Welle zuordnen, bevor `harw-cli` erneut kompiliert werden kann.

## Folgearbeit (für benannte Folgewellen, außerhalb dieser Zuständigkeit)

- **A-ANTH** (W4a, `harw-provider-http/src/{anthropic,anthropic_caps}.rs`): `request.data_block` nach den
  `tool_result`-Blöcken einfügen (cache-stabil); `ModelResponse.reasoning` aus `thinking`/
  `redacted_thinking`-Blöcken befüllen; `stop_reason` vollständig auf `StopReason` abbilden; alle
  Struct-Literal-Stellen (Tabelle oben) auf Konstruktoren/`..Default::default()` migrieren.
- **A-OAI** (W4a, `harw-provider-http/src/{lib,routing,retry}.rs`): `data_block` als user-Item einfügen,
  Reasoning-Items zurückgeben, `finish_reason`/`incomplete` auf `StopReason` abbilden, `RetryingProvider`
  auf `ModelError::is_retryable()` umstellen (statt eigener 408/429/5xx/529-Heuristik); Struct-Literal-Stellen
  migrieren.
- **A-LOOP** (W4a, `harw-core/src/turn_loop.rs`): `ModelResponse.reasoning` in die History übernehmen
  (neue Persistenz-/Wiedergabe-Logik, ggf. neue `ModelMessage`-Variante — **nicht** in dieser Welle
  eingeführt, s. o.), `TurnOutcome` um `StopReason`-Fälle erweitern, Struct-Literal bei `turn_loop.rs:1944`
  und `tests/turn_loop.rs` migrieren, `ModelError::is_retryable()` statt der bisherigen
  `RateLimited`-Spezialprüfung (`turn_loop.rs:935,2281`) verwenden.
- **A-CHILD** (W4a, `harw-core/src/child_controller.rs`): Struct-Literal bei `child_controller.rs:2102`
  migrieren.
- **T-TUI** (W4b, `harw-tui/src/{app,approval,command_exec,session_controller}.rs`): Struct-Literale bei
  `approval.rs:1264`, `app.rs:3805` migrieren; `StopReason`/`reasoning` in der Turn-Anzeige berücksichtigen,
  falls UI-relevant.
- **Orchestrator-Triage**: `harw-cli/src/onboarding.rs:576` einer Welle zuordnen (offene Annahme 4).
- **C-PROTO-Abgleich**: nach C-PROTOs Abschluss `harw_protocol::OpaqueReasoning`s tatsächliche Definition
  gegen offene Annahme 1 prüfen.
