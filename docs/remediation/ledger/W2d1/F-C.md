# W2d-1 / F-C — Fix-Agent `AgentSession::mode_ceiling` (Befund T3 aus Z2d1-tui)

Rolle: focused-coding-task (Sonnet). BUILD-POLICY eingehalten: nur Lesen/grep, kein cargo build/check/test/clippy,
kein make/rustc/rust-analyzer, keine git-Schreibbefehle. **Nichts kompiliert, keine Tests ausgeführt** —
Verifikation durch Lesen.

## Geänderte Dateien

- `harw-core/src/session.rs` — private Hilfsfunktion `mode_activation` extrahiert, `apply_mode` nutzt sie,
  neue `pub fn mode_ceiling(&self) -> SessionActivation`, Doku von `activation()` korrigiert
  (beschreibt jetzt das Verhältnis zu `mode_ceiling()`), drei neue Tests `test_mode_ceiling_*`.

Nicht angefasst: `harw-core/src/activation.rs`, `harw-core/src/mode.rs` (nur gelesen, Vertrag verlangte keine
Änderung dort), keine anderen Dateien.

## Geänderte / neue Signaturen (exakt)

```rust
// session.rs, Modulebene, privat — vor `impl AgentSession`
fn mode_activation(mode: InteractionMode) -> SessionActivation;

// session.rs, impl AgentSession, neu, öffentlich
#[must_use]
pub fn mode_ceiling(&self) -> SessionActivation;
```

`apply_mode` (privat, unverändert in Signatur) ruft jetzt `mode_activation(self.mode)` auf, statt die
`SessionActivation` inline zu bauen:

```rust
fn apply_mode(&mut self) {
    self.activation = self.base_activation.intersect(&mode_activation(self.mode));
    let ceiling = self.mode.permission_ceiling();
    if let (Some(context), Some(base)) =
        (self.spawn_context.as_mut(), self.base_sandbox.as_ref())
    {
        context.sandbox = base.restrict(&ceiling);
    }
}
```

`mode_ceiling`:

```rust
pub fn mode_ceiling(&self) -> SessionActivation {
    self.base_activation.intersect(&mode_activation(self.mode))
}
```

Verhalten unverändert: `mode_activation` ist wortgleich der Code, der vorher inline in `apply_mode` stand
(`SessionActivation::new(mode.tool_profile())` + `enable_tool` je Name aus `mode.allowed_tools()`); `apply_mode`
und `mode_ceiling` teilen sich jetzt exakt dieselbe Modus-Seite des Schnitts, statt sie zweimal zu bauen.

## Befund T3 — geschlossen

**Ursprung** (Z2d1-tui.md): `runtime_commands.rs:146` (`/tools on`) und `tools_command.rs:458`
(`/tools profile`) prüfen nur gegen `base_activation()`, nicht gegen `base_activation() ∩ mode_ceiling()`.
In einem engeren Modus (z. B. `Explore`) kann ein Laufzeit-Toggle dadurch ein Werkzeug wieder sichtbar
machen, das der aktuelle Modus verboten hat, solange es nur nicht von der Basis verboten ist.

**Fix in diesem Knoten (F-C)**: `AgentSession::mode_ceiling()` liefert den fehlenden Referenzpunkt —
`base_activation().intersect(&mode_activation(mode()))`, denselben Schnitt, den `apply_mode` bereits in
`activation()` einsetzt. Unmittelbar nach `set_mode` gilt für jedes Werkzeug
`activation().is_tool_enabled(name) == mode_ceiling().is_tool_enabled(name)` — belegt durch
`test_mode_ceiling_matches_activation_after_set_mode`. Die eigentliche Validierung der Toggle-Pfade
(`runtime_commands.rs`, `tools_command.rs`) gegen diesen neuen Aufruf ist **nicht** Teil dieses Knotens —
laut Vertrag (Z2d1-tui.md, Spalte „Entscheidung/Fix") Aufgabe von **F-T** (owned files dort:
`harw-tui/src/*_command.rs`); dieser Knoten liefert ausschließlich die von F-T benötigte API auf
`AgentSession`.

`activation()`s Doku war vorher rein englisch und beschrieb nur „the activation filter", ohne den Bezug zu
einer Obergrenze zu nennen. Neue Fassung (deutsch, `# Beschreibung`/`# Returns`/`# Nebenläufigkeit`) stellt
explizit fest: `activation()` ist `base_activation() ∩ mode_activation(mode())`, also nach jedem `set_mode`
identisch zu `mode_ceiling()`; ein späterer `activation_mut()`-Toggle darf nur innerhalb dieser Decke bleiben.
(Anmerkung: der im Brief erwähnte „falsche Satz über context budget" vor `activation()` war beim Lesen
dieses Knotens nicht mehr vorhanden — vermutlich bereits durch einen früheren Knoten korrigiert oder der
Brief bezog sich auf eine andere Zeilenzählung. Die inhaltlich verlangte Korrektur — Bezug zu
`mode_ceiling()` herstellen — wurde unabhängig davon umgesetzt.)

## Tests (neu, `harw-core/src/session.rs`, `#[cfg(test)] mod tests`)

- `test_mode_ceiling_full_mode_keeps_base_disabled_tool_out` — Basis `Full` mit `disable_tool("x")`, Modus
  `Work` (Full-Profil, filtert namensbasiert nicht): `mode_ceiling()` hat `"x"` weiterhin deaktiviert,
  `"fs.read"` bleibt sichtbar. Belegt: ein Modus mit Full-Profil kann ein Basis-Verbot nicht aufheben.
- `test_mode_ceiling_explore_mode_intersects_allowlist_with_base` — Basis `Minimal` mit `enable_tool`
  für `"fs.read"` (auch in `EXPLORE_TOOLS`, mode.rs) und `"custom.tool"` (nicht in `EXPLORE_TOOLS`), Modus
  `Explore`. `mode_ceiling()` zeigt `"fs.read"` sichtbar, `"custom.tool"` unsichtbar (nur in Basis) und
  `"fs.list"` unsichtbar (nur in der Explore-Positivliste, nicht in der Basis) — echte Schnittmenge, nicht
  nur die Positivliste allein.
- `test_mode_ceiling_matches_activation_after_set_mode` — `session_with_permissions` +
  `set_mode(Explore)`; Vergleich `activation()` vs. `mode_ceiling()` über `profile()` und
  `is_tool_enabled` an fünf Sondennamen. `SessionActivation` hat **kein** `PartialEq` (geprüft in
  `harw-core/src/activation.rs:154`, `#[derive(Debug, Clone, Default)]`) — deshalb kein direkter
  `assert_eq!` auf die Werte selbst, sondern derselbe Sonden-Vergleich, den `activation.rs`s eigene
  `intersect`-Tests (`assert_is_and_of`) bereits verwenden.

## API-Nachweise (Datei:Zeile, gelesen)

- `harw-core/src/session.rs:547-562` (vor Änderung) `apply_mode`, inline gebaute Modus-Aktivierung.
- `harw-core/src/session.rs:702-717` (vor Änderung) `activation()`-Doku (englisch, ohne Bezug zu einer
  Obergrenze).
- `harw-core/src/session.rs:719-740` `base_activation()` + Doku („Basis-Aktivierung … unabhängig vom
  aktuellen `InteractionMode`").
- `harw-core/src/activation.rs:154` `#[derive(Debug, Clone, Default)]` auf `SessionActivation` — kein
  `PartialEq`; `:167-188` `SessionActivation::new`; `:195-197` `profile()`; `:233-244` `is_tool_enabled`;
  `:395-438` `intersect` (Kommutativ/Idempotenz/Subset laut Moduldoku und eigenen Tests).
- `harw-core/src/mode.rs:189-194` `tool_profile()` (`Plan`/`Explore` → `Minimal`, `Chat`/`Work` → `Full`);
  `:250-256` `allowed_tools()` (`Some` nur für `Plan`/`Explore`); `:54-68` `EXPLORE_TOOLS` (u. a.
  `"fs.read"`, `"fs.list"`, `"deps.source_read"`); `:76-100` `PLAN_TOOLS` (erweitert `EXPLORE_TOOLS`).
- `docs/remediation/ledger/W2d1/Z2d1-tui.md:7` Befund T3, Zuordnung F-C (`AgentSession::mode_ceiling()`)
  vs. F-T (Validierung in den Toggle-Pfaden).

## Risiken für den Parent-Build

Keine über das Übliche hinaus — reine Erweiterung um eine lesende Methode plus eine mechanische
Extraktion ohne Verhaltensänderung; alle bestehenden Tests in `session.rs` bleiben unverändert bestehen
(keine ihrer Assertions hängt an der internen Struktur von `apply_mode`).

## Stubbed imports / dep-requests

Keine.

## Offene Anschlussarbeit (nicht dieser Knoten)

F-T muss `runtime_commands.rs` (`/tools on`, Zeile ~146) und `tools_command.rs` (`/tools profile`, Zeile
~458) so ändern, dass sie gegen `session.mode_ceiling()` validieren bzw. schneiden statt gegen
`session.base_activation()` allein — siehe Z2d1-tui.md Zeile T3, Spalte „Entscheidung/Fix".
