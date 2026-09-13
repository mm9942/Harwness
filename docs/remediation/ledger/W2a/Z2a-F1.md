# Z2a-F1 — Review-Findings A1–A4 aus `review-Z2a.md` behoben

- Welle: W2a (Fix-Agent Z2a-F1)
- Basis: uncommitted Arbeitsstand nach W2A-01/W2A-02 und Z2a-F0
- Schreibbereich: `harw-core/src/session.rs`,
  `harw-core/src/child_controller.rs`,
  `harw-core/tests/session_mode_intersection.rs`, dieses Ledger

Kein `cargo` außer `cargo metadata --offline --no-deps --format-version 1`
(Exit 0, nach den Änderungen geprüft). Kein `make`, kein `rustc`, keine
`git`-Schreibbefehle. Nicht kompiliert, kein Testlauf. Keine Platzhalter,
kein `#[allow(...)]`. Zeilenbreite ≤ 100 Zeichen manuell geprüft
(`awk length>100`: nur die zwei vorbestehenden Byte-Längen-Treffer in
`session_mode_intersection.rs:203,252`, mehrsprachige String-Literale).

## A1 (hoch) — Eltern-Schnitt gehört in die Basis

`harw-core/src/session.rs`, direkt nach `base_activation()`:

```rust
pub fn narrow_base_activation(&mut self, ceiling: &SessionActivation) {
    self.base_activation = self.base_activation.intersect(ceiling);
    self.apply_mode();
}
```

Belegte API: `SessionActivation::intersect(&self, &Self) -> Self`
(`harw-core/src/activation.rs:395`, monoton — das Ergebnis lässt nie mehr zu
als eine der beiden Seiten allein, deshalb ist die Methode auch mehrfach
idempotent). `apply_mode` ist private, idempotent und reihenfolgeunabhängig
(`session.rs`, Kommentar dort) und sendet kein `TurnEvent::ModeChanged` —
die Verengung bleibt für den Event-Sink unsichtbar, wie beim Deckenschnitt
gewollt. Rustdoc begründet ausdrücklich, warum `activation_mut()` für eine
Autoritätsgrenze **nicht** taugt.

`harw-core/src/child_controller.rs` (Schnitt-Block in `admit`, vormals
`let cut = child_session.activation().intersect(&parent_activation);
*child_session.activation_mut() = cut;`) schreibt jetzt

```rust
child_session.narrow_base_activation(&parent_activation);
```

Semantisch identisch für den Ist-Zustand und strikt stärker danach: die
Kind-Session steht bei der Admission im Default-Modus `Chat`
(`ToolProfile::Full`, `allowed_tools() == None`, `mode.rs:189-254`), dort ist
`activation == base_activation`, der Schnitt also derselbe Wert. Neu ist, dass
er den nächsten `set_mode`/`with_mode` überlebt, statt aus der ungeschnittenen
Basis (IR-Fläche bzw. `SessionActivation::default()` mit Profil `Full`)
stillschweigend wieder aufgerissen zu werden.

Test (`child_controller.rs`, Testmodul, neben den bestehenden Schnitt-Tests):
`parent_activation_cut_survives_a_later_mode_switch` — Wurzel verbietet
`shell.exec`, Rolle (`read_and_exec_ir()`) lässt es zu, Kind wird admittiert,
danach `set_mode(InteractionMode::Work)` (weiteste Modus-Decke: Profil `Full`,
keine Allowlist). Erwartet: `shell.exec` bleibt in `activation()` **und** in
`base_activation()` verboten, `fs.read` bleibt offen. Helfer
(`ir_spawner_with_parent_activation`, `read_and_exec_ir`, `spawn_input`),
`SessionManager::get_mut` (`session_manager.rs:60`, `CoreResult<&mut
AgentSession>`) und `SessionActivation`/`ToolProfile`/`ToolName` sind im
Testmodul bereits in Scope; `InteractionMode` wird mit vollem Pfad
(`crate::mode::InteractionMode`) genannt, um keinen neuen Import zu setzen.

## A2/A3 — `with_activation` wendet den Modus an, ohne Clone

```rust
pub fn with_activation(mut self, activation: SessionActivation) -> Self {
    self.base_activation = activation;
    self.apply_mode();
    self
}
```

Damit ist `with_activation` der dritte Basis-Setzer, der `apply_mode()` ruft
(neben `with_executable_agent_ir` und `with_mode`), und die
Reihenfolgeunabhängigkeit der Builder gilt auch für ihn. Der `HashSet`-Clone
(A3) entfällt ersatzlos, weil `apply_mode` `self.activation` ohnehin neu
berechnet. Rustdoc angepasst (die alte Begründung „für `Chat` wäre der Schnitt
die Identität" trug nur im Default-Modus).

Bestandsaufrufer geprüft (`grep -rn '\.with_activation('`):
`harw-core/src/turn_loop.rs:2194` und `harw-core/src/child_controller.rs:4203`
(Test) — beide auf frisch gebauten Sessions im Default-Modus `Chat`, für die
`apply_mode` die Identität ist. Kein Verhaltenswechsel dort.

## A4 — Tests in `session_mode_intersection.rs`

Import erweitert: `use harw_core::activation::{SessionActivation, ToolProfile};`.
Drei neue Tests am Dateiende:

- `with_activation_is_independent_of_builder_order` — genau der Fall aus A2:
  `with_mode(Explore).with_activation(a)` == `with_activation(a).with_mode(Explore)`
  für `fs.read` (in `EXPLORE_TOOLS`), `fs.write` (nicht) und `shell.exec`
  (Basis-Verbot). Ohne den `apply_mode`-Aufruf schlägt die Gleichheit für
  `fs.write` fehl.
- `a_base_set_through_with_activation_survives_a_mode_switch` — A4(a): das
  Basis-Verbot überlebt `Explore → Work → Plan → Chat`, und `Chat` stellt
  `fs.write` vollständig wieder her (keine Ratsche).
- `narrow_base_activation_is_monotone_and_outlives_every_mode` — die neue
  Methode aus A1 über die öffentliche Crate-API: Verengung überlebt
  `set_mode(Work)`, eine zweite disjunkte Verengung nimmt weiter weg und gibt
  nichts zurück.

Die Tests nutzen nur vorhandene Helfer der Datei (`plain_session`,
`executable_agent_ir`, `enabled`); `plain_session` hat keinen Spawn-Kontext,
`apply_mode` überspringt dort den Sandbox-Zweig.

## Offen / nicht in diesem Schreibbereich

Die sieben unter (B) in `review-Z2a.md` gelisteten Compile-Brüche in
`harw-tui`/`harw-cli` bleiben unverändert W2c-Folgearbeit.
