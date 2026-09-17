# Audit: Sandbox und Sicherheit

Prüfumfang: vollständiger Master `docs/planning/user-requirements-from-all-transcripts.md` (849 Zeilen) sowie exakt UR-45 bis UR-47 unter `### Sandbox und Sicherheit` (Master:630-679). Gegenstand war der tatsächliche Laufzeitcode einschließlich Worker-Definitionen, nicht nur die Anforderungsdokumentation. Es wurden keine Build-, Test- oder sonstigen verbotenen Ausführungsbefehle verwendet.

## Ergebnisübersicht

| Punkt | Status | Kurzbefund |
|---|---|---|
| UR-45 | teilweise | Profile und Bubblewrap-Bindungen sind modular; der vorgesehene Cargo-/Sonderworker-Pfad ist aber nicht als startbare Rolle integriert und der Runtime-Netzpfad bleibt leer. |
| UR-46 | teilweise | Fail-closed-Permit-Primitiven und kein direkter Sandbox-Schalter sind vorhanden; der Host-Freigabedialog ist nicht an den TUI-Renderer angeschlossen, Host wird nicht aus der Runtime-Konfiguration gewählt und Permitentscheidungen sind nicht als Auditereignisse persistiert. |
| UR-47 | teilweise | Enge TOML-Profile für Strict/Cargo/tmux/Host existieren; die Sonderworker sind absichtlich nicht registriert, ein Parent-Ausführungspermit ist nicht im Spawn-Kontext verankert und tmux-Schreibfreigabe ist nur deklariert. |

## UR-45 – Sandbox modular statt pauschal zu hart gestalten

**Status: teilweise** (Master:632-646).

**Codebelege**

- `harw-sandbox/src/profile.rs:3-9,18-26`: `SandboxProfile` ist vertrauenswürdiger Runtime-Input, kein Tool-Argument/CLI-Flag; es gibt `Strict`, `Cargo`, `Tmux` und `Host`, wobei Strict der Default bleibt.
- `harw-runtime/src/assembly.rs:367-425`: Die Runtime baut aus `[sandbox]` nur ein validiertes Cargo- oder tmux-Profil; bei Fehlern fällt sie auf Strict zurück. `Host` wird in dieser Auswahl nicht erzeugt.
- `harw-sandbox/src/cargo.rs:10-20,30-52`: Cargo trennt `Inspect`, `BuildOffline` und `Fetch`; Toolchain, Rustup- und Cargo-Home werden als absolute, kanonisierte Hostpfade geprüft.
- `harw-sandbox/src/bwrap.rs:297-349,372-397`: Bubblewrap nutzt immer `--unshare-net` und `--clearenv`; Cargo-Toolchain/Cargo-Home und Workspace werden gezielt, mit read-only bzw. beschreibbarer Bindung je Berechtigung, eingebunden. `Fetch` wird ohne Relay und nichtleeren `NetworkScope` abgewiesen (`:337-341`).
- `harw-runtime/src/sandbox.rs:20-26`: Die Runtime-Wurzelsandbox erhält derzeit immer leeren `NetworkScope`; `NetworkAccess` ist in der Entry-Reduktionstabelle nicht enthalten.
- `harw-registry-defaults/src/profile.rs:220-243`: `cargo-worker`, `sandbox-shell-worker` und `tmux-inspector-worker` sind trotz vorhandener Dateien ausdrücklich nicht in `role_names::ALL` enthalten; die Datei bezeichnet ihre Aufnahme selbst als offenen Befund (`:222-227`).

Damit ist die technische Modulgrenze sicher angelegt, einschließlich eines kontrollierten Offline-Cargo-Pfads. Im tatsächlichen Runtime-Aufbau gibt es aber keine workergebundene Profilauswahl und keinen erreichbaren Cargo-Worker; ein `Fetch`-Pfad kann wegen der leeren Runtime-Netzrechte nicht praktisch genutzt werden. Die Worker-TOMLs allein schließen diese Lücke nicht.

**Restlücke:** Cargo-/tmux-/Shell-Worker müssen aus der vertrauenswürdigen Worker-Definition heraus auswählbar und registriert sein. Für Cargo muss außerdem entschieden werden, wann der kontrollierte Proxy-Egress freigegeben wird; Strict darf dabei unverändert Default bleiben.

**Risikoarmer nächster Schritt:** Nur die Runtime-/Registry-Zuordnung für `cargo-worker` und `sandbox-shell-worker` ergänzen, zunächst ausschließlich `Inspect`/`BuildOffline` ohne Netz freischalten und die bestehende `Fetch`-Ablehnung beibehalten, bis ein explizit begrenzter Egress-Pfad an einen Entry gebunden ist.

## UR-46 – Sandbox-Deaktivierung nur durch intent- und bestätigungsgebundenen Ablauf

**Status: teilweise** (Master:648-665; der eigentliche zweistufige End-to-End-Ablauf ist offen).

**Codebelege**

- `harw-sandbox/src/process_permit.rs:1-7,42-70`: Ein `ProcessPermit` ist weder Tool-Argument noch konfigurierbarer Sandbox-Schalter; der Antrag bindet Sitzung, exakte Worker-Definition, exakten Befehl, kanonischen Workspace und Umgebung.
- `harw-sandbox/src/process_permit.rs:134-170,173-213`: Permits entstehen erst über `issue_after_local_approval`, haben TTL/Scope und werden bei `authorize` vollständig gebunden geprüft; Single-Execution wird atomar verbraucht.
- `harw-tool-shell/src/exec.rs:324-378,403-416`: Host-Ausführung verweigert sich ohne Ledger, Registry oder gültige Sitzungsgenehmigung. Die Sitzungsgenehmigung erzeugt anschließend pro exaktem Antrag einen gebundenen `SessionLease`.
- `harw-cli/src/cli.rs:346-361`: `harw sandbox` kennt nur `status`, `leases` und `revoke`; es gibt dort keinen `disable`-/`enable`-Schalter. `harw-sandbox/src/profile.rs:3-6` schließt Profilsetzung durch Tool/CLI ebenfalls aus.
- `harw-tui/src/host_permit_dialog.rs:264-300`: Der Dialog sendet eine Frage und trägt nur bei expliziter `approve`-Antwort eine Sitzungsfreigabe mit Lease-TTL ein; Timeout, Kanalfehler und Antwortabbruch werden abgelehnt.
- `harw-tui/src/host_permit_dialog.rs:56-71,111-121`: Es gibt aktuell keinen Renderer, der `HostPermitPromptReceiver::recv` pollt. Deshalb bleibt jede Host-Anfrage bis zum 300-Sekunden-Timeout unbeantwortet und scheitert faktisch immer.
- `harw-tui/src/runtime_root.rs:731-745,800-812`: `RootRuntime` und `build_root_runtime` verdrahten nur den normalen `ApprovalPromptReceiver`/`TuiApprovalHandler`; ein `HostPermitDialog` bzw. Host-Prompt-Receiver wird nicht angeschlossen.
- `harw-runtime/src/assembly.rs:380-425,1699-1708`: Die produktive Profilauswahl kennt nur Strict/Cargo/Tmux; Ledger und Registry werden zwar erzeugt, bleiben ohne `SandboxProfile::Host` ungenutzt. Zusätzlich plant `harw-tool-shell/src/exec.rs:411-461` auch nach Host-Autorisierung weiterhin einen Bubblewrap-Start. Das widerspricht dem Host-Worker-Versprechen `harw-registry-defaults/agents/host-process-worker.toml:1-6`, wonach direkt außerhalb von Bubblewrap ausgeführt werden soll.
- Audit/Secrets: `harw-secrets/src/audit/event.rs:1-5,68-84` modelliert unveränderliche, geheime-inhaltsfreie Auditereignisse; `harw-secrets/src/store.rs:195-228,313-327` persistiert jedoch Secret-Mutationen, nicht Host-/Sandbox-Permitentscheidungen. Im Host-Dialog werden nur nicht persistierte `tracing`-Events geschrieben (`harw-tui/src/host_permit_dialog.rs:173-180,281-294`). Der `command_hint` wird als Anzeigeinhalt unverändert geführt (`:149-154`); eine Redaktionsprüfung ist dort nicht vorhanden.
- `harw-sandbox/src/host_permit_session.rs:46-58,268-320` hält Zustimmung und gemerkte Permits nur im Prozess; `forget_session` entfernt die zugehörigen Ledger-Einträge nicht selbst, sondern liefert IDs zum separaten Widerruf zurück. `harw-cli/src/sandbox_cmd.rs:92-131` bestätigt, dass `leases`/`revoke` mit einem frischen Ledger arbeiten und keine laufende Sitzung erreichen.

Die Kernschutzrichtung ist korrekt: kein einfacher Schalter, Default-Deny, exakte Bindung, TTL und Scope. Die geforderte Nutzerinteraktion ist aber nicht funktionsfähig, weil die Host-Prompt-Seite nicht in die TUI-Laufzeit gelangt. Ebenso fehlt die auditierbare Entscheidungsspur; das vorhandene Secrets-Audit ist ein separater Store-Pfad und kein Host-Permit-Audit.

**Restlücke:** Intent-Erkennung, Host-Prompt, sichtbarer Scope/Laufzeitwert, explizites Ja/Nein, Session-Ende/Widerruf und ein redigiertes Auditereignis müssen in derselben laufenden Runtime verbunden werden. Ein `Host`-Profil muss außerdem entweder tatsächlich direkt ausführen oder aus dem Produktversprechen entfernt werden.

**Risikoarmer nächster Schritt:** Den vorhandenen Host-Prompt-Receiver in `RootRuntime` einhängen und zunächst nur eine explizite Ablehnung/Bestätigung mit kurzer, sichtbarer Session-TTL implementieren. Beim Sitzungsende `registry.forget_session` und anschließend `ledger.revoke` für alle IDs atomar im selben Besitzerpfad ausführen; Auditdaten nur als Session-/Worker-/Workspace-Referenz, Scope, TTL und Ergebnis, nie als Befehls- oder Secretinhalt, persistieren.

## UR-47 – Spezielle Ausführungsworker für Shell-/tmux-Fälle

**Status: teilweise** (Master:667-679).

**Codebelege**

- `harw-registry-defaults/agents/sandbox-shell-worker.toml:1-13,22-38,64-70`: enger Strict-Worker, nur `shell.exec`, kein Cargo/tmux/Host, `max_depth = 0`.
- `harw-registry-defaults/agents/cargo-worker.toml:1-19,24-40,66-72`: eigener Cargo-Worker mit ausschließlich `shell.exec` und `max_depth = 0`; das Profil ist als Runtime-Cargo-Modul beschrieben.
- `harw-registry-defaults/agents/tmux-inspector-worker.toml:1-17,22-38,64-70`: eigener tmux-Worker mit engem Toolprofil. `harw-sandbox/src/tmux.rs:19-27,36-58` unterscheidet Inspect/Write und validiert genau einen Unix-Socket; `harw-sandbox/src/bwrap.rs:352-359` bindet ihn an einen festen Sandboxpfad.
- `harw-registry-defaults/agents/host-process-worker.toml:1-17`: Host-Worker deklariert direkten Hostzugriff, `shell.exec` und fail-closed `ProcessPermit`.
- `harw-registry-defaults/src/profile.rs:220-243`: alle drei relevanten Sonderworker (`cargo-worker`, `sandbox-shell-worker`, `tmux-inspector-worker`) sowie `host-process-worker` fehlen aus der startbaren Rollenliste, obwohl ihre Definitionen gefunden werden.
- `harw-core/src/child_controller.rs:3304-3314,3376-3396,3450-3452`: Admission verlangt eine registrierte Rolle, prüft die geschlossene Delegationsmatrix und verhindert Sandbox-Eskalation. Das ist eine Parent-/Sandbox-Grenze, aber kein spezieller Ausführungspermit vor jedem Shell-Ausführungspfad.
- `harw-core/src/child_controller.rs:3520-3555,3570-3585` und `harw-core/src/session.rs:282-303`: an Kinder werden Tools und Sandbox-Rechte vererbt/geschnitten; `SpawnContext` enthält weder `SandboxProfile` noch `ProcessPermit`/Parent-Execution-Approval. Die Nachweisbarkeit „Parent hat diesen Ausführungsweg freigegeben“ ist damit nicht Bestandteil der Child-Admission.
- `harw-tool-shell/src/exec.rs:336-342,367-378` bindet Host-Anträge zwar an die feste Worker-Definition `host-process-worker@1`, aber nicht an eine Parent-Freigabe aus dem Spawn-Kontext. Für alle Profile ruft der Executor danach den Bubblewrap-Plan auf (`:437-461`).
- `harw-sandbox/src/bwrap.rs:299-304,483-492`: Sandbox-Prozesse laufen mit `--die-with-parent` und Kill-on-Drop/PID-Namespace; das verhindert verwaiste Prozesse, bietet aber keinen separaten langlebigen SSH-/tmux-Session-Manager. Eine langlebige Sitzung kann dadurch beim Ende des Workers beendet werden.

Die Dateidefinitionen und die niedrigprivilegierten Profile sind ein brauchbares Gerüst. In der realen Laufzeit sind die Sonderworker jedoch nicht startbar, und die zentrale Parent-Execution-Approval fehlt. Beim tmux-Write-Modus ist eine separate Zustimmung nur als Profilkommentar/Enum-Vertrag beschrieben, nicht über einen angeschlossenen Approval-Pfad erzwungen.

**Restlücke:** Sonderworker müssen registriert und anhand ihrer trusted Definition mit genau einem Profil assembliert werden. Jeder Shell-/Cargo-/tmux-Ausführungsweg braucht einen vom Parent geerbten, vor der Ausführung geprüften Grant; tmux-Write muss denselben expliziten Freigabepfad nutzen wie Host-Zugriff. Für langlebige Sitzungen fehlt eine kontrollierte Lebensdauer, die nicht an den kurzen Worker-Prozess gekoppelt ist.

**Risikoarmer nächster Schritt:** Zuerst nur `sandbox-shell-worker` und `cargo-worker` als `max_depth=0` registrieren und den Child-Grant um eine nicht erweiterbare, profilgebundene Ausführungsabsicht ergänzen. Tmux zunächst ausschließlich `Inspect` zulassen; Write und langlebige Session-Verwaltung erst nach angeschlossener UI-Freigabe und explizitem Shutdown-/Revoke-Pfad aktivieren.

## Sicherheits-Querschnitt: Egress

Der Egress-Unterbau ist restriktiv, aber im Runtime-Entry derzeit nicht freigeschaltet:

- `harw-egress/src/client.rs:1-20,110-150` deaktiviert Umgebungsproxies und Redirects; Allowlist und jede aufgelöste Adresse werden geprüft.
- `harw-egress/src/policy.rs:113-195` prüft URL, Host-Allowlist und Adressklassen; private/lokale Klassen bleiben standardmäßig ausgeschlossen.
- `harw-egress/src/proxy.rs:1-28,673-766` erlaubt nur Unix-Socket/SOCKS-CONNECT, erzwingt Policy/Timeouts/Limits und protokolliert Ziel/Ergebnis/Bytes ohne Nutzdaten.
- `harw-runtime/src/sandbox.rs:20-26` lässt derzeit trotzdem jede Runtime-Wurzelsandbox mit leerem Scope und ohne `NetworkAccess` entstehen.

Das ist sicherer Default, erklärt aber zugleich, warum der konfigurierte Cargo-`Fetch`-Pfad und netzabhängige Sonderworker aktuell kein nutzbarer Produktpfad sind.
