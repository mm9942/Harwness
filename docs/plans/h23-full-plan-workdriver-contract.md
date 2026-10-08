# h23 — Full Mode: Plan-/TODO-Umfang als langlebiger WorkDriver-Job

Status: analysis / contract draft (Spec, kein Code)

## Ist-Zustand (belegt)

- **Enqueue-Freigabe**: `harw-ops/src/work_driver.rs:864-873` — `work_driver.enqueue` gibt
  einen dauerhaften Job für das aktuelle Goal frei.
- **Caller-Limits**: `work_driver.rs:878-898` und `work_driver.rs:761-792` — der Enqueue
  wendet die Caller-Spec an; Overrides dürfen nur verengen (größere Werte werden
  abgelehnt, siehe Schema-Beschreibung `work_driver.rs:267-269`).
- **Single-Attempt / kein Auto-Restart**: `work_driver.rs:989-1017` — ein Lauf läuft bis
  zum Ende des Budgets/der Iterationen; kein automatischer Neustart.
- **Sidecar / run_rounds**: `harw-ops/src/job_worker_work_driver.rs:767-823` — der
  Job-Worker führt die Runden (`run_rounds`) gegen den Job-Input aus.
- **Status / Stop**: `work_driver.enqueue` (`work_driver.rs:874`), `work_driver.status`
  (`work_driver.rs:1142`), `work_driver.stop` (`work_driver.rs:1311`) — Status ist
  read-only, Stop und Enqueue verlangen immer Freigabe.

## Belegte Lücke

- **Kein Feld für bestätigten Plantext/TODO-Umfang im Job-Input**:
  `work_driver.rs:245-254` (`WorkDriverEnqueueArgs` kennt nur `goal_id`, `plan_id`,
  `overrides`) und `work_driver.rs:353-378` (`WorkDriverJobInput` persistiert
  `schema_version`, `goal_id`, `plan_id`, `orchestrator_role`, `requested_by`, `spec`,
  Tenant-Scope) — der vom Nutzer bestätigte Plan-Umfang wird nicht mit dem Job
  persistiert.
- **Kein TUI-Befehl zum Start aus bestätigtem Plan**: `docs/guides/work-driver.md:67-82` —
  es gibt ausdrücklich keinen TUI-Slash- oder `harw`-Subcommand; Start nur über das
  Model-Tool `work_driver.enqueue` bzw. die Web-API, beide immer mit Freigabe. Ein
  Start aus einem bestätigten Plan (Plan-Modus) existiert damit nicht als
  Nutzerfläche.

## Zielmodell

1. **Full Mode hält den Plan-Modus erreichbar.** Der Nutzer kann im Full Mode in den
   Plan-Modus wechseln, einen Plan erarbeiten und bestätigen.
2. **Genau ein Enqueue nach expliziter Nutzerbestätigung.** Aus dem bestätigten Plan
   resultiert höchstens ein `work_driver.enqueue`; der Aufruf bleibt
   nutzerbestätigt (Approval-Regel unverändert).
3. **Umfang wird persistiert.** Der Job-Input (`WorkDriverJobInput`, `work_driver.rs:353-378`)
   erhält ein Feld für den bestätigten Umfang (Plan-Snapshot bzw. TODO-Liste), sodass
   der langlebige Job unabhängig vom späteren Plan-Zustand reproduzierbar ist.
4. **Kein zweiter Driver, kein automatischer Start.** Es gibt genau einen
   WorkDriver-Job; `work_driver.enqueue` wird nie stillschweigend aus `plan.exit`
   (oder einem anderen Pfad) getriggert — der Start ist ein expliziter,
   nutzerbestätigter Schritt.
5. **Bestehendes Verhalten bleibt erhalten.** Caller-Budgets (`work_driver.rs:761-792`,
   `:878-898`) und Single-Attempt/kein Auto-Restart (`work_driver.rs:989-1017`) werden
   nicht verändert.
6. **Langlauf-Sichtbarkeit.** Der Job-Status (`work_driver.status`,
   `work_driver.rs:1142`) macht Langlauf (Runden, Budgets, Fortschritt) sowie
   Stop (`work_driver.rs:1311`), Crash- und Resume-Grenzen für den Nutzer sichtbar.

## Handoff

- Implementierung des Zielmodells in `harw-ops/src/work_driver.rs` (Job-Input-Feld,
  Enqueue-Args) und `harw-ops/src/job_worker_work_driver.rs` (Sidecar konsumiert den
  Umfang in `run_rounds`, `:767-823`).
- Doku-Erweiterung in `docs/guides/work-driver.md` (Abschnitt „Starting a run",
  `:67-82`), sobald der Start aus bestätigtem Plan eine eigene Fläche bekommt.
- Kein Code, kein Commit in diesem Schritt; diese Datei ist der Vertrag, an dem die
  Umsetzung gemessen wird.

## Offene Punkte

- **TUI-Handoff-UI**: h22/h9 hängen an h7; die konkrete Nutzerfläche für den Übergang
  Plan-Modus → bestätigter Enqueue ist noch nicht festgelegt.
- **Form des persistierten Umfangs**: Plan-Revision (als solche referenziert und
  eingefroren) vs. konkrete Step-Liste erzeugt aus `harw-step-list` — beides
  legitimiert `WorkDriverJobInput`; die Wahl beeinflusst, wie der Sidecar den Umfang
  in Runden übersetzt, und ist vor Implementierung zu entscheiden.
