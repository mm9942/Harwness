# F-W — W4, W6, W5 (Z2d-1-Review: harw-cli/src/web.rs)

Welle W2d-1, Remediation `/home/mia/projects/harwness`. Rolle: focused-bug-fix
(Sonnet, mechanisch/testabgedeckt — jede Regression in `root_sandbox`-Bindung
oder in den neuen Routen-Assertions wird sofort von den vorhandenen bzw. neuen
Tests gefangen). Build-Policy eingehalten: **nichts kompiliert/getestet**, nur
`cargo metadata --offline --no-deps --format-version 1` (Exit 0). Keine
git-Schreibbefehle.

Owned files: `harw-cli/src/web.rs`, `docs/remediation/ledger/W2d1/F-W.md`
(neu).

## W4 — zweite Root-Sandbox-Bindung an `cwd` statt Projekt-Wurzelpfad

Befund (Z2d1-web-jobs.md, Zeile W4; `web.rs:236-237`): `serve_web` rief nach
dem Montagebau ein zweites Mal `harw_runtime::root_sandbox(EntryKind::Web,
&cwd)` auf, statt die bereits von der Montage gebaute Wurzel-Sandbox
weiterzuverwenden — und band dabei an `cwd`, nicht an den von
`RuntimeAssemblyBuilder::build` über `discover_project` ermittelten
Projekt-Wurzelpfad (`harw-runtime/src/assembly.rs:488-495`:
`root_sandbox(spec.entry, &project.project_root)`). Zwei Bindungen derselben
Sache, potenziell unterschiedlich.

Fix (`web.rs`, nach dem Montagebau):

```rust
let root_sandbox = assembly.sandbox().clone();
```

- `RuntimeAssembly::sandbox(&self) -> &SandboxSpec` ist `pub const fn`
  (`harw-runtime/src/assembly.rs:1088-1092`) — dieselbe Wurzel-Sandbox, die im
  `SpawnContext` steht und die die Montage einmal beim Bau erzeugt.
  `SandboxSpec` ist `#[derive(Debug, Clone, PartialEq, Eq, Serialize,
  Deserialize)]` (`harw-sandbox/src/lib.rs:825-826`) — `.clone()` liefert eine
  owned `SandboxSpec`, exakt der Typ, den die vorherige Zeile ebenfalls als
  `let`-Bindung erzeugte (`Result<SandboxSpec, _>` vs. jetzt direkt
  `SandboxSpec`, beide über `?`/`.clone()` ohne `Result` an dieser Stelle).
  Die nachfolgende Verwendung (`&root_sandbox` in der `context_factory`-Closure
  → `web_context_for(&factory_assembly, &root_sandbox, ...)`) erwartet
  `&SandboxSpec` — unverändert kompatibel, keine Signaturänderung an
  `web_context_for`/`web_op_context` nötig.
- Frühfehler bleibt erhalten: ein nicht bindbares Arbeitsverzeichnis lässt
  bereits `discover_project`/`root_sandbox` innerhalb von
  `RuntimeAssemblyBuilder::build()` fehlschlagen
  (`harw-runtime/src/assembly.rs:488-495`, propagiert als `RuntimeError` über
  `crate::runtime_web::web_assembly(..)?` in `serve_web`), also weiterhin vor
  dem Binden des Sockets — nur nicht mehr über eine eigene, zweite Zeile in
  `web.rs`. Im Doc-Kommentar dokumentiert (Moduldoc-Abschnitt „`OpContext` je
  Aufruf — das Tier wirkt (F-045)" sowie Inline-Kommentar direkt über der
  geänderten Zeile).
- Imports: `EntryKind` wird weiterhin gebraucht (Testhelfer
  `test_root_sandbox()` ruft `harw_runtime::root_sandbox(EntryKind::Web,
  dir.path())` direkt, `web.rs:417` vor der Änderung — Import bleibt). `cwd`
  bleibt eine gebrauchte Bindung (`web_spec`, `web_assembly`, Fehlermeldung
  „cwd: {error}"). Keine Imports wurden durch diese Änderung unbenutzt; keine
  entfernt.

## W6 — Test: Genehmigungsrouten aus der echten Montage

Befund (Z2d1-web-jobs.md, Zeile W6; `web.rs:512-665` vor dieser Änderung): kein
Test bestätigte, dass `WebRouteTable::from_registry(assembly.operations())`
gegen eine echte `RuntimeAssembly` die `/api/approval-*`-Routen enthält.

Verifiziert vor dem Schreiben:
- `WebRouteTable::from_registry(registry: &OperationRegistry) ->
  Result<Self, WebError>` (`harw-web/src/router.rs:248`);
  `WebRouteTable::find(&self, path: &str) -> Option<&WebAdapter>`
  (`router.rs:267`).
- `WebAdapter::operation_name(&self) -> &str` (`harw-operations/src/
  adapter/web.rs:328`), `WebAdapter::readonly(&self) -> bool`
  (`adapter/web.rs:255`).
- `approval.pending` (`harw-ops/src/approval.rs:320-326`): `web(path =
  "/api/approval-pending", readonly, approval = "none")`, nur
  `Surface::Web`, kein `Surface::ModelTool`.
- `approval.resolve` (`harw-ops/src/approval.rs:444-449`): `web(path =
  "/api/approval-resolve", approval = "none")` (kein `readonly` ⇒
  `readonly: false`), ebenfalls nur `Surface::Web`.
- `build_operations`/`OperationSurface::CommandsOnly`
  (`harw-runtime/src/assembly.rs:759-789`): filtert auf Operationen, die
  **mindestens eine** Fläche tragen, die nicht `Surface::ModelTool` ist
  (`.any(|declared| !matches!(declared, Surface::ModelTool { .. }))`).  Da
  beide Approval-Operationen ausschließlich `Surface::Web` deklarieren, erfüllt
  das `.any(..)` trivially — sie bleiben in der `CommandsOnly`-Registry der
  `EntryKind::Web`-Montage.
- Test-Helfer `test_assembly(with_approval_store: bool) -> (TempDir, TempDir,
  RuntimeAssembly)` bereits vorhanden (B1, `web.rs`, baut über
  `crate::runtime_web::web_assembly` mit `ModelSource::Echo`, leerem
  Temp-Home, `EntryKind::Web`).

Neuer Test am Ende von `mod tests`:

```rust
#[test]
fn test_web_route_table_from_registry_includes_approval_routes() {
    let (_home, _cwd, assembly) = test_assembly(true);

    let routes = WebRouteTable::from_registry(assembly.operations())
        .expect("no two operations claim the same web path");

    let pending = routes
        .find("/api/approval-pending")
        .expect("approval.pending is registered as a web route from the assembly");
    assert_eq!(pending.operation_name(), "approval.pending");
    assert!(pending.readonly(), "approval.pending is declared readonly");

    let resolve = routes
        .find("/api/approval-resolve")
        .expect("approval.resolve is registered as a web route from the assembly");
    assert_eq!(resolve.operation_name(), "approval.resolve");
    assert!(!resolve.readonly(), "approval.resolve is declared mutating");
}
```

Name folgt `test_<funktion>_<szenario>`. Konkrete Assertionen (Pfad → Name,
`readonly`-Flag je Route), kein reiner „läuft ohne Panik"-Test. Keine
Netz-/Prozess-Abhängigkeit (nur `tempfile::TempDir` über `test_assembly`,
bereits von B1 etabliert). `WebRouteTable` ist über den bereits vorhandenen
Import `use harw_web::router::WebRouteTable;` (Zeile 142, Modulkopf) via
`use super::*;` im Testmodul sichtbar — kein neuer Import nötig.

## W5 (nur Doku) — Web-Decke {ReadWorkspace} gilt für alle Tiers

Befund (Z2d1-web-jobs.md, Zeile W5; Entscheidung: {R} bleibt Decke bis
W5/WB-COMP). Verifiziert gegen `docs/remediation/CONTRACTS.md:106`
(Reduktionstabelle, Zeile „Web | nach Tier (W2B-02 `permissions_for_tier`;
Spec: {R}) | Full + CommandsOnly | Fail | None | Closed"): die
`RuntimeSpec`-Decke des `EntryKind::Web`-Profils ist `{ReadWorkspace}` —
unabhängig vom Tier des anfragenden Peers. `permissions_for_tier(tier)`
(`crate::runtime_web::narrow_web_sandbox`) kann diese Decke nur **weiter**
einschneiden, nie anheben (`SandboxSpec::restrict`).

Fix: im Moduldoc-Abschnitt „`OpContext` je Aufruf — das Tier wirkt (F-045)"
(direkt am `narrow_web_sandbox`-Aufruf-Absatz, der bereits die Sandbox-Herkunft
beschreibt) ergänzt: die Web-Decke ist `{ReadWorkspace}` für **alle** Tiers
(Verweis `docs/remediation/CONTRACTS.md:106`); das Tier verengt nur innerhalb
dieser Decke, selbst `Owner` bekommt nie mehr als `ReadWorkspace` im Web-Lauf;
eine Anhebung der Decke (z. B. Schreibrechte für `Owner`) ist keine
Entscheidung dieses Moduls und bleibt W5/WB-COMP vorbehalten. Reine
Dokumentationsänderung, kein Codepfad geändert — `narrow_web_sandbox` selbst
liegt in `runtime_web.rs` (nicht in den Owned files dieses Auftrags) und trägt
bereits eine passende eigene Moduldoku (B1); dieser Auftrag ergänzt nur die
Doku am Aufruf-Ort in `web.rs`.

## Verifikation gesamt

- `cargo metadata --offline --no-deps --format-version 1` — Exit 0.
- Datei `harw-cli/src/web.rs` vollständig gelesen (vorher und nach jeder
  Änderung): Signaturen von `serve_web`, `web_context_for`, `web_op_context`
  unverändert; einzige Codeänderung ist die `root_sandbox`-Bindung (W4);
  restliche Änderungen sind Doc-Kommentare (Modul + Inline) und ein
  angehängter Test (W6).
- Kein `unwrap()`/`expect()` in Produktionspfaden hinzugefügt (nur in Tests,
  wie im übrigen Testmodul bereits durchgängig üblich: `.expect(...)` in
  `#[test]`-Funktionen).
- Kein `#[allow(...)]` hinzugefügt.
- Neuer Test steht am Dateiende innerhalb des bestehenden `#[cfg(test)] mod
  tests { ... }`-Blocks, nach `test_runtime_plan_services_requires_all_three_stores`.
- Keine anderen Dateien verändert.

## Stubbed imports

Keine.

## Annahmen

- `SandboxSpec::clone()` ist eine reine Datenkopie (kein Handle/Deskriptor);
  bestätigt durch die Feldstruktur (`WorkspaceBinding`, `PermissionSet`,
  Netz-Scope) und `#[derive(Clone)]` (`harw-sandbox/src/lib.rs:825-826`) —
  keine `Arc`-Ersetzung nötig, da `RuntimeAssembly::sandbox()` bereits `&Self`
  zurückgibt und die Closure eine eigene, unveränderliche Kopie braucht (wie
  zuvor auch die direkt gebaute `SandboxSpec`).
- `test_assembly(true)` (B1) baut zuverlässig aus einem leeren Temp-Home ohne
  `config.toml` — dieselbe Annahme, die B1 bereits für die übrigen
  `web.rs`-Tests trifft; hier zusätzlich genutzt, um die
  `CommandsOnly`-Registry der echten Montage zu prüfen.

## Ausgabe

```json
{
  "agent": "F-W",
  "files_created": ["/home/mia/projects/harwness/docs/remediation/ledger/W2d1/F-W.md"],
  "files_modified": ["/home/mia/projects/harwness/harw-cli/src/web.rs"],
  "verification": {
    "command": "cargo metadata --offline --no-deps --format-version 1",
    "exit_code": 0,
    "pass": true
  },
  "stubbed_imports": [],
  "assumptions_made": [
    "SandboxSpec::clone() ist eine reine Datenkopie, kein Handle",
    "test_assembly(true) baut zuverlässig aus leerem Temp-Home (wie B1 bereits annimmt)"
  ],
  "blocked": false
}
```
