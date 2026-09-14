# W3 — Agent C-PROTO-F: Re-Export `ResultTrust`/`OpaqueReasoning` + `ToolResultItem`-Literal-Nachzug

Owned: `harw-protocol/src/lib.rs` (nur Re-Export-Zeile), `harw-core/src/history.rs` (nur
`ToolResultItem`-Literale), `harw-tui/src/app.rs` (nur das `ToolResultItem`-Literal ~:3729; das
`ReasoningItem`-Literal daneben, der W1-08-Block und `approval_arming_tests` **nicht** angefasst),
dieses Ledger. Folgeauftrag zu `ledger/W3/C-PROTO.md` §5.1/§5.2 und `ledger/W3/C-MODEL.md`
(„Offene Annahme 1"/„C-PROTO-Abgleich").

BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet/gelinted, kein `make`/`rustc`/
`rust-analyzer`, keine Git-Schreibbefehle, keine Manifest-Änderung. Verifikation ausschließlich
durch Lesen (Signaturen, Imports, Struct-Definitionen, vollständiger Workspace-Grep).

## 1. Re-Export `harw-protocol/src/lib.rs`

`pub use items::{...}` um `OpaqueReasoning` und `ResultTrust` ergänzt, bestehende alphabetische
Konvention der Liste beibehalten:

```rust
pub use items::{
    AssistantMessageItem, ContentPart, ErrorItem, OpaqueReasoning, ReasoningItem, ResultTrust,
    ToolCallItem, ToolCallResult, ToolResultItem, TurnItem, UserMessageItem,
};
```

Damit existieren jetzt `harw_protocol::ResultTrust` und `harw_protocol::OpaqueReasoning` (Crate-Root),
wie in `C-PROTO.md` §1 (Pfadhinweis) und §5.2 gefordert. Beide Typen waren zum Zeitpunkt dieser
Bearbeitung bereits vollständig in `harw-protocol/src/items.rs:108-131` (`ResultTrust`) bzw.
`:158-167` (`OpaqueReasoning`) definiert (C-PROTO hatte sie schon geliefert) — nur der
Crate-Root-Re-Export fehlte.

## 2. `harw-core/src/history.rs` — `OpaqueReasoning`-Import geprüft

Der Auftrag verweist auf `harw_protocol::OpaqueReasoning` als „stubbed import", den C-MODEL benutzt
habe. Durch Lesen verifiziert: dieser Import steht tatsächlich in `harw-core/src/model.rs:101`
(`use harw_protocol::OpaqueReasoning;`, genutzt in `ModelResponse.reasoning: Option<OpaqueReasoning>`
Zeile 504) — **nicht** in `history.rs`, das nur eine Doku-Erwähnung in Prosa trägt
(`history.rs:504`, Kommentar zum Regressionstest `test_conversation_history_roundtrip_preserves_reasoning_item`,
keine kompilierte `use`-Zeile). `history.rs` importiert `OpaqueReasoning` nirgends als Code — kein
Pfad anzugleichen. Mit dem Re-Export aus §1 löst `model.rs:101` jetzt korrekt auf (Datei nicht
angefasst, da nicht in Owned files dieses Auftrags — nur gegengelesen).

## 3. Workspace-weiter Grep `ToolResultItem {` — vollständiges Inventar

```
harw-tui/src/app.rs:3729        history.push(TurnItem::ToolResult(ToolResultItem {
harw-core/src/history.rs:130    self.items.push(TurnItem::ToolResult(ToolResultItem {
harw-core/src/history.rs:323    TurnItem::ToolResult(ToolResultItem {
harw-core/src/history.rs:335    TurnItem::ToolResult(ToolResultItem {
```

Kein weiterer Treffer im gesamten Workspace (inkl. aller `tests/`-Verzeichnisse) — die Liste ist
identisch mit `C-PROTO.md` §5.1, die bereits vollständig war. Alle vier Stellen liegen in Owned
files dieses Auftrags; **keine Folgearbeit-Einträge nötig**, alle sind nachgezogen.

### 3.1 `harw-core/src/history.rs:130` — `ConversationHistory::push_tool_result` (Produktionscode)

```rust
self.items.push(TurnItem::ToolResult(ToolResultItem {
    id: id.clone(),
    call_id,
    result,
    duration_ms,
    trust: ResultTrust::Untrusted,
}));
```

Begründung Untrusted statt Runtime: `push_tool_result` ist die generische Komfort-Methode für
*beliebige* Tool-Ergebnisse; ihre Signatur nimmt kein `trust`-Argument entgegen (Signaturänderung
wäre ein Breaking Change über 19 Aufrufstellen in 5 nicht-eigenen Dateien —
`harw-provider-http/src/{anthropic,lib}.rs`, `harw-core/src/{context_budget,turn_loop,history_tail}.rs`
— außerhalb des Auftragsumfangs). `Untrusted` ist der sichere Default (== `ResultTrust::default()`,
== Verhalten für Alt-Transkripte ohne `trust`-Feld) und korrekt für den überwiegenden Aufrufer-Anteil
(echte Werkzeugausgaben). **Bekannte Einschränkung (Folgearbeit, nicht meine Datei):**
`harw-core/src/turn_loop.rs` ruft `push_tool_result` an mehreren Stellen (`:1032,1174,1216,1310,1465,1531,1612,1768`)
auch für laufzeit-synthetisierte Ergebnisse auf (Cancel-/Admission-/Approval-Denials laut
`C-PROTO.md` §5.1-Empfehlung) — diese werden über diesen Pfad heute ebenfalls als `Untrusted`
markiert, obwohl `Runtime` semantisch korrekter wäre. Das ist **fail-closed sicher** (Untrusted ist
die konservativere Klassifikation, keine Sicherheitsregression), aber nicht provenienzgenau. Um das
zu beheben, braucht `push_tool_result` entweder einen `trust: ResultTrust`-Parameter (Breaking
Change über alle Aufrufer) oder eine zusätzliche Methode `push_tool_result_with_trust(...)` — Aufgabe
für A-LOOP (W4a, `turn_loop.rs` gehört diesem Agenten), da nur dort entschieden werden kann, welche
Aufrufstellen tatsächlich Runtime-synthetisiert sind.

### 3.2 `harw-core/src/history.rs:323` (`fn result`) und `:335` (`fn big_result`) — Testhilfen

Beide bauen synthetische Erfolgs-Payloads, die im Test einen echten Werkzeugaufruf simulieren
(`ToolCallResult::success(...)`) → `trust: ResultTrust::Untrusted` ergänzt, konsistent mit §3.1 und
mit dem `#[serde(default)]`-Verhalten für Alt-Daten.

Import ergänzt (`harw-core/src/history.rs:7-10`): `ResultTrust` in die bestehende
`use harw_protocol::items::{...}`-Zeile aufgenommen (alphabetisch einsortiert). Der
`#[cfg(test)] mod tests`-Block importiert `ResultTrust` transitiv über `use super::*;` (`history.rs:306`),
keine zusätzliche `use`-Zeile im Testmodul nötig.

### 3.3 `harw-tui/src/app.rs:3729` — Test `durable_history_hydrates_core_and_redacts_non_text_visible_content`

```rust
history.push(TurnItem::ToolResult(ToolResultItem {
    id: ItemId::new(),
    call_id,
    result: ToolCallResult::error("do-not-render"),
    duration_ms: 3,
    trust: ResultTrust::Untrusted,
}));
```

Simuliert das Fehlerergebnis eines `read_file`-Werkzeugaufrufs (Kommentar/Testname: „redacts
non-text visible content") → echtes Werkzeugergebnis, `Untrusted`. `ResultTrust` in das
lokale `use harw_protocol::items::{...}` innerhalb der Testfunktion ergänzt
(`app.rs:3696-3699`, alphabetisch einsortiert).

**Nicht angefasst** (wie im Auftrag verlangt): das direkt folgende `TurnItem::Reasoning(ReasoningItem { .. })`-
Literal (`app.rs:3735-3739`, unverändert — kein Feld an `ReasoningItem` ergänzt, siehe `C-MODEL.md`
§5.8), der W1-08-Block und `approval_arming_tests` (keiner von beiden liegt in der Nähe dieser
Testfunktion; per Lesen bestätigt, dass diese Änderung isoliert innerhalb der Test-Funktion
`durable_history_hydrates_core_and_redacts_non_text_visible_content` bleibt).

## 4. API-Nachweise (Datei:Zeile, gelesen)

| Beleg | Fundstelle |
|---|---|
| `ResultTrust` Definition, `#[default] Untrusted`, `Copy` | `harw-protocol/src/items.rs:108-117` |
| `ToolResultItem.trust: ResultTrust` mit `#[serde(default)]` | `harw-protocol/src/items.rs:119-131` |
| `OpaqueReasoning` Definition (`provider: String, model: String, blocks: Vec<serde_json::Value>`) | `harw-protocol/src/items.rs:158-167` |
| bestehende Re-Export-Konvention (alphabetisch, ein `pub use items::{...}`-Block) | `harw-protocol/src/lib.rs:16-19` (vor dieser Änderung) |
| `model.rs` stubbed import `use harw_protocol::OpaqueReasoning;`, Nutzung in `ModelResponse.reasoning` | `harw-core/src/model.rs:101,504` |
| `history.rs` Doku-Erwähnung `harw_protocol::OpaqueReasoning` ist Prosa, keine `use`-Zeile | `harw-core/src/history.rs:504` (Kommentar) |
| vollständiges `ToolResultItem {`-Inventar (nur 4 Treffer workspaceweit) | Grep `harw-tui/src/app.rs:3729`, `harw-core/src/history.rs:130,323,335` |
| `push_tool_result`-Aufrufer außerhalb Owned files (19 Stellen, 5 Dateien) | `harw-provider-http/src/anthropic.rs:1163,1206,1254,1259,1342,1347`; `harw-provider-http/src/lib.rs:2969,3014,3019,3124,3134`; `harw-core/src/context_budget.rs:1910,1922`; `harw-core/src/turn_loop.rs:1032,1174,1216,1310,1465,1531,1612,1768`; `harw-core/src/history_tail.rs:448` |

## 5. Folgearbeit (außerhalb meiner Dateien, nicht angefasst)

- **A-LOOP** (W4a, `harw-core/src/turn_loop.rs`): entscheiden, welche `push_tool_result`-Aufrufe
  tatsächlich laufzeit-synthetisierte Ergebnisse sind (Cancel-/Admission-/Approval-Denials laut
  `C-PROTO.md` §5.1), und dafür `ResultTrust::Runtime` erreichbar machen (neuer Parameter oder
  `push_tool_result_with_trust`-Methode in `history.rs` — **eigene Signaturänderung, nicht Teil
  dieses Auftrags**, da sie 19 Aufrufstellen in 5 nicht-eigenen Dateien berühren würde).
- **C-PROTO.md §5.3/§5.4** (Verdrahtung `render_tool_result` an `to_model_messages`/Wire-Buildern)
  bleibt wie dort beschrieben offen — nicht Teil dieses Nachzugs.
- **C-MODEL.md „Offene Annahme 1"**: hiermit erledigt — `harw_protocol::OpaqueReasoning` existiert,
  ist `Debug + Clone` (aus `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`,
  `items.rs:158`), Feldnamen `provider: String, model: String, blocks: Vec<serde_json::Value>`
  stimmen exakt mit C-MODELs Annahme überein. Keine Anpassung an `model.rs` nötig.
- **dep-request**: Keine.

## 6. Tests

Keine neuen Tests von mir geschrieben — reine additive Literal-Ergänzung an bereits vorhandenen
Struct-Literalen in bestehenden Tests/Produktionscode; die bestehenden Tests in
`harw-protocol/src/items.rs` (`test_result_trust_default_is_untrusted` u. a., siehe `C-PROTO.md` §6)
decken `ResultTrust`/`OpaqueReasoning` bereits ab. Die drei geänderten `history.rs`-Testfunktionen
(`result`, `big_result`, und transitiv jeder Test, der sie aufruft) sowie
`app.rs::durable_history_hydrates_core_and_redacts_non_text_visible_content` bleiben in ihrer
Assertion-Logik unverändert — nur das neue Pflichtfeld wurde ergänzt, kein Verhalten geändert.
