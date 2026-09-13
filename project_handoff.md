# Harwness — Project Handoff

**Stand:** 2026-07-14 UTC  
**Arbeitsziel:** Harwness zu einem eigenständigen Agent-Harness ausbauen, das echte Orchestrierung mit parallelen, langlebigen, gefenceten Worker-Jobs, sicheren Sandbox-/Capability-Grenzen, effizientem Kontextaufbau und lokalem Streamable-HTTP-MCP-Ingress bietet.

Dieses Dokument ist der operative Einstieg für die nächste Person/den nächsten Agenten. Es beschreibt den aktuellen Codezustand, was verifiziert ist, welche Arbeit offen bleibt und in welcher Reihenfolge sie sicher fortzuführen ist.

> **Aktualisierung nach dem ersten Handoff:** Der zwischenzeitlich offene Principal-Session-Patch ist fertig. `harw-mcp-server` hat nun grüne End-to-End-Tests für Cross-Principal-Session-Ablehnung sowie capability-filtered, autorisierte `tools/list`/`tools/call`-Ausführung. Auch `harw serve [--config-dir <DIR>]` lädt und validiert inzwischen die MCP-Listener-Konfiguration, erzeugt Authenticator, Principal-Registry und `DurableMcpSupervisor`. Die nachfolgenden Abschnitte, die diese Punkte noch als offen beschreiben, sind als historische Zwischenstände zu lesen; die konkrete Restarbeit ist unten in Abschnitt 12 zusammengefasst.

---

## 1. Aktueller Gesamtstatus

### Bereits fertig bzw. belastbar vorhanden

- Trusted server-side `ToolExecutionContext`, fail-closed ausgelegt.
- Managed child controller mit Sandbox-/Suggestion-Limits, parallelen Child-Runs, Leasing, Cancellation, Reaping, Tombstones und Capability-Snapshots.
- Durable actor-bound, single-use Approval-Store.
- Durable Child-Lease-Persistenz und Reconciliation für Zombie-Children.
- Kontextbudget-/Assembly-Grenzen und Capability-/Skill-/MCP-Katalog-Snapshots.
- Bubblewrap-basierte Sandbox in `harw-sandbox`.
- Streamable-HTTP-MCP-Client in `harw-core` (JSON und endliche SSE-Body-Verarbeitung; noch kein vollständiger persistenter SSE-Client).
- Standardport ist in `harw-core` als `DEFAULT_STREAMABLE_HTTP_MCP_PORT = 1337` angelegt.

### Aktiver Umsetzungsschwerpunkt

Der neue lokale MCP-Server liegt in **`harw-mcp-server`**. Er ist als eigene Crate im Workspace registriert und bindet ausschließlich Loopback-Adressen. Das Ziel ist **`127.0.0.1:1337/mcp`**.

---

## 2. Durable Jobs und Zombie-Schutz

### Implementiert

#### `harw-job-runtime`

- `LeaseToken { work_id, epoch, nonce }` mit Fencing.
- `StoredJob` enthält server-resolved Input, Scheduling-, Lease- und Completion-State.
- `JobState::Cancelled` ist terminal.
- `JobScope` ist immutable und persistiert:
  - `TenantId`
  - `WorkspaceId`
  - `ApprovalActor` als Submitter
- `JobCancellation` hält Durable Provenance für Supervisor-Cancellations.

#### `harw-session-store::JobStore`

- Atomare, per-Job gelockte JSON-Persistenz unter `jobs/records/` und `jobs/locks/`.
- `admit`, `get`, `list`, `claim`, `renew`, `complete`, `reconcile_expired`.
- `cancel(CancelRequest)` ist atomar und darf `Pending`, `Ready` und `Running` canceln.
- Bei laufender Arbeit:
  1. alte Lease erfassen,
  2. Lease entfernen,
  3. Fencing-Epoch erhöhen,
  4. `JobOutcome::Cancelled` und Cancellation-Provenance persistieren,
  5. alte Lease an den äußeren Child-Controller zurückgeben.
- Ein alter/zombie Worker kann nach Reclaim oder Cancellation nicht mehr renewen oder completen.

### Noch zu verbinden

`CancellationTransition.prior_lease` muss vom tatsächlichen Child-/Worker-Controller konsumiert werden:

1. Durable cancellation/fence schreiben.
2. Exakte laufende Child-Execution anhand der alten Lease lokalisieren.
3. Graceful-Cancel signalisieren.
4. Nach begrenzter Grace-Deadline hart beenden.
5. Ergebnis/Event auditierbar persistieren.

**Nicht** `JobStore` für Prozesssteuerung missbrauchen: der Store bleibt ausschließlich Durable-State-/Fencing-Boundary.

---

## 3. MCP Streamable HTTP Transport

### Dateien

- `harw-mcp-server/src/transport.rs`
- `harw-mcp-server/src/session.rs`
- `harw-mcp-server/src/auth.rs`
- `harw-mcp-server/src/supervisor.rs`
- `harw-mcp-server/src/lib.rs`

### Implementierter HTTP-Vertrag

- Exakter Pfad: `/mcp`.
- Bind lehnt nicht-loopback Socket-Adressen ab.
- Origin-Schutz gegen DNS-Rebinding:
  - kein `Origin` ist für native MCP-Clients akzeptiert,
  - gesetzter Origin muss `localhost` oder Loopback-IP sein.
- `POST` verlangt:
  - `Accept: application/json` **und** `text/event-stream`,
  - `Content-Type: application/json`,
  - maximal 64 KiB JSON-Body.
- Session-Fehler:
  - fehlende/falsche Protokoll-/Session-Angaben: `400`,
  - unbekannte, abgelaufene oder gelöschte Session: `404`.
- `initialize` verlangt JSON-RPC-ID und `params.protocolVersion = "2025-06-18"`.
- `notifications/initialized` ist eine echte Notification ohne ID und liefert leeres `202`.
- `DELETE /mcp` verlangt Session und Protokollversion, validiert beides, liefert leeres `204`.
- `GET`/unbekannte Methoden liefern aktuell `405` samt `Allow: GET, POST, DELETE`; SSE ist noch nicht implementiert.
- `tools/list` liefert gegenwärtig einen sicheren leeren Katalog.
- `serve_until(watch::Receiver<bool>)` verwaltet Connection-Tasks in einem `JoinSet`, statt sie detached zu lassen; Shutdown abortiert und joint Rest-Tasks.

### Tests

`transport::tests::streamable_http_session_lifecycle_is_enforced_over_tcp` bindet `127.0.0.1:0` und testet echten TCP-Handshake inklusive Init, Initialized-Notification, Tools-Katalog, Header-Fehler, Delete, stale session, Fremd-Origin und Shutdown.

---

## 4. MCP Authentication: aktueller Zustand und wichtigster nächster Fix

### Bereits implementiert

`harw-mcp-server/src/auth.rs` enthält:

- `McpAuthenticator`-Trait.
- `StaticBearerAuthenticator` mit serverseitig gelieferten Token-Bytes.
- konstante Arbeitsroutine für Tokenvergleich.
- `AuthenticatedPrincipal { principal_key }`; nur der opaque Key ist für Session-State vorgesehen.
- Transport prüft jede eingehende Anfrage vor Origin-/Protokollverarbeitung. Fehlende oder ungültige Bearer-Credentials liefern `401`.
- `BoundMcpListener::bind()` startet absichtlich mit leerem Authenticator und ist daher fail-closed.
- `BoundMcpListener::bind_authenticated(...)` ist der vorgesehen Composition-Einstieg.

`McpSession` enthält jetzt `principal_key`.

### Unterbrochene Änderung — **zuerst prüfen/abschließen**

Die letzte Änderung wurde beim Patchen unterbrochen, ist aber nach Sichtprüfung größtenteils im Worktree:

- `McpServerError::SessionPrincipalMismatch` wurde ergänzt.
- `request_session(...)` in `transport.rs` gibt nun `McpSession` statt `()` zurück.
- Nachfolgende Requests vergleichen `session.principal_key` mit dem gerade authentifizierten `principal_key`.
- Ein Mismatch wird in `session_error_response` als `400` eingeordnet.

**Vor jeder weiteren Architekturarbeit ausführen:**

```bash
rustfmt --edition 2024 harw-mcp-server/src/{auth,session,transport,lib}.rs
CARGO_HOME=/tmp/harw-cargo CARGO_TARGET_DIR=/tmp/harw-mcp-target RUSTC_WRAPPER= \
  cargo test -p harw-mcp-server
```

Danach einen expliziten Test ergänzen:

1. Session mit Bearer A initialisieren.
2. Folge-Request mit gültigem Bearer B für anderen Principal senden.
3. `400` erwarten; Session darf nicht verwendbar sein.

### Noch fehlende Runtime-Composition

Die HTTP-Authentisierung ist bewusst noch **nicht** in `harw-cli::serve_mcp()` verdrahtet. Der CLI-Start nutzt aktuell weiterhin `BoundMcpListener::bind(...)`; damit ist er nach der Auth-Änderung fail-closed und akzeptiert keine Anfrage. Das ist sicherer als ein offener Listener, aber noch nicht standalone-produktionsfertig.

Nächste Aufgabe:

1. `.harw/config.toml` via `discover_config` laden.
2. Konfiguration validieren.
3. `mcp_listener.principals` in serverseitige Principal-Records auflösen.
4. Credentials mit `harw-cli/src/mcp_auth.rs` auflösen.
5. `StaticBearerAuthenticator::new(...)` erzeugen.
6. `BoundMcpListener::bind_authenticated(...)` nutzen.
7. Den Listener nur starten, wenn `mcp_listener.enabled = true`.
8. Address aus `mcp_listener.listen_addr` verwenden, nicht hard-coden.

---

## 5. MCP Principal-Konfiguration

### Implementierte TOML-Struktur

In `harw-config/src/harness_config.rs`:

```toml
[mcp_listener]
enabled = true
listen_addr = "127.0.0.1:1337"
path = "/mcp"

[[mcp_listener.principals]]
id = "mia-local"
credential_ref = "env:HARW_MCP_TOKEN"
tenant = "mia"
workspace = "harwness"
job_capabilities = ["read_own", "cancel_own"]
```

Typen:

- `McpPrincipalToml`
- `McpJobCapabilityToml`
  - `read_own`
  - `read_workspace`
  - `cancel_own`
  - `cancel_workspace`

Validation in `harw-config/src/discovery.rs`:

- aktivierter Listener benötigt mindestens einen Principal;
- Credential-Refs müssen eindeutig sein;
- `(id, tenant, workspace)` muss eindeutig sein;
- Fähigkeiten dürfen nicht doppelt auftreten;
- Identifier sind konservative ASCII-Komponenten (`[A-Za-z0-9_-]+`).

### Credential Runtime Resolver

`harw-cli/src/mcp_auth.rs`:

- `env:NAME`: unterstützt.
- `file:PATH`: unterstützt und getrimmt.
- Leerwerte: Fehler.
- `keyring:` und `secrets:`: bewusst fail-closed, bis echte Backends an der Runtime-Grenze existieren.
- `constant_time_equal`: Test vorhanden.

**Wichtig:** `harw-config` löst nie Secrets. Keine Token loggen, persistieren oder in `McpSession` ablegen.

---

## 6. Supervisor-Grenze und MCP-Tools

`harw-mcp-server/src/supervisor.rs` enthält bereits:

- `McpPrincipal` mit Actor, Tenant, Workspace und Capability-Set.
- `McpRequestContext`.
- `McpSupervisor`-Trait.
- `DurableMcpSupervisor` für redigierten Jobstatus und autorisierte Cancellation.
- `McpJobStatus` / `McpCancellationReceipt` ohne Lease-Token, Sandbox-Pfade, Worker-ID, Secrets oder raw resolved Input.

Autorisierungsregeln sind getestet:

- `ReadOwn`/`CancelOwn`: exakter `ApprovalActor` plus Scope.
- `ReadWorkspace`/`CancelWorkspace`: nur gleicher Tenant und Workspace.
- Keine Cross-Tenant-/Cross-Workspace-Leaks.

### Noch zu implementieren

Erst nach fertiger CLI-Principal-Composition:

1. Authentifizierten `principal_key` zu `McpPrincipal` auflösen.
2. `McpRequestContext` ausschließlich serverseitig erstellen.
3. `BoundMcpListener` mit `Arc<dyn McpSupervisor>` erweitern.
4. `tools/list` nur die für den Principal zulässigen Tooldefinitionen zurückgeben.
5. `tools/call` implementieren, zunächst klein und sauber:
   - `harw_job_status` (read-only),
   - `harw_job_cancel` (Mutation, approval-/capability-gated).
6. Tool-Argumente strikt als JSON validieren.
7. Toolresultate als strukturierte, redigierte JSON-Ausgabe zurückgeben.
8. Kein `JobStore` direkt aus dem Transport exponieren.

Ein möglicher kleiner Katalog:

```text
harw_job_status — Fetch a redacted durable job status by work ID.
harw_job_cancel — Cancel one authorized durable job; never accepts lease tokens.
```

---

## 7. SSE, Events und Lifecycle: offen

Der Server besitzt noch kein Streamable-HTTP `GET`/SSE.

Beim Implementieren:

- `GET /mcp` nur mit `Accept: text/event-stream`.
- Kein JSON-RPC-Response in einem standalone SSE-Stream, außer explizit zulässigem Resume-Verhalten.
- Pro Session/Principal eine bounded Event-Queue; keine ungebundenen Buffer.
- Events aus Job-Reconciliation, Cancellation, Worker-Completion und Child-Zombie-Reaping speisen.
- Nur eine aktive Delivery eines Events; Disconnect-Cleanup.
- Event-Cursor/Revision aus `StoredJob.revision` verwenden, nicht flüchtige Indizes.
- Shutdown- und Connection-Timeouts ergänzen; aktuelles `JoinSet` ist Grundlage, aber Hyper graceful shutdown/slowloris deadlines fehlen.

---

## 8. CLI- und Konfigurationsarbeit: offen

Aktuelles `harw-cli/src/main.rs`:

- `harw serve` existiert,
- hard-codet bislang noch `127.0.0.1:1337`,
- lädt keine Listener-Config,
- nutzt nach Auth-Härtung den fail-closed Default-Bind und ist daher nicht nutzbar, bis die Composition fertig ist.

Zielinterface:

```text
harw serve [--config-dir <DIR>]
```

Erforderlich:

- `doctor` und `serve` sollen dieselbe validierte Konfiguration verwenden.
- `serve` muss `enabled=false` respektieren und verständlich fehlschlagen.
- Listener-Adresse, Session-TTL und Principal-Records sollen aus config stammen.
- Happy path und Kompatibilitätspfad testen.
- README-Beispiel für `[mcp_listener]` um `[[mcp_listener.principals]]` ergänzen.

---

## 9. Verifikation — bestätigte Ergebnisse

Die folgenden Tests liefen erfolgreich, bevor die **letzte unterbrochene Session-Principal-Patchsequenz** abgeschlossen wurde:

```bash
CARGO_HOME=/tmp/harw-cargo CARGO_TARGET_DIR=/tmp/harw-mcp-target RUSTC_WRAPPER= \
  cargo test -p harw-mcp-server
# 4 tests passed

CARGO_HOME=/tmp/harw-cargo CARGO_TARGET_DIR=/tmp/harw-mcp-target RUSTC_WRAPPER= \
  cargo test -p harw-job-runtime -p harw-session-store
# 13 tests passed

CARGO_HOME=/tmp/harw-cargo CARGO_TARGET_DIR=/tmp/harw-mcp-target RUSTC_WRAPPER= \
  cargo test -p harw-config
# 25 tests passed

CARGO_HOME=/tmp/harw-cargo CARGO_TARGET_DIR=/tmp/harw-mcp-target RUSTC_WRAPPER= \
  cargo test -p harw-cli
# 5 tests passed (vor Transport-Auth-Integration; enthalten den Resolver-Test)
```

Ein früher vollständiger `cargo test --workspace` war grün, aber **vor** den neueren MCP-Server-/Auth-/CLI-Änderungen. Dies nicht als aktuelle Gesamtverifikation ausgeben.

### Disk-Situation

- Der normale Projekt-`target/` ist groß.
- `/tmp/harw-mcp-target` wurde zuletzt gelöscht, um ENOSPC zu beheben; neue Fokus-Builds können es erneut füllen.
- Verwende gezielt `CARGO_TARGET_DIR=/tmp/harw-mcp-target` und räume diesen temporären Ordner nur bei Bedarf wieder weg.
- `RUSTC_WRAPPER=` ist notwendig.

---

## 10. Empfohlene Fortsetzungsreihenfolge

1. **Compile/Test-Recovery:** unterbrochene Principal-Bindung formatieren und testen; Cross-Principal-Session-Test ergänzen.
2. **CLI Composition:** Config laden, Principals + Credentials auflösen, `bind_authenticated` nutzen, fail-closed Serve-Verhalten testen.
3. **Principal Registry:** `principal_key -> McpPrincipal` in der Composition bauen und in Listener/Transport injizieren.
4. **Supervisor-Tools:** `tools/list` capability-filtered; `tools/call` für Status und Cancellation, strikte JSON-Schemas und redigierte Resultate.
5. **Child Cancellation Bridge:** durable `prior_lease` zum Child-Controller verbinden, Grace/force-kill und Audit/Event abschließen.
6. **SSE:** bounded job/event stream, Reconciliation-Events, disconnect and shutdown handling.
7. **Standalone Audit:** CLI, config, sandbox, job lifecycle, tool catalog, auth, SSE, Telegram ingress policy und Docs gegen die ursprüngliche Harness-Anforderung prüfen.
8. **Breite Verifikation:** zuerst paketweise, dann Workspace-Tests; `cargo fmt --check` beachten (historisch gab es unrelated Formatdifferenzen, daher zuerst geänderte Dateien rustfmt'en).

---

## 11. Wichtige Sicherheitsinvarianten

- MCP-`clientInfo`, Origin, Loopback-IP und Session-ID sind keine Autorität.
- Nur eine serverseitig aufgelöste Credential-Referenz erzeugt einen Principal.
- Token niemals loggen oder persistieren.
- `McpSession` speichert nur opaque Principal-Key, kein Secret.
- `StoredJob.scope` ist immutable; kein MCP-Argument darf Tenant, Workspace, Sandbox, Budget, Retry, WorkId oder Worker-ID bestimmen.
- Lease-Tokens niemals über MCP serialisieren.
- Store persistiert/fenced zuerst; Worker-Signalling geschieht danach außerhalb des Store-Locks.
- `keyring:`/`secrets:` nicht als `env:` oder Klartext degradieren.
- Keine Job-Mutation im MCP-Transport, bevor Auth-Composition und Capability-Context vollständig serverseitig vorliegen.

---

## 12. Korrigierter aktueller Stand und Restarbeit

### Seit dem ersten Handoff abgeschlossen

- Session ist an den authentifizierten `principal_key` gebunden; ein anderer gültiger Bearer kann die Session nicht übernehmen.
- `PrincipalRegistry` löst den opaque `principal_key` serverseitig zu `McpPrincipal` auf.
- Der Transport nimmt optional einen `McpSupervisor` an und erzeugt `McpRequestContext` nur nach Session-, Bearer- und Principal-Registry-Prüfung.
- `tools/list` ist capability-gefiltert.
- `tools/call` implementiert redigierte Status- und Cancellation-Operationen gegen `DurableMcpSupervisor`.
- `harw serve [--config-dir <DIR>]` nutzt die konfigurierte Listener-Adresse, verweigert fehlende Konfiguration und `enabled = false`, erzeugt Credential-Authenticator und Supervisor.
- Neu bestätigter Teststatus:

```bash
CARGO_HOME=/tmp/harw-cargo CARGO_TARGET_DIR=/tmp/harw-mcp-target RUSTC_WRAPPER= \
  cargo test -p harw-mcp-server
# 6 tests passed
```

Die beiden neuen End-to-End-Kontrakte sind:

1. `session_rejects_requests_authenticated_as_a_different_principal`
2. `tools_are_capability_filtered_and_authorized_end_to_end`

### Tatsächlich als Nächstes

1. `cargo test -p harw-cli -p harw-config -p harw-mcp-server` nach der neuen Composition laufen lassen und anschließend den relevanten Workspace-Sweep durchführen.
2. `README.md` / Beispielkonfiguration auf `[[mcp_listener.principals]]` und `harw serve --config-dir` aktualisieren.
3. Worker-/Child-Controller mit `CancellationTransition.prior_lease` verbinden, damit ein autorisierter `harw_job_cancel` nicht nur durable fenced, sondern die echte Execution zuverlässig stoppt.
4. Bounded SSE/Event-Delivery für Job-Lifecycle, Reconciliation und Cancellation implementieren.
5. Telegram als untrusted Channel-Boundary weiter an server-resolved Workdir-/Sandbox-/Permission-Policies anbinden; keine Pfade oder Rechte aus Chattext übernehmen.
6. Job admission als allowlisted Definition/structured args ergänzen; niemals rohe `StoredJob.input`, WorkId, Budget, Retry oder Sandbox-Spec aus MCP übernehmen.

### Live execution registry update

`harw-core/src/execution_registry.rs` ist jetzt implementiert und exportiert.
Die Registry keyed live execution controls by the complete `(work_id, epoch,
nonce)` fencing identity, unterstützt graceful cancellation mit bounded
force-abort und behandelt fehlende Bindings als nonfatal typed result.
Die vier fokussierten Registry-Tests sowie die bestehenden Core-Suites sind
grün (`15` Unit-, `7` Child-Controller- und `12` Turn-Loop-Tests).

Der verbleibende Composition-Hook ist explizit: Nach durable `JobStore::claim`
eine `ExecutionControl` registrieren, bevor das Child runnable wird; erst nach
terminaler Completion unregisteren. Im MCP-Cancellation-Sink ausschließlich
`CancellationTransition.prior_lease.token()` nach dem Store-Fence übergeben.
Nie nach Holder oder Session-ID routen.

### Important registry integration correction

A read-only lifecycle audit confirms that `JobStore::claim` is currently not
called by `ManagedAgentSpawner`: child handoff work uses `ChildLeaseStore` and
`SessionId`, while `JobExecutionRegistry` accepts `harw_job_runtime::LeaseToken`.
Do **not** fabricate a `SessionId -> WorkId` conversion or route by holder.

The next design must choose an explicit composition boundary:

- introduce a separate durable `DurableJobRunner` that claims `JobStore` jobs,
  registers the exact `JobClaim.token`, runs an `ExecutionControl`, completes
  with that same token, and unregisters only after durable completion; or
- extend the child-handoff schema/API with an explicit fencing identity and
  migrate `ChildLeaseRecord` before binding it to the registry.

The first option is the smaller safe standalone-job path. The second is needed
only when MCP cancellation is required to control existing child handoff work.

### Bounded lifecycle event bus update

`harw-mcp-server/src/events.rs` now provides a bounded per-session lifecycle
bus with typed `JobUpdated`, `CancellationRequested`, and `Reconciled` events,
monotonic sequence/timestamp metadata, subscriber cleanup, capacity/overflow
handling and lag reporting. It intentionally contains no secrets, lease
credentials, worker identity or raw job input. Three focused event-bus tests
pass. Transport `GET`/SSE wiring is still pending.

### Authenticated Streamable-HTTP GET/SSE update

`harw-mcp-server` now wires `McpEventBus` into authenticated `GET /mcp`.
The endpoint requires a valid bearer, initialized session, matching protocol and
bound principal, plus `Accept: text/event-stream`. It returns bounded SSE
frames with redacted sequence/revision lifecycle data, lag frames and cleanup
on disconnect/subscription drop. Response headers include SSE content type and
no-cache semantics. The raw TCP SSE integration test and all package tests pass
outside the restricted loopback sandbox.

Current MCP package verification: **12 tests passed**.

### Admission boundary audit

A read-only audit confirms `JobStore::admit(&StoredJob)` is persistence-only but
currently too authority-rich for direct MCP/Telegram exposure. No direct
`harw_job_submit` tool should call it. The safe next facade is a server-owned
`JobIntent { workspace_alias, role, task, source_event_id }` plus trusted
`AdmissionContext`; tenant, WorkId, budget, retry, sandbox, permissions,
capabilities and credentials must be resolved server-side from principal,
workspace registry, catalog snapshot and policy/channel intersection.

There is currently no typed configured workspace-registration catalog in
`HarnessConfig` (only `workspace_root`), and Telegram bindings are in-memory.
Admission therefore remains intentionally unexposed until startup can resolve
registered `(TenantId, WorkspaceId)` workspaces and durable pairing/replay
identity. Unpaired Telegram must never create a `StoredJob`.

### Telegram ingress audit

Telegram currently has only in-memory `tenant_bindings`; unpaired traffic falls
back to a `harw:unpaired` sentinel and must remain session-free. Pairing lacks
restart rebuild, expiry/channel validation, single-use consume/revoke and
atomic concurrent redemption. `InboundEvent.raw_event_id` is not durably
claimed before dispatch; no channel-scoped replay gate exists. The approval
callback is still a plain request/decision string rather than an opaque,
authenticated, single-use callback.

Before Telegram may submit `JobIntent`, add durable pairing/replay state:
`redeem_once(channel, code, actor, now)`, `lookup_binding`, `revoke`, and
`claim_once(channel, raw_update_id, received_at)`, with restart-safe records,
channel/scope checks and concurrent single-winner semantics. Never let the
sentinel tenant reach admission or StoredJob persistence.
