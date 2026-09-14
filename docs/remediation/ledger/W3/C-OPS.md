# W3 — Agent C-OPS: OpOutput-Nutzlast, explizite Web-Methode, Ausgabe-Schema

Owned: `harw-operations/src/operation.rs`, `harw-macros/src/operation.rs`, dieses Ledger.
BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet, keine git-Schreibbefehle, keine Manifest-Änderung.
Verifikation durch Lesen (inkl. Klammer-/Struktur-Kontrolle der geänderten Blöcke). Befunde: F-222, F-031
(Register `x-findings-register-w1-w3.md:110,229,359`).

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`, Plan `eventual-wandering-pebble.md` „Teil B“ (Zeilen 172–301,
gefrorene W3-Signaturen Zeile 191–200, W3-Tabelle Zeile 202–219), Findings-Register F-222/F-031.

**Kompiliert erst mit:** allen unter §5 gelisteten Folgearbeiten (Kontraktbruch ist beabsichtigt, siehe Plan
„Wellenmechanik“ — Konsumenten migrieren erst in W4a/W5, Build erst nach Integrations-Audit).

## 1. Gefrorene Signaturen (exakt wie geschrieben)

```rust
// harw-operations/src/operation.rs

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpOutput {
    pub text: String,
    pub data: Option<serde_json::Value>,
}

impl From<String> for OpOutput {
    fn from(text: String) -> Self { Self { text, data: None } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WebMethod { Get, Post }               // wire: "GET" | "POST"

pub enum Surface {
    // … Command, ModelTool, AgentTool unverändert …
    Web { path: &'static str, method: WebMethod, approval: ApprovalPolicy },   // `readonly: bool` entfernt
}

pub struct OperationMeta {
    // … unverändert bis auf: …
    pub args_schema: Option<ArgsSchemaFn>,
    pub output_schema: Option<ArgsSchemaFn>,   // neu, gleicher Zeigertyp wie args_schema
}
```

```rust
// harw-macros/src/operation.rs — #[operation(web(...))]-Grammatik
// vorher: web(path = "...", readonly, approval = "...")
// nachher:
web(path = "...", method = "get" | "post", approval = "none" | "always")
// `readonly` als web(...)-Schlüssel ist jetzt ein unsupported-key-Fehler.
// `method` fehlt => syn::Error: "`web(...)` requires a `method = \"get\"` or `method = \"post\"` key"
// `method` unbekannter Wert => syn::Error: "unknown `method` value `<x>`; expected one of: get, post"
```

Pfadhinweis: `WebMethod` ist **nicht** an der `harw-operations`-Crate-Wurzel re-exportiert (`lib.rs:100-103` re-exportiert
nur `ApprovalPolicy, CommandVisibility, FromRawArgs, OpFuture, OpInput, OpInvocation, OpOutput, Operation,
OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface` — `lib.rs` ist nicht meine Zuständigkeit).
Bis zum Nachziehen (§5.2): `harw_operations::operation::WebMethod` verwenden — das Makro selbst tut das bereits
(`::harw_operations::operation::WebMethod::Get`/`::Post`, analog zum bestehenden Muster für `ArgsSchemaProbe`).

## 2. Entscheidung: additiv statt Trait-Rückgabetyp-Wechsel

`Operation::run` gab schon vor dieser Welle `OpFuture<'a> = Pin<Box<dyn Future<Output = Result<OpOutput, OpError>> + Send>>`
zurück — der Trait-Rückgabetyp war bereits `OpOutput`, es gab nichts umzustellen. Die Entscheidungsfrage aus dem Brief
(„Trait-Rückgabetyp umstellen ODER additiv“) betraf also nicht die Signatur, sondern **wie** `OpOutput` um `data`
erweitert wird, ohne die ~90 bestehenden Konstruktionsstellen einzeln anzufassen (außerhalb meiner Zuständigkeit,
siehe §5). Grep-Befund: `harw-macros/src/operation.rs` **konstruiert selbst kein `OpOutput`** — es ruft nur
`#fn_ident(ctx, args).await` auf; die `OpOutput { text: … }`-Literale liegen ausschließlich in handgeschriebenen
`async fn`-Körpern (mehrheitlich `harw-ops/src/*.rs`, daneben `harw-core-bridge`, `harw-tui`, `harw-web`,
`harw-operations/src/adapter/*.rs`). Da kein Feld-Default ein Struct-Literal vor dem fehlenden `data`-Feld retten kann
(Rust verlangt bei `T { a, b }`-Literalen ohne `..Default::default()` alle Felder), ist der Bruch für jede
Konstruktionsstelle unvermeidlich — additiv heißt hier: `impl From<String> for OpOutput` liefert der Folgewelle einen
mechanischen Ersatz (`OpOutput { text: X }` → `OpOutput::from(X)` bzw. `X.into()`), statt jede Stelle einzeln um
`data: None` zu ergänzen.

## 3. Änderungen je Datei

| Datei | Änderung | Befund |
|---|---|---|
| `harw-operations/src/operation.rs:777-833` | `OpOutput.data: Option<serde_json::Value>`, `impl From<String> for OpOutput` | F-222 |
| `harw-operations/src/operation.rs:251-259` | `Surface::Web.readonly: bool` → `Surface::Web.method: WebMethod` | F-031 |
| `harw-operations/src/operation.rs:265-296` | neuer `pub enum WebMethod { Get, Post }` (Serde `SCREAMING_SNAKE_CASE` → `"GET"`/`"POST"`) | F-031 |
| `harw-operations/src/operation.rs:498-556` | `OperationMeta.output_schema: Option<ArgsSchemaFn>` (Spiegel von `args_schema`), `Default`-Impl ergänzt | F-222 |
| `harw-operations/src/operation.rs` (11 Stellen) | jedes `OperationMeta { … args_schema: None, … }`-Literal (Doku-Beispiele + Testfixtures) um `output_schema: None,` ergänzt | Kontraktfolge |
| `harw-operations/src/operation.rs` (Tests) | `Surface::Web`-Tests auf `method: WebMethod::{Get,Post}` umgestellt, `test_surface_web_readonly_flag_differs` → `test_surface_web_method_differs`; neue Tests `test_web_method_*` (Equality/Copy/Serde-Roundtrip×2/SCREAMING_SNAKE_CASE); `OpOutput`-Tests um `data: None`/`data: Some(..)` ergänzt, neue Tests `test_op_output_data_field_accessible`, `test_op_output_equality_considers_data_field`, `test_op_output_from_string_*` (2); `test_operation_meta_with_output_schema_is_cloneable` neu | F-222, F-031 |
| `harw-macros/src/operation.rs:47-107,118-145` | `OperationArgs.web_readonly: bool` → `web_method: Option<LitStr>` (+ lokale Variable in `parse_operation_args`/`expand_operation`) | F-031 |
| `harw-macros/src/operation.rs:190-209` | `web(...)`-Nested-Meta-Parser: `readonly`-Schlüssel entfernt (jetzt „unsupported key"), `method`-Schlüssel neu geparst | F-031 |
| `harw-macros/src/operation.rs:518-552` | `expand_operation`: `web(...)` validiert `path` **und** `method` als Pflichtfelder (in dieser Reihenfolge, `?`-Kurzschluss), emittiert `Surface::Web { path, method: #method_tokens, approval }` | F-031 |
| `harw-macros/src/operation.rs:790-799` | neue Funktion `map_web_method(&LitStr) -> syn::Result<TokenStream>` (`"get"`/`"post"` → `WebMethod::Get`/`::Post`, sonst Fehler mit Werteliste) | F-031 |
| `harw-macros/src/operation.rs:625-636` | generierte `OperationMeta`-Literal-Emission: `output_schema: ::core::option::Option::None,` (kein Attribut-Schlüssel dafür vorgesehen, additiv für Folgewelle) | F-222 |
| `harw-macros/src/operation.rs` (Tests) | `expand_operation_web_with_path_only_defaults_readonly_false_and_approval_none` → `expand_operation_web_with_path_only_fails_missing_method` (erwartet jetzt Fehler statt Default); `expand_operation_web_readonly_and_approval_always_are_honored` → `expand_operation_web_method_and_approval_are_honored`; neu: `expand_operation_web_method_get_is_honored`, `expand_operation_web_rejects_unknown_method_value`, `parse_operation_args_web_requires_method_at_expand_time`, `parse_operation_args_web_rejects_readonly_key`; `expand_operation_web_coexists_with_command_and_model_tool` und `parse_operation_args_web_requires_path_at_expand_time` an neue Grammatik angepasst | F-031 |

Bewusste Verhaltensänderung (Integrations-Audit): **`Surface::Web` deklariert nach dieser Welle keine `readonly`-Bedeutung
mehr** — Leser, die bislang `readonly` als „ist diese Web-Route nebenwirkungsfrei" gelesen haben (z. B. Admission-Prüfungen
außerhalb von `harw-web`), müssen auf eine eigene Konvention wechseln (`method == WebMethod::Get` ist *nicht* dasselbe
Prädikat, sobald eine Folgewelle eine wirklich sichere `GET`-Route mit `approval != None` kombiniert — das ist mit der
alten Semantik nicht ausdrückbar und war Teil des Problems).

## 4. API-Nachweise (Datei:Zeile, gelesen)

| Item | Beleg |
|---|---|
| `serde_json::Value` derived `Clone, Eq, PartialEq, Hash` (Voraussetzung für `OpOutput: Eq` mit `Option<Value>`-Feld) | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde_json-1.0.150/src/value/mod.rs:115-116` |
| `harw-operations/Cargo.toml`: `serde = { version = "1.0.228", features = ["derive"] }`, `serde_json = "1.0.150"` bereits vorhanden (kein `cargo add` nötig) | `harw-operations/Cargo.toml:9-10` |
| `harw-macros/Cargo.toml`: `syn = { version = "2", features = ["full", "extra-traits"] }`, `quote`, `proc-macro2` bereits vorhanden (keine neue Dependency für `map_web_method`) | `harw-macros/Cargo.toml:9-11` |
| `harw_operations::lib.rs` re-exportiert `Surface`, `ApprovalPolicy` etc. an der Crate-Wurzel, **nicht** `WebMethod` | `harw-operations/src/lib.rs:100-103` |
| bestehendes Muster für nicht-reexportierte Typen im Makro (`ArgsSchemaProbe`, `DerivedArgsSchema`, `NoArgsSchema` über `::harw_operations::operation::…`) — als Vorlage für `map_web_method`s `::harw_operations::operation::WebMethod` übernommen | `harw-macros/src/operation.rs:451-455` (vor dieser Welle) |
| `syn::meta::parser`/`ParseNestedMeta::parse_nested_meta`/`Meta::value()` — bestehendes Muster für `readonly`/`approval` unverändert für `method` wiederverwendet | `harw-macros/src/operation.rs:190-209` (nach dieser Welle) |
| F-031-Beleg: `analyze` deklariert `readonly` UND schreibt dauerhaft in den Plan-Store / startet Fan-out | `harw-ops/src/analyze.rs:879` (`web(path = "/api/analyze", readonly, approval = "none")`), `:953-973` (Schreibpfad) — **nur gelesen, nicht verändert** (harw-ops ist nicht meine Zuständigkeit) |
| F-222-Beleg: `OpOutput` vor dieser Welle nur `{ text: String }` | `harw-operations/src/operation.rs` (Stand vor dieser Welle, Zeile 481-511 laut Finding) |

## 5. Folgearbeit (außerhalb meiner Dateien, nicht angefasst)

### 5.1 `OpOutput { text }`-Literale — **Compile-Bruch bis nachgezogen** (`data` fehlt)

Mechanischer Fix pro Stelle: `OpOutput { text: X }` → `OpOutput::from(X)` bzw. `X.into()` (Verhalten identisch,
`data: None`). Nur Stellen, die zusätzlich `data` setzen wollen, brauchen ein echtes Literal mit beiden Feldern.

| Crate/Datei | Zeilen | Hinweis |
|---|---|---|
| `harw-core-bridge/src/agent_tool.rs` | 108 (Doku, `no_run`), 663, 675, 695, 865, 1488-1503 (`contract_output`, 3 Zweige), 1575, 2105, 2132 | A-BRIDGE (W4a) — besitzt diese Datei bereits für Zeile 35 |
| `harw-tui/src/command_exec.rs` | 521 | gesperrt bis Z2d-2 → D1/D5 (Teil A) |
| `harw-operations/src/adapter/command.rs` | 60 (Doku), 414, 444, 480, 516, 546 | kein W3-Owner benannt — Folgewelle für `harw-operations/src/adapter/**` |
| `harw-operations/src/adapter/web.rs` | 78 (Doku), 506, 534, 569, 603, 637 | dieselbe Datei trägt auch §5.3 (readonly→method) |
| `harw-operations/src/adapter/model_tool.rs` | 330 (Doku), 652, 681, 710, 745, 780, 833, 858, 1042 | wie oben |
| `harw-operations/tests/operations.rs` | 56, 398, 401, 409, 412, 524 | Integrationstests derselben Crate, nicht mein Owned-File |
| `harw-web/src/router.rs` | 331, 351 (Testfixtures) | Teil von §5.3 |
| `harw-ops/src/agent.rs` | 148, 159, 172 | A-OPSPLAN (W4a) besitzt `harw-ops` nicht vollständig — siehe Plan-Tabelle (`plan,goal,analyze,explore,research,lib`); alle übrigen `harw-ops/*.rs` unten sind **außerhalb** der W4a-Dateizuständigkeit und brauchen einen eigenen Nachzug |
| `harw-ops/src/analyze.rs` | 1105 | A-OPSPLAN (W4a) |
| `harw-ops/src/plan.rs` | 743 | A-OPSPLAN (W4a) |
| `harw-ops/src/goal.rs` | 489 | A-OPSPLAN (W4a) |
| `harw-ops/src/explore.rs` | 432 | A-OPSPLAN (W4a) |
| `harw-ops/src/lib.rs` | 164 (`compact_unavailable_output`, kein `Result`) | A-OPSPLAN (W4a) |
| `harw-ops/src/work.rs` | 60 | kein Owner benannt |
| `harw-ops/src/attach.rs` | 113 | kein Owner benannt |
| `harw-ops/src/ps.rs` | 106, 121 | kein Owner benannt |
| `harw-ops/src/help.rs` | 262, 315 | kein Owner benannt |
| `harw-ops/src/provider.rs` | 241, 249, 268, 325, 416, 443, 451, 484 | kein Owner benannt |
| `harw-ops/src/context_proposal.rs` | 187, 199, 206, 268 | kein Owner benannt |
| `harw-ops/src/effort.rs` | 133, 139, 153 | kein Owner benannt |
| `harw-ops/src/plugins.rs` | 128, 143 | kein Owner benannt |
| `harw-ops/src/diff.rs` | 327 | W1-04 besitzt `harw-ops/src/diff.rs` bereits — Diff-Nachzug fällt an dieselbe Zuständigkeit |
| `harw-ops/src/model.rs` | 387, 406 | kein Owner benannt |
| `harw-ops/src/status.rs` | 92 | kein Owner benannt |
| `harw-ops/src/quit.rs` | 62 | kein Owner benannt |
| `harw-ops/src/permissions.rs` | 114, 158 | W2A-05 besitzt `harw-ops/src/permissions.rs` bereits |
| `harw-ops/src/approval.rs` | 334, 345, 483 | A-APPR (W4a) besitzt `harw-ops/src/approval.rs` |
| `harw-ops/src/stop.rs` | 109 | kein Owner benannt |
| `harw-ops/src/skills.rs` | 155, 167, 182 | kein Owner benannt |
| `harw-ops/src/memory.rs` | 102, 117, 138, 151, 203, 212 | W9 (Memory & Knowledge) |
| `harw-ops/src/mode.rs` | 250 | kein Owner benannt |
| `harw-macros/tests/operation.rs` | 34, 64, 103, 144, 198, 259, 298, 339 | Integrationstests derselben Crate, nicht mein Owned-File |
| `harw-macros/tests/compile_fail/08_unknown_authority_reducer.rs` | 37 | trybuild-Fixture — muss ihr erwartetes `.stderr` ggf. NICHT ändern (Fehler entsteht vor Typprüfung), aber der Quelltext selbst ist ein `OpOutput`-Literal und sollte konsistent bleiben |

Empfehlung an die Folgewelle: sed-Pass `OpOutput \{\s*text: (.+?),\s*\}` → `OpOutput::from($1)` deckt die Mehrzahl der
einzeiligen/`text`-only-Fälle; mehrzeilige Literale mit zusätzlicher Logik im Textausdruck einzeln prüfen.

### 5.2 `WebMethod`-Re-Export

`harw-operations/src/lib.rs:100-103` sollte `WebMethod` in die `pub use operation::{…}`-Liste aufnehmen, damit
Konsumenten `harw_operations::WebMethod` statt `harw_operations::operation::WebMethod` schreiben können (Ergonomie,
kein Compile-Blocker — das Makro selbst nutzt bereits den vollqualifizierten Pfad). Nicht meine Zuständigkeit
(`lib.rs` nicht in Owned files).

### 5.3 `Surface::Web { readonly, .. }` — Leser und Schreiber der alten Form

**Wichtigster Befund für die Sicherheitswirkung von F-031:** `harw-web/src/router.rs:38-75` definiert **bereits ein
eigenes** `pub enum WebMethod { Get, Post }` mit `WebMethod::expected_for(readonly: bool) -> Self` — genau die
Ableitung, die F-031 als Schwachstelle benennt. Solange `router.rs` nicht auf das neue `Surface::Web::method`
umgestellt wird (`expected_for` durch direktes Lesen von `route.method()` ersetzen), bleibt die Schwachstelle am
Laufenden System bestehen — dieser Vertragswechsel allein *deklariert* nur die Möglichkeit, eine Route korrekt zu
typisieren, er *erzwingt* an der Admission-Stelle noch nichts. `harw-web` hat außerdem seinen produktiven Aufrufer
`router.rs:185`: `let expected = WebMethod::expected_for(route.readonly());` — nach der Migration wird das
`route.method()` direkt.

| Datei | Zeilen | Art | Hinweis |
|---|---|---|---|
| `harw-web/src/router.rs` | 38-75 | eigener `WebMethod`-Enum + `expected_for(readonly)` | **höchste Priorität**: nach Migration auf `Surface::Web::method` redundant/entfernbar |
| `harw-web/src/router.rs` | 185 | `WebMethod::expected_for(route.readonly())` | Admission-Entscheidung GET/POST — produktiver Aufrufer |
| `harw-web/src/router.rs` | 215-230, 303-396 | `WebRouteTable`, Testoperation mit `readonly`-Feld, mehrere Test-Konstruktionsstellen (308, 321, 364, 610, 642, 661) | WB-SRV (W5) besitzt `harw-web/src/{server,router,…}.rs` |
| `harw-operations/src/adapter/web.rs` | 145-270 | `WebAdapter.readonly: bool`-Feld, `Debug`-Impl, `from_operation`-Destrukturierung (191-198), `readonly()`-Methode + Doku | eigene Folgewelle nötig (kein W3/W4a-Owner benannt) — Feld/Methode auf `method: WebMethod` umstellen |
| `harw-operations/src/adapter/web.rs` | 67-71, 522-526, 551-560, 590-594, 619-623, 651-655 | Doku-Beispiel + 5 Testkonstruktionsstellen | wie oben |
| `harw-operations/src/registry.rs` | 968-1011 | 4 Test-Konstruktionsstellen (`readonly: true`) | Registry-Owner (nicht benannt) — reine Test-Fixtures, `path`-Destrukturierung in Produktionscode (432-438) nutzt `..` und ist bereits unberührt |
| `harw-operations/tests/operations.rs` | 505-509 (Konstruktion), 536-557 (Destrukturierung `path, readonly, approval`) | Integrationstest, nicht Owned-File |
| `harw-ops/src/lib.rs` | 396-409 | Destrukturiert `Surface::Web { path: p, readonly: r, approval: a }` in einer Test-/Prüfroutine; Fehlermeldungstext nennt `readonly` wörtlich | A-OPSPLAN (W4a) besitzt `harw-ops/src/lib.rs` |
| `harw-ops/src/approval.rs` | 60-63 (Moduldoku, Prosa-Erwähnung) | A-APPR (W4a) |
| `xtask/src/webui.rs` | 101-122 (`WebRouteDescriptor.readonly: bool`), 340-360 (`extract_routes_from_source` liest das Token `"readonly"` **textuell** aus dem Quellcode, kein echter Parser), 886-897 (TS-Codegen: `readonly operation/path/method/…` + `let method = if route.readonly { "GET" } else { "POST" }`), 927-1064 (Tests/Golden-Fixtures) | WB-GEN (W5) besitzt `xtask/src/webui.rs`; **Achtung**: diese Datei parst `Surface::Web { … }`-Literale als Text (Klammer-Matching über `find_struct_blocks`), nicht über `syn` — der Umstieg auf `method = "get"/"post"` erfordert eine neue `field_str(web_block, "method")`-Extraktion statt der bisherigen `readonly`-Bool-Heuristik |
| `harw-macros/tests/operation.rs` | 240-361 | drei `web(...)`-Attributnutzungen mit `readonly` (256, 295, 336) und drei `Surface::Web { readonly, .. }`-Literale (273, 310, 359) | Integrationstest derselben Crate, nicht mein Owned-File; bricht doppelt: Attribut-Parsing UND Struct-Literal |

### 5.4 `web(...)`-Attributnutzungen mit `readonly`-Schlüssel — Compile-Bruch (unsupported key)

Jede dieser Stellen erzeugt jetzt einen `syn::Error` beim Kompilieren des jeweiligen Crates (unbekannter
`web(...)`-Schlüssel `readonly`) **und/oder** fehlendes `method` (separater Fehler, falls `readonly` entfernt wird
ohne `method` zu ergänzen):

`harw-ops/src/work.rs:52`, `harw-ops/src/analyze.rs:879` (F-031-Fall — hier muss die Folgewelle bewusst
`method = "post"` wählen, nicht `"get"`, um die Schwachstelle tatsächlich zu schließen), `harw-ops/src/status.rs:77`,
`harw-ops/src/attach.rs:99`, `harw-ops/src/ps.rs:92`, `harw-ops/src/approval.rs:325` (`approval-pending`, bleibt GET),
`harw-ops/src/approval.rs:449` (`approval-resolve`, hat kein `readonly` → nur zur Vollständigkeit, unverändert
kompilierbar da schon ohne `readonly`), `harw-ops/src/help.rs:251`, `harw-ops/src/research.rs:372,433`,
`harw-ops/src/diff.rs:291`, `harw-ops/src/explore.rs:485`. Ohne `readonly`-Schlüssel, aber ebenfalls `method` nachzutragen
(fehlt bisher ganz): `harw-ops/src/plan.rs:486`, `harw-ops/src/permissions.rs:73`, `harw-ops/src/stop.rs:84`,
`harw-ops/src/goal.rs:326` (alle vier bereits mutierend/`approval = "always"` → `method = "post"`).

### 5.5 dep-request

Keine — `serde`/`serde_json` bereits vorhanden in beiden Owned-Crates (§4).

## 6. Tests (neu/geändert, in Owned-Files)

| Datei | Test |
|---|---|
| `harw-operations/src/operation.rs` | `test_op_output_data_field_accessible`, `test_op_output_equality_considers_data_field`, `test_op_output_from_string_sets_text_and_no_data`, `test_op_output_from_string_matches_manual_construction`; bestehende `test_op_output_*` um `data: None` ergänzt; `test_surface_web_method_differs` (ex `test_surface_web_readonly_flag_differs`); `test_web_method_equality`, `test_web_method_inequality`, `test_web_method_is_copy`, `test_web_method_serde_roundtrip_get`, `test_web_method_serde_roundtrip_post`, `test_web_method_serde_uses_screaming_snake_case`; `test_operation_meta_with_output_schema_is_cloneable`; `test_operation_meta_default_has_no_args_schema`/`test_operation_meta_default_fills_only_unnamed_fields` um `output_schema`-Assertions ergänzt; alle Doctests (`OpOutput`, `WebMethod`, `OperationMeta`, `Operation`) auf neue Felder angepasst |
| `harw-macros/src/operation.rs` (`operation_tests`) | `expand_operation_web_with_path_only_fails_missing_method` (ex `…_defaults_readonly_false_and_approval_none`), `expand_operation_web_method_and_approval_are_honored` (ex `…_readonly_and_approval_always_are_honored`), `expand_operation_web_method_get_is_honored`, `expand_operation_web_rejects_unknown_method_value`, `expand_operation_web_coexists_with_command_and_model_tool` (angepasst), `parse_operation_args_web_requires_path_at_expand_time` (angepasst auf `web(method = "get")`), `parse_operation_args_web_requires_method_at_expand_time` (neu), `parse_operation_args_rejects_unknown_web_key` (unverändert, Fehlertext-Substring stabil), `parse_operation_args_web_rejects_readonly_key` (neu, Regressionsschutz für F-031) |

Damit sind die im Brief geforderten Testkategorien abgedeckt: Makro-Parsing (bestehendes Muster fortgeführt),
`From<String>` (2 Tests), `WebMethod`-Serde (2 Roundtrip-Tests + 1 Format-Test).
