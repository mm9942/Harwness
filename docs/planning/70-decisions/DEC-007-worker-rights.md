---
id: DEC-007
title: Worker-Rechte — Lesen überall, Schreiben nur im eigenen Scope
status: accepted
date: 2026-09-27
tags: [decision, rights, sandbox, work-driver]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../../harw-registry-defaults/src/authority.rs
  - ../../../harw-ops/src/work_driver.rs
  - ../../../harw-plan-bridge/src/work_driver.rs
  - ../../../harw-cli/src/job_worker_work_driver.rs
---

# DEC-007 — Worker-Rechte

## Entscheidung
Jeder Worker liest den gesamten Workspace, schreibt aber nur innerhalb der
`owned_paths` seines eigenen Scopes; ein leerer Scope bedeutet reines Lesen.
Das wird von der Rechte-/Sandbox-Schicht erzwungen, nicht nur per Prompt.
`work_driver.status` verlangt `ReadWorkspace`, `work_driver.enqueue`
verlangt `ExecuteProcess`; Enqueue und Stop fragen immer nach Zustimmung
(`approval = "always"`). Limit-Overrides eines Aufrufs dürfen die Grenzen
der Spec nur verengen, nie erweitern.

## Warum
- Lesen über den ganzen Workspace ist nötig, damit ein Worker Kontext
  außerhalb seines eigenen Scopes versteht (geteilte Typen, Aufrufer,
  Konventionen), ohne dass er dort etwas verändern darf.
- Schreibrecht pfadgenau nur über `owned_paths` zu vergeben würde die
  Rechte-/Sandbox-Schicht überfordern; sie kennt nur `{ReadWorkspace}` bzw.
  `{ReadWorkspace, WriteWorkspace}`. Die eigentliche Pfadgrenze prüft die
  Wellen-Admission separat (`validate_patch` gegen `owned_paths`), ein
  Verstoß blockiert die Übernahme des Patches.
- Ein leerer Scope (`owned_paths.is_empty()`) ist die explizite
  Read-only-Rolle: der Worker bekommt nur `{ReadWorkspace}` und den
  Hinweistext „nur lesen und berichten, nichts ändern“.
- `work_driver.enqueue` startet einen dauerhaften Hintergrund-Job — wie
  `job.start` — und braucht deshalb `ExecuteProcess`; `work_driver.status`
  und `work_driver.stop` lesen nur den Job und seine Historie, wie
  `job.status`/`job.stop`, und brauchen deshalb nur `ReadWorkspace`.
- Enqueue/Stop laufen als Operator-Aktion mit `approval = "always"`: die
  Freigabe entscheidet ein Mensch, nicht der Modell-Aufruf selbst.
- Overrides dürfen nur verengen (`narrow_spec`/`narrow_u32`/`narrow_budget`):
  ein Aufrufer kann seine eigenen Limits nicht über die Grenzen der
  Ausgangs-Spec hinaus erweitern; jeder Versuch, ein Limit zu erhöhen, wird
  mit einem Fehler abgelehnt.

## Folgen
- Worker mit `owned_paths` bekommen das Registry-Profil `WorkspaceEdit`
  (Rechte `{Read, Write}`, nur `fs.*`/`doc.*`/`explore.*`/Workspace-`deps.*`),
  Worker ohne `owned_paths` bekommen `ReadOnlyExplore` (`{Read}`); kein
  Worker bekommt Prozess-Werkzeuge — die Sandbox trägt kein pfadgenaues
  Schreibrecht.
- Ehrlicher Stand: eine pfadgenaue `WriteWorkspace` gibt es in der
  Rechte-/Sandbox-Schicht noch nicht. `owned_paths` werden deshalb
  nachträglich erzwungen, nicht präventiv: ein Workspace-Snapshot vor und
  nach jeder Welle (`snapshot_workspace`/`diff_snapshots`), geprüft mit
  demselben `validate_patch`-Mechanismus wie bei Plan-Knoten. Ein Schreiben
  außerhalb des Scopes wird dabei nicht verhindert und nicht automatisch
  zurückgerollt — es blockiert die Welle und eskaliert an den Menschen.
- Offener Folgeschritt: präventive, pfadgenaue Schreibrechte in der
  Sandbox selbst (statt Snapshot-Diff nach der Tat), damit ein
  Scope-Verstoß gar nicht erst aufs Dateisystem kommt.
- Overrides sind eine reine Verengungs-API: neue Felder in
  `WorkDriverOverrides` müssen ebenfalls über `narrow_*`-Helfer laufen,
  sonst entsteht eine stille Rechteausweitung.
- Trade-off: kein Worker kann versehentlich fremden Code einsehen und
  parallel dauerhaft verändern — der Verstoß wird aber erst nach dem
  Schreiben entdeckt, nicht davor; das erzwingt kleine, vorab geplante
  Scopes (siehe DEC-005) als zusätzliche Schadensbegrenzung.

## Wo im Code
- `harw-registry-defaults/src/authority.rs` — `tool_permission` für
  `work_driver.status` → `Permission::ReadWorkspace`, `work_driver.enqueue`
  → `Permission::ExecuteProcess`; `AuthorityReducer`-Obergrenzen.
- `harw-ops/src/work_driver.rs` — `work_driver_enqueue`,
  `WorkDriverOverrides`, `narrow_spec`/`narrow_u32`/`narrow_budget`
  (Overrides verengen nur); `approval = "always"` an der
  `#[harw_macros::tool(...)]`-Deklaration von `work_driver.enqueue`.
- `harw-plan-bridge/src/work_driver.rs` — `WorkScope::owned_paths`;
  `render_task` mit dem Read-only-Hinweis „nur lesen und berichten,
  nichts ändern“ bei leerem Scope.
- `harw-cli/src/job_worker_work_driver.rs` — Modulkommentar: Worker mit
  `owned_paths` → `RegistryProfile::WorkspaceEdit` (`{Read, Write}`), ohne
  → `RegistryProfile::ReadOnlyExplore` (`{Read}`); `owned_rules` und die
  nachträgliche `validate_patch`-Admission (`snapshot_workspace`/
  `diff_snapshots` vor/nach der Welle) gegen die `owned_paths`.

## Verwandt
- [DEC-004 Keine parallelen Builds](DEC-004-no-parallel-builds.md)
- [DEC-005 Kleine Scopes, viele Wellen](DEC-005-small-scopes-waves.md)
- [DEC-006 Modell-agnostischer Treiber](DEC-006-model-agnostic.md)
- [DEC-008 Kein TUI](DEC-008-no-tui.md)
