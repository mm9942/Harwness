# Z2a-F0 — Folgeschäden aus W2A-01/W2A-02 beheben

- Welle: W2a (Fix-Agent)
- Basis: uncommitted Arbeitsstand nach W2A-01 (`harw-core/src/session.rs`,
  `harw-core/tests/session_mode_intersection.rs`) und W2A-02
  (`harw-core/src/child_controller.rs`)
- Agent: Z2a-F0
- Schreibbereich: `harw-core/tests/child_controller.rs`, `harw-core/src/mode.rs`,
  dieses Ledger.

Kein `cargo` außer `cargo metadata --offline --no-deps --format-version 1`
(Exit 0, nach den Änderungen erneut geprüft). Kein `make`, kein `rustc`, keine
`git`-Schreibbefehle. Nicht kompiliert, kein Testlauf. Keine Platzhalter, kein
`#[allow(...)]`.

Pflichtlektüre: `docs/remediation/ledger/W2a/W2A-02.md` (neue Signatur
`with_external_root_parent(self, session_id, ctx, reasoning_effort,
parent_activation: SessionActivation)`, `DEFAULT_CHILD_REASONING_EFFORT =
Medium`), `docs/remediation/ledger/W2a/W2A-01.md` (`mode.rs:16-19` stale
Prosa), `harw-core/src/child_controller.rs` (Stand auf Platte).

## 1. `harw-core/tests/child_controller.rs`

Geprüft per `grep -rn "with_external_root_parent"` über den Workspace: diese
Datei ruft die Funktion **nicht** auf (bestätigt W2A-02, Abschnitt
„Folgearbeit W2c/W2d"). Die 16 betroffenen Aufrufe liegen ausschließlich im
`#[cfg(test)]`-Modul von `harw-core/src/child_controller.rs` selbst — bereits
von W2A-02 um `SessionActivation::default()`/eine explizite Aktivierung
ergänzt (verifiziert an den Fundstellen `external_root_parent_admits_a_child…`,
`effort_clamp_without_an_inherited_base_falls_back_to_the_default`,
`parent_activation_cut_also_applies_without_an_agent_ir`). Für Aufgabe 1a war
in meinem Schreibbereich daher nichts zu ändern.

Geändert: der von W2A-02 explizit als gebrochen benannte Test
`test_clamp_child_reasoning_effort_no_base_with_cap_stays_none` (Z. 783–810
im Alt-Stand):

- Umbenannt zu `test_clamp_child_reasoning_effort_no_base_with_cap_falls_back_to_the_default`.
- Erwartung von `assert_eq!(effective, None)` auf
  `assert_eq!(effective, Some(ReasoningEffort::Medium))` umgestellt: Basis
  fehlt (`None`) → `DEFAULT_CHILD_REASONING_EFFORT` (`Medium`) tritt an ihre
  Stelle, `cap = High` klammert `min(Medium, High) = Medium` — der Cap hat
  hier keinen weiteren Effekt, aber der Default geht nicht verloren.
- Kommentar „A cap is a pure upper bound: without an inherited base it
  introduces no level of its own" korrigiert (beschreibt jetzt den
  Default-Fallback statt `None`).
- Der Setup-Teil (Assertion, dass der Kind-Session-Wert vor dem Clamp noch
  `None` ist) bleibt unverändert gültig: `admit()` setzt `reasoning_effort`
  am Kind nur, wenn `parent_reasoning_effort.is_some()`
  (`child_controller.rs:1894`); `managed_parent()` setzt keinen Effort, also
  bleibt das Kind vor dem expliziten `clamp_child_reasoning_effort`-Aufruf bei
  `None`.

Die übrigen fünf `clamp_child_reasoning_effort`-Tests (Z. 716, 750, 768, 813,
844 im Alt-Stand) setzen jeweils eine explizite Parent-Basis oder prüfen den
Fehlerpfad eines unbekannten Kindes — von der Signaturänderung und der
Default-Fallback-Änderung unberührt, per Lesen bestätigt.

Keine Ratschen-/Aktivierungs-Assertions (W2A-01) in dieser Datei: `grep -n
"activation\|set_mode\|SandboxSpec\|restrict"` findet außer den
`SandboxSpec`-Testhelfern (`test_sandbox`, `SpawnContext.sandbox`,
`ceiling: None`) keine Erwartung, die von der Basis/Ratschen-Korrektur in
`AgentSession::apply_mode` betroffen wäre — diese Testdatei setzt nie einen
`InteractionMode`.

## 2. `harw-core/src/mode.rs:16-19`

Prosa im Modulkopf (Abschnitt „Autoritätsmodell") korrigiert: behauptete
vorher, ein Wechsel von `Explore` zurück nach `Work` stelle entzogene
Permissions „nicht" wieder her, weil der Ceiling stets mit der „bereits
bestehenden" (aktuellen) Sandbox geschnitten werde. Nach W2A-01
(`AgentSession::apply_mode`, `harw-core/src/session.rs`) schneidet jeder
Moduswechsel stattdessen immer von der **Basis-Sandbox** (`base_sandbox`),
nie kumulativ vom zuletzt aktiven Wert. Neuer Text: der Schnitt erfolgt gegen
die Basis, ist deshalb nie kumulativ, und `Explore` → `Work` stellt entzogene
Permissions bis zur Basis wieder her (nie darüber hinaus). Reiner
Doku-Kommentar, keine Codeänderung; die übrige Modul-Prosa (u. a.
`permission_ceiling`-Doc, `WORK_PROMPT`) war nicht Gegenstand des Auftrags und
wurde nicht angefasst.

## Nicht in meinem Schreibbereich

- `harw-cli/src/chat.rs:690`, `harw-tui/src/app.rs:1919`: brechen weiterhin
  durch die W2A-02-Signaturänderung (vierter Parameter fehlt). Bereits von
  W2A-02 als Folgearbeit für W2c/W2d benannt, nicht Teil dieses Auftrags.
