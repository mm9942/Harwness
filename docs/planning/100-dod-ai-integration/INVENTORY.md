# PL-100 — Inventar: Harw-DoD und seine Anbindung an Harwness

> **Status:** Befund (read-only erhoben). Stand: Code bei `2c131382`.
> **Methode:**
> - 12 unabhängige Leser, danach eine Synthese mit Gegenprüfung;
> - die wichtigsten Befunde hat der Orchestrator zusätzlich selbst im Code verifiziert (✔).
>
> **Status-Kürzel:**
> - **W**: implementiert und verdrahtet
> - **U**: implementiert, aber nicht verdrahtet
> - **S**: Stub
> - **P**: nur geplant
>
> Pfade sind relativ zum Repo-Root.

## A. Architektur heute

```
 KERNEL/HOST                          PRIVILEGIERTE PROBES (senden nur, handeln nie)
 tp:sched_process_exec/exit ─┐
 fentry:tcp_v4/v6_connect ───┴─► harw-probe-bpf  (CAP_BPF+CAP_PERFMON, in harw-dod.target)
 fanotify FAN_CLASS_NOTIF ─────► harw-probe-fs   (CAP_SYS_ADMIN, installiert, NICHT im Target)
                                     │ ein JSON-SecurityEvent je SEQPACKET-Datagramm (≤16 KiB)
                                     ▼ /run/harw/sentinel.sock (0770, Gruppe harw-ipc; SO_PEERCRED geloggt, nicht geprüft)
 /proc /sys cgroupfs Reports ──► harw-sentinel  (unprivilegiert, leeres CapabilityBoundingSet, Landlock best effort)
                                  10 Sensoren im Prozess → Sentinel (Health-Automat)
                                  → EvidenceBuffer-Ring (2048 Samples / 4096 Events, wird nie geleert)
                                  → alle 5 s: freeze() → harw-dod-rules (3 Regeln; NetworkScope::empty, baselines=[])
                                  → Finding<RuleChecked> → tracing::warn + Metrik + findings.jsonl v2 ; health.json
                                  ╳ FindingSpool (U) ╳ triage_record (U) ╳ harw-escalator (P) ╳   ← „Bruch 2"
 harw-warden (CAP_DAC_OVERRIDE+CAP_NET_ADMIN; per warden.enable abgeschaltet; v1-Beleg; kein Sender)
   → cgroup.freeze / cgroup.kill / nft-Tabelle "inet harw_warden"

 /var/lib/harw-dod/export (2750, Gruppe harw-dod-readers)
   └► harw-dod-client (read-only) ─┬► harw-ops: dod.status / dod.findings / dod.sensors (auto-approved)
                                   │     ├► 4 security-*-triage-Rollen (nur über delegate_wave des analysis-orchestrator)
                                   │     └► Root-Sessions mit AllWithModelTools
                                   ├► CLI `harw dod …`, TUI-Marke ⛨, DodView, Job-Badge
                                   └► Job↔Befund-Korrelation (E4)
   ├► security_signals-Kontextfragment (eigener Parser; opt-in, Vorgabe aus)
   └► harw-security-hub GET /v1/posture (eigener Leser; kein harwness-Client)
 Triage-Kind gibt ein SecurityVerdict zurück (harwness.security-verdict/v1)
   → wird in harw-core-bridge validiert → niemand verwendet es
```

**Ringe und TCB**
- Die Ringe: Detect = Sensoren, Orient = Regeln + Sentinel, Defend = Escalate + Warden. „Act" bleibt bewusst beim Menschen
  (`docs/design/harw-dod-charter.md:45-69`).
- Die TCB sind vier Binaries mit je einer Privilegklasse.
  - ✔ Der Warden läuft mit `CAP_DAC_OVERRIDE CAP_NET_ADMIN` (`deploy/systemd/harw-warden.service:78`).
  - ✔ `harw-dod.target` zieht nur Sentinel und probe-bpf hoch (`deploy/systemd/harw-dod.target:14`).
  - ✔ Warden und sein Socket haben keinen `[Install]`-Abschnitt und hängen an `ConditionPathExists=…/warden.enable`.

**Agenten können heute nur lesen.**
- Invariante S1: harw ruft nie Escalate oder den Warden auf (`harw-ops/src/dod.rs:26-34`).
- ✔ Der Sentinel nennt die Lücke selbst „Bruch 2": kein Produzent, keine Triage, kein Warden-Sender
  (`harw-dod/crates/harw-sentinel/src/findings.rs:35-49`).

## B. Sensorkatalog

| Sensor | Quelle | Daten | Privileg | Status | Anomalie-Potenzial / Einschränkung |
|---|---|---|---|---|---|
| exec (probe-bpf) | tp sched_process_exec | Pfad, uid, Cgroup-ID → `ProcessExec{path}`; pid/ppid/tgid/comm werden verworfen (`collect.rs:325-395`) | CAP_BPF, CAP_PERFMON | W, aber **keine Regel** | neue/seltene Binaries, Exec aus /tmp |
| exit (probe-bpf) | tp sched_process_exit | nur Header | dito | geladen, nie gemeldet | Prozesslebensdauer |
| tcp-connect / flow | fentry tcp_v4/v6_connect | ✔ `EgressFlow{dest IP, port}`, Actor{uid}, **ohne pid** (`harw-dod-flow/src/report.rs:157-183`) | dito | W, Regel egress-flow | Exfiltration, C2; ✔ Flut bei `egress_allow_cidrs = []` |
| fsmon (probe-fs) | fanotify FAN_MODIFY/CLOSE_WRITE | `FileWrite{path}`, uid, auid | CAP_SYS_ADMIN | Binary W, nicht aktiv; Platzhalter-Scope `/srv/harw/watched`; **keine Regel** | Persistenz; auid vermutlich immer None; Overflow wird nicht gemeldet (`source.rs:373-378`) |
| listener | /proc/net/{tcp,udp}{,6} + /proc/pid | `ListenerOpened{port}`, voller Schnappschuss je Poll | keins | W, **keine Regel**; auid immer None (`owner.rs:249`) | Backdoor-Listener |
| cpu, memory, netcounters | /proc/stat, meminfo, net/dev | kumulative Zähler | keins | W | Miner, Leck, Exfiltrationsspitze; **keine Raten, keine Baselines** |
| thermal, blockio, gpu | /sys/class/*, /sys/block | °C, I/O-Zähler, GPU-Last/VRAM | keins | W, vermutlich blind | ✔ `ReadScope::from_roots` ohne Alias-Root (`sensors.rs:252,279,295`); **Host-Prüfung nötig** |
| cgroup | direkte Kinder der Cgroup-Root (≤16) | memory.current/max, pids, cpu | keins | W | Fork-Bombe, OOM; `harw-job-*` unsichtbar |
| workspace-drift | Cargo-Graph und Lockfile | `StructureDrift` | keins | W, unter systemd wirkungslos (siehe H5) | Supply-Chain-Drift |
| scanreport | `<home>`-Reports *.json/*.sarif | `StructureDrift` (Hardness Observed erzwungen) | keins | W | eine kaputte Datei blendet den ganzen Sensor (`sensor.rs:211-213`) |
| container | libpod/docker/crio-Scopes | Cap-Bits, privileged, seccomp, NNP | keins | U | privilegierte oder fremde Container |
| authlog + netlink audit | NETLINK_AUDIT | `AuthEvent{outcome}`, uid/auid | CAP_AUDIT_READ | U; `harw-probe-netlink` P | Login-Anomalien; der Parser erwartet das auditd-Dateiformat |
| procmon (alt) | — | — | — | U (durch probe-bpf abgelöst) | — |

✔ Der Sentinel registriert 10 Sensoren (`sensors.rs:313-324`). Dokumente, die „neun" nennen, sind veraltet.

## C. Daten- und Befundmodell

**Events und Samples**
- `SecurityEvent{sensor, observed_at, actor: Option<Actor{uid, auid, cgroup}>, kind}` mit geschlossenem `EventKind`:
  ProcessExec, FileWrite, EgressFlow, ListenerOpened, AuthEvent, StructureDrift, SensorDegraded
  (`harw-dod-signals/src/event.rs:241-304`).
- `HostSample{sensor, observed_at, metric, value: f64}`.

**Typestate der Befunde**
- `Finding<Raw|RuleChecked|Triaged>`. Die Felder sind `pub(crate)`, deshalb kann nur harw-dod-rules Befunde erzeugen
  (`finding.rs:170-265`).
- `FindingKind{RuleTriggered, Anomaly}`, Severity Info..Critical, `Hardness{Observed, Correlated, Inferred}`,
  `Verdict{Confirmed, FalsePositive, NeedsReview}`.

**Über Prozessgrenzen**
- `FindingRecord`: versioniert, `deny_unknown_fields`, BLAKE3-Digest (kein MAC).
- Er enthält die volle `SecurityEvidence` und ist damit das sensibelste Artefakt (`finding.rs:797-990`).
- `triage_record()` berechnet den Digest neu und übernimmt die Severity aus dem Record, nie vom Agenten (`finding.rs:1135-1163`).
- Das alte, ungebundene `triage()` ist noch öffentlich und wird von escalate re-exportiert.

**Verdict des Agenten**
- `SecurityVerdict{finding, bound_evidence: ContentDigest, classification, severity, rationale, suggested_response: Monitor|Escalate|Contain (ohne Ziel), issued_by, issued_at}`
  (`verdict.rs:170-203`).

**Bereinigung entlang des Wegs**
1. Regeln schreiben Zusammenfassungen. Drift-`detail` kann angreiferkontrollierten SARIF-Text enthalten.
2. Export: bereinigt, gekappt auf 512 B.
3. Client: `rule_id` nur `[A-Za-z0-9._:-]` und ≤128 B; Zeilen ≤16 KiB; Tail ≤256 KiB; keine Symlinks.
4. Ops: jedes Feld gekappt auf 256 Zeichen; Markierung „unverified source" für Exporte, die nicht vom System stammen.

**Ablage**

| Ablage | Inhalt | Aufbewahrung |
|---|---|---|
| Ring | Events/Samples | nur im Speicher |
| `findings.jsonl` v2 | Befundzeilen | 0640, rotiert bei 16 MiB, eine Generation |
| `health.json` | Sensor-Gesundheit | atomar je Zyklus |
| Telemetrie-JSONL | Metriken | 10 MiB × 20, niemand liest sie zurück |
| Journal | ✔ jedes Event in voller Debug-Ausgabe (`main.rs:920-921`) | System-Journal |
| `FreezeStore` | Freeze-Einträge | `.active.json` / `.resolved.json` |

Kein Befund verweist auf das auslösende Event. Die Scope-Zuordnung wird nachträglich abgeleitet, und nur wenn sie eindeutig ist.

## D. Abwehr- und Aktionskatalog

| Aktion | Ausführer | Privileg | Gating | Heute durch KI auslösbar? |
|---|---|---|---|---|
| FreezeCgroup / ReleaseCgroup | Warden `CgroupV2Executor` (`executor.rs:180-253`) | DAC_OVERRIDE (+NET_ADMIN beim Release) | ab `RuleTriggered`; v1-Beleg | **nein**, es gibt keinen Sender |
| IsolateNetwork | `NftNetworkIsolator` (nft als Subprozess) | NET_ADMIN | nur `Escalated` | nein |
| KillProcessTree | schreibt `cgroup.kill` | DAC_OVERRIDE | nur `Escalated`; `kill_requires_human` **nicht durchgesetzt** | nein |
| `authorize_freeze` / `_thaw` / `_stage_gated` | harw-dod-escalate (`freeze_ops.rs:165,257,313`); liefert seit WP 3.0 ein unsigniertes `AuthorizedAction`, keinen v1-Beleg (signiert erst `harw-escalator`, WP 3.2) | — | die Ladder (`harw-dod-ladder`) verlangt `Confirmed`; keine echte Regel erzeugt Critical | nur in Tests aufgerufen |
| `process.kill` | harw-killer (pidfd, SIGKILL) | gleiche euid | `ExecuteProcess` + ALWAYS_ASK, **außer unter FullAccess** | **ja**, nur im Full-Profil |
| `job.stop` | harw-tool-job | eigene Jobs | Eigentum | ja |
| `shell.exec` + `request_host` | Host-Eskalation | User | TUI-Permit; `SessionLease` gilt global (`escalation.rs:389-394`) | ja, nur in der TUI |
| `host.sudo_exec` | `/usr/bin/sudo -k --` | root | zwei Rollen, ALWAYS_ASK, TUI-Dialog | ja, nur in der TUI |

**Warden-Autorisierung**
- ✔ Der Produktionseingang prüft nur den v1-`AuthorizationProof` (`harw-dod-warden/src/warden.rs:230-231`).
- ✔ Die Proto-Crate selbst nennt v1 fälschbar (F-001, `proof.rs:3-7`).
- ✔ SO_PEERCRED wird nur geloggt (`harw-warden/src/ipc.rs:17-33`).
- v2 `SignedAuthorization` (keyed BLAKE3, TTL ≤120 s, Nonce, Präfix-Policy) ist U: Der Nonce-Ledger liegt nur im Speicher, und Schlüssel werden nicht geladen.

**Config-Knöpfe ohne Wirkung**
- `kill_requires_human`, `auto_freeze` und `proof_key_dir` werden nur geparst (`dod_cmd.rs:108`).
- `allowed_cgroup_prefixes` nutzen nur die Doctor-Prüfungen der CLI.

## E. Agenten-Anbindung heute

**Werkzeuge**
- `dod.status`, `dod.findings` und `dod.sensors`: Observer, nur lesend, auto-approved (`harw-ops/src/dod.rs:2054-2167`).
- Filter von `dod.findings`: severity, rule, since_hours, limit 1–200.

**Rollen**
- Vier Rollen `security-{egress,baseline,structure,endpoint}-triage`:
  - `NoTools` plus die drei `dod.*`, ReadOnly-Reducer;
  - 40k Tokens, 20 Calls, 120 s, `max_depth` 0;
  - Rückgabe `harwness.security-verdict/v1`.
- Erreichbar nur, wenn ein Modell im `analysis-orchestrator` delegiert.
- `intel-scout` und `context-steward` sind Sackgassen: nicht in `role_names::ALL`, kein `[return]`. **Behoben (O11):** Beide stehen in `role_names::ALL` (`INTEL_SCOUT`, `CONTEXT_STEWARD`) und deklarieren `[return] contract = "harwness.return.envelope@1"`, nie ein Verdict. Profil `NoTools`, Reducer `ReadOnly`, `max_depth` 0, kein `dod.*`, kein Skill-Katalog. Im System-Prompt tragen sie eine eigene Rollenbeschreibung (`profile::role_description_for_role`), nicht die Triage-Beschreibung von `NoTools`. `intel-scout` bekommt sein einziges Werkzeug `deps.locked` rollengebunden (`profile::lockfile_tools_for_role`, montiert in `harw-runtime/src/children.rs`); `context-steward` hat kein Werkzeug. Delegationsziel eines Orchestrators sind beide noch nicht (`analysis-orchestrator.toml` `[delegation].targets`, Familie `security` `[workers]`).

**Signale im Kontext**
- `security.signals`: High/Critical je Regel über 24 h, opt-in.
- ✔ Eigener Parser: Zeilen mit Zeitstempel in der Zukunft werden mitgezählt (`harw-runtime/src/security_signals.rs:73`).
- Der Skill `harw-dod` wird per Stichwort injiziert. Child-Sessions bekommen seit WP 1.5 einen SkillInjector. **Behoben (WP 1.5):** Die Fabrik baut bei jeder Admission eines Kindes, das den Skill-Katalog bekommt, wie für eine Wurzel-Sitzung einen eigenen `SkillInjector` (`RuntimeChildRegistryFactory::child_skill_injector` in `harw-runtime/src/children.rs`). Sein Werkzeug-Filter kennt die Werkzeuge, die das Kind höchstens aufrufen kann: Registry, geschnitten mit `[tools]` der (geklemmten) Definition und mit den Rechten des Elternteils (`ParentGrant::permissions`; die Kind-Sandbox ist nie weiter). Weder der Reducer der Basisrolle noch die Eltern-Registry (`ParentGrant::tools`) schneiden: der `test-engineer` (Basis `analyst`, Reducer ohne `ExecuteProcess`) hält `cargo.test_one` und bekommt `cargo-tools`, auch wenn der Elternteil das Werkzeug nicht registriert. Abstrakte Fähigkeits-Labels der Skills (`filesystem.read`, `filesystem.write`, `process.execute`) gelten als aufrufbar, wenn ein aufrufbares Werkzeug ihr Recht trägt; Labels mit Schreib- oder Ausführungsrecht gelten all-of (`ToolAvailability::with_required`), ein lesendes Kind bekommt also keinen Skill, der `filesystem.write` nennt. Fest gebundene Skills der Rolle spielt er nie ein zweites Mal ein (`SkillInjector::with_excluded_skills`). Benutzerdefinierte Agenten laufen über denselben Weg. Die Triage-Rollen, `intel-scout` und `context-steward` stehen in `SKILL_CATALOG_EXCLUDED_ROLES` und bekommen keinen Injektor, ebenso benutzerdefinierte Agenten über diesen Rollen (geprüft wird die Basisrolle); `harw-dod` bleibt der fest gebundene Skill der Triage-Rollen. `child_session_observers` übergibt ihn in `ChildSessionObservers::skill_injector`; `admit_inner` schneidet ihn nach `narrow_base_activation` mit `SkillInjector::narrowed_to_callable` auf die Kind-Aktivierung (Tests `harw-core/src/child_controller/tests/skill_injector_handover.rs`).

**Trigger:** keine. Es gibt keinen Watcher, keinen Cursor, kein Dedup und keinen Router von Regel zu Rolle.

**Was fehlt**
1. ✔ `finding_view` hat keinen Digest (`dod.rs:1263-1275`), `binds()` verlangt aber einen.
2. ✔ Der Skill erwähnt `SecurityVerdict` nicht.
3. ✔ Die Rollen nutzten `program = "triage"` (Fehler-Triage, `agents/context-programs/triage.toml:1-7`). **Behoben (WP 1.3):** Alle vier binden jetzt das Kontextprogramm `security-triage` (`harw-registry-defaults/agents/context-programs/security-triage.toml`; `program = "security-triage"` in `security-egress-triage.toml:139`, `security-baseline-triage.toml:139`, `security-structure-triage.toml:137`, `security-endpoint-triage.toml:155`).
4. ✔ Keine Verdict-Senke: `TriageSubmission` hat weder Produzent noch Konsument, und `harw-escalator` existiert nicht.
5. U: Plan-Bridge `security_bridge`, Uplink `FindingSummary`/`HealthBeat`, Hub `/v1/posture`.
6. Kein `dod.doctor`, `dod.chain`, `dod.metrics` und kein Cursor-Werkzeug.
7. ✔ Von der Fassade `harw-dod` hängt keine Crate ab; `FindingSpool` wird nur in harw-dod-sentinel referenziert.

## F. Prozesssteuerung und Datenabruf für Agenten

**Lesend**
- `sys.*` (12 /proc-Werkzeuge, cmdline/env maskiert, nur im Full-Profil).
- ✔ `process.list` (argv **unmaskiert**, `harw-killer/src/api.rs:297`).
- `job.logs` und die drei `dod.*`.
- Es gibt kein Journal/Syslog-Werkzeug und keinen Cgroup-Filter für `sys.ps`. **Teilweise behoben (Owner-Befund 4505015316):** `sys.ps`, `sys.pgrep` und `sys.top` filtern per `ScopePolicy` (`scope = "own"` als Vorgabe). Die Montage baut den `SysToolProvider` aus der geteilten Freigabe-Zelle (`scope = "all"` nur unter Full Access) und nimmt die bestätigte delegierte Job-Wurzel (`JobWiring::delegated_cgroup_root`) in den eigenen Umfang auf (`harw-registry-defaults/src/profile.rs`, `sys_tool_provider`). Ein Journal-Werkzeug fehlt weiter.

**Ändernd:** siehe D. Für Agenten gibt es nur SIGKILL auf Prozesse derselben uid, kein TERM. Es gibt kein renice, kein SIGSTOP und keinen Cgroup-Freeze.

**Rollen passen nicht:** `uia-shell-worker` und `host-process-worker` haben Shell, sudo und Jobs, aber weder `process.*` noch `sys.*`.

**Lücke zwischen Befund und Ziel:** Eine `FindingLine` hat weder pid noch exe noch uid. Der Cgroup-`scope` ist der einzige Griff, und er passt zum Warden, nicht zu `process.kill`.

## G. Pläne und Stand

**Charter, Dekomposition, Integration**
- Erledigt: Ringe, Invarianten C1–C8, Privileg-Gates (`gate_edges`, `gate_privileges`, `gate_warden`).
- Veraltet: D7 und die C-Build-Gates stehen als offen, existieren aber.
- Als erledigt geführt, aber fehlend: `sensor_parse_errors_total`, `warden_unaudited_actions`, Journal-Backend für authlog.

**Abweichungen vom Authority-Dokument**
- BPF ist in C geschrieben statt in Rust/aya.
- `install` aktiviert und startet Dienste.
- Der Toolchain-Schritt ruft `apt-get` auf.
- Ausgeliefert wird `active_profile = "host"`.

**PL-96 W01 (P0, offen):** v2-signierte Anfrage auf dem echten Pfad, kein v1-Eingang mehr, ein Nonce-Ledger, der Neustarts übersteht
(`docs/planning/96-capability-closure/05-implementation-waves-and-gates.md:26`).

**PL-97 (in Umsetzung)**
- Laut §9.1 gelandet: D1–D5, B1–B5, C1–C5, M1–M3, E1–E5, E3b/c. X1b fehlt.
- Die Kopfzeile behauptet, E4 fehle. Das ist veraltet, E4 ist im Sentinel vorhanden.
- Offen nach §9.2:
  - G1 (Tool-Budget);
  - E4-Provenienz bei mehreren Cgroups und die `≥`-Badge;
  - Children unconfined;
  - `harw serve`/`harw web` ignorieren `[jobs] cgroup_root`;
  - statisches `config suggest`;
  - `--grant-read` fehlt;
  - `permission_request!` ohne Konsument;
  - SkillInjector für Children: behoben (PL-100 WP 1.5, siehe E);
  - `install` aktiviert automatisch.

**Observability**
- Die Sinks (File/Prom/OTLP) existieren.
- Das Gate für den `security.*`-Export wurde nie freigegeben.
- Die Runtime emittiert an `NullSink`, die Null-Zähler fehlen, der S7-Audit-Spiegel ist nicht eingeplant.

## H. Größte Lücken und Risiken (gerankt)

1. **Fälschbarer Warden-Eingang ohne Peer-Allowlist** (✔ siehe D). Sobald der Warden aktiv ist, kann jedes Mitglied von `harw-warden-clients` jede Cgroup beenden.
2. **Befund-Flut.**
   - ✔ `freeze` leert den Ring nicht (`buffer.rs:329-333`).
   - ✔ `poll_once` wertet den ganzen Ring je Zyklus aus (`main.rs:926-933`).
   - ✔ Die Regel stempelt `ctx.now` (`egress_flow.rs:111`).
   - ✔ Der leere `NetworkScope` erlaubt nichts (`findings.rs:226`, `harw-dod/config.toml:11`).
   - Folge: Jede ausgehende Verbindung wird alle 5 s erneut zu einem High-Befund mit neuer ID.
3. **Kein Dispatch, keine Verdict-Senke, kein Binden** (siehe E).
4. **Dünne Regelbasis:**
   - ✔ nur drei Regeln;
   - ProcessExec, FileWrite, ListenerOpened, AuthEvent und SensorDegraded haben keine Regel;
   - die Baselines sind leer;
   - Zähler werden nie zu Raten.
5. **Sentinel-Landlock vermutlich wirkungslos.** ✔ `--workspace-root` steht auf „." (`cli.rs:169`), die Unit setzt kein `WorkingDirectory`, und `default_roots`
   fügt den Workspace als Leseroot hinzu (`sandbox.rs:360`). Die Lese-Sandbox umfasst damit „/".
6. **Fälschbare Sensor-IPC speist Text in LLM-Kontext.** ✔ SO_PEERCRED wird nicht geprüft (`harw-sentinel/src/ipc.rs:174`).
7. **Korrelation geht verloren:** pid/ppid/comm/euid werden verworfen, ✔ Flows haben keine pid, und Befunde haben keine Provenienz.
8. **Blinde Sensoren:**
   - auid fehlt bei probe-fs;
   - thermal, blockio und gpu sind vermutlich blind;
   - der fanotify-Overflow wird nicht gemeldet;
   - der authlog-Parser passt nicht;
   - die Kürzung ist umgehbar.
9. **Freigabe-Knöpfe ohne Wirkung.** `ApprovalActor` gibt der Aufrufer selbst vor (`freeze_ops.rs:145-151`).
10. **Missbrauchbare Host-Pfade:** ✔ `SessionLease` gilt global, und ✔ `process.kill` fragt unter FullAccess nicht nach und zeigt argv.
11. **Sensibles Journal, kein manipulationssicheres Audit.** ✔ `main.rs:920-921`. Das Warden-Audit läuft nur über tracing.
12. **BPF-Manifest:** Der SHA-256 wird zur Laufzeit nie geprüft.
13. **Abweichende Parser.** ✔ `security_signals.rs:73` gegenüber harw-dod-client.
14. **Ungetestete privilegierte Pfade:** Es gibt keinen echten Test für BPF, fanotify, Landlock, Warden-IPC oder Escalate→Warden.

**Aufgelöste Widersprüche zwischen den Lesern**
- Es sind 10 Sensoren, nicht 9.
- Der Warden hat DAC_OVERRIDE und NET_ADMIN.
- E4 ist gelandet.
- Flows haben keine pid.
- EgressFlowRule feuert, und zwar bei jedem Egress und dazu wiederholt.
- `allowed_cgroup_prefixes` wird nur diagnostisch genutzt.
